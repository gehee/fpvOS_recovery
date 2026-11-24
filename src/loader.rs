//! Native driver for the Artosyn "USB download gadget" U-Boot HID loader
//! (1d6b:0100) on macOS, over libusb. macOS WebHID cannot reach this gadget
//! (it routes the undeclared output reports to EP0, which the gadget never
//! reads), but a root process that detaches the kernel HID driver and claims
//! the interface can drive the interrupt endpoints directly.
//!
//! The protocol and the RAM/bootloader layouts are the fpvOS_flasher
//! `P1HidRecovery` ones: "v1" (P1 stock 1.0.44) and "v2" (P1 stock 2.x, the
//! SteadyDigital). A P1's is found by probing, v1 first, as the flasher does.
//! Only RAM reads/writes inside approved windows and U-Boot commands are
//! issued; no NAND/flash command is ever sent.

use std::ffi::c_void;
use std::os::raw::{c_int, c_uchar, c_uint};
use std::thread::sleep;
use std::time::{Duration, Instant};

// ---- libusb FFI (system libusb-1.0; no crate dependency) --------------------

#[repr(C)]
struct Transfer {
    dev_handle: *mut c_void,
    flags: u8,
    endpoint: c_uchar,
    transfer_type: c_uchar,
    timeout: c_uint,
    status: c_int,
    length: c_int,
    actual_length: c_int,
    callback: Option<extern "C" fn(*mut Transfer)>,
    user_data: *mut c_void,
    buffer: *mut c_uchar,
    num_iso_packets: c_int,
}

const LIBUSB_TRANSFER_TYPE_INTERRUPT: c_uchar = 3;
const LIBUSB_TRANSFER_COMPLETED: c_int = 0;

extern "C" {
    fn libusb_init(ctx: *mut *mut c_void) -> c_int;
    fn libusb_exit(ctx: *mut c_void);
    fn libusb_open_device_with_vid_pid(ctx: *mut c_void, vid: u16, pid: u16) -> *mut c_void;
    fn libusb_close(h: *mut c_void);
    fn libusb_get_device(h: *mut c_void) -> *mut c_void;
    fn libusb_get_device_speed(dev: *mut c_void) -> c_int;
    fn libusb_detach_kernel_driver(h: *mut c_void, iface: c_int) -> c_int;
    fn libusb_claim_interface(h: *mut c_void, iface: c_int) -> c_int;
    fn libusb_release_interface(h: *mut c_void, iface: c_int) -> c_int;
    fn libusb_interrupt_transfer(
        h: *mut c_void,
        endpoint: c_uchar,
        data: *mut c_uchar,
        length: c_int,
        transferred: *mut c_int,
        timeout: c_uint,
    ) -> c_int;
    fn libusb_alloc_transfer(iso_packets: c_int) -> *mut Transfer;
    fn libusb_free_transfer(t: *mut Transfer);
    fn libusb_submit_transfer(t: *mut Transfer) -> c_int;
    fn libusb_handle_events(ctx: *mut c_void) -> c_int;
    fn libusb_get_device_list(ctx: *mut c_void, list: *mut *mut *mut c_void) -> isize;
    fn libusb_free_device_list(list: *mut *mut c_void, unref_devices: c_int);
    fn libusb_get_device_descriptor(dev: *mut c_void, desc: *mut DeviceDescriptor) -> c_int;
}

#[repr(C)]
#[derive(Default)]
struct DeviceDescriptor {
    b_length: u8,
    b_descriptor_type: u8,
    bcd_usb: u16,
    b_device_class: u8,
    b_device_sub_class: u8,
    b_device_protocol: u8,
    b_max_packet_size0: u8,
    id_vendor: u16,
    id_product: u16,
    bcd_device: u16,
    i_manufacturer: u8,
    i_product: u8,
    i_serial_number: u8,
    b_num_configurations: u8,
}

