# `make bench-compare-lib`: IPv4 / IPv6 lookups across libraries

Compares the **lookups of IP addresses** of dir-24-8 with seven libraries,
each running in a native runner of its own language, and writes a
standalone dark HTML report plus JSON and CSV results:

```text
benchmarks/reports/bench-compare-lib.html   report (offline, interactive)
benchmarks/reports/bench-compare-lib.json   every measurement and repetition
benchmarks/reports/bench-compare-lib.csv    one row per measurement
```

Measured, for IPv4 and IPv6 separately (throughput with 1, 4 and 8 worker
threads, latency and memory on one thread):

* **throughput** (lookups/s, amortized ns per lookup), with 1 thread on
  every scenario and with 1, 4 and 8 threads on the mixed scenario
  (multithreaded scaling);
* **latency** of individually timed lookups (p1 to p99, p99.9 with at
  least 100 000 samples);
* **memory** of the structure versus the number of input prefixes (resident
  memory added by the structure, bytes per prefix, peak RSS) and the build
  (or open) time.

## Running it

```bash
make bench-compare-lib                          # standard profile
make bench-compare-lib BENCH_PROFILE=smoke      # 1k and 10k prefixes, every library, under a minute
make bench-compare-lib BENCH_PROFILE=full       # 1k .. 10M prefixes, two detail sizes
make bench-compare-lib BENCH_SIZES=1000,10000,100000,1000000 BENCH_SEED=42 BENCH_REPETITIONS=5   # more repetitions
make bench-compare-lib BENCH_LIBS=dir-24-8,bart,libmaxminddb
make bench-compare-lib BENCH_REPORT_ONLY=1      # rebuild HTML and CSV (and the README summary) from the JSON
make bench-compare-lib BENCH_README=0           # measure without touching README.md
```

Requirements: Linux (`/proc` is read for memory), `go` 1.21 or later on
`PATH` (it fetches the pinned `go1.27.3` toolchain and the modules through
the Go proxy, verified by `go.sum`), `cc`, `curl`, `tar`, `sha256sum`.
Dependencies, corpora, MMDB files and raw runner outputs are cached under
`target/bench-compare-lib/`.

| Variable | Default (standard) | Meaning |
|---|---|---|
| `BENCH_PROFILE` | `standard` | `smoke`, `standard` or `full` |
| `BENCH_SIZES` | `1k,10k,100k,1M` | input prefixes per table (full: `1k,10k,50k,100k,250k,500k,1M,5M,10M`) |
| `BENCH_DETAIL_SIZES` | `100k` | sizes measured on every scenario (others: mixed only) |
| `BENCH_FAMILIES` | `ipv4,ipv6` | |
| `BENCH_SEED` | `42` | corpus seed |
| `BENCH_REPETITIONS` | `2` | independent processes per (library, family, size) |
| `BENCH_QUERIES` | `1048576` | queries per scenario (power of two) |
| `BENCH_TARGET_MS` | `40` | duration of one throughput measurement |
| `BENCH_LATENCY_SAMPLES` | `50000` | individually timed lookups per latency measurement (p99.9 needs 100 000) |
| `BENCH_LIBS` | all | subset of library ids |
| `BENCH_BUDGET` | `900` | measurement budget in seconds (0: none) |
| `BENCH_TIMEOUT` | `300` | limit per runner process |
| `BENCH_MEM_LIMIT_MIB` | 80 % of `MemAvailable` | resident memory limit per runner process |
| `BENCH_GOAMD64` | `v1` | `GOAMD64` of the Go build (the same for every Go library) |
| `BENCH_OUT`, `BENCH_CACHE` | `benchmarks/reports`, `target/bench-compare-lib` | output and cache directories |
| `BENCH_THREADS` | `1,4,8` | worker threads of the throughput measurements (mixed scenario; 1 is always included; smoke: `1,4`) |
| `BENCH_README` | `auto` | regenerate the "Performance Summary" block of the repository `README.md` (between `<!-- bench-compare-lib:summary:start -->` and `<!-- bench-compare-lib:summary:end -->`): `auto` for complete runs of any profile but `smoke`, `1` always, `0` never; also applies to `BENCH_REPORT_ONLY=1` |

Profiles (none is a cartesian product of every dimension):

