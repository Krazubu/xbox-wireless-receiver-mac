// SPDX-License-Identifier: GPL-2.0-or-later
//! Machine-readable status for the menu bar app.
//!
//! With `--json`, the driver writes one JSON object per line on stdout
//! (`{"event": "...", ...}`) and reads commands, one per line, on stdin.
//! Other stdout lines are human-readable and should be ignored by the app.
//!
//! Events: `waiting`, `starting` {adapter, pid}, `ready` {address, channel},
//! `pairing` {on}, `connected` {wcid, mac}, `controller` {wcid, firmware,
//! product}, `battery` {wcid, kind, level}, `disconnected` {wcid, reason},
//! `stopped` {message}, `error` {message, firmware_missing?}.
//! Commands: `pair` (open pairing for 60 s).

use std::{
    io::{BufRead, Write},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use serde_json::Value;

static JSON: AtomicBool = AtomicBool::new(false);
static PAIR_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Turn on JSON events and start reading commands from stdin.
pub fn enable() {
    JSON.store(true, Ordering::Relaxed);
    thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match line.trim() {
                "pair" => PAIR_REQUESTED.store(true, Ordering::Relaxed),
                "" => {}
                other => log::warn!("unknown command {other:?}"),
            }
        }
    });
}

/// Emit a status event: `emit("connected", json!({"wcid": 1}))`.
pub fn emit(event: &str, fields: Value) {
    if !JSON.load(Ordering::Relaxed) {
        return;
    }
    let mut obj = match fields {
        Value::Object(m) => m,
        _ => Default::default(),
    };
    obj.insert("event".into(), Value::String(event.into()));
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", Value::Object(obj));
    let _ = out.flush();
}

/// True once after the app asked for pairing.
pub fn take_pair_request() -> bool {
    PAIR_REQUESTED.swap(false, Ordering::Relaxed)
}