/// Non-intrusively enumerate USB (no open/claim) and report which compatible
/// devices are connected, as (P1 Air stock 1d6b:0101, SteadyDigital stock
/// 1d6b:0210, bootloader 1d6b:0100). Used by the UI to show what is plugged in.
pub fn present() -> (bool, bool, bool) {
    let mut ctx: *mut c_void = std::ptr::null_mut();
    if unsafe { libusb_init(&mut ctx) } != 0 {
        return (false, false, false);
    }
    let mut list: *mut *mut c_void = std::ptr::null_mut();
    let n = unsafe { libusb_get_device_list(ctx, &mut list) };
    let (mut p1, mut sd, mut boot) = (false, false, false);
    if n > 0 && !list.is_null() {
        for i in 0..n {
            let dev = unsafe { *list.offset(i) };
            if dev.is_null() {
                continue;
            }
            let mut d = DeviceDescriptor::default();
            if unsafe { libusb_get_device_descriptor(dev, &mut d) } == 0 && d.id_vendor == VID {
                match d.id_product {
                    PID => boot = true,
                    0x0210 => sd = true,
                    0x0101 => p1 = true,
                    _ => {}
                }
            }
        }
        unsafe { libusb_free_device_list(list, 1) };
    }
    unsafe { libusb_exit(ctx) };
    (p1, sd, boot)
}

// ---- Protocol / layout constants (fpvOS_flasher v2, this unit) --------------

const VID: u16 = 0x1d6b;
const PID: u16 = 0x0100;
const EP_OUT: c_uchar = 0x02;
const EP_IN: c_uchar = 0x81;

const MAGIC: [u8; 8] = [4, 85, 255, 170, 65, 82, 84, 79];

const SCRATCH: u64 = 0x24bf_f000;

/// The v1 boot line's settings (fdt_high, initrd_high, bootm_boot_mode) in
/// commands of at most 15 characters, for one-packet mode; names too long for
/// that are assembled and `run`. Leaves a/r/f/n/s, which load and boot use.
const V1_SHORT_ENV: [&str; 15] = [
    "set H ffffffff", "set H $H$H", "set fdt_high $H",
    "set A setenv", "set i initrd_hi", "set c $A $i", "set c ${c}gh $H", "run c",
    "set B bootm_", "set C boot_", "set D mode", "set E nosec",
    "set c $A $B$C", "set c $c$D $E", "run c",
];

// RAM load destinations.
pub const KERNEL_ADDR: u64 = 0x2440_0000;
pub const INITRD_ADDR: u64 = 0x2600_0000;
pub const DTB_ADDR: u64 = 0x2208_0000;

/// An approved bootloader fingerprint: a 64-byte region and its SHA-256.
pub struct Fingerprint {
    pub address: u64,
    pub sha256: &'static str,
}

/// One bootloader's RAM layout and how it is driven (fpvOS_flasher
/// transports/p1-hid.js `layouts`, fingerprints from its recovery manifest).
pub struct Layout {
    /// What it is, for the log: the stock it comes with.
    pub family: &'static str,
    magic: u64,
    table: u64,
    callback: u64,
    command: u64,
    /// Dispatch-table callbacks (the 0 entries must read back as 0).
    callbacks: [u64; 7],
    fingerprints: [Fingerprint; 4],
    /// Data bytes per write frame: v1 takes 960; v2 commits one 64-byte packet
    /// per frame (48 header + 15 data) and larger frames corrupt its RAM.
    write_chunk: usize,
    /// v2 takes U-Boot commands of at most 15 characters; v1 takes long ones.
    pub short_commands: bool,
}

/// P1 stock 1.0.44.
pub const V1: Layout = Layout {
    family: "stock 1.0.44",
    magic: 0x2ffb_f378,
    table: 0x2ffb_f290,
    callback: 0x2ffb_f330,
    command: 0x2ff6_5e98,
    callbacks: [0x2ff8_6448, 0, 0, 0, 0x2ff8_6788, 0x2ff8_6950, 0],
    fingerprints: [
        Fingerprint { address: 0x2ff8_6788, sha256: "8d64ac5d0fbbab044f2669df89f474bd4264515027e58d387e256e1ea9a5e700" },
        Fingerprint { address: 0x2ff8_6950, sha256: "873222ba22f63c746288b66083ee88421c941cfdd2dc335532c42b1c2077a788" },
        Fingerprint { address: 0x2ff6_5e98, sha256: "d5e7caa635ba5af4b785d0e665f8ddeccadd6c2f7e3afaa483725954a328992e" },
        Fingerprint { address: 0x2ff8_5b50, sha256: "fa1863401a64e73cb609ac67db1e64163a8998c6d83c7672557afcec67709910" },
    ],
    write_chunk: 960,
    short_commands: false,
};

