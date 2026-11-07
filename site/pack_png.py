"""Finishes the captures of site/charts.mjs (site/charts.sh runs it).

* stitches the bands of the full-report preview (<name>.partNN.png);
* stores every image as a 256-colour palette PNG when that is visually
  lossless (mean error below 0.5 / 255 on every channel), else as RGB.

usage: python3 site/pack_png.py <capture-dir> <out-dir>    (needs Pillow)
"""

import glob
import os
import re
import sys

from PIL import Image, ImageChops, ImageStat

Image.MAX_IMAGE_PIXELS = None  # the full-report preview is about 124 Mpx


def stitch(capture_dir):
    groups = {}
    for part in sorted(glob.glob(os.path.join(capture_dir, "*.part[0-9][0-9].png"))):
        name = re.sub(r"\.part\d\d\.png$", ".png", part)
        groups.setdefault(name, []).append(part)
    for name, parts in groups.items():
        bands = [Image.open(p).convert("RGB") for p in parts]
        full = Image.new("RGB", (bands[0].width, sum(b.height for b in bands)))
        y = 0
        for b in bands:
            full.paste(b, (0, y))
            y += b.height
        full.save(name)
        for p in parts:
            os.remove(p)


def pack(capture_dir, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    for path in sorted(glob.glob(os.path.join(capture_dir, "*.png"))):
        rgb = Image.open(path).convert("RGB")
        palette = rgb.quantize(colors=256, method=Image.Quantize.MEDIANCUT)
        error = max(ImageStat.Stat(ImageChops.difference(rgb, palette.convert("RGB"))).mean)
        target = os.path.join(out_dir, os.path.basename(path))
        (palette if error < 0.5 else rgb).save(target, optimize=True)
        kind = "palette" if error < 0.5 else "rgb"
        print(f"{os.path.basename(path):28} {rgb.width}x{rgb.height:<6} {kind:7} {os.path.getsize(target) / 1e3:7.0f} kB")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    stitch(sys.argv[1])
    pack(sys.argv[1], sys.argv[2])
