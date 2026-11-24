#!/bin/bash
# Refresh the recovery bundles and fonts this app embeds from an fpvOS_flasher
# checkout (the web flasher serves the same files), and repin their SHA-256 in
# src/engine.rs. Usage: tools/sync-bundles.sh ../fpvOS_flasher
set -euo pipefail
FLASHER=${1:?usage: tools/sync-bundles.sh <fpvOS_flasher checkout>}
cd "$(dirname "$0")/.."
copy() { mkdir -p "$(dirname "$2")"; cp "$FLASHER/$1" "$2"; }
for f in kernel.img skyzone-steadydigital.dtb recovery-initrd.img manifest.json; do
  copy "assets/steadydigital-updater/browser-v1/$f" "assets/steadydigital-updater/browser-v1/$f"
done
for f in kernel.img betafpv-p1-air.dtb recovery-initrd.img manifest.json; do
  copy "assets/p1-updater/browser-v1/$f" "assets/p1-updater/browser-v1/$f"
done
for f in ChakraPetch-Medium.ttf ChakraPetch-SemiBold.ttf OFL.txt; do copy "fonts/$f" "fonts/$f"; done
pin() {
  local sum; sum=$(shasum -a 256 "$2" | cut -d' ' -f1)
  sed -i.bak "s/^const $1: &str = \"[0-9a-f]*\";/const $1: \&str = \"$sum\";/" src/engine.rs && rm -f src/engine.rs.bak
  echo "$1 $sum"
}
sd=assets/steadydigital-updater/browser-v1; p1=assets/p1-updater/browser-v1
pin SD_KERNEL_SHA $sd/kernel.img; pin SD_DTB_SHA $sd/skyzone-steadydigital.dtb; pin SD_INITRD_SHA $sd/recovery-initrd.img
pin P1_KERNEL_SHA $p1/kernel.img; pin P1_DTB_SHA $p1/betafpv-p1-air.dtb; pin P1_INITRD_SHA $p1/recovery-initrd.img
echo "Synced from $FLASHER ($(git -C "$FLASHER" rev-parse --short HEAD 2>/dev/null || echo '?'))."
