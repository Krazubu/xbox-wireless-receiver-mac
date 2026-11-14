// SPDX-License-Identifier: GPL-2.0-or-later
//! Adapter event loop: USB messages, 802.11 frames, pairing and clients.
//!
//! Message and frame handling ported from xone (transport/dongle.c),
//! Copyright (C) 2021 Severin von Wnuck-Lipinski.

use std::{
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use nusb::{
    transfer::{Bulk, In},
    Endpoint,
};

use crate::{
    gip::{self, Event, GamepadState},
    hid::{Rumble, VirtualGamepad},
    mt76::*,
};

const MAX_CLIENTS: usize = 16;
const PAIRING_TIMEOUT: Duration = Duration::from_secs(60);
const LEN_CMD_PKT: usize = 2048; // xone uses 0x654; macOS wants a multiple of 512
const LEN_WLAN_PKT: usize = 0x8400;
const RXWI_LEN: usize = 32;

const EVT_BUTTON: u32 = 0x04;
const EVT_PACKET_RX: u32 = 0x0c;
const EVT_CLIENT_LOST: u32 = 0x0e;

struct Slot {
    address: MacAddr,
    gip: gip::Client,
    last_input: Option<GamepadState>,
    coverage: Coverage,
    /// Virtual HID gamepad seen by macOS (None if it could not be created).
    hid: Option<VirtualGamepad>,
    guide: bool,
    /// Firmware version from the GIP announce, e.g. "5.23.6.0".
    firmware: String,
}

impl Slot {
    fn send_hid(&self) {
        if let Some(hid) = &self.hid {
            let r = crate::hid::report(&self.last_input.unwrap_or_default(), self.guide);
            if let Err(e) = hid.send(&r) {
                log::warn!("{e:#}");
            }
        }
    }
}

/// Tracks which controls have been seen, to check that every one works.
#[derive(Default)]
struct Coverage {
    seen: Vec<&'static str>,
}

const CONTROLS: [&str; 27] = [
    "A", "B", "X", "Y", "Up", "Down", "Left", "Right", "LB", "RB", "LS", "RS", "Menu",
    "View", "Share", "Guide", "LT", "RT", "L-left", "L-right", "L-up", "L-down", "R-left",
    "R-right", "R-up", "R-down", "(all done)",
];

impl Coverage {
    fn mark(&mut self, control: &'static str) {
        if self.seen.contains(&control) {
            return;
        }
        self.seen.push(control);
        let left: Vec<&str> = CONTROLS[..CONTROLS.len() - 1]
            .iter()
            .copied()
            .filter(|c| !self.seen.contains(c))
            .collect();
        if left.is_empty() {
            log::debug!(">>> OK {control}. All {} controls work!", CONTROLS.len() - 1);
        } else {
            log::debug!(">>> OK {control}. Remaining ({}): {}", left.len(), left.join(" "));
        }
    }

    fn update(&mut self, s: &GamepadState) {
        const BUTTONS: [(u16, &str); 14] = [
            (1 << 2, "Menu"),
            (1 << 3, "View"),
            (1 << 4, "A"),
            (1 << 5, "B"),
            (1 << 6, "X"),
            (1 << 7, "Y"),
            (1 << 8, "Up"),
            (1 << 9, "Down"),
            (1 << 10, "Left"),
            (1 << 11, "Right"),
            (1 << 12, "LB"),
            (1 << 13, "RB"),
            (1 << 14, "LS"),
            (1 << 15, "RS"),
        ];
        for (m, n) in BUTTONS {
            if s.buttons & m != 0 {
                self.mark(n);
            }
        }
        if s.share {
            self.mark("Share");
        }
        // Triggers range 0..=1023; sticks need a near-full deflection.
        if s.trigger_left > 900 {
            self.mark("LT");
        }
        if s.trigger_right > 900 {
            self.mark("RT");
        }
        const FULL: i16 = 28000;
        let axes = [
            (s.stick_left.0, "L-left", "L-right"),
            (s.stick_left.1, "L-down", "L-up"),
            (s.stick_right.0, "R-left", "R-right"),
            (s.stick_right.1, "R-down", "R-up"),
        ];
        for (v, neg, pos) in axes {
            if v < -FULL {
                self.mark(neg);
            }
            if v > FULL {
                self.mark(pos);
            }
        }
    }
}

/// Messages for the event loop, from the USB readers and the HID callbacks.
enum Msg {
    Usb(Vec<u8>),
    ReaderFailed,
    Rumble { wcid: u8, rumble: Rumble },
}

pub struct Dongle {
    mt: Mt76,
    clients: [Option<Slot>; MAX_CLIENTS],
    pairing_until: Option<Instant>,
    tx: Sender<Msg>,
    rx: Option<Receiver<Msg>>,
}

fn spawn_reader(mut ep: Endpoint<Bulk, In>, len: usize, tx: Sender<Msg>) {
    thread::spawn(move || {
        for _ in 0..8 {
            let buf = ep.allocate(len);
            ep.submit(buf);
        }
        loop {
            let Some(c) = ep.wait_next_complete(Duration::from_secs(3600)) else {
                continue;
            };
            if let Err(e) = c.status {
                log::error!("bulk IN {:#04x} failed: {e}", ep.endpoint_address());
                let _ = tx.send(Msg::ReaderFailed);
                return;
            }
            if c.actual_len > 0 && tx.send(Msg::Usb(c.buffer[..c.actual_len].to_vec())).is_err() {
                return;
            }
            let mut buf = c.buffer;
            buf.clear();
            buf.set_requested_len(len);
            ep.submit(buf);
        }
    });
}

/// 802.11 header length for management and data frames (None for others).
fn hdr_len(fc: u16) -> Option<usize> {
    let order = fc & 0x8000 != 0;
    match fc & FC_FTYPE_MASK {
        FC_FTYPE_MGMT => Some(24 + if order { 4 } else { 0 }),
        FC_FTYPE_DATA => {
            let mut len = 24;
            if fc & 0x0300 == 0x0300 {
                len += 6;
            }
            if fc & 0x0080 != 0 {
                len += 2;
                if order {
                    len += 4;
                }
            }
            Some(len)
        }
        _ => None,
    }
}

impl Dongle {
    pub fn new(mt: Mt76) -> Self {
        let (tx, rx) = mpsc::channel();
        Self { mt, clients: Default::default(), pairing_until: None, tx, rx: Some(rx) }
    }

    pub fn run(mut self) -> Result<()> {
        let rx = self.rx.take().expect("run called once");
        spawn_reader(self.mt.in_endpoint(EP_IN_CMD)?, LEN_CMD_PKT, self.tx.clone());
        spawn_reader(self.mt.in_endpoint(EP_IN_WLAN)?, LEN_WLAN_PKT, self.tx.clone());

        log::info!("initializing radio...");
        self.mt.init_radio().context("radio init")?;
        // Like xone: start in pairing mode so already-paired controllers reconnect.
        self.set_pairing(true)?;
        println!(
            "ready. Turn a paired controller on, or press the pair button on the \
             controller (and on the adapter to restart pairing). Ctrl-C to quit."
        );
        self.event_loop(rx)
    }

    fn event_loop(&mut self, rx: Receiver<Msg>) -> Result<()> {
        loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Msg::Usb(msg)) => {
                    if let Err(e) = self.process_message(&msg) {
                        log::warn!("message processing failed: {e:#}");
                        log::debug!("message: {}", hex(&msg));
                    }
                }
                Ok(Msg::Rumble { wcid, rumble }) => {
                    if let Err(e) = self.send_rumble(wcid, &rumble) {
                        log::warn!("rumble failed: {e:#}");
                    }
                }
                Ok(Msg::ReaderFailed) | Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("adapter disconnected");
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if self.pairing_until.is_some_and(|t| Instant::now() > t) {
                log::info!("pairing timed out");
                self.set_pairing(false)?;
            }
        }
    }

    fn send_rumble(&mut self, wcid: u8, rumble: &Rumble) -> Result<()> {
        let Some(slot) = self.clients.get_mut(wcid as usize - 1).and_then(Option::as_mut) else {
            return Ok(());
        };
        log::debug!("[{wcid}] rumble {rumble:?}");
        let pkt = slot.gip.rumble(rumble);
        let addr = slot.address;
        self.mt.send_client_data(wcid, &addr, &pkt)
    }

    fn client_count(&self) -> usize {
        self.clients.iter().flatten().count()
    }

    fn set_pairing(&mut self, enable: bool) -> Result<()> {
        if enable == self.pairing_until.is_some() {
            if enable {
                self.pairing_until = Some(Instant::now() + PAIRING_TIMEOUT);
            }
            return Ok(());
        }
        self.mt.set_pairing(enable)?;
        let led = if enable {
            LedMode::Blink
        } else if self.client_count() > 0 {
            LedMode::On
        } else {
            LedMode::Off
        };
        self.mt.set_led_mode(led)?;
        self.pairing_until = enable.then(|| Instant::now() + PAIRING_TIMEOUT);
        log::info!("pairing {}", if enable { "enabled (60 s)" } else { "disabled" });
        Ok(())
    }

    fn process_message(&mut self, msg: &[u8]) -> Result<()> {
        if msg.len() < 8 {
            return Ok(());
        }
        let info = u32::from_le_bytes(msg[0..4].try_into().unwrap());
        let port = (info >> 27) & 0x7;
        // Ignore command responses.
        if (info >> 16) & 0xf == 0x01 {
            return Ok(());
        }
        let body = &msg[4..msg.len() - 4];
        log::trace!(
            "msg port {port} evt {:#x} len {}: {}",
            (info >> 20) & 0xf,
            body.len(),
            hex(&body[..body.len().min(64)])
        );
        if port == 0 {
            return self.process_wlan(body);
        }
        if port != CPU_RX_PORT {
            return Ok(());
        }
        match (info >> 20) & 0xf {
            EVT_BUTTON => {
                println!("adapter pair button pressed");
                self.set_pairing(true)
            }
            EVT_PACKET_RX => self.process_wlan(body),
            EVT_CLIENT_LOST => match body.first() {
                Some(&wcid) => self.remove_client(wcid, "lost"),
                None => Ok(()),
            },
            other => {
                log::debug!("MCU event {other:#x}: {}", hex(body));
                Ok(())
            }
        }
    }

    fn process_wlan(&mut self, data: &[u8]) -> Result<()> {
        if data.len() < RXWI_LEN + 2 {
            return Ok(());
        }
        let rxinfo = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let ctl = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let mut frame = data[RXWI_LEN..].to_vec();
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        let Some(hlen) = hdr_len(fc) else { return Ok(()) };
        // 2 bytes of padding after the 802.11 header.
        if rxinfo & (1 << 14) != 0 {
            if frame.len() < hlen + 2 {
                return Ok(());
            }
            frame.drain(hlen..hlen + 2);
        }
        let mpdu_len = ((ctl >> 16) & 0x3fff) as usize;
        log::trace!(
            "frame fc {fc:#06x} wcid {} mpdu {mpdu_len} have {} from {}",
            ctl & 0xff,
            frame.len(),
            fmt_mac(&frame.get(10..16).and_then(|s| s.try_into().ok()).unwrap_or([0; 6]))
        );
        if mpdu_len > frame.len() || mpdu_len < hlen {
            return Ok(()); // e.g. our own beacon looped back
        }
        frame.truncate(mpdu_len);
        let wcid = (ctl & 0xff) as u8;
        let addr2: MacAddr = frame[10..16].try_into().unwrap();
        let payload = &frame[hlen..];

        match (fc & FC_FTYPE_MASK, fc & FC_STYPE_MASK) {
            (FC_FTYPE_DATA, FC_STYPE_QOS_DATA) => {
                let known = (1..=MAX_CLIENTS as u8).contains(&wcid)
                    && self.clients[wcid as usize - 1].is_some();
                if known {
                    return self.handle_data(wcid, payload);
                }
                // Fall back to the sender address if the hardware did not match a WCID.
                match self.clients.iter().position(|c| c.as_ref().is_some_and(|c| c.address == addr2)) {
                    Some(i) => {
                        log::debug!("data from {} with wcid {wcid}, using slot {}", fmt_mac(&addr2), i + 1);
                        self.handle_data(i as u8 + 1, payload)
                    }
                    None => {
                        log::debug!("data from unknown client {} (wcid {wcid}, fc {fc:#06x})", fmt_mac(&addr2));
                        Ok(())
                    }
                }
            }
            (FC_FTYPE_MGMT, FC_STYPE_ASSOC_REQ) => {
                // Channel scans produce spurious frames with invalid addresses.
                if addr2[0] & 1 != 0 || addr2 == [0; 6] {
                    return Ok(());
                }
                self.add_client(addr2)
            }
            (FC_FTYPE_MGMT, FC_STYPE_DISASSOC) => self.remove_client(wcid, "disassociated"),
            (FC_FTYPE_MGMT, s) if s == WLAN_RESERVED as u16 => {
                log::debug!("client command frame from {}: {}", fmt_mac(&addr2), hex(payload));
                if payload.len() < 2 || payload[0] != WLAN_RESERVED {
                    return Ok(());
                }
                match payload[1] {
                    CLIENT_PAIR_REQ => {
                        // Keep pairing (open RX filter) until the controller associates;
                        // xone closes it here, which seems to lose some controllers.
                        println!("pairing request from {}", fmt_mac(&addr2));
                        self.mt.pair_client(&addr2)
                    }
                    CLIENT_ENABLE_ENCRYPTION => {
                        log::warn!(
                            "client wcid {wcid} asks for link encryption (not implemented yet)"
                        );
                        Ok(())
                    }
                    other => {
                        log::debug!("client command {other:#04x} from {}", fmt_mac(&addr2));
                        Ok(())
                    }
                }
            }
            _ => Ok(()),
        }
    }

    fn add_client(&mut self, addr: MacAddr) -> Result<()> {
        if self.clients.iter().flatten().any(|c| c.address == addr) {
            return Ok(());
        }
        let Some(i) = self.clients.iter().position(Option::is_none) else {
            log::warn!("no free client slot for {}", fmt_mac(&addr));
            return Ok(());
        };
        let wcid = i as u8 + 1;
        self.mt.associate_client(wcid, &addr)?;
        self.clients[i] = Some(Slot {
            address: addr,
            gip: gip::Client::new(),
            last_input: None,
            coverage: Coverage::default(),
            hid: None,
            guide: false,
            firmware: String::new(),
        });
        println!("controller {} connected (wcid {wcid})", fmt_mac(&addr));
        // Close pairing once a controller is in; this also turns the LED solid.
        if self.pairing_until.is_some() {
            self.set_pairing(false)
        } else {
            self.mt.set_led_mode(LedMode::On)
        }
    }

    fn remove_client(&mut self, wcid: u8, why: &str) -> Result<()> {
        if wcid == 0 || wcid as usize > MAX_CLIENTS {
            return Ok(());
        }
        let Some(slot) = self.clients[wcid as usize - 1].take() else { return Ok(()) };
        println!("controller {} {why} (wcid {wcid})", fmt_mac(&slot.address));
        self.mt.remove_client(wcid)?;
        if self.client_count() == 0 && self.pairing_until.is_none() {
            self.mt.set_led_mode(LedMode::Off)?;
        }
        Ok(())
    }

    fn handle_data(&mut self, wcid: u8, payload: &[u8]) -> Result<()> {
        if wcid == 0 || wcid as usize > MAX_CLIENTS {
            return Ok(());
        }
        let Some(slot) = self.clients[wcid as usize - 1].as_mut() else {
            return Ok(());
        };
        log::trace!("wcid {wcid} GIP in: {}", hex(payload));
        let (out, events) = slot.gip.process(payload)?;
        let addr = slot.address;
        for pkt in out {
            log::trace!("wcid {wcid} GIP out: {}", hex(&pkt));
            self.mt.send_client_data(wcid, &addr, &pkt)?;
        }
        let mut disconnected = false;
        for ev in events {
            let slot = self.clients[wcid as usize - 1].as_mut().unwrap();
            match ev {
                Event::Input(state) => {
                    if slot.last_input != Some(state) {
                        log::debug!("[{wcid}] {}", state.describe());
                        slot.coverage.update(&state);
                        slot.last_input = Some(state);
                        slot.send_hid();
                    }
                }
                Event::Announced { vendor, product, firmware } => {
                    println!(
                        "[{wcid}] announce: vendor {vendor:#06x} product {product:#06x} firmware {firmware}"
                    );
                    slot.firmware = firmware;
                }
                Event::Identified { len } => {
                    println!("[{wcid}] identified ({len} bytes of metadata), sent power on + LED");
                    if slot.hid.is_none() {
                        let tx = Mutex::new(self.tx.clone());
                        let on_rumble = move |rumble| {
                            let _ = tx.lock().unwrap().send(Msg::Rumble { wcid, rumble });
                        };
                        match VirtualGamepad::new(&fmt_mac(&addr), &slot.firmware, on_rumble) {
                            Ok(hid) => {
                                println!("[{wcid}] virtual Xbox Wireless Controller (Bluetooth 045e:0b13) created");
                                slot.hid = Some(hid);
                            }
                            Err(e) => log::warn!("[{wcid}] {e:#}"),
                        }
                    }
                }
                Event::Battery { kind, level } => {
                    log::debug!("[{wcid}] battery type {kind} level {level}/3")
                }
                Event::Guide(down) => {
                    log::debug!("[{wcid}] Guide {}", if down { "down" } else { "up" });
                    if down {
                        slot.coverage.mark("Guide");
                    }
                    slot.guide = down;
                    slot.send_hid();
                }
                Event::Disconnected => disconnected = true,
                Event::Other { command, internal, payload } => log::debug!(
                    "[{wcid}] {} command {command:#04x}: {}",
                    if internal { "system" } else { "vendor" },
                    hex(&payload)
                ),
            }
        }
        if disconnected {
            self.remove_client(wcid, "turned off")?;
        }
        Ok(())
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
