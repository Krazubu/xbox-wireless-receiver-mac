// SPDX-License-Identifier: GPL-2.0-or-later
//! Userspace driver for the Xbox Wireless Adapter on macOS.

mod dongle;
mod gip;
mod hid;
mod mt76;

use std::{
    path::PathBuf,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use nusb::{DeviceInfo, MaybeFuture};

use mt76::{Firmware, Mt76, ADAPTERS, MICROSOFT_VID};

const USAGE: &str =
    "usage: xbox-dongle <list | load-firmware <fw.bin> | run <fw.bin> | serve <fw.bin>>";

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
fn load_firmware(path: PathBuf) -> Result<Mt76> {
    let info = find_adapter()?.context("no Xbox Wireless Adapter found")?;
    println!("adapter: {}", describe(&info));
    let path = if path.is_dir() {
        path.join(format!("xbox_dongle_{:04x}.bin", info.product_id()))
    } else {
        path
    };
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
/// unplugged or fails. Meant for the launchd agent.
fn serve(fw: PathBuf) -> Result<()> {
    loop {
        if find_adapter()?.is_none() {
            log::info!("waiting for an Xbox Wireless Adapter...");
            wait_for_adapter(Duration::MAX)?;
        }
        let result = load_firmware(fw.clone()).and_then(|mt| dongle::Dongle::new(mt).run());
        if let Err(e) = result {
            log::warn!("adapter stopped: {e:#}");
        }
        sleep(Duration::from_secs(2));
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1);
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
