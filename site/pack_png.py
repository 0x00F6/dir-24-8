"""Finishes the captures of site/charts.mjs (site/charts.sh runs it).

Stores every image as a 256-colour palette PNG when that is visually
lossless (mean error below 0.5 / 255 on every channel), else as RGB.

usage: python3 site/pack_png.py <capture-dir> <out-dir>    (needs Pillow)
"""

import glob
import os
import sys

from PIL import Image, ImageChops, ImageStat


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
    pack(sys.argv[1], sys.argv[2])