/// P1 stock 2.x and the SteadyDigital.
pub const V2: Layout = Layout {
    family: "stock 2.x",
    magic: 0x2feb_ebd0,
    table: 0x2feb_eae8,
    callback: 0x2feb_eb88,
    command: 0x2fe6_51d8,
    callbacks: [0x2fe8_5898, 0, 0, 0, 0x2fe8_5bd8, 0x2fe8_5da0, 0],
    fingerprints: [
        Fingerprint { address: 0x2fe8_5bd8, sha256: "3f059998c3d1b2f6088ef74287b05398519601401bacc14515d73c91c856eaee" },
        Fingerprint { address: 0x2fe8_5da0, sha256: "fb5c0b1747f7e78003f92bba2591dd13b1437cf81a4d2c0b68f52d7fbd394065" },
        Fingerprint { address: 0x2fe6_51d8, sha256: "d5e7caa635ba5af4b785d0e665f8ddeccadd6c2f7e3afaa483725954a328992e" },
        Fingerprint { address: 0x2fe8_4fa0, sha256: "fa1863401a64e73cb609ac67db1e64163a8998c6d83c7672557afcec67709910" },
    ],
    write_chunk: 15,
    short_commands: true,
};

const WRITE_DEPTH: i32 = 8; // async frames kept in flight

// ---- Loader session ---------------------------------------------------------

/// Outcome of a single open attempt.
pub enum Opened {
    /// Claimed; carries the negotiated USB speed name.
    Yes(&'static str),
    /// Enumerated but the interface could not be claimed (macOS: not root;
    /// Windows: not bound to WinUSB).
    Unclaimable,
    /// Not visible to libusb at all.
    Absent,
}

pub struct Loader {
    ctx: *mut c_void,
    handle: *mut c_void,
    speed: &'static str,
    verified: bool,
    read_chunk: usize,
    /// Per-board override of the layout's write size (the SteadyDigital's).
    write_chunk: Option<usize>,

    /// The candidates, probed in order; `layout` is the one found.
    candidates: &'static [&'static Layout],
    layout: &'static Layout,
}

impl Loader {
    /// Initialise libusb. Does not open the device yet.
    pub fn new() -> Result<Self, String> {
        let mut ctx: *mut c_void = std::ptr::null_mut();
        if unsafe { libusb_init(&mut ctx) } != 0 {
            return Err("Could not initialise libusb.".into());
        }
        Ok(Loader {
            ctx,
            handle: std::ptr::null_mut(),
            speed: "unknown",
            verified: false,
            read_chunk: 8,
            write_chunk: None,
            candidates: &[&V2],
            layout: &V2,
        })
    }

    /// Set the per-board read size and, where the board fixes it, write size
    /// (None: the bootloader's own, v1 960 or v2 15).
    pub fn set_chunks(&mut self, read_chunk: usize, write_chunk: Option<usize>) {
        self.read_chunk = read_chunk;
        self.write_chunk = write_chunk;
    }

