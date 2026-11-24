// Link libusb-1.0. Override its location with LIBUSB_LIB_DIR.
//   macOS:   brew install libusb            (links -lusb-1.0 from Homebrew;
//            the app bundle vendors the dylib)
//   Windows: statically, so the exe needs no libusb-1.0.dll beside it:
//            vcpkg install libusb:x64-windows-static-md and set LIBUSB_LIB_DIR
//            to its lib dir (libusb-1.0.lib), or, cross-building with MinGW,
//            libusb's release MinGW64/static dir (libusb-1.0.a).
// On Windows the WinUSB driver helper is embedded too (see winusb_helper), so
// the download is one self-contained exe that even runs from inside the zip.
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    println!("cargo:rerun-if-env-changed=LIBUSB_LIB_DIR");
    if let Ok(dir) = std::env::var("LIBUSB_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    }
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "gnu" {
        println!("cargo:rustc-link-lib=static=usb-1.0");
    } else if target_os == "windows" {
        println!("cargo:rustc-link-lib=static=libusb-1.0");
    } else {
        println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
        println!("cargo:rustc-link-search=native=/usr/local/lib");
        println!("cargo:rustc-link-lib=usb-1.0");
    }
    if target_os == "windows" {
        windows_icon(&target_env);
        winusb_helper();
    }
}

// The WinUSB driver helper (libwdi, built by windows/build-winusb-helper.sh),
// embedded into the exe and written out when the driver is needed. Taken from
// FPVOS_WINUSB_HELPER, else windows/out/fpvos-winusb.exe. Without one the
// build still works: the app then looks for the helper beside itself, and
// failing that shows the manual Zadig steps.
fn winusb_helper() {
    println!("cargo:rerun-if-env-changed=FPVOS_WINUSB_HELPER");
    let src = std::env::var("FPVOS_WINUSB_HELPER").unwrap_or_else(|_| "windows/out/fpvos-winusb.exe".into());
    println!("cargo:rerun-if-changed={src}");
    let dest = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("fpvos-winusb.exe");
    if std::fs::copy(&src, &dest).is_err() {
        println!("cargo:warning=no WinUSB helper at {src}: the driver install falls back to Zadig");
        std::fs::write(&dest, b"").unwrap();
    }
}

// The exe's icon (Explorer, the taskbar): packaging/fpvos.ico as a Win32
// resource. embed-resource finds the SDK's rc.exe for MSVC, and windres for
// MinGW (x86_64-w64-mingw32-windres when cross-built). A release without its
// icon is a build failure, not a silent fallback.
fn windows_icon(_target_env: &str) {
    println!("cargo:rerun-if-changed=packaging/fpvos.ico");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = std::fs::canonicalize("packaging/fpvos.ico").expect("packaging/fpvos.ico");
    let ico = ico.display().to_string().replace('\\', "/");
    let rc = out.join("icon.rc");
    // Who made it and what it is, as Explorer's Details tab and antivirus
    // engines read them: an exe without any looks anonymous.
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<u32> = ver.split(['.', '-']).filter_map(|p| p.parse().ok()).chain([0, 0, 0, 0]).take(4).collect();
    let (a, b, c, d) = (nums[0], nums[1], nums[2], nums[3]);
    std::fs::write(
        &rc,
        format!(
            r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "fpvOS"
      VALUE "FileDescription", "fpvOS Recovery"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "fpvos-recovery"
      VALUE "LegalCopyright", "fpvOS contributors"
      VALUE "OriginalFilename", "fpvOS Recovery.exe"
      VALUE "ProductName", "fpvOS Recovery"
      VALUE "ProductVersion", "{ver}"
      VALUE "Comments", "Puts an fpvOS-compatible FPV unit into fpvOS recovery mode over USB. https://github.com/gehee/fpvOS_recovery"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
        ),
    )
    .unwrap();
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_required()
        .expect("compiling the Windows icon resource");
}
