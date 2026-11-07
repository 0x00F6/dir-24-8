#!/usr/bin/env bash
# Publishes dir-24-8-derive and dir-24-8 VERSION to crates.io
# (`make publish VERSION=X.Y.Z`):
#
#   1. checks: semver VERSION, clean working tree on the default branch, in
#      sync with origin (the published sources are a pushed commit), both
#      crate versions (and the dir-24-8-derive requirement) equal to VERSION,
#      a crates.io token (cargo login or CARGO_REGISTRY_TOKEN);
#   2. crates whose VERSION is already on crates.io are skipped, so a run
#      interrupted between the two crates can be resumed;
#   3. `make ci` and `make doc-crates` (CHECKS=0 skips them);
#   4. `cargo publish`, dir-24-8-derive first (cargo orders them and waits
#      for each one to reach the index).
#
# DRY_RUN=1 runs everything but the upload (`cargo publish --dry-run`,
# which still packages and builds both crates). A version cannot be
# overwritten or deleted on crates.io, only yanked (`cargo yank`).
set -euo pipefail

version=${VERSION:-}
dry_run=${DRY_RUN:-0}
crates=(dir-24-8-derive dir-24-8)
die() {
  printf 'publish: %s\n' "$*" >&2
  exit 1
}
step() { printf '\n==> %s\n' "$*"; }
# Sparse index path of a crate name of 4 or more characters.
index_url() { printf 'https://index.crates.io/%s/%s/%s' "${1:0:2}" "${1:2:2}" "$1"; }

semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
[[ $version =~ $semver ]] || die "VERSION=X.Y.Z required (semver), got '$version'"
cd "$(git rev-parse --show-toplevel)"

step "checks for $version"
command -v jq >/dev/null || die "jq is not installed"
command -v curl >/dev/null || die "curl is not installed"
git remote get-url origin >/dev/null 2>&1 || die "no origin remote"
default=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)
default=${default#origin/}
[[ -n $default ]] || default=$(git remote show origin | sed -n 's/.*HEAD branch: //p')
[[ -n $default ]] || die "cannot find the default branch of origin"

[[ -z $(git status --porcelain) ]] || die "the working tree has uncommitted or untracked changes"
branch=$(git symbolic-ref --quiet --short HEAD) || die "detached HEAD"
[[ $branch == "$default" ]] || die "on branch $branch: crates are published from $default"
git fetch --quiet origin "$default"
[[ $(git rev-parse HEAD) == $(git rev-parse "origin/$default") ]] ||
  die "$default and origin/$default differ: push or pull first"

versions=$(cargo metadata --no-deps --format-version 1 | jq -r --arg v "$version" '
  .packages[] | select(.name == "dir-24-8" or .name == "dir-24-8-derive")
  | (select(.version != $v) | "\(.name) is \(.version)"),
    (.dependencies[] | select(.name == "dir-24-8-derive" and .req != "^\($v)")
     | "the dir-24-8-derive requirement is \(.req)")')
[[ -z $versions ]] || die "versions differ from $version: $(echo "$versions" | paste -sd ';' -)
  set version = \"$version\" in Cargo.toml and derive/Cargo.toml (and the dir-24-8-derive
  dependency), commit and push, then run make publish VERSION=$version again"

if [[ $dry_run != 1 && -z ${CARGO_REGISTRY_TOKEN:-} ]] &&
  ! grep -qs '^\[registry\]' "${CARGO_HOME:-$HOME/.cargo}/credentials.toml"; then
  die "no crates.io token: run cargo login (token from https://crates.io/settings/tokens) or set CARGO_REGISTRY_TOKEN"
fi

index=$(mktemp)
trap 'rm -f "$index"' EXIT
pending=()
for crate in "${crates[@]}"; do
  code=$(curl -sS -o "$index" -w '%{http_code}' \
    -A 'dir-24-8 publish script' "$(index_url "$crate")") || die "cannot reach index.crates.io"
  case $code in
    200) jq -e --arg v "$version" 'select(.vers == $v)' "$index" >/dev/null &&
      { echo "skip: $crate $version is already on crates.io"; continue; } ;;
    404) ;;
    *) die "index.crates.io answered HTTP $code for $crate" ;;
  esac
  pending+=("$crate")
done
[[ ${#pending[@]} -gt 0 ]] || die "$version of both crates is already on crates.io: bump the versions"
echo "ok: $default at $(git rev-parse --short HEAD), to publish: ${pending[*]} $version"

if [[ ${CHECKS:-1} == 0 ]]; then
  step "make ci doc-crates skipped (CHECKS=0)"
else
  step "make ci doc-crates"
  make ci doc-crates
  [[ -z $(git status --porcelain) ]] || die "the checks changed the working tree: $(git status --porcelain | head -3)"
fi

args=()
for crate in "${pending[@]}"; do args+=(-p "$crate"); done
if [[ $dry_run == 1 ]]; then
  step "cargo publish --dry-run ${args[*]}"
  cargo publish --dry-run "${args[@]}"
  step "dry run: nothing was uploaded"
else
  step "cargo publish ${args[*]}"
  cargo publish "${args[@]}"
  step "published"
  for crate in "${pending[@]}"; do
    echo "  https://crates.io/crates/$crate/$version   https://docs.rs/$crate/$version"
  done
fi
