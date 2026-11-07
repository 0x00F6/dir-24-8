#!/usr/bin/env bash
# jq programs and the envsubst variable list are single-quoted on purpose;
# the HTML fragments (CHARTS) hold literal quotes.
# shellcheck disable=SC2016,SC2089,SC2090
# Assembles the GitHub Pages site (.github/workflows/pages.yml): the landing
# page (site/index.html), the reports of `make bench-compare` and
# `make bench-compare-lib`, the API documentation (target/doc, when built)
# and the media shown by the page.
#
# usage: site/build.sh <out-dir>
#   REPORTS=<dir>      reports to publish (default benchmarks/reports)
#   SITE_REPO_URL=<u>  repository link (default from GITHUB_REPOSITORY)
# Needs jq and envsubst. A missing report is shown as "not available".
set -euo pipefail

out=${1:?usage: site/build.sh <out-dir>}
root=$(cd "$(dirname "$0")/.." && pwd)
reports=${REPORTS:-$root/benchmarks/reports}
repo_url=${SITE_REPO_URL:-https://github.com/${GITHUB_REPOSITORY:-0x00F6/dir-24-8}}

rm -rf "$out"
mkdir -p "$out/assets" "$out/bench-compare" "$out/bench-compare-lib"
touch "$out/.nojekyll"
cp "$root/site/style.css" "$out/"
cp "$root/docs/images/logo.png" "$root/benchmarks/charts/lookup-flamegraph.svg" "$out/assets/"
cp "$root/docs/videos/dir_24_8_lookup_cpu.mp4" "$root/docs/videos/dir_24_8_lookup_cpu.png" "$out/assets/"

# jq helpers: sizes (1000 -> 1k), lists, minutes.
JQ_LIB='
def size: if . >= 1000000 and . % 1000000 == 0 then "\(. / 1000000)M"
          elif . >= 1000 and . % 1000 == 0 then "\(. / 1000)k" else "\(.)" end;
def sizes: map(size) | join(", ");
def minutes: if . == null then "?" elif . < 60 then "\(. | floor) s" else "\(. / 60 | floor) min" end;
def date: if . == null then "?" else todate[0:10] end;
'

# Report card: $1 title, $2 description, $3 report page, $4 JSON, $5 CSV,
# $6 jq program printing the <dl> rows and status (HTML, already escaped).
card() {
  local title=$1 desc=$2 page=$3 json=$4 csv=$5 rows=$6
  local dir
  dir=$(dirname "$page")
  if [[ -f "$out/$page" && -f "$out/$json" ]]; then
    local meta
    meta=$(jq -r "$JQ_LIB $rows" "$out/$json")
    cat <<EOF
<article class="card report">
  <h3>$title</h3>
  <p class="desc">$desc</p>
  <dl class="meta">
$meta
  </dl>
  <div class="links">
    <a class="btn primary" href="$page">Open the report</a>
    <a href="$json">JSON</a>
    <a href="$csv">CSV</a>
  </div>
</article>
EOF
  else
    cat <<EOF
<article class="card report">
  <h3>$title</h3>
  <p class="desc">$desc</p>
  <p class="empty">No report in this build ($dir).</p>
</article>
EOF
  fi
}

if [[ -f "$reports/report.html" ]]; then
  cp "$reports/report.html" "$reports/results.json" "$reports/results.csv" "$out/bench-compare/"
fi
if [[ -f "$reports/bench-compare-lib.html" ]]; then
  cp "$reports/bench-compare-lib.html" "$reports/bench-compare-lib.json" "$reports/bench-compare-lib.csv" \
    "$out/bench-compare-lib/"
fi

CARD_COMPARE=$(card \
  'Rust structures <code>make bench-compare</code>' \
  'dir-24-8 against Rust LPM and membership structures: isolated-call latency percentiles, throughput with several threads, memory and build time, SIMD versus scalar batches per batch size.' \
  bench-compare/report.html bench-compare/results.json bench-compare/results.csv '
  .meta as $m
  | ([.runs[].status] | group_by(.) | map("\(length) \(.[0])") | join(", ")) as $counts
  | ([.runs[] | select(.status == "failed" or .status == "invalid")] | length) as $bad
  | "    <dt>Run</dt><dd>\($m.env.timestamp_unix | date) · \($m.duration_s | minutes) · <span class=\"status \(if $bad > 0 then "bad" elif $m.complete then "ok" else "warn" end)\">\(if $bad > 0 then "\($bad) failed" elif $m.complete then "complete" else "partial" end)</span></dd>",
    "    <dt>Machine</dt><dd>\($m.env.cpu | @html)</dd>",
    "    <dt>Batch kernel</dt><dd>\(($m.env.batch_kernel // $m.env.auto_simd_backend // "?") | @html)</dd>",
    "    <dt>Matrix</dt><dd>\(.engines | length) structures · \($m.settings.sizes | sizes) entries · \($m.settings.threads | map(tostring) | join(", ")) threads</dd>",
    "    <dt>Runs</dt><dd>\($counts | @html)</dd>"
')

CARD_LIB=$(card \
  'Go, C and Rust libraries <code>make bench-compare-lib</code>' \
  'dir-24-8 against bart, go-iptrie, both cidrangers, libmaxminddb and Rust crates, each in a native runner of its language: membership and LPM throughput, latency, memory per prefix.' \
  bench-compare-lib/bench-compare-lib.html bench-compare-lib/bench-compare-lib.json bench-compare-lib/bench-compare-lib.csv '
  .meta as $m
  | "    <dt>Run</dt><dd>\($m.started_unix | date) · \($m.duration_s | minutes) · <span class=\"status \(if $m.status == "complete" then "ok" elif $m.status == "partial" then "warn" else "bad" end)\">\($m.status | @html)</span></dd>",
    "    <dt>Machine</dt><dd>\($m.env.cpu | @html)</dd>",
    "    <dt>Profile</dt><dd>\($m.profile | @html), \($m.settings.repetitions) processes per point</dd>",
    "    <dt>Matrix</dt><dd>\(.libs | length) libraries · \($m.settings.sizes | sizes) prefixes · \($m.settings.threads | map(tostring) | join(", ")) threads</dd>",
    "    <dt>Measurements</dt><dd>\($m.status_reason | sub("^measurements: "; "") | @html)</dd>"
')

if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
  BENCH_NOTE="Measured for release $GITHUB_REF_NAME by the GitHub Actions run that built this page, on a shared virtual machine: compare structures within one report rather than absolute numbers across machines. The README shows measurements from dedicated hardware."
else
  BENCH_NOTE='Measured on the machine shown on each card. Compare structures within one report rather than absolute numbers across machines.'
fi

# Benchmark charts (site/charts.tsv order), as refreshed by site/charts.sh.
png_size() { # width and height from the PNG header (IHDR)
  local b
  read -r -a b < <(od -An -tu1 -j16 -N8 "$1")
  echo "$(( (b[0] << 24) | (b[1] << 16) | (b[2] << 8) | b[3] )) $(( (b[4] << 24) | (b[5] << 16) | (b[6] << 8) | b[7] ))"
}
CHARTS=""
mkdir -p "$out/assets/charts"
while IFS=$'\t' read -r name title alt _; do
  [[ -z $name || $name == \#* ]] && continue
  png=$root/benchmarks/charts/$name.png
  [[ -f $png ]] || continue
  cp "$png" "$out/assets/charts/"
  read -r w h < <(png_size "$png")
  printf -v figure '<figure class="chart"><a href="assets/charts/%s.png"><img class="figure-img" src="assets/charts/%s.png" alt="%s" width="%s" height="%s" loading="lazy"></a><figcaption>%s</figcaption></figure>\n' \
    "$name" "$name" "$alt" "$w" "$h" "$title"
  CHARTS+=$figure
done < "$root/site/charts.tsv"

if [[ -d "$root/target/doc/dir_24_8" ]]; then
  cp -r "$root/target/doc" "$out/api"
  API_HREF=api/dir_24_8/index.html
else
  API_HREF=https://docs.rs/dir-24-8
fi

SITE_COMMIT=${GITHUB_SHA:-$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)}
SITE_COMMIT_SHORT=${SITE_COMMIT:0:7}
# Version tag that triggered the build (pages.yml), else the crate version.
if [[ ${GITHUB_REF_TYPE:-} == tag ]]; then
  SITE_VERSION=$GITHUB_REF_NAME
else
  SITE_VERSION=v$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
fi
SITE_DATE=$(date -u +%Y-%m-%d)
SITE_REPO_URL=$repo_url
export CARD_COMPARE CARD_LIB BENCH_NOTE API_HREF SITE_COMMIT SITE_COMMIT_SHORT SITE_DATE SITE_REPO_URL SITE_VERSION CHARTS
envsubst '$CARD_COMPARE $CARD_LIB $BENCH_NOTE $API_HREF $SITE_COMMIT $SITE_COMMIT_SHORT $SITE_DATE $SITE_REPO_URL $SITE_VERSION $CHARTS' \
  < "$root/site/index.html" > "$out/index.html"

echo "site assembled in $out:"
(cd "$out" && find . -maxdepth 2 -type f | sort | grep -v '^./api/' && [[ -d api ]] && echo "./api/ (API documentation)") || true
du -sh "$out" | cut -f1
