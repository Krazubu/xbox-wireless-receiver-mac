// SPDX-License-Identifier: GPL-2.0-or-later
//! Low-level access to the MT76 radio inside the Xbox Wireless Adapter.
//!
//! Register map, radio init and frame formats ported from xone
//! (transport/mt76.c, mt76_defs.h), Copyright (C) 2021 Severin von Wnuck-Lipinski.
//! Warm firmware reset from xow (dongle/mt76.cpp), same author.

use std::{thread::sleep, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use nusb::{
    transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient},
    Device, Endpoint, Interface, MaybeFuture,
};

pub const MICROSOFT_VID: u16 = 0x045e;
/// Known adapters: (product id, description, firmware file inside the driver CAB).
pub const ADAPTERS: &[(u16, &str, &str)] = &[
    (0x02e6, "Xbox Wireless Adapter (original)", "FW_ACC_00U.bin"),
    (0x02fe, "Xbox Wireless Adapter (slim, 2017)", "FW_ACC_00U.bin"),
    (0x02f9, "Built-in adapter (ASUS, Lenovo)", "FW_ACC_CL.bin"),
    (0x091e, "Built-in adapter (Surface Book 2)", "FW_ACC_BR.bin"),
];

const USB_TIMEOUT: Duration = Duration::from_millis(1000);
const POLL_RETRIES: usize = 50;

pub const EP_OUT: u8 = 0x04;
pub const EP_IN_WLAN: u8 = 0x84;
pub const EP_IN_CMD: u8 = 0x85;

// Vendor requests.
const VEND_DEV_MODE: u8 = 0x01;
const VEND_MULTI_WRITE: u8 = 0x06;
const VEND_MULTI_READ: u8 = 0x07;
const VEND_WRITE_CFG: u8 = 0x46;
const VEND_READ_CFG: u8 = 0x47;

/// Address flag selecting the CFG register space.
const TYPE_CFG: u32 = 1 << 30;

// Registers.
const CMB_CTRL: u32 = 0x0020;
const EFUSE_CTRL: u32 = 0x0024;
const EFUSE_DATA_BASE: u32 = 0x0028;
const LDO_CTRL_1: u32 = 0x0070;
const XO_CTRL5: u32 = 0x0114;
const XO_CTRL6: u32 = 0x0118;
const RF_PATCH: u32 = 0x0130;
const WPDMA_GLO_CFG: u32 = 0x0208;
const WMM_AIFSN: u32 = 0x0214;
const WMM_CWMIN: u32 = 0x0218;
const WMM_CWMAX: u32 = 0x021c;
const FCE_DMA_ADDR: u32 = 0x0230;
const FCE_DMA_LEN: u32 = 0x0234;
const USB_DMA_CFG: u32 = 0x0238;
const TSO_CTRL: u32 = 0x0250;
const PBF_SYS_CTRL: u32 = 0x0400;
const PBF_CFG: u32 = 0x0404;
const PBF_TX_MAX_PCNT: u32 = 0x0408;
const RF_BYPASS_0: u32 = 0x0504;
const RF_SETTING_0: u32 = 0x050c;
const FCE_PSE_CTRL: u32 = 0x0800;
const FCE_L2_STUFF: u32 = 0x080c;
const TX_CPU_FROM_FCE_BASE_PTR: u32 = 0x09a0;
const TX_CPU_FROM_FCE_MAX_COUNT: u32 = 0x09a4;
const TX_CPU_FROM_FCE_CPU_DESC_IDX: u32 = 0x09a8;
const FCE_PDMA_GLOBAL_CONF: u32 = 0x09c4;
const PAUSE_ENABLE_CONTROL1: u32 = 0x0a38;
const FCE_SKIP_FS: u32 = 0x0a6c;
const MAC_SYS_CTRL: u32 = 0x1004;
const MAC_ADDR_DW0: u32 = 0x1008;
const MAC_BSSID_DW0: u32 = 0x1010;
const MAX_LEN_CFG: u32 = 0x1018;
const AMPDU_MAX_LEN_20M1S: u32 = 0x1030;
const AMPDU_MAX_LEN_20M2S: u32 = 0x1034;
const XIFS_TIME_CFG: u32 = 0x1100;
const BKOFF_SLOT_CFG: u32 = 0x1104;
const CH_TIME_CFG: u32 = 0x110c;
const BEACON_TIME_CFG: u32 = 0x1114;
const PWR_PIN_CFG: u32 = 0x1204;
const RF_PA_MODE_ADJ0: u32 = 0x1228;
const RF_PA_MODE_ADJ1: u32 = 0x122c;
const DACCLK_EN_DLY_CFG: u32 = 0x1264;
const EDCA_CFG_BASE: u32 = 0x1300;
const TX_PIN_CFG: u32 = 0x1328;
const TX_SW_CFG0: u32 = 0x1330;
const TX_SW_CFG1: u32 = 0x1334;
const TXOP_CTRL_CFG: u32 = 0x1340;
const TX_RTS_CFG: u32 = 0x1344;
const TX_TIMEOUT_CFG: u32 = 0x1348;
const TX_RETRY_CFG: u32 = 0x134c;
const CCK_PROT_CFG: u32 = 0x1364;
const OFDM_PROT_CFG: u32 = 0x1368;
const MM20_PROT_CFG: u32 = 0x136c;
const GF20_PROT_CFG: u32 = 0x1374;
const GF40_PROT_CFG: u32 = 0x1378;
const EXP_ACK_TIME: u32 = 0x1380;
const TX0_RF_GAIN_CORR: u32 = 0x13a0;
const TX1_RF_GAIN_CORR: u32 = 0x13a4;
const TX_ALC_CFG_2: u32 = 0x13a8;
const TX_ALC_CFG_3: u32 = 0x13ac;
const TX_ALC_CFG_0: u32 = 0x13b0;
const TX_ALC_CFG_4: u32 = 0x13c0;
const TX_PROT_CFG6: u32 = 0x13e0;
const TX_PROT_CFG7: u32 = 0x13e4;
const TX_PROT_CFG8: u32 = 0x13e8;
const PIFS_TX_CFG: u32 = 0x13ec;
const RX_FILTR_CFG: u32 = 0x1400;
const AUTO_RSP_CFG: u32 = 0x1404;
const LEGACY_BASIC_RATE: u32 = 0x1408;
const HT_BASIC_RATE: u32 = 0x140c;
const EXT_CCA_CFG: u32 = 0x141c;
const PN_PAD_MODE: u32 = 0x150c;
const TXOP_HLDR_ET: u32 = 0x1608;
const WCID_ADDR_BASE: u32 = 0x1800;
const BBP_AGC_BASE: u32 = 0x2300;
const BEACON_BASE: u32 = 0xc000;
const USB_U3DMA_CFG: u32 = 0x9018;

