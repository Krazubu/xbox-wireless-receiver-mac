// SPDX-License-Identifier: GPL-2.0-or-later
//! Virtual HID gamepad exposed to macOS through IOHIDUserDevice.
//!
//! The device imitates an Xbox Series controller connected over Bluetooth LE
//! (045e:0b13): same report descriptor, IDs and transport as the one bluetoothd
//! creates. macOS' GameController personality matches that product and accepts
//! virtual devices for it (`GCIOMatchVirtual`), so games see a real Xbox pad.
//!
//! Creating the device needs the restricted entitlement
//! `com.apple.developer.hid.virtual.device`. Without a provisioning profile that
//! only works with SIP disabled and AMFI relaxed (see README).

use std::{
    ffi::{c_void, CString},
    ptr,
};

use anyhow::{bail, Result};
use block2::{Block, RcBlock};

use crate::gip::GamepadState;

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFIndex = isize;
type IOHIDUserDeviceRef = *mut c_void;

#[repr(C)]
struct CFDictionaryKeyCallBacks([u8; 0]);
#[repr(C)]
struct CFDictionaryValueCallBacks([u8; 0]);

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(dict: CFMutableDictionaryRef, key: CFTypeRef, value: CFTypeRef);
    fn CFStringCreateWithCString(alloc: CFAllocatorRef, s: *const i8, enc: u32) -> CFStringRef;
    fn CFNumberCreate(alloc: CFAllocatorRef, ty: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFDataCreate(alloc: CFAllocatorRef, bytes: *const u8, len: CFIndex) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDUserDeviceCreateWithProperties(
        allocator: CFAllocatorRef,
        properties: CFDictionaryRef,
        options: u32,
    ) -> IOHIDUserDeviceRef;
    fn IOHIDUserDeviceHandleReportWithTimeStamp(
        device: IOHIDUserDeviceRef,
        timestamp: u64,
        report: *const u8,
        len: CFIndex,
    ) -> i32;
    fn IOHIDUserDeviceSetDispatchQueue(device: IOHIDUserDeviceRef, queue: DispatchQueue);
    fn IOHIDUserDeviceRegisterSetReportBlock(device: IOHIDUserDeviceRef, block: &SetReportBlock);
    fn IOHIDUserDeviceActivate(device: IOHIDUserDeviceRef);
    fn IOHIDUserDeviceCancel(device: IOHIDUserDeviceRef);
}

type DispatchQueue = *mut c_void;
/// `IOReturn (^)(IOHIDReportType type, uint32_t reportID, const uint8_t *report, CFIndex len)`
type SetReportBlock = Block<dyn Fn(u32, u32, *const u8, CFIndex) -> i32>;
const K_IOHID_REPORT_TYPE_OUTPUT: u32 = 1;
const K_IORETURN_UNSUPPORTED: i32 = 0xe00002c7_u32 as i32;

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
}

/// Rumble command from macOS (output report 3, without the report ID):
/// motor enable bits, then magnitudes 0..=100 for left trigger, right trigger,
/// strong (left) and weak (right) motors, then duration, delay and loop count.
pub type Rumble = [u8; 8];

pub const XBOX_VID: i32 = 0x045e;
/// Xbox Series X|S controller, Bluetooth LE.
pub const XBOX_SERIES_BT_PID: i32 = 0x0b13;
/// VersionNumber reported by the real controller over Bluetooth (fw 5.23).
const VERSION_NUMBER: i32 = 1315;

/// Report descriptor read from a real Xbox Series controller over Bluetooth
/// (firmware 5.23.6.0) with `ioreg`. Input report 1 (16 bytes after the ID):
///   0..8   X, Y, Z, Rz       u16, 0..65535, centre 32768, Y down
///   8..10  brake (LT)        10 bits
///   10..12 accelerator (RT)  10 bits
///   12     hat switch        low nibble, 1..8 clockwise from north, 0 = centred
///   13..15 buttons 1..15
///   15     bit 0: consumer "Record" (Share)
/// Output report 3 is rumble (not handled yet).
const REPORT_DESCRIPTOR_HEX: &str = "05010905a10185010901a10009300931150027ffff0000950275108102c00901a10009320935150027ffff0000950275108102c0050209c5150026ff039501750a810215002500750695018103050209c4150026ff039501750a81021500250075069501810305010939150125083500463b016614007504950181427504950115002500350045006500810305091901290f150025017501950f810215002500750195018103050c0ab2001500250195017501810215002500750795018103050f09218503a102099715002501750495019102150025007504950191030970150025647508950491020950660110550e150026ff0075089501910209a7150026ff0075089501910265005500097c150026ff00750895019102c0c0";

fn report_descriptor() -> Vec<u8> {
    (0..REPORT_DESCRIPTOR_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&REPORT_DESCRIPTOR_HEX[i..i + 2], 16).unwrap())
        .collect()
}

