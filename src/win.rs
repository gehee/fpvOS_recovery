//! Windows, natively: the present USB devices and their drivers (SetupAPI),
//! the stock unit's COM port and the reboot packet written to it, running the
//! driver helper elevated and opening a link (the shell). No PowerShell: a
//! hidden `powershell -ExecutionPolicy Bypass` every few seconds is exactly
//! what antivirus heuristics look for, and it was slow besides.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

type HANDLE = *mut c_void;
type HDEVINFO = *mut c_void;
type HKEY = *mut c_void;
type BOOL = i32;

const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
const DIGCF_PRESENT: u32 = 0x2;
const DIGCF_ALLCLASSES: u32 = 0x4;
const SPDRP_SERVICE: u32 = 0x4;
const DICS_FLAG_GLOBAL: u32 = 0x1;
const DIREG_DEV: u32 = 0x1;
const KEY_READ: u32 = 0x20019;
const REG_SZ: u32 = 1;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const SEE_MASK_NOCLOSEPROCESS: u32 = 0x40;
const SEE_MASK_NOASYNC: u32 = 0x100;
const SW_HIDE: i32 = 0;
const SW_SHOWNORMAL: i32 = 1;
const INFINITE: u32 = 0xFFFF_FFFF;
const ERROR_CANCELLED: u32 = 1223;

#[repr(C)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

// {4D36E978-E325-11CE-BFC1-08002BE10318}: serial and parallel ports.
const GUID_DEVCLASS_PORTS: GUID =
    GUID { data1: 0x4D36E978, data2: 0xE325, data3: 0x11CE, data4: [0xBF, 0xC1, 0x08, 0x00, 0x2B, 0xE1, 0x03, 0x18] };

#[repr(C)]
struct SP_DEVINFO_DATA {
    cbSize: u32,
    class_guid: GUID,
    dev_inst: u32,
    reserved: usize,
}