const MAC_SYS_CTRL_RESET_CSR: u32 = 1 << 0;
const MAC_SYS_CTRL_RESET_BBP: u32 = 1 << 1;
const MAC_SYS_CTRL_ENABLE_TX: u32 = 1 << 2;
const MAC_SYS_CTRL_ENABLE_RX: u32 = 1 << 3;
const USB_DMA_CFG_RX_BULK_EN: u32 = 1 << 22;
const USB_DMA_CFG_TX_BULK_EN: u32 = 1 << 23;
const EFUSE_CTRL_KICK: u32 = 1 << 30;
const EFUSE_CTRL_AIN_MASK: u32 = 0x3ff << 16;
const EFUSE_CTRL_MODE_MASK: u32 = 0x3 << 6;
const BEACON_TIME_CFG_ACTIVE: u32 = (1 << 20) | (1 << 19) | (0x3 << 17) | (1 << 16) | 0x0640;
const RX_FILTER_PAIRING: u32 = 0x014f13;
const RX_FILTER_NORMAL: u32 = 0x017f17;

/// Offset of WLAN registers in the MCU memory map (for burst writes).
const MCU_MEMMAP_WLAN: u32 = 0x41_0000;

// EFUSE addresses.
const EE_CHIP_ID: u16 = 0x0000;
const EE_MAC_ADDR: u16 = 0x0004;
const EE_XTAL_TRIM_1: u16 = 0x003a;
const EE_TX_POWER_0_START_2G: u16 = 0x0056;
const EE_TX_POWER_0_START_5G: u16 = 0x0062;
const EE_XTAL_TRIM_2: u16 = 0x009e;
const TX_POWER_GROUP_SIZE_5G: u16 = 5;

const FW_RESET_IVB: u16 = 0x01;
const FW_LOAD_IVB: u16 = 0x12;
const FW_ILM_OFFSET: u32 = 0x08_0000;
const FW_DLM_OFFSET: u32 = 0x11_0800;
const FW_CHUNK_SIZE: usize = 0x3800;

// MCU commands.
const CMD_FUN_SET_OP: u32 = 1;
const CMD_LOAD_CR: u32 = 2;
const CMD_INIT_GAIN_OP: u32 = 3;
const CMD_BURST_WRITE: u32 = 8;
const CMD_LED_MODE_OP: u32 = 16;
const CMD_POWER_SAVING_OP: u32 = 20;
const CMD_WOW_FEATURE: u32 = 24;
const CMD_SWITCH_CHANNEL_OP: u32 = 30;
const CMD_CALIBRATION_OP: u32 = 31;

const Q_SELECT: u32 = 1;
const RADIO_OFF: u32 = 0x30;
const RADIO_ON: u32 = 0x31;
const RF_BBP_CR: u8 = 2;
const CAL_TEMP_SENSOR: u32 = 2;
const CAL_RXDCOC: u32 = 3;
const CAL_RC: u32 = 4;

// Commands understood by Microsoft's firmware (wrapped in CMD_INIT_GAIN_OP).
const MS_SET_MAC_ADDRESS: u32 = 0x00;
const MS_ADD_CLIENT: u32 = 0x01;
const MS_REMOVE_CLIENT: u32 = 0x02;
const MS_SET_IDLE_TIME: u32 = 0x05;
const MS_SET_CHAN_CANDIDATES: u32 = 0x07;

// MCU message header (4 bytes, little endian).
const MCU_MSG_TYPE_CMD: u32 = 1 << 30;
const MCU_MSG_PORT_SHIFT: u32 = 27;
const MCU_MSG_CMD_TYPE_SHIFT: u32 = 20;
const WLAN_PORT: u32 = 0;
pub const CPU_RX_PORT: u32 = 1;
const CPU_TX_PORT: u32 = 2;
const QSEL_EDCA: u32 = 2;
const TXD_INFO_QSEL_SHIFT: u32 = 25;
const TXD_INFO_WIV: u32 = 1 << 24;
const TXD_INFO_80211: u32 = 1 << 19;

