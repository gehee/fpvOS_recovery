#!/usr/bin/env python3
"""Build the fpvOS Recovery app icons from the fpvOS wordmark.

    python3 make-icons.py WORDMARK.png      (needs Pillow; macOS for the .icns)

WORDMARK.png is fpvOS's assets/fpvos-logo.png (white "fpv", lime "OS" on navy).
Writes, beside this script:
  icon.png       1024 px master, also embedded as the running window's icon
  AppIcon.icns   macOS bundle icon (Contents/Resources, CFBundleIconFile)
  fpvos.ico      Windows exe icon (embedded by build.rs)
The look is the app's KESTREL theme: navy ground, lime ring, wordmark.
"""
import os, shutil, subprocess, sys, tempfile
from PIL import Image, ImageChops, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
GROUND = (0x06, 0x20, 0x2f, 255)
ACCENT = (0xb3, 0xff, 0x00, 255)
SS = 4  # supersampling for smooth edges


def wordmark(path):
    """The letters alone, background keyed out (letters are white or lime;
    the ground and the drop shadow are dark navy)."""
    src = Image.open(path).convert('RGB')
    r, g, b = src.split()
    # Luminance-ish key: navy ground and shadow sit far below the letters.
    key = ImageChops.lighter(r, g).point(lambda v: 0 if v < 90 else 255 if v > 170 else (v - 90) * 255 // 80)
    out = src.convert('RGBA')
    out.putalpha(key)
    return out.crop(key.getbbox())


def master(mark, size=1024):
    S = size * SS
    img = Image.new('RGBA', (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # macOS app-icon grid: 824/1024 body, ~185 corner radius, centred.
    body = S * 824 // 1024
    off = (S - body) // 2
    rad = S * 185 // 1024
    # Soft drop shadow under the body.
    shadow = Image.new('RGBA', (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        [off, off + S // 64, off + body, off + body + S // 64], rad, fill=(0, 0, 0, 110))
    img = Image.alpha_composite(img, shadow.filter(ImageFilter.GaussianBlur(S // 80)))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([off, off, off + body, off + body], rad, fill=GROUND)
    inset = S * 34 // 1024
    ring = S * 14 // 1024
    d.rounded_rectangle([off + inset, off + inset, off + body - inset, off + body - inset],
                        rad - inset, outline=ACCENT, width=ring)
    w = body * 70 // 100
    h = mark.height * w // mark.width
    m = mark.resize((w, h), Image.LANCZOS)
    img.alpha_composite(m, ((S - w) // 2, (S - h) // 2))
    return img.resize((size, size), Image.LANCZOS)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    icon = master(wordmark(sys.argv[1]))
    icon.save(os.path.join(HERE, 'icon.png'))
    icon.save(os.path.join(HERE, 'fpvos.ico'),
              sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    if shutil.which('iconutil'):
        with tempfile.TemporaryDirectory() as t:
            iconset = os.path.join(t, 'AppIcon.iconset')
            os.mkdir(iconset)
            for pt in (16, 32, 128, 256, 512):
                for scale in (1, 2):
                    px = pt * scale
                    name = f'icon_{pt}x{pt}' + ('@2x' if scale == 2 else '') + '.png'
                    icon.resize((px, px), Image.LANCZOS).save(os.path.join(iconset, name))
            subprocess.run(['iconutil', '-c', 'icns', iconset, '-o', os.path.join(HERE, 'AppIcon.icns')], check=True)
    else:
        print('iconutil not found (not macOS): AppIcon.icns left as it was')


if __name__ == '__main__':
    main()
