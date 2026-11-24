#!/bin/bash
# Build fpvos-winusb.exe: libwdi's wdi-simple, cross-compiled with MinGW-w64
# (x86_64-w64-mingw32-gcc on macOS or Linux) and patched to bind Windows'
# inbox WinUSB driver (Windows 8 and later) without the WDK coinstallers.
# fpvOS Recovery runs it, elevated, as:
#   fpvos-winusb.exe --vid 0x1D6B --pid 0x0100 --type 0 ...
# which binds WinUSB to the bootloader's hardware id only.
# libwdi is LGPL-3.0 (https://github.com/pbatard/libwdi); the patch is
# libwdi-inbox-winusb.patch beside this script.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TAG=v1.5.1
COMMIT=9b23b82a2dd1cbffc16d46c212f92c6bf8c0c602
SRC="$HERE/libwdi"
OUT="$HERE/out"
CC=${CC:-x86_64-w64-mingw32-gcc}
WINDRES=${WINDRES:-x86_64-w64-mingw32-windres}

rm -rf "$SRC" "$OUT"
git -c advice.detachedHead=false clone -q --depth 1 --branch "$TAG" https://github.com/pbatard/libwdi.git "$SRC"
[ "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT" ] || { echo "libwdi $TAG is not $COMMIT" >&2; exit 1; }
git -C "$SRC" apply --whitespace=nowarn "$HERE/libwdi-inbox-winusb.patch"
cat > "$SRC/config.h" <<'CFG'
/* fpvOS: MinGW x64 cross build, inbox WinUSB only */
#define _GNU_SOURCE
#define WDF_VER 1011
#define OPT_M64
#define INSTALLER_PATH_64 "."
#define ENABLE_LOGGING 1
#define INCLUDE_DEBUG_LOGGING 1
CFG

FL=(-std=gnu11 -I"$SRC" -I"$SRC/libwdi" -Os -DWINVER=0x0601 -D_WIN32_WINNT=0x0601 -D_WIN32_IE=0x0700
    -DCOBJMACROS -m64 -D_WIN64 -Wno-pointer-sign -Wno-incompatible-pointer-types -Wno-int-conversion
    -Wno-stringop-truncation)
mkdir -p "$OUT"
cd "$SRC/libwdi"
# The installer is embedded into the library, then extracted and run at install time.
$CC "${FL[@]}" installer.c -o installer_x64.exe -static -lsetupapi -lnewdev -lole32
${CC_FOR_BUILD:-cc} -I.. -include ../config.h embedder.c -o embedder
./embedder embedded.h
for f in logging tokenizer vid_data pki libwdi_dlg libwdi; do
  $CC "${FL[@]}" -c "$f.c" -o "$OUT/$f.o"
done
printf '1 24 "%s"\n' "$SRC/examples/elevation.manifest" > "$OUT/helper.rc"
# Publisher details (Explorer's Details tab; antivirus engines read them too).
cat >> "$OUT/helper.rc" <<'RC'
1 VERSIONINFO
FILEVERSION 1,5,1,0
PRODUCTVERSION 1,5,1,0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "fpvOS"
      VALUE "FileDescription", "fpvOS Recovery USB driver setup (libwdi)"
      VALUE "FileVersion", "1.5.1"
      VALUE "InternalName", "fpvos-winusb"
      VALUE "LegalCopyright", "libwdi: Pete Batard and contributors, LGPL-3.0"
      VALUE "OriginalFilename", "fpvos-winusb.exe"
      VALUE "ProductName", "fpvOS Recovery"
      VALUE "ProductVersion", "1.5.1"
      VALUE "Comments", "Binds Windows' WinUSB driver to the fpvOS recovery bootloader (USB 1D6B:0100) only. https://github.com/gehee/fpvOS_recovery"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
RC
$WINDRES "$OUT/helper.rc" -O coff -o "$OUT/helper_rc.o"
$CC "${FL[@]}" "$SRC/examples/wdi-simple.c" "$OUT"/*.o -o "$OUT/fpvos-winusb.exe" -static -s \
  -lsetupapi -lole32 -lntdll -lnewdev -lshell32 -luuid -lcomctl32 -lgdi32 -lversion -lcrypt32
rm -f "$OUT"/*.o "$OUT/helper.rc"
echo "Built $OUT/fpvos-winusb.exe"
