# AGENTS.md

Guidance for humans and coding agents working on this repository.

## Conventions

* Rust stable, edition 2024, MSRV **1.89** (AVX-512 intrinsics and target
  features were stabilized in 1.89). Do not use nightly features.
* Code, comments, docs and commit messages are in English.
* `cargo fmt` formatting, `cargo clippy -D warnings` clean (with and without
  features, and for `aarch64-unknown-linux-gnu`).
* The public surface is small and re-exported from `src/lib.rs`; new public
  items need docs (`#![warn(missing_docs)]`).
* Lookup kernels are not configurable: single lookups always run
  `simd::scalar`; batch lookups run `scalar` too unless the `batch-simd`
  feature is enabled, which compiles the AVX-512/AVX2 (x86-64) and NEON
  (AArch64) batch kernels and picks one at table construction
  (`Kernel::detect`). Do not add a runtime kernel or backend selector.
* Generic types must not gain needless bounds through derives: `LookupMatch`
  and `MatchId` implement `Clone`/`Copy`/`PartialEq` by hand so that no
  `T: Clone`/`T: Copy` bound appears. Only the merging insert modes
  (`Merge`, `SerdeMerge`) require `T: DeepMerge` (or serde bounds); the
  mode is a type parameter of `insert`, so `Replace` works for any `T`.

## Layout