// GIP button bit -> Bluetooth report button bit (classic Xbox BT layout).
const BUTTON_MAP: [(u16, u16); 10] = [
    (1 << 4, 1 << 0),   // A
    (1 << 5, 1 << 1),   // B
    (1 << 6, 1 << 3),   // X
    (1 << 7, 1 << 4),   // Y
    (1 << 12, 1 << 6),  // LB
    (1 << 13, 1 << 7),  // RB
    (1 << 3, 1 << 10),  // View
    (1 << 2, 1 << 11),  // Menu
    (1 << 14, 1 << 13), // LS
    (1 << 15, 1 << 14), // RS
];
const BT_GUIDE: u16 = 1 << 12;

pub const REPORT_LEN: usize = 17;

/// Build input report 1 (report ID included).
pub fn report(s: &GamepadState, guide: bool) -> [u8; REPORT_LEN] {
    let mut buttons = 0u16;
    for (gip, bt) in BUTTON_MAP {
        if s.buttons & gip != 0 {
            buttons |= bt;
        }
    }
    if guide {
        buttons |= BT_GUIDE;
    }
    let (up, down, left, right) = (
        s.buttons & (1 << 8) != 0,
        s.buttons & (1 << 9) != 0,
        s.buttons & (1 << 10) != 0,
        s.buttons & (1 << 11) != 0,
    );
    let hat: u8 = match (up, right, down, left) {
        (true, false, false, false) => 1,
        (true, true, false, false) => 2,
        (false, true, false, false) => 3,
        (false, true, true, false) => 4,
        (false, false, true, false) => 5,
        (false, false, true, true) => 6,
        (false, false, false, true) => 7,
        (true, false, false, true) => 8,
        _ => 0,
    };
    // GIP axes are signed with Y up; the BT report is unsigned with Y down.
    let x = |v: i16| (v as i32 + 32768) as u16;
    let y = |v: i16| (32767 - v as i32).clamp(0, 65535) as u16;
    let mut r = [0u8; REPORT_LEN];
    r[0] = 0x01; // report ID
    r[1..3].copy_from_slice(&x(s.stick_left.0).to_le_bytes());
    r[3..5].copy_from_slice(&y(s.stick_left.1).to_le_bytes());
    r[5..7].copy_from_slice(&x(s.stick_right.0).to_le_bytes());
    r[7..9].copy_from_slice(&y(s.stick_right.1).to_le_bytes());
    r[9..11].copy_from_slice(&s.trigger_left.min(1023).to_le_bytes());
    r[11..13].copy_from_slice(&s.trigger_right.min(1023).to_le_bytes());
    r[13] = hat;
    r[14..16].copy_from_slice(&buttons.to_le_bytes());
    r[16] = s.share as u8;
    r
}

pub struct VirtualGamepad {
    device: IOHIDUserDeviceRef,
    queue: DispatchQueue,
    _set_report: RcBlock<dyn Fn(u32, u32, *const u8, CFIndex) -> i32>,
}

/// Parse an output report from macOS into a rumble command.
fn parse_output(report_id: u32, data: &[u8]) -> Option<Rumble> {
    // Depending on the caller the buffer may or may not start with the report ID.
    let body = match data {
        [3, rest @ ..] if rest.len() == 8 => rest,
        _ if data.len() == 8 && report_id == 3 => data,
        _ => return None,
    };
    body.try_into().ok()
}

// The IOHIDUserDevice is only used from the event-loop thread.
unsafe impl Send for VirtualGamepad {}