#[repr(C)]
struct SHELLEXECUTEINFOW {
    cbSize: u32,
    fMask: u32,
    hwnd: *mut c_void,
    lpVerb: *const u16,
    lpFile: *const u16,
    lpParameters: *const u16,
    lpDirectory: *const u16,
    nShow: i32,
    hInstApp: *mut c_void,
    lpIDList: *mut c_void,
    lpClass: *const u16,
    hkeyClass: HKEY,
    dwHotKey: u32,
    hIconOrMonitor: *mut c_void,
    hProcess: HANDLE,
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(class: *const GUID, enumerator: *const u16, parent: *mut c_void, flags: u32) -> HDEVINFO;
    fn SetupDiEnumDeviceInfo(set: HDEVINFO, index: u32, data: *mut SP_DEVINFO_DATA) -> BOOL;
    fn SetupDiGetDeviceInstanceIdW(set: HDEVINFO, data: *mut SP_DEVINFO_DATA, id: *mut u16, len: u32, need: *mut u32) -> BOOL;
    fn SetupDiGetDeviceRegistryPropertyW(
        set: HDEVINFO,
        data: *mut SP_DEVINFO_DATA,
        property: u32,
        kind: *mut u32,
        buf: *mut u8,
        len: u32,
        need: *mut u32,
    ) -> BOOL;
    fn SetupDiOpenDevRegKey(set: HDEVINFO, data: *mut SP_DEVINFO_DATA, scope: u32, profile: u32, kind: u32, access: u32) -> HKEY;
    fn SetupDiDestroyDeviceInfoList(set: HDEVINFO) -> BOOL;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegQueryValueExW(key: HKEY, name: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
    fn RegCloseKey(key: HKEY) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *mut c_void, disp: u32, flags: u32, tmpl: HANDLE) -> HANDLE;
    fn GetCommState(file: HANDLE, dcb: *mut u32) -> BOOL;
    fn BuildCommDCBW(def: *const u16, dcb: *mut u32) -> BOOL;
    fn SetCommState(file: HANDLE, dcb: *const u32) -> BOOL;
    fn WriteFile(file: HANDLE, buf: *const u8, len: u32, written: *mut u32, overlapped: *mut c_void) -> BOOL;
    fn FlushFileBuffers(file: HANDLE) -> BOOL;
    fn CloseHandle(h: HANDLE) -> BOOL;
    fn WaitForSingleObject(h: HANDLE, ms: u32) -> u32;
    fn GetExitCodeProcess(h: HANDLE, code: *mut u32) -> BOOL;
    fn GetLastError() -> u32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut SHELLEXECUTEINFOW) -> BOOL;
    fn ShellExecuteW(hwnd: *mut c_void, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> *mut c_void;
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// A present device: its instance id (upper case, e.g. `USB\VID_1D6B&PID_0100\…`)
/// and the service (driver) bound to it, empty for none.
pub struct Device {
    pub instance: String,
    pub service: String,
}

/// The present devices of one setup class, or (no class) everything the USB
/// enumerator reports.
fn devices(class: Option<&GUID>, mut each: impl FnMut(HDEVINFO, &mut SP_DEVINFO_DATA, String)) {
    let usb = wide("USB");
    let (guid, enumerator, flags) = match class {
        Some(g) => (g as *const GUID, null(), DIGCF_PRESENT),
        None => (null(), usb.as_ptr(), DIGCF_PRESENT | DIGCF_ALLCLASSES),
    };
    unsafe {
        let set = SetupDiGetClassDevsW(guid, enumerator, null_mut(), flags);
        if set == INVALID_HANDLE_VALUE {
            return;
        }
        let mut i = 0;
        loop {
            let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, i, &mut data) == 0 {
                break;
            }
            i += 1;
            let mut id = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(set, &mut data, id.as_mut_ptr(), id.len() as u32, null_mut()) != 0 {
                each(set, &mut data, from_wide(&id).to_uppercase());
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
}

/// Every present device on the USB bus, with its driver.
pub fn usb_devices() -> Vec<Device> {
    let mut out = Vec::new();
    devices(None, |set, data, instance| {
        let mut buf = [0u16; 256];
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                set,
                data,
                SPDRP_SERVICE,
                null_mut(),
                buf.as_mut_ptr() as *mut u8,
                (buf.len() * 2) as u32,
                null_mut(),
            )
        };
        let service = if ok != 0 { from_wide(&buf) } else { String::new() };
        out.push(Device { instance, service });
    });
    out
}

/// The COM port name (e.g. "COM5") of the first present port whose instance id
/// contains one of `ids` (e.g. "VID_1D6B&PID_0210").
pub fn com_port(ids: &[&str]) -> Option<String> {
    let mut found = None;
    devices(Some(&GUID_DEVCLASS_PORTS), |set, data, instance| {
        if found.is_some() || !ids.iter().any(|id| instance.contains(id)) {
            return;
        }
        unsafe {
            let key = SetupDiOpenDevRegKey(set, data, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ);
            if key == INVALID_HANDLE_VALUE {
                return;
            }
            let name = wide("PortName");
            let mut buf = [0u16; 64];
            let mut len = (buf.len() * 2) as u32;
            let mut kind = 0u32;
            if RegQueryValueExW(key, name.as_ptr(), null_mut(), &mut kind, buf.as_mut_ptr() as *mut u8, &mut len) == 0
                && kind == REG_SZ
            {
                found = Some(from_wide(&buf));
            }
            RegCloseKey(key);
        }
    });
    found
}

/// Write `bytes` to a COM port at 115200 8N1.
pub fn write_serial(port: &str, bytes: &[u8]) -> Result<(), String> {
    let path = wide(format!(r"\\.\{port}"));
    unsafe {
        let h = CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null_mut(), OPEN_EXISTING, 0, null_mut());
        if h == INVALID_HANDLE_VALUE {
            return Err(format!("Could not open {port} (error {}). Close any other program using it.", GetLastError()));
        }
        // DCB is 28 bytes; its first field is its own length.
        let mut dcb = [0u32; 7];
        dcb[0] = 28;
        let def = wide("baud=115200 parity=N data=8 stop=1");
        let configured = GetCommState(h, dcb.as_mut_ptr()) != 0
            && BuildCommDCBW(def.as_ptr(), dcb.as_mut_ptr()) != 0
            && SetCommState(h, dcb.as_ptr()) != 0;
        if !configured {
            let e = GetLastError();
            CloseHandle(h);
            return Err(format!("Could not configure {port} (error {e})."));
        }
        let mut written = 0u32;
        let ok = WriteFile(h, bytes.as_ptr(), bytes.len() as u32, &mut written, null_mut()) != 0;
        FlushFileBuffers(h);
        std::thread::sleep(std::time::Duration::from_millis(150));
        CloseHandle(h);
        if ok && written as usize == bytes.len() {
            Ok(())
        } else {
            Err(format!("Could not write to {port}."))
        }
    }
}

/// How an elevated run ended.
pub enum Elevated {
    Exited(u32),
    Declined,
    Failed(u32),
}

/// Run `exe` with `args` as administrator (the UAC prompt) and wait for it.
pub fn run_elevated(exe: &Path, args: &str) -> Elevated {
    let verb = wide("runas");
    let file = wide(exe.as_os_str());
    let params = wide(args);
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            let e = GetLastError();
            return if e == ERROR_CANCELLED { Elevated::Declined } else { Elevated::Failed(e) };
        }
        if info.hProcess.is_null() {
            return Elevated::Failed(0);
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        Elevated::Exited(code)
    }
}

/// Open a URL in the default browser.
pub fn open_url(url: &str) {
    let op = wide("open");
    let target = wide(url);
    unsafe {
        ShellExecuteW(null_mut(), op.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}