| Profile | Sizes | Detail size (all scenarios) | Repetitions | Queries | Latency samples |
|---|---|---|---|---|---|
| smoke | 1k, 10k | 10k | 1 | 16 384 | 4 096 |
| standard | 1k, 10k, 100k, 1M | 100k | 2 | 1 048 576 | 50 000 (about 13 min here with 5 repetitions) |
| full | 1k ... 10M (9 sizes) | 100k, 1M | 2 | 1 048 576 | 200 000 |

The size curves use the mixed scenario; hits, misses and locality are
detailed at the detail sizes.

## Libraries

| Id in the report | Language | Repository | Version | Membership | LPM lookup |
|---|---|---|---|---|---|
| `dir-24-8` (reference), shown as `dir-24-8 (AVX-512)` here | Rust | this repository | workspace | `exist_ipv4/6` | `lookup_ipv4/6` (payload) |
| `go-cidranger-libp2p` | Go | [libp2p/go-cidranger](https://github.com/libp2p/go-cidranger) | v1.1.0 | `Contains` | **adapted LPM**: `ContainingNetworks` + longest prefix |
| `cidranger-yl2chen` | Go | [yl2chen/cidranger](https://github.com/yl2chen/cidranger) | v1.0.2 | `Contains` | **adapted LPM**: `ContainingNetworks` + longest prefix |
| `go-iptrie` | Go | [phemmer/go-iptrie](https://github.com/phemmer/go-iptrie) | v0.0.0-20240326174613-ba542f5282c9 | `Contains` | `Find` |
| `bart` | Go | [gaissmai/bart](https://github.com/gaissmai/bart) | v0.30.0 | `Table[uint32].Contains` | `Table[uint32].Lookup` |
| `prefix-trie` | Rust | [tiborschneider/prefix-trie](https://github.com/tiborschneider/prefix-trie) | 0.10 (`Cargo.lock`) | adapted: `get_lpm(host).is_some()` | `get_lpm(host /32 or /128)` |
| `iprange` | Rust | [sticnarf/iprange-rs](https://github.com/sticnarf/iprange-rs) | 0.6 (`Cargo.lock`) | `contains` | N/A: set without associated values |
| `libmaxminddb` | C | [maxmind/libmaxminddb](https://github.com/maxmind/libmaxminddb) | 1.14.1 (SHA-256 pinned) | `MMDB_lookup_sockaddr` + `found_entry` | + `MMDB_aget_value` (uint32 record id) |

* **dir-24-8** runs its default configuration. Its single lookups and
  `exist` always use the scalar kernels (the report labels it
  `dir-24-8 (scalar)`); its SIMD kernels (feature `batch-simd`) only serve
  batch lookups, which this comparison does not measure. Results below
  that list a `dir-24-8 (AVX-512)` series predate this: they measured the
  former SIMD single path, which the scalar one beat and replaced.
* **The two cidrangers**: `libp2p/go-cidranger` is a fork of
  `yl2chen/cidranger` ("due to upstream inactivity"), the same
  path-compressed trie (`NewPCTrieRanger()`, used for both) with later
  changes. They are two modules, two adapters (`lib_cidranger_*.go`, same
  logic, distinct imports) and two series, not two independent algorithms.
  Neither has a longest-prefix API: the adapter calls
  `ContainingNetworks(ip)` and keeps the entry with the longest prefix,
  whatever the order of the returned slice; the allocation of that slice is
  part of the measured cost. The dependency step checks that `go.mod` has
  no `replace` directive and that both modules are compiled with different
  hashes; each runner reports the Go package of its ranger at run time.
* All Go libraries are compiled into **one** binary: one module
  (`go/go.mod`, `toolchain go1.27.3`), one toolchain, the same flags
  (`CGO_ENABLED=0`, `-trimpath`, `GOAMD64=v1`). `go version -m` of that
  binary (the modules really compiled, with their `go.sum` hashes) is in the
  report. Runtime: `GOGC` default (100), `GOMAXPROCS` default.
* `go-iptrie` stores IPv4 as IPv4-mapped IPv6 internally and stores values
  as `any`; `bart` is used as `Table[uint32]` (with values, the same table
  for membership and lookup); `iprange` merges the networks (`simplify`):
  its unit count (ranges kept) is reported next to the input prefixes.
* `libmaxminddb` reads an MMDB file written from the same corpus (next
  section).

## Corpora

Generated once in Rust by the shared generator (`benches/support/data.rs`,
also used by `make bench` and `make bench-compare`) and its independent
oracle (one hash map per prefix length), cached by (family, size, seed,
queries, corpus and generator versions) and reused only when the FNV-1a 64
hash of every file matches. Every runner reads the same files:

* distinct canonical CIDRs, BGP-like lengths (IPv4 /8 to /32, mostly /24;
  IPv6 /16 to /128, mostly /48 and /32), clustered so that networks nest,
  with host routes (/32, /128); no `/0` (default routes only in the edge
  cases); record id = index + 1 (unique, deterministic, non-zero);
* scenarios of `BENCH_QUERIES` addresses, same order for every runner:
  `mixed` (50 % hits, shuffled), `hits` (a random address inside a random
  prefix), `misses` (verified absent by the oracle: near misses next to a
  prefix and uniform addresses), `local` (90 % of the queries from a hot set
  of 1 024 mixed addresses, 10 % from the mixed stream); hit rates are
  measured from the expected results and stored;
* `q-validate.bin`: first and last address of up to 8 192 sampled
  prefixes, the addresses just before and after them, the first and last
  address of the family;
* IPv4 addresses are given to every library in their 4-byte form; IPv6
  queries stay in `2000::/3`, so no IPv4-mapped address reaches a table
  (`corpus::normalize` maps `::ffff:a.b.c.d` to IPv4 for textual inputs).

Edge corpora (validation only, linear-scan oracle): `nested` (nested
networks, boundaries, host routes, misses), `default` (the same with the
family's `/0`) and `duplicates` (two CIDRs inserted twice with different
ids: the last insertion must win).

File layout (little endian header, network-order addresses):

```text
header (32 bytes): magic[8] ("DLPCPF01" prefixes, "DLPCQF01" queries), u32 version = 1,
                   u32 family (4 | 6), u64 count, u32 record size, u32 0
prefixes.bin:      address (4 | 16 bytes), u8 length, 3 zero bytes, u32 record id
q-<scenario>.bin:  address (4 | 16 bytes), u32 expected record id (0: no match)
corpus.json:       metadata (hashes, hit rates, length distribution, nesting)
```

### MMDB file of libmaxminddb

`go/cmd/mmdbgen` writes it with MaxMind's official Go writer (`mmdbwriter`
v1.2.0), a preparation tool, not a competitor: prefixes inserted by
increasing length (the writer replaces everything inside an inserted
network, so the most specific network wins; equal CIDRs keep the last id),
one `uint32` per network (the record id), an IPv4 or IPv6 tree, record size
32, reserved networks allowed, IPv4 aliasing disabled. Its fabrication time
is reported separately from `MMDB_open` and from the lookups. The C runner
compares the returned ids with the oracle (never the returned netmask: the
tree may split networks into other blocks).

## Runners and protocol

| File | Responsibility |
|---|---|
| `benchmarks/compare/src/libcmp/` | Rust: orchestrator, corpora, dependency preparation, Rust runner, aggregation, report |
| `benchmarks/compare-lib/go/` | Go runner (`cmd/runner`, one adapter file per library), MMDB writer (`cmd/mmdbgen`) |
| `benchmarks/compare-lib/c/runner.c` | C runner (libmaxminddb) |
| `benchmarks/compare/assets/lib-report.*`, `kit.js`, `report.css` | report template, script and styles (kit and theme shared with `bench-compare`) |

Each runner process measures one library on one corpus. It reads a plan
(text, one directive per line):

```text
lib <id>                      corpus <dir>            mmdb <file>   (C only)
target_ms <ms>                warmup <lookups>        prewarm_ms <ms>     edge (validation only)
validate <scenario> <limit>   (0: every query)
throughput <op> <scenario> [threads]   (op: contains | lookup; threads: default 1)
latency <op> <scenario> <samples>
```

and prints JSON lines: `meta` (library, build settings, Go modules, clock
cost and resolution), `build` (build or open time, RSS before and after,
heap delta, units), `validation`, then `throughput` (exact lookups, wall
time, checksum and the oracle's), `latency` (raw nearest-rank percentiles),
`skip`, `memory` (after the measurements) and `end` (peak RSS). Lookup
loops run inside the runner, in its language, with direct calls; the
orchestrator only starts processes and reads their output (no per-lookup
process, IPC, JSON or cgo).

## Methodology

* **Processes**: one fresh process per (library, family, size, repetition),
  one library loaded. Repetitions run in rounds by increasing size; the
  library order rotates between rounds and sizes. Each process is stopped
  after `BENCH_TIMEOUT` (status `timeout`) or when its RSS exceeds the limit
  (`failed`); a size is skipped when the peak memory scaled from the
  previous size would not fit (`skipped`, with the estimate). Nothing is
  dropped silently: every planned measurement has an entry and a status
  (`ok`, `unsupported`, `skipped`, `failed`, `timeout`).
* **Outside timing**: corpus parsing, conversion to native types (Go
  `netip.Addr` or `net.IP` slices of one buffer, C `sockaddr_in` /
  `sockaddr_in6`, Rust addresses or host prefixes), table construction,
  MMDB open, buffer allocation, validation, warm-up (one pass over the
  queries of each measured scenario per operation, stopped after
  `prewarm_ms`: 250 ms in the standard profile, enough for a full pass of
  every library faster than 4 M lookups/s), and before each throughput
  measurement an untimed calibration run.
* **Validation** before any timing, for membership and lookup: the
  boundary file whole in every repetition, and the first queries of each
  scenario (standard: 65 536 in the first repetition, 16 384 in the
  others; smoke: all). A difference invalidates that operation (`failed`, measurements
  skipped). During the timed loops a checksum (sum of ids, or number of
  hits) is compared with the oracle's for the same queries; lookup errors
  (Go `error`, MMDB error codes) are counted apart from misses.
* **Throughput**: an untimed calibration run (`warmup` lookups) sizes the
  timed loop to about `BENCH_TARGET_MS` (at least 16 384 lookups); the loop walks the scenario's
  queries in order (wrapping). Lookups per second = exact count / monotonic
  wall time. The report shows the median of the repetitions with the
  slowest and the fastest; libraries whose range overlaps the fastest are
  shown as tied, never as winners. Ratios name their reference (dir-24-8)
  and only compare the same family, operation, size and scenario.
* **Multithreaded throughput**: for each thread count, the workers share
  the library's index read-only in one process; they are created before
  the clock and released together (Rust: scoped threads and an atomic
  flag; Go: goroutines and a closed channel, `GOMAXPROCS` default; C:
  pthreads and a C11 atomic flag). Every thread count runs the same total
  number of lookups as the 1-thread measurement of that operation and
  scenario, split in contiguous ranges of the query array; throughput =
  total lookups / wall time from the release to the end of the last worker,
  and the checksum of all workers is compared with the oracle's. No CPU
  affinity; the report shows throughput and speed-up versus threads.
* **Latency**: a separate campaign times each lookup between two reads of
  the monotonic clock, the same clock in every runner (`CLOCK_MONOTONIC`
  through the vDSO: `runtime.nanotime` in Go, `Instant` in Rust,
  `clock_gettime` in C). The median cost of two back-to-back reads is
  subtracted; the resolution (smallest step) is reported; percentiles under
  three steps or under the clock cost are flagged as limited by the
  instrumentation. On AMD Zen the clock moves in 10 ns steps, so the
  fastest structures' medians are at the resolution. Percentiles are
  nearest-rank over one repetition's samples; the report shows their median
  over the repetitions. They are never derived from averaged loops.
* **Memory**: `VmRSS` before the build (queries and input prefixes
  already allocated), after the build and after the measurements, freed
  memory being returned first (Go GC + `FreeOSMemory`, Rust and C
  `malloc_trim`). Structure memory = RSS after the measurements - RSS
  before the build: allocator and GC overhead included, and for
  memory-mapped data (dir-24-8 roots, MMDB files) the pages touched by the
  measured queries. Also reported: the allocator
  estimate (Go heap delta, Rust counting allocator, MMDB file size), the
  peak RSS of the process, bytes per input prefix and the library's own
  unit count when it exposes one. RSS is page-granular: values at 1k
  prefixes are coarse.
* **Runtimes**: Go GC at its default (`runtime.GC()` runs between
  measurements, outside timed sections); Rust release profile
  (`codegen-units = 1`, thin LTO, no `target-cpu`); C `-O2 -pthread`. No
  CPU affinity.

## Results of the standard profile on this machine

`make bench-compare-lib` (standard: 360 runner processes, 820 measurements
plus 44 not applicable, status complete, 801 s of measurement), AMD Ryzen 7
PRO 7840U (8 cores / 16 threads, Zen 4, AVX-512), 27 GB RAM, Ubuntu 26.04,
Linux 7.0, Rust 1.98.1, Go 1.27.3, gcc 15.2. The machine was in use (load
average 3.7 at the start, 12.0 at the end): the slowest and fastest
repetitions of a measurement can differ by 2x, and the report marks
overlapping ranges as ties. 1M input prefixes, mixed scenario (50 % hits),
medians of 5 repetitions.

One thread (throughput, individually timed latency, memory), sorted by
membership throughput:

| IPv4 | Membership (M/s) | LPM lookup (M/s) | LPM p50 / p99 (ns) | Structure RAM | B / prefix | Build (open) |
|---|---:|---:|---:|---:|---:|---:|
| bart | 61.9 | 6.0 | 181 / 1 052 | 38.2 MiB | 40 | 366 ms |
| dir-24-8 (scalar) | 55.8 | 28.7 | 170 / 992 | 152.9 MiB | 160 | 760 ms |
| iprange | 46.3 | N/A | N/A | 13.7 MiB | 14 | 167 ms |
| dir-24-8 (AVX-512) | 29.6 | 16.1 | 150 / 731 | 152.8 MiB | 160 | 712 ms |
| prefix-trie | 12.5 | 7.3 | 50 / 661 | 20.8 MiB | 22 | 218 ms |
| go-iptrie | 11.0 | 0.8 | 1 263 / 6 302 | 143.4 MiB | 150 | 1 516 ms |
| go-cidranger-libp2p | 7.6 | 0.6 (adapted) | 2 134 / 6 733 | 260.5 MiB | 273 | 2 702 ms |
| cidranger-yl2chen | 5.7 | 0.3 (adapted) | 3 807 / 15 390 | 716.6 MiB | 751 | 9 876 ms |
| libmaxminddb | 2.8 | 2.3 | 271 / 1 784 | 26.5 MiB | 28 | 0.1 ms (MMDB written in 4.0 s) |

| IPv6 | Membership (M/s) | LPM lookup (M/s) | LPM p50 / p99 (ns) | Structure RAM | B / prefix | Build (open) |
|---|---:|---:|---:|---:|---:|---:|
| bart | 12.4 | 2.6 | 390 / 2 294 | 102.5 MiB | 108 | 530 ms |
| dir-24-8 (scalar) | 8.6 | 5.8 | 451 / 2 705 | 1 652.9 MiB | 1 733 | 4 812 ms |
| dir-24-8 (AVX-512) | 7.4 | 5.7 | 471 / 2 645 | 1 652.8 MiB | 1 733 | 4 810 ms |
| iprange | 4.9 | N/A | N/A | 76.5 MiB | 80 | 957 ms |
| prefix-trie | 3.3 | 2.7 | 331 / 1 713 | 54.3 MiB | 57 | 422 ms |
| go-iptrie | 3.3 | 0.7 | 1 213 / 5 411 | 152.6 MiB | 160 | 1 595 ms |
| libmaxminddb | 1.5 | 1.4 | 701 / 3 186 | 110.1 MiB | 115 | 0.1 ms (MMDB written in 6.9 s) |
| go-cidranger-libp2p | 1.3 | 0.5 (adapted) | 2 294 / 10 570 | 318.5 MiB | 334 | 3 864 ms |
| cidranger-yl2chen | 1.0 | 0.3 (adapted) | 3 627 / 14 728 | 775.0 MiB | 813 | 14 451 ms |

Multithreaded throughput (same total number of lookups for every thread
count, workers sharing one index), sorted by IPv4 throughput at 8 threads:

| Aggregate LPM lookup, M/s (membership for iprange) | IPv4 1T | 4T | 8T | 8T / 1T | IPv6 1T | 4T | 8T | 8T / 1T |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| iprange (membership) | 46.3 | 180.1 | 338.8 | 7.3x | 4.9 | 22.8 | 42.7 | 8.8x |
| dir-24-8 (scalar) | 28.7 | 82.6 | 149.9 | 5.2x | 5.8 | 20.6 | 32.9 | 5.6x |
| prefix-trie | 7.3 | 39.8 | 93.1 | 12.8x | 2.7 | 11.4 | 24.0 | 8.9x |
| dir-24-8 (AVX-512) | 16.1 | 62.6 | 91.6 | 5.7x | 5.7 | 20.6 | 30.6 | 5.4x |
| bart | 6.0 | 21.1 | 41.3 | 6.9x | 2.6 | 10.8 | 21.6 | 8.2x |
| libmaxminddb | 2.3 | 10.6 | 25.8 | 11.0x | 1.4 | 5.7 | 12.0 | 8.9x |
| go-iptrie | 0.8 | 4.0 | 7.7 | 9.3x | 0.7 | 3.5 | 6.8 | 9.7x |
| go-cidranger-libp2p | 0.6 | 2.2 | 4.2 | 7.4x | 0.5 | 1.8 | 3.2 | 7.0x |
| cidranger-yl2chen | 0.3 | 1.2 | 2.4 | 8.6x | 0.3 | 1.3 | 2.4 | 9.6x |

What this run shows (and only for this run):

* **dir-24-8, scalar versus AVX-512** (one thread, every size, membership
  and lookup): the scalar kernels give 1.78-2.4x the throughput of the
  default AVX-512 configuration for IPv4 and 1.02-1.17x for IPv6;
  individually timed, both have similar medians (150-170 ns IPv4,
  450-470 ns IPv6 at 1M): one lookup is dominated by its memory accesses,
  and the AVX-512 path gathers a single lane.
* **LPM lookup**: dir-24-8 has the highest throughput in both families at
  1M, with 1 and with 8 threads (scalar: 28.7 and 149.9 M/s IPv4, 5.8 and
  32.9 M/s IPv6), ahead of prefix-trie and bart. Individually timed,
  prefix-trie's IPv4 median (50 ns) is below dir-24-8's (150-170 ns): a
  single lookup pays the random access to the 64 MiB root, which the
  throughput loop overlaps.
* **Threads**: from 1 to 8 threads, dir-24-8 gains 5.2-5.7x (its accesses
  to large tables are limited by the memory system), the other structures
  7-13x. Gains above 8x (prefix-trie, libmaxminddb, go-iptrie) come from
  the larger total of private caches with 8 cores and, on this mobile CPU,
  a higher memory latency seen by a single active thread; the 8-thread
  runs last only a few milliseconds on a busy machine, so these ratios are
  approximate.
* **Membership**: bart's `Contains` is the fastest on one thread (61.9 M/s
  IPv4, 12.4 M/s IPv6), dir-24-8 (scalar) second; with 8 threads iprange's
  merged ranges reach 338.8 M/s (IPv4).
* **Memory**: dir-24-8 trades memory for speed (160 B per IPv4 prefix,
  1.7 KiB per IPv6 prefix at 1M, mostly its 64 MiB roots and IPv6 groups;
  the same for both backends); iprange, prefix-trie, libmaxminddb and bart
  stay at 14-115 B per prefix. The original cidranger needs 2.4-2.8x the
  memory of its libp2p fork and 3.6-3.7x its build time; both adapted LPM
  lookups allocate the list of containing networks.
* **libmaxminddb**: opening the memory-mapped file takes 0.1 ms; writing
  it (mmdbwriter, preparation) takes 4-7 s at 1M.

The report (`benchmarks/reports/bench-compare-lib.html`) has every size,
scenario and thread count, the latency distributions and the
per-repetition values.

## Reading the results

* Compare a column only within one family, operation, scenario and size;
  there is no global ranking across operations.
* "Adapted" operations (cidranger LPM, prefix-trie membership) measure the
  adapter built from the library's public API, its allocations included.
* Latencies at the clock resolution (prefix-trie and the smaller tables
  on this machine) only bound the median; throughput is the better
  comparison there.
* `dir-24-8 (scalar)` is the library's default configuration (scalar
  single lookups); `dir-24-8 (AVX-512)` rows come from runs made before
  single lookups became scalar-only.
* The machine's load is in the environment table; repeated runs on a busy
  machine can differ by several tens of percent.
