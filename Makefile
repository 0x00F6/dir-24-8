# dir-24-8 developer commands. `make` or `make help` lists them.
#
# Benchmarks have two targets; every option is a variable:
#
#   make bench          Criterion benchmarks of the library
#     FILTER=<regex>       Criterion filter (e.g. FILTER='^batch/ipv4')
#     BENCHES=<list>       lookup,update,snapshot (default: lookup,update)
#     SIMD=0               build without the `batch-simd` feature: batches use the scalar kernel
#                          (default: `batch-simd` on, batches use the CPU's AVX-512/AVX2/NEON kernel;
#                          single lookups are scalar either way)
#     BENCH_TIME=<secs>    measurement time per benchmark (default 1.5)
#     V4_ROUTES / V6_ROUTES  fixture sizes (default 200000 / 100000)
#     SNAPSHOT_SIZES=<list>    snapshot bench: routes per table (default 10k,100k,1M)
#     SNAPSHOT_PROFILES=<list> snapshot bench: XZ profiles, e.g. 9e,9e+sorted,6,9e+t4+b8 (default 9e,9e+sorted,6)
#     SNAPSHOT_FAMILIES=<list> snapshot bench: ipv4,ipv6     SNAPSHOT_REPS=<n> repetitions (default 3)
#
#   make bench-compare  two comparisons, one report: benchmarks/reports/report.html (a tab each)
#                       structures: dir-24-8 against Rust LPM / membership structures, in process
#                         -> results.json, results.csv
#                       libraries: single-thread and multithreaded IPv4/IPv6 lookups of dir-24-8 against
#                         go-cidranger (libp2p), cidranger (yl2chen), go-iptrie, bart (Go), prefix-trie,
#                         iprange (Rust) and libmaxminddb (C), each in a native runner of its language
#                         -> libraries.json, libraries.csv
#                         (needs go >= 1.21 on PATH (it fetches the pinned go1.27.3), cc, curl, tar, sha256sum)
#     PARTS=<list>         structures,libraries or all (default all)
#    Shared by both parts:
#     FAMILIES=<list>      ipv4,ipv6
#     THREADS=<list>       worker threads (default 1,4,8; libraries: the profile's, always with 1)
#     OUT=<dir>            reports directory (default benchmarks/reports)
#     REPORT_ONLY=1        regenerate the CSV files and report.html from OUT/results.json and
#                          OUT/libraries.json without measuring (a part without results is left out)
#     README=auto|1|0      refresh the generated blocks of README.md (auto, the default: "Detailed results"
#                          for complete runs of the default structures up to 1M entries or more, "Performance
#                          Summary" for complete runs of the standard and full library profiles)
#    Structures part:
#     SIZES=<list>         entries per table (default 1k,10k,50k,100k,250k,500k,1M,2M,5M)
#     OPS=<list>           lookup,batch,exists      DISTS=<list>    random,hits,misses
#     BATCH=<list>         addresses per batch call (default 256)
#     BATCH_SWEEP=<list>   extra batch sizes for dir-24-8 (SIMD and scalar batches), first thread
#                          count only, for the batch-size chart (default 1,4,8,16,32,64,128,512,1024;
#                          none = off). dir-24-8[scalar] is measured by bench-compare-scalar, the same
#                          binary built without the `batch-simd` feature
#     HIT_RATIO=<r>        hits in the random distribution: 50, 50% or 0.5 (default 50%)
#     ENGINES=<list>       default | all | name substrings, combinable (default: default)
#     QUERIES=<n> VALIDATE=<n> SEED=<n> WARMUP_MS=<ms> WINDOW_MS=<ms> REPS=<n> LAT_SAMPLES=<n> LAT_BATCHES=<n>
#     LAT_MAX_MS=<ms>      cap of one latency pass for slow structures (default 100)
#     MEM_LIMIT_MIB=<n>    memory limit per structure in MiB, auto = 70% of MemAvailable (default: none)
#     BUDGET=<s>           time budget of the part (default 3600, 0 = none)
#     TIMEOUT=<s>          limit per measuring process (default 600)
#     PIN=0                do not pin worker threads to cores
#    Libraries part:
#     LIB_PROFILE=<p>      smoke | standard (default) | full: the defaults of every LIB_* option
#     LIB_SIZES=<list>     input prefixes (standard 1k,10k,100k,1M; full 1k..10M)
#     LIB_REPETITIONS=<n>  independent processes per library and size (standard and full: 2)
#     LIB_SEED=<n> LIB_LIBS=<ids> LIB_DETAIL_SIZES=<list> LIB_QUERIES=<n> LIB_TARGET_MS=<ms>
#     LIB_LATENCY_SAMPLES=<n> LIB_GOAMD64=v1 LIB_TIMEOUT=<s> LIB_MEM_LIMIT_MIB=<n> LIB_CACHE=<dir>
#     LIB_BUDGET=<s>       time budget of the part (standard 900)
#
# Profiling and coverage (not benchmarks):
#   make flamegraph      flame graph of every lookup method -> benchmarks/charts/lookup-flamegraph.svg
#     PROFILE_SECONDS=<s>  profiled time per lookup method (default 1)
#   make coverage        line/region/function coverage of the library -> target/llvm-cov/html
#
# Release (GitHub):
#   make release VERSION=X.Y.Z   checks (clean tree on the default branch in sync with origin, crate
#                                versions = VERSION, unused tag), make ci, packages, tag vX.Y.Z, push,
#                                gh release create with generated notes and the .crate files; the
#                                pushed tag also rebuilds the GitHub Pages site (pages.yml)
#     DRY_RUN=1   checks only, print the tag/push/release commands   DRAFT=1   draft release
#     NOTES=<file> release notes instead of generated ones           CHECKS=0  skip make ci
#
# Publication (crates.io):
#   make publish VERSION=X.Y.Z   checks (clean tree on the default branch in sync with origin, crate
#                                versions = VERSION, crates.io token), make ci doc-crates, then
#                                cargo publish of dir-24-8-derive and dir-24-8 (a version already on
#                                crates.io is skipped; a published version cannot be replaced)
#     DRY_RUN=1   everything but the upload (cargo publish --dry-run)   CHECKS=0  skip make ci doc-crates
#
# Other variables:
#   TEST_THREADS=<n>     test threads (default 8; every table maps 64 MiB per family)
#   NIGHTLY=<toolchain>  toolchain of doc-crates, as docs.rs documents with nightly (default nightly)
#   NO_COLOR=1           disable colors

