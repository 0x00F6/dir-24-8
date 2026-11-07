#!/usr/bin/env bash
# Regenerates the benchmark images of README.md (benchmarks/charts/*.png)
# from the HTML reports of `make bench-compare` and `make bench-compare-lib`:
# captures with headless Chrome (site/charts.mjs), then stitching and palette
# compression (site/pack_png.py). The flame graph (make flamegraph) is not
# touched. .github/workflows/pages.yml runs it for every version tag.
#
# usage: site/charts.sh [reports-dir] [out-dir]
#   defaults: benchmarks/reports, benchmarks/charts
#   README_FILE=<path>  README whose benchmark-charts block lists the images
#                       (default README.md); site/charts.tsv gives the order
# Needs Node 22+, Chrome or Chromium (CHROME=<path>), Python 3 with Pillow.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
reports=${1:-$root/benchmarks/reports}
out=${2:-$root/benchmarks/charts}
for f in report.html bench-compare-lib.html; do
  [[ -f $reports/$f ]] || { echo "charts: $reports/$f missing (run the benchmark first)" >&2; exit 1; }
done

capture=$(mktemp -d)
trap 'rm -rf "$capture"' EXIT
node "$root/site/charts.mjs" "$reports/report.html" "$reports/bench-compare-lib.html" "$capture"
python3 "$root/site/pack_png.py" "$capture" "$out"

# README block `benchmark-charts`: every listed image this run produced.
readme=${README_FILE:-$root/README.md}
block=$(mktemp)
trap 'rm -rf "$capture" "$block"' EXIT
while IFS=$'\t' read -r name title alt width; do
  [[ -z $name || $name == \#* ]] && continue
  [[ -f $out/$name.png ]] || continue
  printf '<p align="center"><strong>%s</strong><br><img src="benchmarks/charts/%s.png" alt="%s" width="%s"></p>\n\n' \
    "$title" "$name" "$alt" "$width" >> "$block"
done < "$root/site/charts.tsv"
python3 "$root/site/readme_blocks.py" set "$readme" benchmark-charts "$block"
