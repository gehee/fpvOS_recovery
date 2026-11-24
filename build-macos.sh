#!/bin/sh
set -eu
cd "$(dirname "$0")"

# This tool links the system libusb-1.0 (used only to claim the Artosyn USB
# download gadget, which macOS WebHID/IOKit will not let a normal app reach).
if [ ! -f /opt/homebrew/lib/libusb-1.0.dylib ] && [ ! -f /usr/local/lib/libusb-1.0.dylib ] \
    && [ ! -f "${LIBUSB_LIB_DIR:-/nonexistent}/libusb-1.0.dylib" ]; then
    echo "libusb-1.0 not found. Install it with:  brew install libusb" >&2
    echo "(or set LIBUSB_LIB_DIR to the directory holding libusb-1.0.dylib)" >&2
    exit 1
fi

cargo build --release
echo "Built target/release/fpvos-recovery"
echo "Run it with: sudo ./target/release/fpvos-recovery"
