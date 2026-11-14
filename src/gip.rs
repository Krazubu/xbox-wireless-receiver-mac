// SPDX-License-Identifier: GPL-2.0-or-later
//! Gaming Input Protocol (GIP), host side.
//!
//! Framing follows [MS-GIPUSB] and xone (bus/protocol.c), Copyright (C) 2021
//! Severin von Wnuck-Lipinski.

use anyhow::{bail, ensure, Result};

// Core (system) commands, sent with OPT_INTERNAL.
const CMD_ACKNOWLEDGE: u8 = 0x01;
const CMD_ANNOUNCE: u8 = 0x02;
const CMD_STATUS: u8 = 0x03;
const CMD_IDENTIFY: u8 = 0x04;
const CMD_POWER: u8 = 0x05;
const CMD_AUTHENTICATE: u8 = 0x06;
const CMD_VIRTUAL_KEY: u8 = 0x07;
const CMD_LED: u8 = 0x0a;
// Gamepad (vendor) commands.
const CMD_RUMBLE: u8 = 0x09;
const CMD_INPUT: u8 = 0x20;

const OPT_ACKNOWLEDGE: u8 = 1 << 4;
const OPT_INTERNAL: u8 = 1 << 5;
const OPT_CHUNK_START: u8 = 1 << 6;
const OPT_CHUNK: u8 = 1 << 7;
const OPT_CLIENT_ID: u8 = 0x0f;

const PWR_ON: u8 = 0x00;
const LED_ON: u8 = 0x01;
const LED_BRIGHTNESS_DEFAULT: u8 = 20;
const VKEY_LEFT_WIN: u8 = 0x5b;

#[derive(Debug, Clone, Copy)]
struct Header {
    command: u8,
    options: u8,
    sequence: u8,
    packet_length: u32,
    chunk_offset: u32,
}

fn decode_varint(data: &[u8]) -> (u32, usize) {
    let mut val = 0u32;
    let mut i = 0;
    while i < 4 && i < data.len() {
        val |= ((data[i] & 0x7f) as u32) << (i * 7);
        if data[i] & 0x80 == 0 {
            break;
        }
        i += 1;
    }
    (val, i + 1)
}

fn encode_varint(mut val: u32, out: &mut Vec<u8>) {
    loop {
        let mut b = (val & 0x7f) as u8;
        val >>= 7;
        if val != 0 {
            b |= 0x80;
        }
        out.push(b);
        if val == 0 {
            break;
        }
    }
}

fn decode_header(data: &[u8]) -> Result<(Header, usize)> {
    ensure!(data.len() >= 4, "GIP header too short");
    let (packet_length, n) = decode_varint(&data[3..]);
    let mut len = 3 + n;
    let mut chunk_offset = 0;
    if data[1] & OPT_CHUNK != 0 {
        let (v, n) = decode_varint(&data[len..]);
        chunk_offset = v;
        len += n;
    }
    Ok((
        Header {
            command: data[0],
            options: data[1],
            sequence: data[2],
            packet_length,
            chunk_offset,
        },
        len,
    ))
}