    /// The bootloaders this board may have, probed in this order. A P1 has
    /// v1 (1.0.44) or v2 (2.x): v1 first, v2 only once v1 never answered - a v1
    /// probe on a v2 loader is harmless (a 2.1.2 unit took four), a v2 probe on
    /// a v1 loader has never been tried, and this order never sends one to a
    /// v1 loader that answers.
    pub fn set_candidates(&mut self, candidates: &'static [&'static Layout]) {
        self.candidates = candidates;
        self.layout = candidates[0];
    }

    /// The bootloader found by `verify`.
    pub fn layout(&self) -> &'static Layout {
        self.layout
    }

    /// Every transfer to the loader one 64-byte USB packet: frames of 15 data
    /// bytes, commands of 15 characters. The v2 loader needs it everywhere.
    /// macOS needs it for v1 too: there the stock 1.0.44 loader's multi-packet
    /// frames (960-byte data, the one-line crc32) came out wrong - every image
    /// failed its checksum (2026-11-23) - while Windows and Linux take them.
    fn one_packet(&self) -> bool {
        self.layout.short_commands || cfg!(target_os = "macos")
    }

    fn chunk(&self) -> usize {
        if self.one_packet() {
            return self.write_chunk.unwrap_or(V2.write_chunk);
        }
        self.write_chunk.unwrap_or(self.layout.write_chunk)
    }

    /// One attempt to open and claim the loader's interface. Platform code
    /// drives its own wait loop around this so it can give OS-specific guidance.
    pub fn try_open(&mut self) -> Opened {
        if self.opened() {
            return Opened::Yes(self.speed);
        }
        let h = unsafe { libusb_open_device_with_vid_pid(self.ctx, VID, PID) };
        if h.is_null() {
            return Opened::Absent;
        }
        // The macOS HID driver owns the interface until detached. On Windows the
        // device must already be bound to WinUSB (libusb cannot rebind it), so
        // there is nothing to detach.
        #[cfg(not(target_os = "windows"))]
        unsafe {
            libusb_detach_kernel_driver(h, 0);
        }
        if unsafe { libusb_claim_interface(h, 0) } == 0 {
            self.handle = h;
            let dev = unsafe { libusb_get_device(h) };
            self.speed = match unsafe { libusb_get_device_speed(dev) } {
                3 => "high",
                2 => "full",
                1 => "low",
                _ => "unknown",
            };
            Opened::Yes(self.speed)
        } else {
            unsafe { libusb_close(h) };
            Opened::Unclaimable
        }
    }

    /// Poll `try_open` until it claims the loader or `wait` elapses.
    pub fn open_wait(&mut self, wait: Duration) -> Result<&'static str, String> {
        let deadline = Instant::now() + wait;
        loop {
            match self.try_open() {
                Opened::Yes(s) => return Ok(s),
                Opened::Unclaimable if Instant::now() >= deadline => {
                    return Err("Found the USB download gadget but could not claim its interface.".into())
                }
                Opened::Absent if Instant::now() >= deadline => {
                    return Err("The USB download gadget (1d6b:0100) did not appear. Power-cycle the unit and retry.".into())
                }
                _ => sleep(Duration::from_millis(300)),
            }
        }
    }

    fn opened(&self) -> bool {
        !self.handle.is_null()
    }

    // --- framing ---

    fn frame(kind: u16, body: &[u8], first: u8) -> Vec<u8> {
        let mut p = vec![0u8; 48 + body.len()];
        p[..8].copy_from_slice(&MAGIC);
        p[0] = first;
        p[16..20].copy_from_slice(&((48 + body.len()) as u32).to_le_bytes());
        p[20..24].copy_from_slice(&1u32.to_le_bytes());
        p[28] = 1;
        p[36..38].copy_from_slice(&kind.to_le_bytes());
        p[38..40].copy_from_slice(&12u16.to_le_bytes());
        p[40..44].copy_from_slice(&(body.len() as u32).to_le_bytes());
        p[48..].copy_from_slice(body);
        p
    }

    fn setup(tag: &[u8; 4], count: u32, addr: u64) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(tag);
        b[4..8].copy_from_slice(&count.to_le_bytes());
        b[8..16].copy_from_slice(&addr.to_le_bytes());
        b
    }

    fn out(&self, kind: u16, body: &[u8], first: u8) -> Result<(), String> {
        let mut p = Self::frame(kind, body, first);
        let mut transferred: c_int = 0;
        let r = unsafe {
            libusb_interrupt_transfer(self.handle, EP_OUT, p.as_mut_ptr(), p.len() as c_int, &mut transferred, 3000)
        };
        if r != 0 || transferred as usize != p.len() {
            return Err(format!("USB transfer failed (code {r}). Power-cycle the unit and retry."));
        }
        Ok(())
    }

    fn read(&self, addr: u64, size: usize) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; size];
        let mut off = 0usize;
        while off < size {
            let count = std::cmp::min(self.read_chunk, size - off);
            self.out(5, &Self::setup(b"ufgt", count as u32, addr + off as u64), 4)?;
            // Ask for exactly the reply's length. A transfer only ends on a short
            // packet or a full buffer: the P1's 16-byte reads answer 48 + 16 = 64
            // bytes, one full-speed packet exactly, so a larger buffer would wait
            // for a packet that never comes (libusb -7). The SteadyDigital's 8-byte
            // reads (56 bytes) are short packets and end either way.
            let want = 48 + count;
            let mut buf = [0u8; 128];
            let mut got: c_int = 0;
            let r = unsafe {
                libusb_interrupt_transfer(self.handle, EP_IN, buf.as_mut_ptr(), want as c_int, &mut got, 1500)
            };
            if r != 0 {
                return Err(format!("No reply from the bootloader (code {r})."));
            }
            if got as usize != 48 + count
                || buf[1..8] != MAGIC[1..8]
                || u16::from_le_bytes([buf[36], buf[37]]) != 2
                || u32::from_le_bytes([buf[40], buf[41], buf[42], buf[43]]) as usize != count
            {
                return Err("Malformed reply from the bootloader.".into());
            }
            out[off..off + count].copy_from_slice(&buf[48..48 + count]);
            off += count;
        }
        Ok(out)
    }

    fn read_u64(&self, addr: u64) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.read(addr, 8)?[..8].try_into().unwrap()))
    }

    /// Synchronous write of an approved RAM window.
    fn write_sync(&self, addr: u64, data: &[u8]) -> Result<(), String> {
        self.write_sync_progress(addr, data, |_, _| {})
    }

    /// Synchronous write, one frame at a time: each waits for the last, as the
    /// web flasher writes (it awaits every report). `progress(done, total)`.
    fn write_sync_progress(&self, addr: u64, data: &[u8], mut progress: impl FnMut(usize, usize)) -> Result<(), String> {
        self.out(4, &Self::setup(b"uftr", data.len() as u32, addr), 4)?;
        let mut off = 0usize;
        while off < data.len() {
            let mut c = std::cmp::min(self.chunk(), data.len() - off);
            if c == 16 {
                c = 8; // a 16-byte body is read as a transfer setup, not data
            }
            self.out(4, &data[off..off + c], 4)?;
            off += c;
            if off % 65536 < c || off == data.len() {
                progress(off, data.len());
            }
        }
        Ok(())
    }

    /// Pipelined write: several interrupt-OUT frames in flight at once, which
    /// reaches the full-speed 1 ms frame ceiling (~14.6 KiB/s) instead of the
    /// ~8 KiB/s of one-at-a-time transfers. `progress(done, total)`.
    fn write_pipelined(
        &self,
        addr: u64,
        data: &[u8],
        mut progress: impl FnMut(usize, usize),
    ) -> Result<(), String> {
        self.out(4, &Self::setup(b"uftr", data.len() as u32, addr), 4)?;
        let mut state = Counters { inflight: 0, failed: 0 };
        let st: *mut Counters = &mut state;
        let total = data.len();
        let mut off = 0usize;
        let mut last_report = 0usize;
        while off < total || state.inflight > 0 {
            while off < total && state.inflight < WRITE_DEPTH && state.failed == 0 {
                let mut c = std::cmp::min(self.chunk(), total - off);
                if c == 16 {
                    c = 8; // a 16-byte body is read as a transfer setup, not data
                }
                let boxed = Self::frame(4, &data[off..off + c], 4).into_boxed_slice();
                let len = boxed.len() as c_int;
                let ptr = Box::into_raw(boxed) as *mut u8;
                let t = unsafe { libusb_alloc_transfer(0) };
                if t.is_null() {
                    unsafe { drop(Vec::from_raw_parts(ptr, len as usize, len as usize)) };
                    return Err("libusb_alloc_transfer failed.".into());
                }
                unsafe {
                    (*t).dev_handle = self.handle;
                    (*t).endpoint = EP_OUT;
                    (*t).transfer_type = LIBUSB_TRANSFER_TYPE_INTERRUPT;
                    (*t).timeout = 5000;
                    (*t).buffer = ptr;
                    (*t).length = len;
                    (*t).callback = Some(write_cb);
                    (*t).user_data = st as *mut c_void;
                    if libusb_submit_transfer(t) != 0 {
                        drop(Vec::from_raw_parts(ptr, len as usize, len as usize));
                        libusb_free_transfer(t);
                        state.failed += 1;
                        break;
                    }
                }
                state.inflight += 1;
                off += c;
            }
            if state.failed > 0 && state.inflight == 0 {
                return Err("USB write transfer failed. Power-cycle the unit and retry.".into());
            }
            unsafe { libusb_handle_events(self.ctx) };
            if off - last_report >= 65536 || off == total {
                progress(off.min(total), total);
                last_report = off;
            }
        }
        if state.failed > 0 {
            return Err("USB write transfer failed. Power-cycle the unit and retry.".into());
        }
        Ok(())
    }

    fn command(&self, text: &str) -> Result<(), String> {
        if self.one_packet() && text.len() > 15 {
            return Err(format!("internal: command too long ({text})"));
        }
        self.out(6, text.as_bytes(), 4)?;
        sleep(Duration::from_millis(200));
        Ok(())
    }

    // --- probe + verify ---

    /// Whether the loader answers at `l`'s magic (a read: changes nothing).
    fn answers(&self, l: &Layout) -> bool {
        matches!(self.read(l.magic, 8), Ok(m) if m == MAGIC)
    }

    /// Find the bootloader: first read-only (one adapted by an earlier try
    /// answers already), then each candidate's probe in turn. Linux hidraw
    /// passes report 4/255 straight through; macOS and Windows need the
    /// one-byte report-255 probe before the gadget answers on report 4.
    fn adapt(&mut self) -> Result<(), String> {
        for l in self.candidates {
            if self.answers(l) {
                self.layout = l;
                return Ok(());
            }
        }
        for l in self.candidates {
            for _ in 0..4 {
                self.out(4, &Self::setup(b"uftr", 1, l.magic), 255)?;
                self.out(4, &[4u8], 255)?;
                sleep(Duration::from_millis(200));
                if self.answers(l) {
                    self.layout = l;
                    return Ok(());
                }
            }
        }
        let tried: Vec<&str> = self.candidates.iter().map(|l| l.family).collect();
        Err(format!(
            "The bootloader did not answer (tried the {} bootloader{}). Power-cycle the unit (cut external power too) and retry.",
            tried.join(" and "),
            if tried.len() > 1 { "s" } else { "" }
        ))
    }

    /// Verify the four fingerprints and the dispatch table, then enable the RAM
    /// command callback. Nothing is written to RAM if any check fails.
    pub fn verify(&mut self, sha256: impl Fn(&[u8]) -> String) -> Result<(), String> {
        self.adapt()?;
        let l = self.layout;
        for f in l.fingerprints.iter() {
            if sha256(&self.read(f.address, 64)?) != f.sha256 {
                return Err(format!("Unsupported bootloader: it answers like the {} one, but its code does not match; no firmware was written. Power-cycle the unit.", l.family));
            }
        }
        let table = self.read(l.table, 168)?;
        for i in 0..7 {
            let kind = u64::from_le_bytes(table[i * 24..i * 24 + 8].try_into().unwrap());
            let zero = u64::from_le_bytes(table[i * 24 + 8..i * 24 + 16].try_into().unwrap());
            let ptr = u64::from_le_bytes(table[i * 24 + 16..i * 24 + 24].try_into().unwrap());
            let expect_kind = if i >= 4 { i as u64 } else { 0 };
            let ok_ptr = ptr == l.callbacks[i] || (i == 6 && ptr == l.command);
            if kind != expect_kind || zero != 0 || !ok_ptr {
                return Err("Unrecognized bootloader dispatch table; no firmware was written.".into());
            }
        }
        self.write_sync(l.callback, &l.command.to_le_bytes())?;
        if self.read_u64(l.callback)? != l.command {
            return Err("RAM callback verification failed; no firmware was written.".into());
        }
        self.verified = true;
        Ok(())
    }

    /// Write a RAM image and verify it on-device with U-Boot's `crc32`.
    pub fn load_image(
        &self,
        addr: u64,
        data: &[u8],
        crc32_host: u32,
        progress: impl FnMut(usize, usize),
    ) -> Result<(), String> {
        if !self.verified {
            return Err("internal: load before verify".into());
        }
        if data.len() <= 4096 {
            self.write_sync_progress(addr, data, progress)?;
        } else {
            self.write_pipelined(addr, data, progress)?;
        }
        // crc32 <addr> <len> <store>; in one-packet mode through short vars so
        // each command stays within 15 characters. U-Boot stores it big-endian.
        self.write_sync(SCRATCH, &[0u8; 4])?;
        if self.one_packet() {
            self.command(&format!("set a {addr:x}"))?;
            self.command(&format!("set n {:x}", data.len()))?;
            self.command(&format!("set s {SCRATCH:x}"))?;
            self.command("crc32 $a $n $s")?;
        } else {
            self.command(&format!("crc32 {addr:x} {:x} {SCRATCH:x}", data.len()))?;
        }
        let got = self.read(SCRATCH, 4)?;
        if u32::from_be_bytes([got[0], got[1], got[2], got[3]]) != crc32_host {
            return Err("On-device checksum mismatch; recovery will not be booted.".into());
        }
        Ok(())
    }

    /// Run a fixed list of short U-Boot commands (each within the 15-character
    /// frame limit), after verification only.
    pub fn commands(&self, list: &[&str]) -> Result<(), String> {
        if !self.verified {
            return Err("internal: commands before verify".into());
        }
        for c in list {
            self.command(c)?;
        }
        Ok(())
    }

    /// Boot the loaded recovery: `bootm <kernel> <initrd> <dtb>`. After this the
    /// unit boots into fpvOS recovery and drops off USB, so there is no reply.
    pub fn boot(&self) -> Result<(), String> {
        if !self.verified {
            return Err("internal: boot before verify".into());
        }
        if !self.one_packet() {
            // v1, as the web flasher boots it from stock 1.0.44 (proven there):
            // U-Boot's own bootargs, the images kept where they were loaded.
            self.out(6, format!("setenv bootm_boot_mode nosec; setenv fdt_high ffffffffffffffff; setenv initrd_high ffffffffffffffff; bootm {KERNEL_ADDR:x} {INITRD_ADDR:x} {DTB_ADDR:x}").as_bytes(), 4)?;
            return Ok(());
        }
        if !self.layout.short_commands {
            // v1 in one-packet mode (macOS): that same line's settings, built
            // from short commands (v2 gets them with its kernel line, in engine).
            for c in V1_SHORT_ENV {
                self.command(c)?;
            }
        }
        self.command(&format!("set a {KERNEL_ADDR:x}"))?;
        self.command(&format!("set r {INITRD_ADDR:x}"))?;
        self.command(&format!("set f {DTB_ADDR:x}"))?;
        self.out(6, b"bootm $a $r $f", 4)?;
        Ok(())
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        unsafe {
            if self.opened() {
                libusb_release_interface(self.handle, 0);
                libusb_close(self.handle);
            }
            if !self.ctx.is_null() {
                libusb_exit(self.ctx);
            }
        }
    }
}

#[repr(C)]
struct Counters {
    inflight: i32,
    failed: i32,
}

extern "C" fn write_cb(t: *mut Transfer) {
    unsafe {
        let tr = &*t;
        let st = tr.user_data as *mut Counters;
        if tr.status != LIBUSB_TRANSFER_COMPLETED || tr.actual_length != tr.length {
            (*st).failed += 1;
        }
        (*st).inflight -= 1;
        let len = tr.length as usize;
        drop(Vec::from_raw_parts(tr.buffer, len, len));
        libusb_free_transfer(t);
    }
}