fn cfstr(s: &str) -> CFStringRef {
    let c = CString::new(s).unwrap();
    // SAFETY: valid NUL-terminated string; caller releases the result.
    unsafe { CFStringCreateWithCString(ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

impl VirtualGamepad {
    /// `serial` should be unique per controller (we use its MAC address).
    /// `on_rumble` runs on a dispatch queue thread whenever macOS sends rumble.
    pub fn new(
        serial: &str,
        firmware: &str,
        on_rumble: impl Fn(Rumble) + Send + Sync + 'static,
    ) -> Result<Self> {
        let descriptor = report_descriptor();
        // SAFETY: plain CoreFoundation object construction; every object we create
        // is released after being retained by the dictionary.
        unsafe {
            let dict = CFDictionaryCreateMutable(
                ptr::null(),
                0,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let set = |k: &str, v: CFTypeRef| {
                let key = cfstr(k);
                CFDictionarySetValue(dict, key, v);
                CFRelease(key);
                CFRelease(v);
            };
            let num = |n: i32| {
                CFNumberCreate(ptr::null(), K_CF_NUMBER_SINT32_TYPE, &n as *const i32 as *const c_void)
            };
            set(
                "ReportDescriptor",
                CFDataCreate(ptr::null(), descriptor.as_ptr(), descriptor.len() as CFIndex),
            );
            set("VendorID", num(XBOX_VID));
            set("VendorIDSource", num(2)); // USB-IF
            set("ProductID", num(XBOX_SERIES_BT_PID));
            set("VersionNumber", num(VERSION_NUMBER));
            set("CountryCode", num(0));
            set("ReportInterval", num(8000));
            set("Product", cfstr("Xbox Wireless Controller"));
            set("Manufacturer", cfstr("Microsoft"));
            set("SerialNumber", cfstr(serial));
            set("Transport", cfstr("Bluetooth Low Energy"));
            set("kBTFirmwareRevisionKey", cfstr(firmware));
            let device = IOHIDUserDeviceCreateWithProperties(ptr::null(), dict, 0);
            CFRelease(dict);
            if device.is_null() {
                bail!(
                    "IOHIDUserDeviceCreateWithProperties refused: the binary needs the \
                     com.apple.developer.hid.virtual.device entitlement (see README)"
                );
            }
            let set_report = RcBlock::new(
                move |ty: u32, id: u32, report: *const u8, len: CFIndex| -> i32 {
                    if ty != K_IOHID_REPORT_TYPE_OUTPUT || report.is_null() || len <= 0 {
                        return K_IORETURN_UNSUPPORTED;
                    }
                    // SAFETY: IOKit passes a buffer of `len` bytes valid for the call.
                    let data = std::slice::from_raw_parts(report, len as usize);
                    match parse_output(id, data) {
                        Some(r) => {
                            on_rumble(r);
                            0
                        }
                        None => {
                            log::debug!("ignored output report {id}: {data:02x?}");
                            K_IORETURN_UNSUPPORTED
                        }
                    }
                },
            );
            let queue = dispatch_queue_create(c"xbox-dongle.hid".as_ptr(), ptr::null());
            IOHIDUserDeviceSetDispatchQueue(device, queue);
            IOHIDUserDeviceRegisterSetReportBlock(device, &set_report);
            IOHIDUserDeviceActivate(device);
            Ok(Self { device, queue, _set_report: set_report })
        }
    }

    pub fn send(&self, report: &[u8]) -> Result<()> {
        // SAFETY: device is a live IOHIDUserDevice, report is a valid buffer.
        let ret = unsafe {
            IOHIDUserDeviceHandleReportWithTimeStamp(
                self.device,
                mach_absolute_time(),
                report.as_ptr(),
                report.len() as CFIndex,
            )
        };
        if ret != 0 {
            bail!("IOHIDUserDeviceHandleReport failed: {ret:#x}");
        }
        Ok(())
    }
}

impl Drop for VirtualGamepad {
    fn drop(&mut self) {
        // SAFETY: we own both references. Cancelling stops callbacks; releasing the
        // device removes it from the system.
        unsafe {
            IOHIDUserDeviceCancel(self.device);
            CFRelease(self.device);
            dispatch_release(self.queue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_dump() {
        let d = report_descriptor();
        assert_eq!(d.len(), 283);
        assert_eq!(&d[..8], &[0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01]);
        assert_eq!(&d[d.len() - 2..], &[0xc0, 0xc0]);
    }

    #[test]
    fn output_report_parsing() {
        let r = [0x0f, 10, 20, 30, 40, 50, 0, 1];
        let mut with_id = vec![3];
        with_id.extend_from_slice(&r);
        assert_eq!(parse_output(3, &with_id), Some(r));
        assert_eq!(parse_output(3, &r), Some(r));
        assert_eq!(parse_output(1, &[0u8; 4]), None);
    }

    #[test]
    fn report_layout() {
        let s = GamepadState {
            share: true,
            buttons: (1 << 4) | (1 << 8) | (1 << 11) | (1 << 2), // A, up+right, Menu
            trigger_left: 1023,
            trigger_right: 2000,
            stick_left: (0, i16::MIN),
            stick_right: (i16::MAX, i16::MAX),
        };
        let r = report(&s, true);
        let u16_at = |o: usize| u16::from_le_bytes([r[o], r[o + 1]]);
        assert_eq!(r[0], 1);
        assert_eq!(u16_at(1), 32768); // centred X
        assert_eq!(u16_at(3), 65535); // stick fully down
        assert_eq!(u16_at(5), 65535); // right X full right
        assert_eq!(u16_at(7), 0); // right Y fully up
        assert_eq!(u16_at(9), 1023);
        assert_eq!(u16_at(11), 1023); // clamped
        assert_eq!(r[13], 2); // up-right
        assert_eq!(u16_at(14), 1 | (1 << 11) | BT_GUIDE);
        assert_eq!(r[16], 1);
    }
}