/// Encode a non-chunked packet. Header length must be even.
fn encode(command: u8, options: u8, sequence: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![command, options, sequence];
    encode_varint(payload.len() as u32, &mut out);
    if out.len() % 2 == 1 {
        *out.last_mut().unwrap() |= 0x80;
        out.push(0);
    }
    out.extend_from_slice(payload);
    out
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GamepadState {
    /// Share button (Series controllers): fixed offset from the end of the report.
    pub share: bool,
    pub buttons: u16,
    pub trigger_left: u16,
    pub trigger_right: u16,
    pub stick_left: (i16, i16),
    pub stick_right: (i16, i16),
}

impl GamepadState {
    pub fn describe(&self) -> String {
        const NAMES: [(u16, &str); 14] = [
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
        let mut pressed: Vec<&str> = NAMES
            .iter()
            .filter(|(m, _)| self.buttons & m != 0)
            .map(|(_, n)| *n)
            .collect();
        if self.share {
            pressed.push("Share");
        }
        format!(
            "LT {:4} RT {:4}  L ({:6},{:6})  R ({:6},{:6})  [{}]",
            self.trigger_left,
            self.trigger_right,
            self.stick_left.0,
            self.stick_left.1,
            self.stick_right.0,
            self.stick_right.1,
            pressed.join(" ")
        )
    }
}

#[derive(Debug)]
pub enum Event {
    Announced { vendor: u16, product: u16, firmware: String },
    Identified { len: usize },
    Battery { kind: u8, level: u8 },
    Disconnected,
    Guide(bool),
    Input(GamepadState),
    Other { command: u8, internal: bool, payload: Vec<u8> },
}

struct ChunkBuf {
    command: u8,
    options: u8,
    data: Vec<u8>,
}

/// Host-side GIP state for one wireless client.
pub struct Client {
    sequence: u8,
    chunk: Option<ChunkBuf>,
    identified: bool,
}

impl Client {
    pub fn new() -> Self {
        Self { sequence: 0, chunk: None, identified: false }
    }

    fn next_seq(&mut self) -> u8 {
        self.sequence = self.sequence.wrapping_add(1);
        if self.sequence == 0 {
            self.sequence = 1;
        }
        self.sequence
    }

    fn system(&mut self, id: u8, command: u8, payload: &[u8]) -> Vec<u8> {
        let seq = self.next_seq();
        encode(command, id | OPT_INTERNAL, seq, payload)
    }

    /// Rumble packet (gamepad command 0x09): motor enable bits, magnitudes 0..=100
    /// for left trigger, right trigger, left (strong) and right (weak) motors,
    /// then duration, delay and repeat count (same layout as the Bluetooth report).
    pub fn rumble(&mut self, r: &[u8; 8]) -> Vec<u8> {
        let seq = self.next_seq();
        let mut p = vec![0u8];
        p.extend_from_slice(r);
        encode(CMD_RUMBLE, 0, seq, &p)
    }

    fn ack(&self, hdr: &Header) -> Vec<u8> {
        let id = hdr.options & OPT_CLIENT_ID;
        let len = hdr.chunk_offset + hdr.packet_length;
        let remaining = match (&self.chunk, hdr.options & OPT_CHUNK != 0) {
            (Some(b), true) => (b.data.len() as u32).saturating_sub(len),
            _ => 0,
        };
        let mut p = vec![0, hdr.command, id | OPT_INTERNAL];
        p.extend_from_slice(&(len as u16).to_le_bytes());
        p.extend_from_slice(&[0, 0]);
        p.extend_from_slice(&(remaining as u16).to_le_bytes());
        encode(CMD_ACKNOWLEDGE, id | OPT_INTERNAL, hdr.sequence, &p)
    }

    /// Process a received GIP buffer (may hold several packets).
    /// Returns packets to send back and decoded events.
    pub fn process(&mut self, mut data: &[u8]) -> Result<(Vec<Vec<u8>>, Vec<Event>)> {
        let mut out = Vec::new();
        let mut events = Vec::new();
        while data.len() > 3 {
            let (mut hdr, hlen) = decode_header(data)?;
            let end = hlen + hdr.packet_length as usize;
            ensure!(data.len() >= end, "truncated GIP packet ({} < {end})", data.len());
            let payload = &data[hlen..end];
            data = &data[end..];

            if hdr.options & OPT_CHUNK_START != 0 {
                // Chunk offset of the first chunk is the total length.
                self.chunk = Some(ChunkBuf {
                    command: hdr.command,
                    options: hdr.options & !(OPT_ACKNOWLEDGE | OPT_CHUNK_START),
                    data: vec![0; hdr.chunk_offset as usize],
                });
                hdr.chunk_offset = 0;
            }

            if hdr.options & OPT_CHUNK != 0 {
                let Some(buf) = self.chunk.as_mut() else {
                    if hdr.packet_length == 0 {
                        continue; // spurious completion from older gamepads
                    }
                    bail!("chunk for command {:#04x} without a start", hdr.command);
                };
                ensure!(buf.command == hdr.command, "conflicting chunked packet");
                if hdr.packet_length > 0 {
                    let off = hdr.chunk_offset as usize;
                    let len = off + payload.len();
                    ensure!(len <= buf.data.len(), "chunk overflows buffer");
                    buf.data[off..len].copy_from_slice(payload);
                    let last = len == buf.data.len();
                    if hdr.options & OPT_ACKNOWLEDGE != 0 || last {
                        out.push(self.ack(&hdr));
                    }
                    continue;
                }
                // Empty chunk: transfer complete.
                let buf = self.chunk.take().unwrap();
                let h = Header { options: buf.options, ..hdr };
                self.dispatch(&h, &buf.data, &mut out, &mut events)?;
                continue;
            }

            if hdr.options & OPT_ACKNOWLEDGE != 0 {
                out.push(self.ack(&hdr));
            }
            self.dispatch(&hdr, payload, &mut out, &mut events)?;
        }
        Ok((out, events))
    }

    fn dispatch(
        &mut self,
        hdr: &Header,
        p: &[u8],
        out: &mut Vec<Vec<u8>>,
        events: &mut Vec<Event>,
    ) -> Result<()> {
        let id = hdr.options & OPT_CLIENT_ID;
        let internal = hdr.options & OPT_INTERNAL != 0;
        let le16 = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
        match (internal, hdr.command) {
            (true, CMD_ACKNOWLEDGE) => {}
            (true, CMD_ANNOUNCE) => {
                ensure!(p.len() >= 28, "short announce ({} bytes)", p.len());
                events.push(Event::Announced {
                    vendor: le16(8),
                    product: le16(10),
                    firmware: format!("{}.{}.{}.{}", le16(12), le16(14), le16(16), le16(18)),
                });
                if !self.identified {
                    out.push(self.system(id, CMD_IDENTIFY, &[]));
                }
            }
            (true, CMD_IDENTIFY) => {
                events.push(Event::Identified { len: p.len() });
                if !self.identified {
                    self.identified = true;
                    // Gamepad start-up as in xone's gamepad driver: power on, LED on,
                    // then tell the controller authentication is complete (the host
                    // "succeeds the security exchange by default", MS-GIPUSB 5.1).
                    out.push(self.system(id, CMD_POWER, &[PWR_ON]));
                    out.push(self.system(id, CMD_LED, &[0, LED_ON, LED_BRIGHTNESS_DEFAULT]));
                    out.push(self.system(id, CMD_AUTHENTICATE, &[0x01, 0x00]));
                }
            }
            (true, CMD_STATUS) => {
                ensure!(!p.is_empty(), "empty status");
                if p[0] & 0x80 == 0 {
                    events.push(Event::Disconnected);
                } else {
                    events.push(Event::Battery { kind: (p[0] >> 2) & 3, level: p[0] & 3 });
                }
            }
            (true, CMD_VIRTUAL_KEY) if p.len() == 2 && p[1] == VKEY_LEFT_WIN => {
                events.push(Event::Guide(p[0] != 0));
            }
            (false, CMD_INPUT) if p.len() >= 14 => {
                // xone: the share byte is 18 bytes from the end, or 26 when the report
                // ends with 8 bytes of latency counters (DLI) — offset 18 either way on
                // Series controllers (36-byte report, or 44 with DLI).
                let share = p.len() >= 36 && p[18] != 0;
                events.push(Event::Input(GamepadState {
                    share,
                    buttons: le16(0),
                    trigger_left: le16(2),
                    trigger_right: le16(4),
                    stick_left: (le16(6) as i16, le16(8) as i16),
                    stick_right: (le16(10) as i16, le16(12) as i16),
                }));
            }
            _ => events.push(Event::Other { command: hdr.command, internal, payload: p.to_vec() }),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0u32, 1, 0x7f, 0x80, 0x3a, 0x1234, 0xffff] {
            let mut b = Vec::new();
            encode_varint(v, &mut b);
            assert_eq!(decode_varint(&b), (v, b.len()));
        }
    }

    #[test]
    fn header_is_even_and_decodes() {
        let p = encode(CMD_POWER, OPT_INTERNAL, 7, &[PWR_ON]);
        assert_eq!(p, vec![0x05, 0x20, 0x07, 0x01, 0x00]);
        let (h, n) = decode_header(&p).unwrap();
        assert_eq!((h.command, h.sequence, h.packet_length, n), (CMD_POWER, 7, 1, 4));
    }

    #[test]
    fn announce_triggers_identify() {
        let mut c = Client::new();
        let mut pkt = encode(CMD_ANNOUNCE, OPT_INTERNAL, 1, &[0u8; 28]);
        pkt[1] |= OPT_ACKNOWLEDGE;
        let (out, ev) = c.process(&pkt).unwrap();
        assert!(matches!(ev[0], Event::Announced { .. }));
        assert_eq!(out.len(), 2); // ack + identify request
        assert_eq!(out[0][0], CMD_ACKNOWLEDGE);
        assert_eq!(out[1][0], CMD_IDENTIFY);
    }

    #[test]
    fn chunked_identify_reassembles() {
        let mut c = Client::new();
        let total = 100u32;
        // first chunk: CHUNK_START|CHUNK, offset = total length
        let mut a = vec![CMD_IDENTIFY, OPT_INTERNAL | OPT_CHUNK_START | OPT_CHUNK | OPT_ACKNOWLEDGE, 1];
        encode_varint(60, &mut a);
        encode_varint(total, &mut a);
        a.extend_from_slice(&[1u8; 60]);
        let mut b = vec![CMD_IDENTIFY, OPT_INTERNAL | OPT_CHUNK, 2];
        encode_varint(40, &mut b);
        encode_varint(60, &mut b);
        b.extend_from_slice(&[2u8; 40]);
        let mut end = vec![CMD_IDENTIFY, OPT_INTERNAL | OPT_CHUNK, 3, 0];
        encode_varint(100, &mut end);
        let (o1, e1) = c.process(&a).unwrap();
        assert_eq!((o1.len(), e1.len()), (1, 0));
        let (o2, _) = c.process(&b).unwrap();
        assert_eq!(o2.len(), 1); // last chunk is always acknowledged
        let (_, e3) = c.process(&end).unwrap();
        assert!(matches!(e3[0], Event::Identified { len: 100 }));
    }
}
