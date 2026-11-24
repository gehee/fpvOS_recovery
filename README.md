# fpvOS Recovery

A small native app (macOS and Windows) that puts an fpvOS-compatible device
(Skyzone SteadyDigital, BETAFPV P1 Air) into **fpvOS recovery mode**, so the fpvOS web flasher can take over and install over
USB. It is the one desktop-specific step; everything after it is plain Web
Serial and works on any OS. The window is styled like the web flasher and shows
the connected compatible hardware live.

It enters recovery through the stock Artosyn U-Boot **"USB download gadget" HID
loader** — the same RAM recovery the browser tool uses on Linux. A native app is
needed because neither macOS WebHID nor the Windows HID stack will deliver the
loader's reports; the app talks to its interrupt endpoints directly over libusb.

Everything it does lives in RAM. It never writes NAND/flash: a power cycle
returns the unit to untouched stock at any point (up until the web flasher
writes flash).

## What it does

1. Reboots the unit from stock into the Artosyn `1d6b:0100` bootloader.
2. Claims that gadget over libusb and runs the loader's report-255 probe: for a
   P1 the 1.0.44 (`v1`) loader's first, the 2.x (`v2`) one's only if that gets
   no reply.
3. Verifies the bootloader before writing anything: four 64-byte fingerprints
   and the command dispatch table must match the known-good layout found.
4. Loads the recovery device tree, initrd and kernel into RAM and checksums each
   one **on the device** with U-Boot's `crc32`, showing progress and a live
   time-left estimate.
5. Boots it into fpvOS recovery mode, then offers a button to open the web
   flasher to finish over Web Serial.

