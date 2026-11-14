// SPDX-License-Identifier: GPL-2.0-or-later
//! Userspace driver for the Xbox Wireless Adapter on macOS.

mod dongle;
mod gip;
mod hid;
mod mt76;
mod status;

use std::{
    path::PathBuf,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use nusb::{DeviceInfo, MaybeFuture};
use serde_json::json;

use mt76::{Firmware, Mt76, ADAPTERS, MICROSOFT_VID};

const USAGE: &str = "usage: xbox-dongle [--json] <list | load-firmware <fw> | run <fw> | serve <fw>>\n\
     <fw> is a firmware file or a directory of xbox_dongle_<pid>.bin files";

fn find_adapter() -> Result<Option<DeviceInfo>> {
    Ok(nusb::list_devices()
        .wait()?
        .find(|d| d.vendor_id() == MICROSOFT_VID && ADAPTERS.iter().any(|a| a.0 == d.product_id())))
}

fn describe(d: &DeviceInfo) -> String {
    let (_, name, fw) = ADAPTERS.iter().find(|a| a.0 == d.product_id()).unwrap();
    format!(
        "{name} [{:04x}:{:04x}] location {:#010x}, firmware file {fw}",
        d.vendor_id(),
        d.product_id(),
        d.location_id()
    )
}

fn wait_for_adapter(timeout: Duration) -> Result<DeviceInfo> {
    let start = Instant::now();
    loop {
        if let Some(d) = find_adapter()? {
            return Ok(d);
        }
        if start.elapsed() > timeout {
            bail!("adapter did not come back within {timeout:?}");
        }
        sleep(Duration::from_millis(100));
    }
}

fn cmd_list() -> Result<()> {
    match find_adapter()? {
        Some(d) => println!("{}", describe(&d)),
        None => println!("no Xbox Wireless Adapter found"),
    }
    Ok(())
}

/// Upload and start the firmware, then return a freshly opened adapter.
/// `path` is a firmware file, or a directory holding `xbox_dongle_<pid>.bin`
/// files (as written by scripts/fetch-firmware.sh).
fn firmware_path(path: &std::path::Path, pid: u16) -> PathBuf {
    if path.is_dir() {
        path.join(format!("xbox_dongle_{pid:04x}.bin"))
    } else {
        path.to_path_buf()
    }
}

fn adapter_name(pid: u16) -> &'static str {
    ADAPTERS.iter().find(|a| a.0 == pid).map_or("Xbox Wireless Adapter", |a| a.1)
}

fn load_firmware(path: PathBuf) -> Result<Mt76> {
    let info = find_adapter()?.context("no Xbox Wireless Adapter found")?;
    println!("adapter: {}", describe(&info));
    status::emit(
        "starting",
        json!({"adapter": adapter_name(info.product_id()), "pid": format!("{:04x}", info.product_id())}),
    );
    let path = firmware_path(&path, info.product_id());
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let fw = Firmware::parse(&data)?;
    println!(
        "firmware: {} build {} fw_ver {:#06x} build_ver {:#06x}, ILM {} B, DLM {} B",
        path.display(),
        fw.build_time,
        fw.fw_ver,
        fw.build_ver,
        fw.ilm.len(),
        fw.dlm.len()
    );
    let mt = Mt76::open(info.open().wait()?)?;

    if mt.firmware_running()? {
        println!("firmware already running, resetting the MCU before reloading");
        mt.reset_firmware()?;
    }

    mt.upload_and_start(&fw)?;
    sleep(Duration::from_millis(500));
    let info = wait_for_adapter(Duration::from_secs(10))?;
    let mt = Mt76::open(info.open().wait()?)?;
    if !mt.firmware_started()? {
        bail!("firmware did not report running after start");
    }
    println!("firmware is running");
    Ok(mt)
}

/// Run forever: wait for the adapter, drive it, and start over when it is
/// unplugged or fails. Used by the menu bar app and the launchd agent.
fn serve(fw: PathBuf) -> Result<()> {
    loop {
        if find_adapter()?.is_none() {
            log::info!("waiting for an Xbox Wireless Adapter...");
            status::emit("waiting", json!({}));
            wait_for_adapter(Duration::MAX)?;
        }
        // Wait (quietly) for the firmware file if it is missing.
        if let Some(info) = find_adapter()? {
            let path = firmware_path(&fw, info.product_id());
            if !path.exists() {
                let pid = format!("{:04x}", info.product_id());
                log::warn!("missing firmware {}", path.display());
                status::emit(
                    "error",
                    json!({"message": format!("firmware missing for adapter {pid}"), "firmware_missing": pid}),
                );
                while !path.exists() && find_adapter()?.is_some() {
                    sleep(Duration::from_secs(1));
                }
                continue;
            }
        }
        let result = load_firmware(fw.clone()).and_then(|mt| dongle::Dongle::new(mt).run());
        if let Err(e) = result {
            log::warn!("adapter stopped: {e:#}");
            status::emit("stopped", json!({"message": format!("{e:#}")}));
        }
        sleep(Duration::from_secs(2));
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("--json") {
        args.next();
        status::enable();
    }
    let cmd = args.next();
    let mut fw = || args.next().map(PathBuf::from).context(USAGE);
    match cmd.as_deref() {
        Some("list") | None => cmd_list(),
        Some("load-firmware") => load_firmware(fw()?).map(drop),
        Some("run") => dongle::Dongle::new(load_firmware(fw()?)?).run(),
        Some("serve") => serve(fw()?),
        Some(other) => bail!("unknown command {other:?}\n{USAGE}"),
    }
}