.DEFAULT_GOAL := help
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

CARGO ?= cargo
FILTER ?=
BENCHES ?= lookup,update
TEST_THREADS ?= 8
PROFILE_SECONDS ?= 1
NIGHTLY ?= nightly
DOCSRS_DIR = target/docs-rs
# README files crates.io shows: dir-24-8, dir-24-8-derive.
CRATES_IO_PAGES = README.crates.md derive/README.md
comma := ,

ifeq ($(NO_COLOR),)
  C_TITLE := \033[1;36m
  C_CAT   := \033[1;33m
  C_CMD   := \033[32m
  C_ERR   := \033[1;31m
  C_OFF   := \033[0m
else
  C_TITLE :=
  C_CAT   :=
  C_CMD   :=
  C_ERR   :=
  C_OFF   :=
endif

# Features of the aarch64 cross check: `snapshot` is left out because it
# compiles liblzma (C) for the target, which needs an aarch64 C toolchain;
# its code has nothing architecture-specific (native AArch64 CI builds it).
CROSS_FEATURES = derive,serde
# Every library feature but `batch-simd` (scalar batch kernel).
SCALAR_FEATURES = derive,serde,snapshot
BENCH_FEATURES = $(SCALAR_FEATURES)$(if $(filter 0,$(SIMD)),,$(comma)batch-simd)
EXAMPLES = $(basename $(notdir $(wildcard examples/*.rs)))
COMPARE_BIN = $${CARGO_TARGET_DIR:-target}/release/bench-compare
# Forwards Makefile variable $(1) to environment variable $(2) when it is set.
fwd = $(if $($(1)),$(2)='$($(1))')
BENCH_ENV = $(call fwd,BENCH_TIME,BENCH_TIME) $(call fwd,V4_ROUTES,BENCH_V4_ROUTES) \
            $(call fwd,V6_ROUTES,BENCH_V6_ROUTES) \
            $(call fwd,SNAPSHOT_SIZES,BENCH_SNAPSHOT_SIZES) $(call fwd,SNAPSHOT_PROFILES,BENCH_SNAPSHOT_PROFILES) \
            $(call fwd,SNAPSHOT_FAMILIES,BENCH_SNAPSHOT_FAMILIES) $(call fwd,SNAPSHOT_REPS,BENCH_SNAPSHOT_REPS)
# Options of the libraries part: LIB_<KEY> -> BENCH_LIB_<KEY>.
LIB_KEYS = PROFILE SIZES REPETITIONS SEED QUERIES TARGET_MS LATENCY_SAMPLES DETAIL_SIZES LIBS BUDGET \
           TIMEOUT MEM_LIMIT_MIB GOAMD64 CACHE
COMPARE_ENV = $(call fwd,PARTS,BENCH_PARTS) $(foreach k,$(LIB_KEYS),$(call fwd,LIB_$(k),BENCH_LIB_$(k))) \
              $(call fwd,SIZES,BENCH_SIZES) $(call fwd,FAMILIES,BENCH_FAMILIES) \
              $(call fwd,OPS,BENCH_OPS) $(call fwd,DISTS,BENCH_DISTS) $(call fwd,THREADS,BENCH_THREADS) \
              $(call fwd,BATCH,BENCH_BATCH) $(call fwd,BATCH_SWEEP,BENCH_BATCH_SWEEP) \
              $(call fwd,HIT_RATIO,BENCH_HIT_RATIO) \
              $(call fwd,ENGINES,BENCH_ENGINES) $(call fwd,QUERIES,BENCH_QUERIES) $(call fwd,SEED,BENCH_SEED) \
              $(call fwd,VALIDATE,BENCH_VALIDATE) \
              $(call fwd,WARMUP_MS,BENCH_WARMUP_MS) $(call fwd,WINDOW_MS,BENCH_WINDOW_MS) \
              $(call fwd,REPS,BENCH_REPS) $(call fwd,LAT_SAMPLES,BENCH_LAT_SAMPLES) \
              $(call fwd,LAT_BATCHES,BENCH_LAT_BATCHES) $(call fwd,LAT_MAX_MS,BENCH_LAT_MAX_MS) \
              $(call fwd,MEM_LIMIT_MIB,BENCH_MEM_LIMIT_MIB) \
              $(call fwd,BUDGET,BENCH_BUDGET) $(call fwd,TIMEOUT,BENCH_TIMEOUT) $(call fwd,PIN,BENCH_PIN) \
              $(call fwd,OUT,BENCH_OUT) $(call fwd,REPORT_ONLY,BENCH_REPORT_ONLY) \
              $(call fwd,README,BENCH_README)

.PHONY: help check check-aarch64 build build-release fmt fmt-check lint test test-scalar test-simd \
        coverage bench bench-compare flamegraph examples doc doc-open doc-crates \
        release publish clean ci

##@ Help
help: ## Show this help
	@printf "$(C_TITLE)dir-24-8$(C_OFF) - make targets\n"
	@awk 'BEGIN {FS = ":.*## "} \
	  /^##@/ { printf "\n$(C_CAT)%s$(C_OFF)\n", substr($$0, 5); next } \
	  /^[a-zA-Z0-9_-]+:.*## / { printf "  $(C_CMD)%-16s$(C_OFF) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@printf "\n$(C_CAT)Benchmark variables$(C_OFF) (every option is documented in the Makefile header)\n"
	@printf "  $(C_CMD)bench$(C_OFF)            FILTER=<regex> BENCHES=lookup,update,snapshot SIMD=0 BENCH_TIME=<s>\n"
	@printf "  $(C_CMD)bench-compare$(C_OFF)    PARTS=structures,libraries FAMILIES=ipv4,ipv6 THREADS=1,4,8 OUT=<dir> REPORT_ONLY=1\n"
	@printf "                   structures: SIZES=1k,...,5M OPS=lookup,batch,exists DISTS=random,hits,misses\n"
	@printf "                   BATCH=256 BATCH_SWEEP=1,...,1024 HIT_RATIO=50%% ENGINES=default|all|<names>\n"
	@printf "                   MEM_LIMIT_MIB=<n>|auto BUDGET=<s> TIMEOUT=<s> WINDOW_MS=<ms> REPS=<n> LAT_SAMPLES=<n> PIN=0\n"
	@printf "                   libraries: LIB_PROFILE=smoke|standard|full LIB_SIZES=1k,1M LIB_REPETITIONS=2 LIB_LIBS=<ids>\n"
	@printf "\nOther variables: TEST_THREADS, NIGHTLY, NO_COLOR\n"

##@ Build
check: ## Type-check every target, all features
	$(CARGO) check --workspace --all-targets --all-features

check-aarch64: ## Type-check and lint the library (scalar and NEON batch kernels) for aarch64
	@rustup target list --installed 2>/dev/null | grep -q aarch64-unknown-linux-gnu || \
	  { printf "$(C_ERR)target aarch64-unknown-linux-gnu missing: rustup target add aarch64-unknown-linux-gnu$(C_OFF)\n"; exit 1; }
	$(CARGO) check -p dir-24-8 --lib --features $(CROSS_FEATURES),batch-simd --target aarch64-unknown-linux-gnu
	$(CARGO) clippy -p dir-24-8 --lib --features $(CROSS_FEATURES),batch-simd --target aarch64-unknown-linux-gnu -- -D warnings
	$(CARGO) clippy -p dir-24-8 --lib --features $(CROSS_FEATURES) --target aarch64-unknown-linux-gnu -- -D warnings

build: ## Debug build of the library
	$(CARGO) build -p dir-24-8 --all-features

build-release: ## Optimized build of the library
	$(CARGO) build -p dir-24-8 --release --all-features

##@ Quality
fmt: ## Format the code
	$(CARGO) fmt --all

fmt-check: ## Check formatting
	$(CARGO) fmt --all -- --check

lint: ## Clippy on every target, with and without features, warnings are errors
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
	$(CARGO) clippy -p dir-24-8 --all-targets --no-default-features -- -D warnings
	$(CARGO) clippy -p dir-24-8 --all-targets --features $(SCALAR_FEATURES) -- -D warnings
	$(CARGO) clippy -p dir-24-8-compare --all-targets --no-default-features -- -D warnings

##@ Tests
test: ## Unit, integration, property and doc tests, with and without features (scalar and SIMD batches)
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) test -p dir-24-8
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) test -p dir-24-8 --all-features
	$(CARGO) test -p dir-24-8-derive
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) test -p dir-24-8-compare --release

test-scalar: ## Test suite with every feature but batch-simd (scalar batch kernel)
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) test -p dir-24-8 --features $(SCALAR_FEATURES)

test-simd: ## Test suite with batch-simd: the CPU's batch kernel, plus every SIMD kernel it supports against scalar
	@$(CARGO) run --quiet --release --features batch-simd --example batch_kernel | head -n 2
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) test -p dir-24-8 --all-features

coverage: ## Line, region and function coverage of the library, all features (needs cargo-llvm-cov) -> target/llvm-cov/html
	@$(CARGO) llvm-cov --version >/dev/null 2>&1 || \
	  { printf "$(C_ERR)cargo-llvm-cov missing: cargo install cargo-llvm-cov$(C_OFF)\n"; exit 1; }
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) llvm-cov -p dir-24-8 --all-features --html
	$(CARGO) llvm-cov report -p dir-24-8 --summary-only

##@ Benchmarks
bench: ## Library benchmarks (lookups, updates; snapshots with BENCHES=snapshot); see the variables below
	$(BENCH_ENV) $(CARGO) bench -p dir-24-8 --features $(BENCH_FEATURES) $(foreach b,$(subst $(comma), ,$(BENCHES)),--bench $(b)) -- $(FILTER)

bench-compare: ## Rust structures and Go/C/Rust libraries -> benchmarks/reports/report.html (+ JSON, CSV)
	@# Scalar-batch build first (measures dir-24-8[scalar]), kept next to the default one.
	$(CARGO) build --release -p dir-24-8-compare --no-default-features --bin bench-compare
	cp -f $(COMPARE_BIN) $(COMPARE_BIN)-scalar
	@# The default build is also the Rust runner of the libraries part, whose report shows its build time.
	@s=$$(date +%s%N); echo "$(CARGO) build --release -p dir-24-8-compare --bin bench-compare"; \
	  $(CARGO) build --release -p dir-24-8-compare --bin bench-compare; \
	  e=$$(date +%s%N); echo "$(strip $(COMPARE_ENV) $(COMPARE_BIN))"; \
	  BENCH_RUST_BUILD_MS=$$(( (e - s) / 1000000 )) $(COMPARE_ENV) $(COMPARE_BIN)

##@ Profiling
flamegraph: ## Flame graph of every lookup method, IPv4 and IPv6 (perf + cargo-flamegraph) -> benchmarks/charts/lookup-flamegraph.svg
	@command -v perf >/dev/null || { printf "$(C_ERR)perf missing$(C_OFF)\n"; exit 1; }
	@$(CARGO) flamegraph --version >/dev/null 2>&1 || \
	  { printf "$(C_ERR)cargo-flamegraph missing: cargo install flamegraph$(C_OFF)\n"; exit 1; }
	@mkdir -p target/flamegraph benchmarks/charts
	@# Frame-pointer unwinding (DWARF stacks lost most frames), in a separate
	@# target directory so that the RUSTFLAGS do not invalidate target/release.
	cd target/flamegraph && CARGO_TARGET_DIR="$(CURDIR)/target/flamegraph/build" \
	  RUSTFLAGS="-C force-frame-pointers=yes" $(CARGO) flamegraph --manifest-path ../../Cargo.toml \
	  -p dir-24-8 --release --features batch-simd --example lookup_profile --deterministic \
	  --cmd "record -F 1999 --call-graph fp -g" \
	  --title "dir-24-8 lookup methods (IPv4 + IPv6, batch-simd)" -o lookup-flamegraph.svg -- $(PROFILE_SECONDS)
	rm -f target/flamegraph/perf.data target/flamegraph/perf.data.old
	cp target/flamegraph/lookup-flamegraph.svg benchmarks/charts/lookup-flamegraph.svg

##@ Examples
examples: ## Run every example (each asserts its results)
	@for ex in $(EXAMPLES); do \
	  printf "$(C_CMD)==> %s$(C_OFF)\n" "$$ex"; \
	  $(CARGO) run --quiet --release --all-features --example "$$ex" > /dev/null; \
	done
	@printf "all examples passed\n"

##@ Documentation
doc: ## API documentation, warnings are errors
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc -p dir-24-8 --all-features --no-deps

doc-open: ## Build and open the API documentation
	$(CARGO) doc -p dir-24-8 --all-features --no-deps --open

doc-crates: ## crates.io + docs.rs pages: API docs built like docs.rs (nightly), README examples and links, packages -> target/docs-rs/doc
	@rustup run $(NIGHTLY) rustc --version >/dev/null 2>&1 || \
	  { printf "$(C_ERR)toolchain $(NIGHTLY) missing (docs.rs documents with nightly): rustup toolchain install $(NIGHTLY)$(C_OFF)\n"; exit 1; }
	@printf "$(C_CMD)==> API documentation as docs.rs builds it ([package.metadata.docs.rs]: all features, --cfg docsrs)$(C_OFF)\n"
	@# Own target directory: nightly artifacts do not invalidate the stable ones.
	CARGO_TARGET_DIR=$(DOCSRS_DIR) RUSTDOCFLAGS="--cfg docsrs -D warnings" \
	  rustup run $(NIGHTLY) cargo doc -p dir-24-8 -p dir-24-8-derive --all-features --no-deps
	@printf "$(C_CMD)==> crates.io pages: examples ($(CRATES_IO_PAGES))$(C_OFF)\n"
	RUST_TEST_THREADS=$(TEST_THREADS) $(CARGO) test -p dir-24-8 --all-features --doc ReadmeCratesDoctests
	$(CARGO) test -p dir-24-8-derive --doc ReadmeDoctests
	@printf "$(C_CMD)==> crates.io pages: links (crates.io resolves no relative link or image)$(C_OFF)\n"
	@awk 'function check(t) { if (t !~ /^(https?:\/\/|mailto:|#)/) { printf "%s:%d: relative link %s\n", FILENAME, FNR, t; bad = 1 } } \
	  FNR == 1 { code = 0 } /^[[:space:]]*(```|~~~)/ { code = !code; next } code { next } \
	  { line = $$0; gsub(/`[^`]*`/, "", line); \
	    if (match(line, /^[[:space:]]*\[[^]]+\]:[[:space:]]*[^[:space:]]+/)) { \
	      t = line; sub(/^[[:space:]]*\[[^]]+\]:[[:space:]]*/, "", t); sub(/[[:space:]].*/, "", t); check(t) } \
	    while (match(line, /\]\([^)]+\)|(src|href)="[^"]*"/)) { \
	      t = substr(line, RSTART, RLENGTH); line = substr(line, RSTART + RLENGTH); \
	      sub(/^(\]\(|src="|href=")/, "", t); sub(/[)"]$$/, "", t); sub(/[[:space:]].*/, "", t); check(t) } } \
	  END { if (!bad) print "no relative link"; exit bad }' $(CRATES_IO_PAGES)
	@printf "$(C_CMD)==> packages (files and size uploaded to crates.io)$(C_OFF)\n"
	$(CARGO) package -p dir-24-8-derive -p dir-24-8 --no-verify --allow-dirty
	@printf "\nAPI documentation: $(DOCSRS_DIR)/doc/dir_24_8/index.html (open it with xdg-open)\n"

##@ Release
release: ## Publish the GitHub release vVERSION (VERSION=X.Y.Z; DRY_RUN=1, DRAFT=1, NOTES=<file>, CHECKS=0)
	@VERSION='$(VERSION)' DRY_RUN='$(DRY_RUN)' DRAFT='$(DRAFT)' NOTES='$(NOTES)' CHECKS='$(CHECKS)' \
	  scripts/release.sh

publish: ## Publish dir-24-8-derive and dir-24-8 VERSION to crates.io (VERSION=X.Y.Z; DRY_RUN=1, CHECKS=0)
	@VERSION='$(VERSION)' DRY_RUN='$(DRY_RUN)' CHECKS='$(CHECKS)' scripts/publish.sh

##@ Maintenance
clean: ## Remove build artifacts and cached benchmark datasets (cargo clean; reports are kept)
	$(CARGO) clean

ci: fmt-check lint test examples doc ## Everything CI runs (plus the aarch64 check when available)
	@if rustup target list --installed 2>/dev/null | grep -q aarch64-unknown-linux-gnu; then \
	  $(MAKE) --no-print-directory check-aarch64; \
	else printf "aarch64 target not installed: NEON type-check not executed\n"; fi