The recovery bundles (`assets/steadydigital-updater/browser-v1/`,
`assets/p1-updater/browser-v1/`) and the Chakra Petch fonts (`fonts/`, OFL) are
copies of the web flasher's own files
([fpvOS_flasher](https://github.com/gehee/fpvOS_flasher)), embedded at build time
and checked against their SHA-256 at startup. `tools/sync-bundles.sh
<fpvOS_flasher checkout>` refreshes them and repins the hashes in
`src/engine.rs`.

## macOS

```sh
brew install libusb
./build-macos.sh
./target/release/fpvos-recovery
```

Run the app and press **Start recovery**; macOS prompts for your administrator
password when it needs to claim USB (the window itself runs as you, and launches a short
elevated helper). No `sudo` needed to open the app or to see the connected
device. `--cli` runs the flow in the terminal instead (there, use `sudo`).

The app is not notarized, so a browser download meets Gatekeeper's "cannot
verify" block the first time. The web flasher's main macOS path avoids it with
a Terminal command that fetches the same zip with curl (curl sets no quarantine
flag, so Gatekeeper never assesses the app) and opens it from a temporary
folder:

```sh
D=$(mktemp -d) && curl -fsSL 'https://github.com/gehee/fpvOS_recovery/releases/latest/download/fpvOS-Recovery-macOS.zip' -o "$D/app.zip" && ditto -xk "$D/app.zip" "$D" && open "$D/fpvOS Recovery.app"
```

It relies on the zip keeping `fpvOS Recovery.app` at its top level (`ditto
--keepParent` in the workflow). Notarizing the app would make the plain
download open just as cleanly.

## Windows

The download is one self-contained `fpvOS Recovery.exe`: libusb is linked in
statically and the WinUSB helper is embedded, so it runs as is, even opened
straight from the zip.

The Windows HID driver will not pass the loader's reports, so the loader must be
bound to **WinUSB** once per PC. The app does it: it writes out its embedded
`fpvos-winusb.exe`, which asks for administrator rights with one UAC prompt
and binds the inbox WinUSB driver to the bootloader's hardware id
`USB\VID_1D6B&PID_0100` only. Other devices are left alone. If the helper is
missing, the prompt is declined or the install fails, the app falls back to
showing the manual [Zadig](https://zadig.akeo.ie) steps (Options → List All
Devices → "USB download gadget" `1D6B 0100` → WinUSB) and continues once it's
bound. The helper is libwdi's `wdi-simple` (LGPL-3.0, Zadig's driver library),
built by `windows/build-winusb-helper.sh`; the build embeds
`windows/out/fpvos-winusb.exe`, or the file `FPVOS_WINUSB_HELPER` names.

It talks to Windows through its own device APIs (SetupAPI for the devices and
their drivers, the COM port directly for the reboot, ShellExecute for the one
elevation): no PowerShell or command shell, which antivirus heuristics flag.
The helper is written to `%LOCALAPPDATA%\fpvOS Recovery`, and both exes carry
publisher details. Unsigned, a new build can still be flagged by machine-learning
detections until it is signed or Microsoft clears it.

Build with the Rust MSVC toolchain and a static libusb
(`vcpkg install libusb:x64-windows-static-md`):

```powershell
$env:LIBUSB_LIB_DIR = "C:\vcpkg\installed\x64-windows-static-md\lib"
./build-windows.ps1
./target/release/fpvos-recovery.exe
```

The app needs no administrator rights; only the one-time driver install does.

To cross-build from macOS instead (`brew install rustup mingw-w64`, plus
libusb's release `MinGW64/static` folder):

```sh
rustup target add x86_64-pc-windows-gnu
windows/build-winusb-helper.sh   # -> windows/out/fpvos-winusb.exe, embedded next
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  LIBUSB_LIB_DIR=/path/to/libusb/MinGW64/static \
  cargo build --release --target x86_64-pc-windows-gnu
```

## Dry run

Pass `--dry-run` (window or `--cli`) to enter the bootloader, verify it, and
load + on-device-checksum every recovery image without booting. Power-cycle
afterwards to restore stock.

## Design previews

`--demo STATE` opens the window on a made-up screen with no USB activity
(`idle`, `found`, `bootloader`, `cancelled`, `auth`, `loading`, `driver`,
`done`, `dry`, `failed`), and `--screenshot FILE.png` saves the window once
drawn and quits. Together they check the design of every screen without
hardware.

## Scope and limits

- **macOS and Windows.** Linux/ChromeOS already have the browser flasher, so the
  app refuses to run there and points at it.
- **Bootloader layouts are pinned:** the SteadyDigital and a P1 on stock 2.x
  have the `v2` loader, a P1 on stock 1.0.44 the `v1` one, as in the web
  flasher. A P1's is found by probing, v1 first (a v1 probe on a v2 loader is
  harmless; a v1 loader that answers is never sent the v2 one). Each must then
  match its pinned fingerprints. A unit whose bootloader does not match is
  refused rather than guessed at.
- The embedded recovery bundles are the flasher's browser-v1 builds at the
  time of the last sync. Sync and rebuild the app to ship newer ones; verify
  their target and provenance before distributing.
- Verified on hardware: the P1 Air on stock 2.1.2 and the SteadyDigital, from
  macOS and Windows; the P1 Air on stock 1.0.44 from Windows (0.4.2) and macOS
  (0.4.5), its loader detected (2026-11-23).
- **macOS sends the loader one USB packet at a time.** There, the stock 1.0.44
  loader's multi-packet transfers (its 960-byte data frames, its one-line
  `crc32` and boot commands) came out wrong, so on macOS v1 is driven as v2 is:
  15-byte frames and commands of at most 15 characters. Windows and Linux keep
  v1's large frames and long commands, as the web flasher sends them.

## License

GPLv3, as fpvOS and the fpvOS flasher ([LICENSE](LICENSE)). Bundled with it:

- **The recovery images** in `assets/` (Linux kernel, device tree, initramfs)
  are fpvOS builds, under the GPL. Each `manifest.json` names their source: the
  [fpvOS](https://github.com/gehee/fpvOS) commit (`source_commit`) and the
  Buildroot configuration (`build_config`) they were built from.
- **libusb** (LGPL-2.1), linked into the app: vendored as a library in the
  macOS app, built in on Windows. Its license ships with each download.
- **libwdi**'s `wdi-simple` (LGPL-3.0), the Windows driver helper, built from
  source by `windows/build-winusb-helper.sh`; its license ships with the zip.
- **Chakra Petch** (`fonts/`), under the SIL Open Font License
  ([fonts/OFL.txt](fonts/OFL.txt)).

## Why not SSH over USB?

Stock's design brings up a USB network (`usb0` at 192.168.3.99) with root
dropbear, but on the hardware tested the USB interface never received an IPv4
address on any boot, so that path was unreachable. The 10.0.0.1 address that
answers ARP is the radio link, not a host-reachable service. The HID loader is
the reliable recovery entry point on this unit.