// TXWI fields.
const TXWI_FLAGS_TS: u16 = 1 << 3;
const TXWI_FLAGS_MPDU_DENSITY_4: u16 = 5 << 5;
const TXWI_RATE_OFDM: u16 = 1 << 13;
const TXWI_ACK_CTL_REQ: u8 = 1 << 0;
const TXWI_ACK_CTL_NSEQ: u8 = 1 << 1;

// 802.11.
pub const FC_FTYPE_MASK: u16 = 0x000c;
pub const FC_STYPE_MASK: u16 = 0x00f0;
pub const FC_FTYPE_MGMT: u16 = 0x0000;
pub const FC_FTYPE_DATA: u16 = 0x0008;
pub const FC_STYPE_ASSOC_REQ: u16 = 0x0000;
const FC_STYPE_ASSOC_RESP: u16 = 0x0010;
const FC_STYPE_BEACON: u16 = 0x0080;
pub const FC_STYPE_DISASSOC: u16 = 0x00a0;
pub const FC_STYPE_QOS_DATA: u16 = 0x0080;
const FC_FROMDS: u16 = 0x0200;
/// Reserved management subtype used by Microsoft for controller commands.
pub const WLAN_RESERVED: u8 = 0x70;

pub const CLIENT_PAIR_REQ: u8 = 0x01;
const CLIENT_PAIR_RESP: u8 = 0x02;
pub const CLIENT_ENABLE_ENCRYPTION: u8 = 0x10;

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum LedMode {
    Blink = 0x00,
    On = 0x01,
    Off = 0x02,
}

pub type MacAddr = [u8; 6];

pub fn fmt_mac(a: &MacAddr) -> String {
    a.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

#[derive(Clone, Copy, Debug)]
struct Channel {
    index: u8,
    band: u8,
    /// 0 = 20 MHz, 1 = 40 MHz, 2 = 80 MHz.
    bandwidth: u8,
    group: u8,
    scan: bool,
    power: u8,
}

const fn ch(index: u8, band: u8, bandwidth: u8, group: u8, scan: bool) -> Channel {
    Channel { index, band, bandwidth, group, scan, power: 0 }
}

const CHANNELS: [Channel; 12] = [
    ch(0x01, 1, 0, 0, true),
    ch(0x06, 2, 0, 0, true),
    ch(0x0b, 3, 0, 0, true),
    ch(0x24, 1, 1, 1, true),
    ch(0x28, 1, 1, 1, false),
    ch(0x2c, 2, 1, 1, true),
    ch(0x30, 2, 1, 1, false),
    ch(0x95, 1, 2, 5, true),
    ch(0x99, 1, 2, 5, false),
    ch(0x9d, 2, 2, 5, true),
    ch(0xa1, 2, 2, 5, false),
    ch(0xa5, 2, 2, 5, false),
];

/// Parsed MT76 firmware image: 32-byte header, then ILM, then DLM.
pub struct Firmware<'a> {
    pub build_ver: u16,
    pub fw_ver: u16,
    pub build_time: String,
    pub ilm: &'a [u8],
    pub dlm: &'a [u8],
}

impl<'a> Firmware<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        const HDR: usize = 32;
        ensure!(data.len() >= HDR, "firmware too short ({} bytes)", data.len());
        let le32 = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
        let le16 = |o: usize| u16::from_le_bytes(data[o..o + 2].try_into().unwrap());
        let (ilm_len, dlm_len) = (le32(0), le32(4));
        ensure!(
            data.len() == HDR + ilm_len + dlm_len,
            "firmware size mismatch: file {} bytes, header says {}",
            data.len(),
            HDR + ilm_len + dlm_len
        );
        let build_time = String::from_utf8_lossy(&data[16..32])
            .trim_end_matches('\0')
            .to_string();
        Ok(Self {
            build_ver: le16(8),
            fw_ver: le16(10),
            build_time,
            ilm: &data[HDR..HDR + ilm_len],
            dlm: &data[HDR + ilm_len..],
        })
    }
}

/// Build a TXWI descriptor (20 bytes).
fn txwi(flags: u16, ack_ctl: u8, wcid: u8, len: usize) -> [u8; 20] {
    let mut t = [0u8; 20];
    t[0..2].copy_from_slice(&flags.to_le_bytes());
    t[2..4].copy_from_slice(&TXWI_RATE_OFDM.to_le_bytes());
    t[4] = ack_ctl;
    t[5] = wcid;
    t[6..8].copy_from_slice(&(len as u16).to_le_bytes());
    t
}

/// 802.11 3-address header (24 bytes).
fn hdr3(fc: u16, addr1: &MacAddr, addr2: &MacAddr, addr3: &MacAddr) -> [u8; 24] {
    let mut h = [0u8; 24];
    h[0..2].copy_from_slice(&fc.to_le_bytes());
    h[4..10].copy_from_slice(addr1);
    h[10..16].copy_from_slice(addr2);
    h[16..22].copy_from_slice(addr3);
    h
}

pub struct Mt76 {
    device: Device,
    interface: Interface,
    ep_out: Endpoint<Bulk, Out>,
    pub address: MacAddr,
    channels: [Channel; 12],
    channel: usize,
}