| Path | Responsibility |
|---|---|
| `src/lib.rs` | crate docs and exports |
| `src/table/` | `IpTable` facade, `Config`, stats, `TableBuffer` (mmap/huge pages), `GroupPool`, engine limits |
| `src/ipv4/` | DIR-24-8 engine (insert/remove/fold) |
| `src/ipv6/` | 24/8 multi-level trie engine |
| `src/lookup/` | `LookupMatch`, `MatchId`, batch drivers (`BatchKey`, sinks), batch `Kernel` detection and dispatch (with the SIMD-versus-scalar kernel tests) |
| `src/simd/` | kernels: `scalar` (single lookups and default batches), `avx2`, `avx512` (x86-64), `neon` (AArch64) batch kernels behind `batch-simd`, `Tables` view |
| `src/route/` | cell encoding, normalized prefixes, route store, prefix index |
| `src/mode.rs` | insert modes (`Replace`, `Merge`, `MergeWith`, `SerdeMerge`), sealed `InsertMode` trait |
| `src/merge/` | `DeepMerge`, payload merge rules, serde adapter (feature `serde`) |
| `src/snapshot/` | compressed snapshots (feature `snapshot`): format v1 spec in `mod.rs`, `format.rs` header/body writer and parser, `xz.rs` liblzma setup and error classification, `options.rs`, `file.rs` atomic publication, `error.rs` |
| `derive/` | `dir-24-8-derive`: `#[derive(DeepMerge)]` (proc macro, re-exported by the `derive` feature) |
| `src/error.rs` | public errors |
| `tests/` | integration, oracle, property, concurrency, no-allocation tests |
| `examples/` | runnable examples (each asserts its results) |
| `benches/` | Criterion benches; `benches/support/data.rs` is the shared data generator (route tables, oracle, query distributions) used by Criterion and `bench-compare` |
| `benchmarks/compare/` | `bench-compare` crate (separate package, its dependencies never reach the library): `engines/` competitors, `adapter.rs` uniform engine interface, `dataset.rs` cached datasets, `bench.rs` measurement process, `main.rs` orchestration, `report.rs` + `assets/` JSON/CSV/HTML reports |
| `benchmarks/compare/src/libcmp/` | `bench-compare-lib` (same crate): corpora (`corpus.rs`), dependency preparation (`deps.rs`), Rust runner (`runner.rs`), orchestration, aggregation, report (`assets/lib-report.*`, shared `kit.js` and `report.css`) |
| `benchmarks/compare-lib/` | native runners of `bench-compare-lib`: `go/` (one module: cidranger x2, go-iptrie, bart, MMDB writer), `c/runner.c` (libmaxminddb); protocol and methodology in its `README.md` |
| `benchmarks/reports/` | generated reports (git-ignored) |
| `benchmarks/charts/` | images shown by `README.md`: report captures and the lookup flame graph (`make flamegraph`); excluded from the crate package |
| `docs/` | architecture and performance notes |
| `scripts/release.sh` | `make release`: GitHub release (checks, `make ci`, packages, tag, `gh release create`) |
| `scripts/publish.sh` | `make publish`: crates.io publication of `dir-24-8-derive` then `dir-24-8` (checks, `make ci doc-crates`, `cargo publish`; versions already on crates.io are skipped) |
| `site/` | GitHub Pages site (`.github/workflows/pages.yml`: full mode with fresh benchmarks for version tags `vX.Y.Z` pushed by `make release` or by hand; site mode, reusing the deployed reports, for pushes to master that change `site/` or the page's assets): landing page template and style, `build.sh` (assembles it with the reports measured by the workflow and the API docs), `charts.sh` + `charts.mjs` + `pack_png.py` (captures `benchmarks/charts/*.png` from the reports; list and captions in `charts.tsv`), `readme_blocks.py` and `publish-results.sh` (copy the regenerated README blocks and charts to `master`, committed by github-actions[bot]) |
| `README.md`, `README.crates.md` | repository page and crates.io page (`readme` in `Cargo.toml`); both are doctested through `src/lib.rs` (`derive/README.md`, the crates.io page of `dir-24-8-derive`, through `derive/src/lib.rs`), keep their shared examples in sync; `make doc-crates` checks the crates.io pages and builds the docs.rs documentation. Generated blocks of `README.md`, edit their generator, not the block: `bench-compare-lib:summary` (`make bench-compare-lib`, `libcmp/readme.rs`, `BENCH_README`), `bench-compare:details` (`make bench-compare`, `readme.rs`, `README`), `benchmark-charts` (`site/charts.sh`, `site/charts.tsv`) |

## Validation commands

```bash
make fmt-check lint        # formatting + clippy (all targets, with/without features)
make test                  # library tests with and without `serde`, comparison engine tests
make test-scalar test-simd # scalar batches only / batch-simd (CPU kernel + every SIMD kernel vs scalar)
make check-aarch64         # aarch64 type-check, scalar and NEON (needs the rustup target)
make examples doc          # examples assert their results; rustdoc with -D warnings
make doc-crates            # crates.io + docs.rs pages: docs.rs-like rustdoc (nightly), README examples and links, packages
make bench                 # Criterion (FILTER, BENCHES, SIMD=0, BENCH_TIME)
make bench BENCHES=snapshot  # snapshot size/time/memory (SNAPSHOT_SIZES, SNAPSHOT_PROFILES, SNAPSHOT_REPS)
make bench-compare         # comparison + reports in benchmarks/reports/ (SIZES, THREADS, ENGINES, ...)
make bench-compare REPORT_ONLY=1   # regenerate the reports from results.json
make bench-compare-lib     # lookups vs Go/C/Rust libraries -> benchmarks/reports/bench-compare-lib.html (BENCH_PROFILE=smoke|standard|full, BENCH_* variables)
make ci                    # what CI runs
make flamegraph            # perf profile of every lookup method -> benchmarks/charts/lookup-flamegraph.svg
make release VERSION=X.Y.Z # GitHub release vX.Y.Z (checks, make ci, tag, gh release; DRY_RUN=1 to rehearse)
make publish VERSION=X.Y.Z # crates.io: dir-24-8-derive then dir-24-8 (checks, make ci doc-crates; DRY_RUN=1 to rehearse)
make coverage              # cargo-llvm-cov report of the library (target/llvm-cov/html)
```

The Makefile has three benchmark targets (`bench`, `bench-compare`,
`bench-compare-lib`); add options as variables, not targets. Every `bench-compare` option is
listed in the Makefile header and in `bench-compare --help`.

Tests without `batch-simd` exercise the scalar batch kernel; with it
(`--all-features`), the integration tests use the kernel detected on the
CPU and the unit tests of `lookup::dispatch` compare every SIMD kernel the
CPU supports (AVX2 too on an AVX-512 machine) with the scalar one; kernels
the CPU lacks are reported as not executed on stderr. Tests build many
64 MiB roots: keep `RUST_TEST_THREADS` moderate (the Makefile uses 8).

## Snapshot format

The format specification lives in the `src/snapshot/mod.rs` module
documentation and is versioned (`FORMAT_VERSION` in `format.rs`). Any change
of the body layout, of a field's meaning, of the payload encoding or of the
codec settings readers depend on bumps the version and updates the
specification; the 16-byte header layout never changes. Loading must keep
validating every count, length and CIDR before use, never size an
allocation from a declared value, and verify the end of the XZ stream
before returning a table. The `snapshot` feature links liblzma (C), so the
aarch64 cross check (`make check-aarch64`) leaves it out.

## Table invariants (must hold after every public mutation)

1. Every cell holds the longest route covering all its addresses (or `0`).
2. `tbl24` has exactly 2^24 cells; the group pool always contains the
   reserved group 0; group indices stay below 2^23; cells only reference
   allocated groups and live route slots.
3. No group is uniform: a group whose 256 cells are equal terminal cells is
   folded into its parent cell and released (IPv6 folds bottom-up).
4. IPv4 groups contain only terminal cells; IPv6 level-13 groups contain
   only terminal cells.
5. The prefix index maps exactly the live routes, per-length counts match.
6. A failed mutation (capacity, allocation) leaves the table unchanged:
   limit checks, then reservations, then writes.

Changing the maintenance code requires running the oracle and property
tests with and without `batch-simd` (`make test`).

## Unsafe code

`unsafe` is confined to:

* `simd/avx2.rs`, `simd/avx512.rs`, `simd/neon.rs` (feature `batch-simd`):
  intrinsics with memory operands (loads, stores, gathers). Each block has a `// SAFETY:` comment.
  Gather indices are either 24-bit root indices or clamped with
  `vpminud`/`vminq_u32` to `tbl8.len() - 1`.
* `lookup/dispatch.rs`: calls of `#[target_feature]` kernels; sound because
  `Kernel` SIMD variants are only constructed after runtime detection
  (`Kernel::detect`).
* `simd/tables.rs`: unchecked root read (index masked to 24 bits) and the
  `Tables::new` constructor contract.
* `table/buffer.rs`: owned raw allocations (`mmap`/`munmap`/`madvise`, or a
  leaked `Vec`), `Send`/`Sync` impls.
* `lookup/result.rs`: `&mut [MatchId]` -> `&mut [u32]` (`repr(transparent)`).
* `simd/mod.rs`: prefetch hints (`prefetcht0`, `prfm`), which cannot fault.
* `snapshot/xz.rs`: two liblzma FFI calls the `liblzma` crate does not
  wrap (`lzma_lzma_preset`, `lzma_raw_encoder_memusage`), used to read
  preset dictionary sizes and compute encoder memory.

Never treat `Option<LookupMatch>` buffers as integer arrays: kernels write
raw cells into dedicated `u32` buffers and sinks convert them.

## Performance rules

* Justify every hot-path change with a measurement (A/B, several rounds:
  the development machine is noisy). Record kept and rejected variants in
  `docs/performance.md`.
* Lookups never allocate, never clone `T`, never lock (`tests/no_alloc.rs`).
* Prepare data and buffers outside timed sections, validate results before
  measuring, use `std::hint::black_box` on inputs and outputs.
* Do not derive latency percentiles from averaged batch timings: latency
  samples time one isolated call (or one whole batch call); amortized costs
  are reported as means.
* Do not run anything heavy while `bench-compare` runs: the structures are
  measured one after the other precisely so that they get the same machine.
* A cross-compiled or emulated kernel is not a performance measurement.
* `bench-compare` measures dir-24-8 twice: `dir-24-8` (built with
  `batch-simd`) and `dir-24-8[scalar]` (the `bench-compare-scalar` binary,
  built without it); the Makefile builds both.
* Keep the comparison fair: same routes, payloads, queries, order and batch
  sizes for every engine; monomorphized adapters, no dynamic dispatch.
* `bench-compare-lib`: every runner implements the same plan protocol and
  measurement steps (`benchmarks/compare-lib/README.md`); a change of the
  methodology goes into the three runners (Rust, Go, C) at once. Lookup
  loops stay in the library's own adapter (direct calls), the two
  cidranger adapters keep the same logic, and all Go libraries share one
  module, toolchain and set of flags.
