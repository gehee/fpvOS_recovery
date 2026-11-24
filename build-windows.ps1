$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# Prerequisites:
#   - Rust (MSVC toolchain): https://rustup.rs
#   - libusb for Windows, as a static library (linked into the exe). Via vcpkg:
#       vcpkg install libusb:x64-windows-static
#   - windows\out\fpvos-winusb.exe (windows/build-winusb-helper.sh), embedded
#     into the exe; or set FPVOS_WINUSB_HELPER to the helper to embed.

if (-not $env:LIBUSB_LIB_DIR) {
    Write-Host "Set LIBUSB_LIB_DIR to the folder containing libusb-1.0.lib before building."
    Write-Host 'e.g.  $env:LIBUSB_LIB_DIR = "C:\vcpkg\installed\x64-windows-static\lib"'
    exit 1
}

cargo build --release

Write-Host "Built target\release\fpvos-recovery.exe"
Write-Host "Run it:  .\target\release\fpvos-recovery.exe"