impl Mt76 {
    pub fn open(device: Device) -> Result<Self> {
        let interface = device
            .claim_interface(0)
            .wait()
            .context("claiming interface 0 (is another driver using the adapter?)")?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(EP_OUT)
            .context("opening bulk OUT endpoint 0x04")?;
        Ok(Self {
            device,
            interface,
            ep_out,
            address: [0; 6],
            channels: CHANNELS,
            channel: 0,
        })
    }

    pub fn in_endpoint(&self, address: u8) -> Result<Endpoint<Bulk, In>> {
        self.interface
            .endpoint::<Bulk, In>(address)
            .with_context(|| format!("opening bulk IN endpoint {address:#04x}"))
    }

    pub fn channel_index(&self) -> u8 {
        self.channels[self.channel].index
    }

    // ---- register access -------------------------------------------------

    pub fn read_register(&self, addr: u32) -> Result<u32> {
        let (request, addr) = if addr & TYPE_CFG != 0 {
            (VEND_READ_CFG, addr & !TYPE_CFG)
        } else {
            (VEND_MULTI_READ, addr)
        };
        let data = self
            .device
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value: (addr >> 16) as u16,
                    index: addr as u16,
                    length: 4,
                },
                USB_TIMEOUT,
            )
            .wait()
            .with_context(|| format!("reading register {addr:#06x}"))?;
        ensure!(data.len() == 4, "short register read ({} bytes)", data.len());
        Ok(u32::from_le_bytes(data[..4].try_into().unwrap()))
    }

    pub fn write_register(&self, addr: u32, val: u32) -> Result<()> {
        let (request, addr) = if addr & TYPE_CFG != 0 {
            (VEND_WRITE_CFG, addr & !TYPE_CFG)
        } else {
            (VEND_MULTI_WRITE, addr)
        };
        self.device
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value: (addr >> 16) as u16,
                    index: addr as u16,
                    data: &val.to_le_bytes(),
                },
                USB_TIMEOUT,
            )
            .wait()
            .with_context(|| format!("writing register {addr:#06x}"))
    }

    fn poll(&self, addr: u32, mask: u32, val: u32) -> Result<bool> {
        let mut last = None;
        for _ in 0..POLL_RETRIES {
            let reg = self.read_register(addr)?;
            if reg & mask == val {
                return Ok(true);
            }
            if last != Some(reg) {
                log::debug!("poll {addr:#x}: {reg:#010x} (want {val:#x} under mask {mask:#x})");
                last = Some(reg);
            }
            sleep(Duration::from_millis(15));
        }
        Ok(false)
    }

    fn read_efuse(&self, addr: u16, len: usize) -> Result<Vec<u8>> {
        let mut ctrl = self.read_register(EFUSE_CTRL)?;
        ctrl &= !(EFUSE_CTRL_AIN_MASK | EFUSE_CTRL_MODE_MASK);
        ctrl |= EFUSE_CTRL_KICK | ((((addr & !0x0f) as u32) << 16) & EFUSE_CTRL_AIN_MASK);
        self.write_register(EFUSE_CTRL, ctrl)?;
        if !self.poll(EFUSE_CTRL, EFUSE_CTRL_KICK, 0)? {
            bail!("timeout reading EFUSE at {addr:#x}");
        }
        let mut out = Vec::with_capacity(len);
        for i in (0..len).step_by(4) {
            let offset = (addr as u32 & 0x0c) + i as u32;
            let val = self.read_register(EFUSE_DATA_BASE + offset)?;
            let n = (len - i).min(4);
            out.extend_from_slice(&val.to_le_bytes()[..n]);
        }
        Ok(out)
    }

    // ---- MCU messages ----------------------------------------------------

    /// Wrap `payload` in an MCU message (header, padding to 4 bytes, 4-byte trailer)
    /// and send it on the bulk OUT endpoint.
    fn send_message(&mut self, info: u32, payload: &[u8]) -> Result<()> {
        let len = payload.len().next_multiple_of(4);
        let mut buf = Vec::with_capacity(4 + len + 4);
        buf.extend_from_slice(&(info | len as u32).to_le_bytes());
        buf.extend_from_slice(payload);
        buf.resize(4 + len + 4, 0);
        let c = self.ep_out.transfer_blocking(buf.into(), USB_TIMEOUT);
        c.status.context("bulk OUT transfer")?;
        Ok(())
    }

    /// Send an MCU command.
    pub fn send_command(&mut self, cmd: u32, payload: &[u8]) -> Result<()> {
        let info = MCU_MSG_TYPE_CMD
            | (CPU_TX_PORT << MCU_MSG_PORT_SHIFT)
            | ((cmd & 0x7f) << MCU_MSG_CMD_TYPE_SHIFT);
        self.send_message(info, payload)
    }

    /// Send a raw 802.11 frame (TXWI is prepended here).
    fn send_wlan(&mut self, frame: &[u8]) -> Result<()> {
        let mut buf = txwi(TXWI_FLAGS_MPDU_DENSITY_4, TXWI_ACK_CTL_REQ, 0xff, frame.len()).to_vec();
        buf.extend_from_slice(frame);
        let info = (WLAN_PORT << MCU_MSG_PORT_SHIFT)
            | (QSEL_EDCA << TXD_INFO_QSEL_SHIFT)
            | TXD_INFO_WIV
            | TXD_INFO_80211;
        self.send_message(info, &buf)
    }

    fn cmd_u32s(&mut self, cmd: u32, vals: &[u32]) -> Result<()> {
        let p: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.send_command(cmd, &p)
    }

    fn send_ms_command(&mut self, ms_cmd: u32, data: &[u8]) -> Result<()> {
        let mut p = ms_cmd.to_le_bytes().to_vec();
        p.extend_from_slice(data);
        self.send_command(CMD_INIT_GAIN_OP, &p)
    }

    fn write_burst(&mut self, idx: u32, data: &[u8]) -> Result<()> {
        let mut p = (idx + MCU_MEMMAP_WLAN).to_le_bytes().to_vec();
        p.extend_from_slice(data);
        self.send_command(CMD_BURST_WRITE, &p)
    }

    pub fn set_led_mode(&mut self, mode: LedMode) -> Result<()> {
        self.cmd_u32s(CMD_LED_MODE_OP, &[mode as u32])
    }

    fn set_power_mode(&mut self, mode: u32) -> Result<()> {
        self.cmd_u32s(CMD_POWER_SAVING_OP, &[mode])
    }

    fn calibrate(&mut self, calib: u32, val: u32) -> Result<()> {
        self.cmd_u32s(CMD_CALIBRATION_OP, &[calib, val])
    }

    fn switch_channel(&mut self, ch: Channel) -> Result<()> {
        let mut msg = [0u8; 20];
        msg[0] = ch.index;
        msg[4..6].copy_from_slice(&0x0101u16.to_le_bytes()); // TX and RX stream 1
        msg[16] = ch.bandwidth;
        msg[17] = ch.power;
        msg[18] = ch.scan as u8;
        self.send_command(CMD_SWITCH_CHANNEL_OP, &msg)
    }

    // ---- firmware --------------------------------------------------------

    fn dev_mode(&self, value: u16) -> Result<()> {
        self.device
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: VEND_DEV_MODE,
                    value,
                    index: 0,
                    data: &[],
                },
                USB_TIMEOUT,
            )
            .wait()
            .with_context(|| format!("DEV_MODE {value:#x}"))
    }

    fn apply_rf_patch(&self) -> Result<()> {
        let val = self.read_register(RF_PATCH | TYPE_CFG)?;
        self.write_register(RF_PATCH | TYPE_CFG, val & !(1 << 19))
    }

    fn send_firmware_part(&mut self, offset: u32, data: &[u8]) -> Result<()> {
        for (i, chunk) in data.chunks(FW_CHUNK_SIZE).enumerate() {
            let pos = (i * FW_CHUNK_SIZE) as u32;
            let len = chunk.len().next_multiple_of(4) as u32;
            self.write_register(FCE_DMA_ADDR | TYPE_CFG, offset + pos)?;
            self.write_register(FCE_DMA_LEN | TYPE_CFG, len << 16)?;
            self.send_command(0, chunk)?;
            let complete = 0xc000_0000 | (len << 16);
            if !self.poll(FCE_DMA_LEN | TYPE_CFG, 0xffff_ffff, complete)? {
                bail!("timeout waiting for firmware chunk at {:#x}", offset + pos);
            }
        }
        Ok(())
    }

    /// True when the MCU is already running firmware (it survives until unplug).
    pub fn firmware_running(&self) -> Result<bool> {
        Ok(self.read_register(FCE_DMA_ADDR | TYPE_CFG)? != 0)
    }

    /// Reset the MCU when firmware is already running, so it can be uploaded again.
    pub fn reset_firmware(&self) -> Result<()> {
        self.apply_rf_patch()?;
        self.dev_mode(FW_RESET_IVB)?;
        // xow waits for 0x80000000; the 02fe adapter reports 0x00000000 instead.
        // Either way the "running" bit 0 clears once the MCU has reset.
        if !self.poll(FCE_DMA_ADDR | TYPE_CFG, 0x01, 0x00)? {
            bail!("timeout waiting for firmware reset");
        }
        Ok(())
    }

    /// Upload firmware and start it.
    ///
    /// xone notes the chip may drop off the USB bus and re-enumerate afterwards,
    /// so the caller reopens the device before using it again.
    pub fn upload_and_start(mut self, fw: &Firmware) -> Result<()> {
        self.write_register(
            USB_U3DMA_CFG | TYPE_CFG,
            USB_DMA_CFG_TX_BULK_EN | USB_DMA_CFG_RX_BULK_EN,
        )?;
        self.write_register(FCE_PSE_CTRL, 0x01)?;
        self.write_register(TX_CPU_FROM_FCE_BASE_PTR, 0x0040_0230)?;
        self.write_register(TX_CPU_FROM_FCE_MAX_COUNT, 0x01)?;
        self.write_register(TX_CPU_FROM_FCE_CPU_DESC_IDX, 0x01)?;
        self.write_register(FCE_PDMA_GLOBAL_CONF, 0x44)?;
        self.write_register(FCE_SKIP_FS, 0x03)?;

        log::info!("uploading ILM ({} bytes)", fw.ilm.len());
        self.send_firmware_part(FW_ILM_OFFSET, fw.ilm)?;
        log::info!("uploading DLM ({} bytes)", fw.dlm.len());
        self.send_firmware_part(FW_DLM_OFFSET, fw.dlm)?;

        self.write_register(FCE_DMA_ADDR | TYPE_CFG, 0)?;
        self.apply_rf_patch()?;
        log::info!("starting MCU");
        if let Err(e) = self.dev_mode(FW_LOAD_IVB) {
            log::debug!("load IVB returned {e:#} (expected if the chip re-enumerated)");
        }
        Ok(())
    }

    /// Check, after (re)opening, that the firmware reports it is running.
    pub fn firmware_started(&self) -> Result<bool> {
        self.poll(FCE_DMA_ADDR | TYPE_CFG, 0x01, 0x01)
    }

    // ---- radio -----------------------------------------------------------

    fn init_registers(&self) -> Result<()> {
        let w = |a, v| self.write_register(a, v);
        w(MAC_SYS_CTRL, MAC_SYS_CTRL_RESET_BBP | MAC_SYS_CTRL_RESET_CSR)?;
        w(USB_DMA_CFG, 0)?;
        w(MAC_SYS_CTRL, 0)?;
        w(PWR_PIN_CFG, 0)?;
        w(LDO_CTRL_1, 0x6b006464)?;
        w(WPDMA_GLO_CFG, 0x70)?;
        w(WMM_AIFSN, 0x2273)?;
        w(WMM_CWMIN, 0x2344)?;
        w(WMM_CWMAX, 0x34aa)?;
        w(FCE_DMA_ADDR, 0x041200)?;
        w(TSO_CTRL, 0)?;
        w(PBF_SYS_CTRL, 0x080c00)?;
        w(PBF_TX_MAX_PCNT, 0x1fbf1f1f)?;
        w(FCE_PSE_CTRL, 0x01)?;
        w(MAC_SYS_CTRL, MAC_SYS_CTRL_ENABLE_RX | MAC_SYS_CTRL_ENABLE_TX)?;
        w(AUTO_RSP_CFG, 0x13)?;
        w(MAX_LEN_CFG, 0x3e3fff)?;
        w(AMPDU_MAX_LEN_20M1S, 0xfffc9855)?;
        w(AMPDU_MAX_LEN_20M2S, 0xff)?;
        w(BKOFF_SLOT_CFG, 0x0109)?;
        w(PWR_PIN_CFG, 0)?;
        w(EDCA_CFG_BASE, 0x064320)?;
        w(EDCA_CFG_BASE + 4, 0x0a4700)?;
        w(EDCA_CFG_BASE + 8, 0x043238)?;
        w(EDCA_CFG_BASE + 12, 0x03212f)?;
        w(TX_PIN_CFG, 0x150f0f)?;
        w(TX_SW_CFG0, 0x101001)?;
        w(TX_SW_CFG1, 0x010000)?;
        w(TXOP_CTRL_CFG, 0x10583f)?;
        w(TX_TIMEOUT_CFG, 0x0a0f90)?;
        w(TX_RETRY_CFG, 0x47d01f0f)?;
        w(CCK_PROT_CFG, 0x03f40003)?;
        w(OFDM_PROT_CFG, 0x03f40003)?;
        w(MM20_PROT_CFG, 0x01742004)?;
        w(GF20_PROT_CFG, 0x01742004)?;
        w(GF40_PROT_CFG, 0x03f42084)?;
        w(EXP_ACK_TIME, 0x2c00dc)?;
        w(TX_ALC_CFG_2, 0x22160a00)?;
        w(TX_ALC_CFG_3, 0x22160a76)?;
        w(TX_ALC_CFG_0, 0x3f3f1818)?;
        w(TX_ALC_CFG_4, 0x0606)?;
        w(PIFS_TX_CFG, 0x060fff)?;
        w(RX_FILTR_CFG, RX_FILTER_NORMAL)?;
        w(LEGACY_BASIC_RATE, 0x017f)?;
        w(HT_BASIC_RATE, 0x8003)?;
        w(PN_PAD_MODE, 0x02)?;
        w(TXOP_HLDR_ET, 0x02)?;
        w(TX_PROT_CFG6, 0xe3f42004)?;
        w(TX_PROT_CFG7, 0xe3f42084)?;
        w(TX_PROT_CFG8, 0xe3f42104)?;
        w(DACCLK_EN_DLY_CFG, 0)?;
        w(RF_PA_MODE_ADJ0, 0xee000000)?;
        w(RF_PA_MODE_ADJ1, 0xee000000)?;
        w(TX0_RF_GAIN_CORR, 0x0f3c3c3c)?;
        w(TX1_RF_GAIN_CORR, 0x0f3c3c3c)?;
        w(PBF_CFG, 0x1efebcf5)?;
        w(PAUSE_ENABLE_CONTROL1, 0x0a)?;
        w(RF_BYPASS_0, 0x7f000000)?;
        w(RF_SETTING_0, 0x1a800000)?;
        w(XIFS_TIME_CFG, 0x33a40e0a)?;
        w(FCE_L2_STUFF, 0x03ff0223)?;
        w(TX_RTS_CFG, 0)?;
        w(BEACON_TIME_CFG, 0x0640)?;
        w(EXT_CCA_CFG, 0xf0e4)?;
        w(CH_TIME_CFG, 0x015f)
    }

    fn calibrate_crystal(&self) -> Result<()> {
        let trim = self.read_efuse(EE_XTAL_TRIM_2, 4)?;
        let mut val = ((trim[3] as u16) << 8) | trim[2] as u16;
        let mut offset = (val & 0x7f) as i16;
        if val & 0xff == 0xff {
            offset = 0;
        } else if val & 0x80 != 0 {
            offset = -offset;
        }
        val >>= 8;
        if val == 0 || val == 0xff {
            let trim = self.read_efuse(EE_XTAL_TRIM_1, 4)?;
            val = (((trim[3] as u16) << 8) | trim[2] as u16) & 0xff;
            if val == 0 || val == 0xff {
                val = 0x14;
            }
        }
        let val = ((val & 0x7f) as i16 + offset) as u16 as u32;
        let ctrl = self.read_register(XO_CTRL5 | TYPE_CFG)?;
        self.write_register(XO_CTRL5 | TYPE_CFG, (ctrl & !(0x7f << 8)) | (val << 8))?;
        self.write_register(XO_CTRL6 | TYPE_CFG, 0x7f << 8)?;
        self.write_register(CMB_CTRL, 0x0091a7ff)
    }

    fn init_address(&mut self) -> Result<()> {
        let fuse = self.read_efuse(EE_MAC_ADDR, 6)?;
        let mut addr: MacAddr = fuse[..6].try_into().unwrap();
        log::debug!("EFUSE MAC address {}", fmt_mac(&addr));
        if addr == [0x62, 0x45, 0xb4, 0xe7, 0xa4, 0xef] {
            log::warn!("MAC address {} is shared by counterfeit adapters", fmt_mac(&addr));
        }
        // Controllers only connect to 62:45:bx:xx:xx:xx.
        if addr[0] != 0x62 {
            addr[0] = 0x62;
            addr[1] = 0x45;
            addr[2] = 0xbd;
        }
        self.address = addr;
        self.write_burst(MAC_ADDR_DW0, &addr)?;
        self.write_burst(MAC_BSSID_DW0, &addr)?;
        self.send_ms_command(MS_SET_MAC_ADDRESS, &addr)
    }

    fn calibrate_radio(&mut self) -> Result<()> {
        self.write_register(BBP_AGC_BASE + (8 << 2), 0x18365efa)?;
        self.write_register(BBP_AGC_BASE + (9 << 2), 0x18365efa)?;
        self.write_register(MAC_SYS_CTRL, 0)?;
        self.write_register(RF_BYPASS_0, 0)?;
        self.write_register(RF_SETTING_0, 0)?;
        self.calibrate(CAL_TEMP_SENSOR, 0)?;
        self.calibrate(CAL_RXDCOC, 1)?;
        self.calibrate(CAL_RC, 0)?;
        self.write_register(MAC_SYS_CTRL, MAC_SYS_CTRL_ENABLE_RX | MAC_SYS_CTRL_ENABLE_TX)
    }

    fn channel_power(&self, ch: &Channel) -> Result<u8> {
        let (addr, idx) = if ch.bandwidth == 0 {
            (EE_TX_POWER_0_START_2G, 4)
        } else {
            (EE_TX_POWER_0_START_5G + ch.group as u16 * TX_POWER_GROUP_SIZE_5G, 5)
        };
        let entry = self.read_efuse(addr, 8)?;
        let target = entry[idx];
        let offset = entry[idx + ch.band as usize];
        // Offset in 0.5 dB steps.
        Ok(if offset & 0x80 != 0 {
            if offset & 0x40 != 0 {
                target.wrapping_add(offset & 0x3f)
            } else {
                target.wrapping_sub(offset & 0x3f)
            }
        } else {
            target
        })
    }

    fn init_channels(&mut self) -> Result<()> {
        self.write_register(RX_FILTR_CFG, RX_FILTER_PAIRING)?;
        let mut best: Option<usize> = None;
        let mut best_pow = 0;
        for i in 0..self.channels.len() {
            let pow = self.channel_power(&self.channels[i])?;
            self.channels[i].power = pow;
            self.switch_channel(self.channels[i])?;
            if pow > best_pow {
                best = Some(i);
                best_pow = pow;
            }
            log::debug!("channel {:#04x}: power {pow}", self.channels[i].index);
        }
        self.channel = best.unwrap_or(self.channels.len() - 1);
        self.write_register(RX_FILTR_CFG, RX_FILTER_NORMAL)?;
        log::info!("selected channel {:#04x}", self.channel_index());

        self.channels[self.channel].scan = true;
        self.switch_channel(self.channels[self.channel])?;
        self.set_power_mode(RADIO_OFF)?;
        sleep(Duration::from_millis(50));
        self.set_power_mode(RADIO_ON)?;
        self.channels[self.channel].scan = false;
        self.switch_channel(self.channels[self.channel])?;

        // Channel candidates: enable, best channel, count, then the others.
        let best = self.channel_index() as u32;
        let mut vals = vec![1, best, self.channels.len() as u32 - 1];
        vals.extend(
            self.channels
                .iter()
                .map(|c| c.index as u32)
                .filter(|&c| c != best),
        );
        let p: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.send_ms_command(MS_SET_CHAN_CANDIDATES, &p)
    }

    /// Bring the radio up. The bulk IN endpoints should already be read.
    pub fn init_radio(&mut self) -> Result<()> {
        if let Ok(id) = self.read_efuse(EE_CHIP_ID, 4) {
            log::info!("chip id {:#06x}", ((id[1] as u16) << 8) | id[2] as u16);
        }
        self.cmd_u32s(CMD_FUN_SET_OP, &[Q_SELECT, 1])?;
        self.set_power_mode(RADIO_ON)?;
        self.send_command(CMD_LOAD_CR, &[RF_BBP_CR, 0, 0, 0])?;
        self.init_registers()?;
        self.calibrate_crystal()?;
        self.init_address()?;
        log::info!("adapter MAC address {}", fmt_mac(&self.address));
        // Prevent clients from disconnecting when idle.
        self.send_ms_command(MS_SET_IDLE_TIME, &64u32.to_le_bytes())?;
        self.calibrate_radio()?;
        self.init_channels()?;
        // Mandatory delay after channel change.
        sleep(Duration::from_secs(1));
        // Normal traffic routing, wake-on-wireless off.
        let mut p = 3u32.to_le_bytes().to_vec();
        p.push(0);
        self.send_command(CMD_WOW_FEATURE, &p)?;
        let mut p = 1u32.to_le_bytes().to_vec();
        p.extend_from_slice(&[0, self.channel_index()]);
        self.send_command(CMD_WOW_FEATURE, &p)?;
        self.set_pairing(false)
    }

    fn write_beacon(&mut self, pair: bool) -> Result<()> {
        // Information element with Microsoft's OUI (00:50:f2).
        let mut ie: [u8; 20] = [
            0x00, 0x00, 0xdd, 0x10, 0x00, 0x50, 0xf2, 0x11, 0x01, 0x10, pair as u8, 0xa5, 0x30,
            0x99, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        ie[14] = self.channel_index();
        let a = self.address;
        let mut mgmt = hdr3(FC_FTYPE_MGMT | FC_STYPE_BEACON, &[0xff; 6], &a, &a).to_vec();
        mgmt.extend_from_slice(&[0; 8]); // timestamp
        mgmt.extend_from_slice(&100u16.to_le_bytes()); // beacon interval
        mgmt.extend_from_slice(&0xc631u16.to_le_bytes()); // capability info
        let mut buf = txwi(TXWI_FLAGS_TS, TXWI_ACK_CTL_NSEQ, 0, mgmt.len() + ie.len()).to_vec();
        buf.extend_from_slice(&mgmt);
        buf.extend_from_slice(&ie);
        self.write_burst(BEACON_BASE, &buf)
    }

    pub fn set_pairing(&mut self, enable: bool) -> Result<()> {
        self.write_register(
            RX_FILTR_CFG,
            if enable { RX_FILTER_PAIRING } else { RX_FILTER_NORMAL },
        )?;
        self.write_beacon(enable)?;
        self.write_register(BEACON_TIME_CFG, BEACON_TIME_CFG_ACTIVE)
    }

    // ---- clients ---------------------------------------------------------

    pub fn pair_client(&mut self, addr: &MacAddr) -> Result<()> {
        let a = self.address;
        let mut f = hdr3(FC_FTYPE_MGMT | WLAN_RESERVED as u16, addr, &a, &a).to_vec();
        f.extend_from_slice(&[WLAN_RESERVED, CLIENT_PAIR_RESP]);
        f.extend_from_slice(&[0x00, 0x45, 0x55, 0x01, 0x0f, 0x8f, 0xff, 0x87, 0x1f]);
        self.send_wlan(&f)
    }

    pub fn associate_client(&mut self, wcid: u8, addr: &MacAddr) -> Result<()> {
        let a = self.address;
        let mut f = hdr3(FC_FTYPE_MGMT | FC_STYPE_ASSOC_RESP, addr, &a, &a).to_vec();
        f.extend_from_slice(&0u16.to_le_bytes()); // capability info
        f.extend_from_slice(&0x0110u16.to_le_bytes()); // status code (original)
        f.extend_from_slice(&0x0f00u16.to_le_bytes()); // association id (original)
        f.extend_from_slice(&[0; 8]);
        self.write_burst(WCID_ADDR_BASE + wcid as u32 * 8, addr)?;
        self.send_ms_command(
            MS_ADD_CLIENT,
            &[wcid - 1, 0x00, 0x00, 0x00, 0x40, 0x1f, 0x00, 0x00],
        )?;
        self.send_wlan(&f)
    }

    pub fn remove_client(&mut self, wcid: u8) -> Result<()> {
        self.send_ms_command(MS_REMOVE_CLIENT, &[wcid - 1, 0, 0, 0])?;
        self.write_burst(WCID_ADDR_BASE + wcid as u32 * 8, &[0; 6])
    }

    /// Send a GIP buffer to a client as a QoS data frame.
    pub fn send_client_data(&mut self, wcid: u8, addr: &MacAddr, gip: &[u8]) -> Result<()> {
        let a = self.address;
        let mut hdr = hdr3(FC_FTYPE_DATA | FC_STYPE_QOS_DATA | FC_FROMDS, addr, &a, &a).to_vec();
        hdr[2..4].copy_from_slice(&144u16.to_le_bytes()); // duration (µs)
        hdr.extend_from_slice(&[0, 0]); // QoS control
        let mut buf = vec![0x00, 0x00, 0x00 /* data queue */, wcid - 1, 0, 0, 0, 0];
        buf.extend_from_slice(&txwi(
            TXWI_FLAGS_MPDU_DENSITY_4,
            TXWI_ACK_CTL_REQ,
            wcid - 1,
            hdr.len() + gip.len(),
        ));
        buf.extend_from_slice(&hdr);
        buf.extend_from_slice(&[0, 0]); // L2 padding
        buf.extend_from_slice(gip);
        self.send_command(0, &buf)
    }
}
