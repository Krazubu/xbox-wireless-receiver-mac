# xbox-dongle

Userspace driver for the **Xbox Wireless Adapter** (Microsoft's USB dongle) on
macOS. It drives the dongle, pairs and connects Xbox controllers, and presents
them to macOS as genuine Xbox controllers, recognized by the GameController
framework (native Mac games), SDL, Steam and web browsers.

Written in Rust, with no kernel extension or DriverKit, and without a paid
Apple developer account — at the cost of reduced system security (see
[Requirements](#requirements-security)).

## Status

Tested on an M4 Mac (macOS 27.2), "slim" adapter `045e:02fe`, Xbox Series
controller (firmware 5.23.6.0).

| Feature | Status |
|---|---|
| Firmware loading (cold and warm) | ✅ |
| Pairing, automatic reconnection, LEDs | ✅ |
| Inputs: 15 buttons, d-pad, triggers, sticks | ✅ |
| Recognized by GameController as an Xbox controller | ✅ |
| Rumble (grips and triggers) | ✅ |
| Guide button delivered to applications | ❌ reserved by macOS |
| Radio link encryption | ❌ not implemented (not needed for this controller) |
| Several controllers at once | ⚠️ supported in the code, never tried |
| Headset audio, controller firmware updates | ❌ |

Supported adapters: `045e:02e6` (original), `045e:02fe` (slim), `045e:02f9` and
`045e:091e` (built-in). Only the `02fe` has been tried.

## Requirements (security)

The virtual controller is created with the `IOHIDUserDevice` API, which
requires the restricted entitlement `com.apple.developer.hid.virtual.device`.
Without an Apple provisioning profile, a binary signed with that entitlement is
killed at launch unless **SIP is disabled and AMFI is relaxed**.

On an Apple Silicon Mac: shut down, hold the power button → **Options** →
**Utilities → Terminal**, then:

```bash
csrutil disable
nvram boot-args="amfi_get_out_of_my_way=1"
```

and restart. Check with `csrutil status` and `nvram boot-args`.

> ⚠️ This lowers security for the whole machine: SIP no longer protects the
> system, and AMFI no longer checks signatures or entitlements of any program.
> iPhone/iPad apps on the Mac may stop working.

To revert (same recovery Terminal):

```bash
nvram -d boot-args
csrutil enable
```

Without these settings the driver still works (pairing, inputs in the log) with
an unsigned `cargo build`, but no controller shows up in macOS.

Other requirements: Rust (2024 edition), the Xcode command line tools
(`codesign`, `swiftc`), `curl`.

## Installation

1. **Fetch the adapter firmware** from the Microsoft Update Catalog. It is not
   redistributed with this project and remains subject to
   [Microsoft's terms of use](https://www.microsoft.com/en-us/legal/terms-of-use).
   Get your adapter model with `cargo run -- list`.

   ```bash
   ./scripts/fetch-firmware.sh 02fe
   ```

2. **Install the menu bar app**:

   ```bash
   ./scripts/install-app.sh
   ```

   The script builds `Xbox Dongle.app` (the Swift app plus the driver, signed
   with the entitlement and bundled inside), installs it in `/Applications`,
   copies the firmware to `~/Library/Application Support/xbox-dongle/firmware/`
   and launches the app. It removes the launchd agent if it was installed.

   The app starts the driver, restarts it if it exits, and shows its state in
   the menu bar:

   | Icon | State |
   |---|---|
   | dimmed controller | waiting for the adapter |
   | controller outline | adapter ready, no controller |
   | filled controller | at least one controller connected |
   | blinking | pairing in progress |
   | triangle | error (details in the menu) |

   The menu (in French) lists the controllers and their battery, and offers
   pairing (« Appairer une manette »), firmware download if it is missing
   (« Télécharger le firmware… »), open at login (« Ouvrir à l'ouverture de
   session »), the log and a driver restart.

   Log: `~/Library/Logs/xbox-dongle.log`.

   *Without the app*: `./scripts/install-agent.sh` installs the driver alone as
   a launchd agent (started at login, no icon). Do not use both at once.

3. **Pair a controller**: press the button on the adapter (its LED blinks),
   then hold the controller's pair button until its logo blinks fast. After
   that, the controller reconnects by itself when turned on.

   If the controller was paired with the Mac over Bluetooth, forget it in
   System Settings → Bluetooth, otherwise it will reconnect there instead of to
   the adapter.

Uninstall: quit the app and delete `/Applications/Xbox Dongle.app` (and
`~/Library/Application Support/xbox-dongle`), or run
`./scripts/uninstall-agent.sh` for the agent setup.

## Manual use

```bash
./scripts/build-signed.sh            # release build, signed
./target/release/xbox-dongle list    # detect the adapter
./target/release/xbox-dongle run firmware/xbox_dongle_02fe.bin
```

| Command | Purpose |
|---|---|
| `list` | Show the connected adapter and the firmware it needs |
| `load-firmware <fw>` | Only load the firmware |
| `run <fw>` | Drive the adapter until it is unplugged |
| `serve <fw>` | Like `run`, but waits and restarts forever (used by the app and the agent) |

`<fw>` is a firmware file or a directory of `xbox_dongle_<pid>.bin` files (the
right one is picked for the connected adapter).

With `--json` as the first argument, the driver writes its state as JSON on
stdout, one line per event, and reads commands on stdin (`pair`). This is how
the app talks to it, see [src/status.rs](src/status.rs).

Verbose logs: `RUST_LOG=xbox_dongle=debug` (button checklist, rumble,
controller command frames) or `=trace` (every radio and GIP frame).

Do not use `run` while the app or the agent is running: only one program can
drive the adapter. Quit the app from its menu, or stop the agent with
`launchctl bootout gui/$(id -u)/com.github.xbox-dongle`.

## How it works

```
controller ──radio──▶ adapter (MT76 chip + Microsoft firmware)
                         │ USB (vendor requests, bulk)
                         ▼
                  xbox-dongle (nusb)  ◀──JSON──▶  Xbox Dongle.app (menu bar)
                    ├─ mt76.rs    firmware, radio, beacon, 802.11 clients
                    ├─ dongle.rs  event loop, pairing, clients
                    ├─ gip.rs     GIP protocol (announce, identify, inputs, rumble)
                    ├─ hid.rs     IOHIDUserDevice virtual controller
                    └─ status.rs  JSON state and commands for the app
                         │
                         ▼
           macOS: GameController, SDL, Steam, browsers
```

- **Adapter.** A MediaTek MT76 chip (`0x7613`) on a proprietary 802.11 link at
  5 GHz. The driver uploads its firmware, configures the radio, sends the
  pairing beacon and associates controllers (up to 16).
- **GIP.** Controllers speak the *Gaming Input Protocol*, documented by
  Microsoft in [MS-GIPUSB](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-gipusb/e7c90904-5e21-426e-b9ad-d82adeee0dbc).
- **Virtual controller.** It mirrors an Xbox Series controller over Bluetooth
  LE (`045e:0b13`) exactly: same HID report descriptor (read from a real
  controller with `ioreg`), same properties. macOS itself creates Bluetooth LE
  controllers as virtual devices, and its GameController personality accepts
  virtual devices for this model (`GCIOMatchVirtual`), so the controller is
  recognized as an Xbox one. Rumble arrives in output report 3, which has the
  same layout as the GIP rumble command.

## Notable differences from xone

- After a pairing request, pairing stays open until the controller associates
  (xone closes it right away, which sometimes left the controller stuck
  searching).
- Warm firmware restart uses xow's method (reset, then reload); the `02fe`
  adapter reports the reset as `0` rather than `0x80000000`.
- Share button: byte at index 18 of the input report, with or without the
  latency counters at the end of the report.

## Credits and license

GPL-2.0-or-later. The radio and GIP code is ported from
[xone](https://github.com/dlundqvist/xone) and [xow](https://github.com/medusalix/xow)
(Severin von Wnuck-Lipinski and contributors, GPL-2.0-or-later), with help from
Microsoft's MS-GIPUSB specification.

The adapter firmware belongs to Microsoft and is not distributed here.
