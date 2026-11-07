# Benchmarks

Three Makefile targets, every option is a variable:

| Command | What it does |
|---|---|
| `make bench` | Criterion benchmarks of the library (`benches/lookup.rs`, `benches/update.rs`); `BENCHES=snapshot` runs the snapshot measurements (`benches/snapshot.rs`) |
| `make bench-compare` | comparison with other structures, JSON/CSV results and a standalone HTML report in `benchmarks/reports/` |
| `make bench-compare-lib` | IPv4/IPv6 lookups (membership, LPM) of dir-24-8 against seven Go, C and Rust libraries, each in a native runner: throughput, latency, memory per prefix ([compare-lib/README.md](compare-lib/README.md)) |

```bash
make bench                                   # every Criterion group
make bench FILTER='^batch/ipv4' BENCH_TIME=0.5
make bench BENCHES=update                    # construction, insert/remove, merge, footprints
make bench SIMD=0                            # built without batch-simd: scalar batches (default: the CPU's SIMD kernel)
make bench BENCHES=snapshot                  # snapshots: size, save/load time, memory (SNAPSHOT_SIZES, SNAPSHOT_PROFILES, SNAPSHOT_REPS)

make bench-compare                           # full matrix (see below)
make bench-compare SIZES=1k,100k FAMILIES=ipv4 THREADS=1,8
make bench-compare ENGINES=all               # every registered structure
make bench-compare ENGINES=default,patricia  # default selection + one more
make bench-compare HIT_RATIO=90% BATCH=64,256 WINDOW_MS=20 REPS=7
make bench-compare BATCH_SWEEP=8,64,512      # batch sizes of the SIMD-versus-scalar chart (none = off)
make bench-compare REPORT_ONLY=1             # rebuild report.html/results.csv from results.json
```

`bench-compare --help` (binary `target/release/bench-compare`) lists every
option; each one also has a `BENCH_*` environment variable.

Both suites share the data generator `benches/support/data.rs`: BGP-like
route tables, an independent LPM oracle and the query distributions below.

## Snapshots (`benches/snapshot.rs`)

A custom harness (compression runs take seconds): for every family, size
(`SNAPSHOT_SIZES`, default `10k,100k,1M`) and XZ profile
(`SNAPSHOT_PROFILES`, default `9e,9e+sorted,6`; tokens `<level>`,
`<level>e`, `sorted`, `t<threads>`, `b<MiB>`), it builds a table from the
shared generator (payload `Option<u32>`), saves it to a `Vec<u8>` and loads
it back `SNAPSHOT_REPS` times (default 3, medians reported), and checks the
restored table (routes and 20 000 lookups) before printing a Markdown row:
uncompressed and compressed bytes, bytes per route, ratio, save and load
times, the time to build the same table by insertion, the encoder memory
computed by liblzma, and resident memory on Linux, separating the table
(`table`) from what each operation adds on top of it (`save +peak`,
`load +peak`, compared with `build +peak` for an ordinary construction).

## `bench-compare`

### Matrix

| Dimension | Default | Variable |
|---|---|---|
| Address family | IPv4, IPv6 (separate results) | `FAMILIES` |
| Entries (distinct prefixes stored) | 1k, 10k, 50k, 100k, 250k, 500k, 1M, 2M, 5M (larger sizes on request, e.g. `SIZES=10M`) | `SIZES` |
| Operation | lookup, batch (array), exists | `OPS` |
| Distribution | random (50 % hits), only existing, only absent | `DISTS`, `HIT_RATIO` |
| Threads | 1, 4, 8 | `THREADS` |
| Batch size | 256 addresses per call | `BATCH` |
| Batch-size sweep | 1, 4, 8, 16, 32, 64, 128, 512, 1024 addresses per call for native batch APIs (dir-24-8), first thread count only | `BATCH_SWEEP` |
| Queries | 1 048 576 per distribution (distinct from the entries, larger than the caches) | `QUERIES` |
| Validation | first 131 072 queries per distribution and operation (0 = all) | `VALIDATE` |

### Structures

The default selection (13 entries) covers this crate with SIMD and with
scalar batches, the classic
algorithms (Poptrie, Tree Bitmap, radix tree, LC-trie, sorted ranges,
hash per prefix length) and the most used Rust crates for IP or prefix
lookups. The first 8 get one solid color each, the following ones the same
hues dashed with square markers; `ENGINES=all` adds the remaining entries.
Every structure based on a radix tree or trie says so in its name
(`radix-tree(...)`, `radix-trie(...)`, `radix_trie(...)`, `iptrie-radix(...)`).

| Name | Default | Algorithm | Source |
|---|---|---|---|
| `dir-24-8` | yes | DIR-24-8 (IPv4) + 24/8 multi-level trie (IPv6), built with `batch-simd`: batches through the SIMD kernel detected on the CPU (AVX-512, AVX2, NEON), single lookups scalar | this repository |
| `dir-24-8[scalar]` | yes | the same code built without `batch-simd` (scalar batch kernel), measured by `bench-compare-scalar`, which `make bench-compare` builds next to `bench-compare` (`BENCH_SCALAR_EXE` overrides its path; missing, the entry is reported as not applicable) | this repository |
| `poptrie(s=18,k=6)` | yes | Poptrie (Asai & Ohara), direct pointing on 18 bits | own implementation of the paper |
| `ip_network_table(0.2)` | yes | Tree Bitmap (`IpNetworkTable`) | crate `ip_network_table` (~8 M downloads) |
| `prefix-trie(0.10)` | yes | Tree Bitmap style multibit trie (`PrefixMap`) | crate `prefix-trie` |
| `sorted-intervals` | yes | disjoint LPM segments + binary search | own implementation |
| `hashmap-lpm(fxhash)` | yes | one hash probe per present prefix length | own implementation |
| `roaring(0.12)` | yes | `RoaringBitmap` of covered IPv4 addresses (exists only) | crate `roaring` |
| `radix-tree(patricia)` | yes | radix tree: path-compressed binary radix trie (PATRICIA, as in BSD routing tables) | own implementation |
| `radix_trie(0.3)` | yes | generic radix trie (16-way nibble nodes, path compression), prefixes stored as bit strings | crate `radix_trie` (~100 M downloads) |
| `iptrie-lctrie(0.11)` | yes | LC-trie (level-compressed trie, the Linux kernel IPv4 FIB algorithm) | crate `iptrie` |
| `rangemap(1.8)` | yes | `RangeInclusiveMap` (BTreeMap of disjoint address ranges carrying the LPM winner) | crate `rangemap` (~48 M downloads) |
| `patricia_tree(0.10)` | yes | generic PATRICIA radix tree over bit strings | crate `patricia_tree` (~2 M downloads) |
| `iptrie-radix(0.11)` | no | radix tree for IP prefixes (`RTrieMap`, uncompressed form of the LC-trie above) | crate `iptrie` |
| `treebitmap-crate(0.5)` | no | `IpLookupTable`, the Tree Bitmap inside `ip_network_table` | crate `ip_network_table-deps-treebitmap` |
| `radix-trie(stride=4,16-way)`, `radix-trie(stride=8,256-way)` | no | multibit radix tries with controlled prefix expansion | own implementations |
| `radix-trie(binary)`, `head(2^k)+radix-trie(binary)` | no | binary radix trie, alone or below a 2^k direct table | own implementations |
| `tree-bitmap(stride=4)`, `poptrie(s=16,k=6)`, `hashmap-lpm(std)`, `bitmap-dense(2^32-bit)` | no | see the report's structure table | own implementations |

Generic crates (`radix_trie`, `patricia_tree`) work on byte strings: a
prefix is stored as one byte per bit and a lookup converts the address into
its 32 or 128-byte bit string on the stack (part of the measured cost, as it
would be for a user of these crates). None of the added crates reports its
own memory; the harness measures every structure with its counting allocator.

Adaptations, listed in the report: structures without a batch API are
measured as a loop of single lookups; without an existence API as
`lookup(ip).is_some()`; membership-only structures (`roaring`,
`bitmap-dense`) answer only `exists`, for IPv4 only. Every LPM structure returns the same `Option<LookupMatch<'_, u32>>`
(prefix + payload); competitors map an address to a route index and read the
shared entry table, which is counted in their memory footprint.

### Data

* Routes: distinct normalized BGP-like prefixes (IPv4 /8../32, mostly /24, in
  1.0.0.0-223.255.255.255; IPv6 /16../128, mostly /48 and /32, in 2000::/3),
  one route in four without payload. Fixed seed (`SEED`).
* `hits`: a random route, a random address inside it.
* `misses`: addresses covered by no prefix (verified with the oracle): half
  next to a route (inside its prefix widened by 8 bits), half uniform over
  the family space (IPv6: 2000::/3).
* `random`: hits and misses shuffled, `HIT_RATIO` hits.
* Expected results come from the oracle (one hash map per prefix length).

A dataset is generated once per (family, size) in its own process and cached
in `target/bench-compare/data/`, keyed by the generator version and every
parameter; later runs reuse it (`make clean` removes it).

### Measurement

Each (size, family, structure) runs in its own process, one after the other:

1. load the dataset (inputs: routes, entry table, queries, expected results);
2. build the structure once (timed), with memory probes around it;
3. validate the first `VALIDATE` queries of every selected distribution
   through every measured operation and batch size against the expected
   results (every query's expected result was computed by the oracle when the
   dataset was generated); a mismatch marks the structure `invalid` and
   nothing is measured;
4. for each operation x distribution: calibrate the cost of one unit during
   `WARMUP_MS`, then for each thread count spawn the workers (pinned to
   distinct physical cores first, buffers allocated before timing), run one
   discarded repetition and `REPS` repetitions of ~`WINDOW_MS` each with a
   synchronized start, then one latency pass.

* **Throughput**: all addresses / (last worker end - first worker start),
  median over the repetitions with the slowest and fastest. Each worker
  processes the same amount of work from its own offset of the query set,
  with a cursor that keeps advancing across repetitions and into the latency
  pass: no repetition replays addresses the worker has just looked up.
* **Latency (single address)**: `LAT_SAMPLES` samples per configuration,
  each one timing one isolated call between fenced timestamps; the cost of an
  empty timed section is subtracted. The clock is the core cycle counter
  (`rdpmc` on a `perf_event` user-mode counter, 1-cycle resolution) converted
  with the frequency measured over the same pass, or the TSC when perf events
  are not permitted (`/proc/sys/kernel/perf_event_paranoid` > 2). The TSC of
  AMD Zen CPUs advances in 10 ns steps, too coarse for single lookups.
* **Latency (batch)**: `LAT_BATCHES` samples, each one a whole batch call.
* Slow structures get fewer latency samples so that one pass lasts about
  `LAT_MAX_MS` (100 ms), never below 2 000 single-address samples or 200
  batch calls; the count of every configuration is in the report.
  The amortized cost per address (wall time x threads / addresses) is an
  average and is never presented as a percentile.
* Percentiles: nearest rank over all samples of a configuration (min, p50,
  p90, p95, p99, max, plus the mean).
* Results and outputs go through `std::hint::black_box`.
* Query sets are larger than the caches: with 65 536 queries the working set
  of a 100k-entry table (about 12 MB) sat at the size of the shared 16 MB L3,
  and the same configuration varied by up to 5x between processes depending
  on the background load; with 2^20 queries random lookups measure the memory
  system and the spread dropped to about 2x on this (busy) machine.

### Memory

Measured in the process that builds the structure:

| Value | Definition |
|---|---|
| structure heap | live bytes of the counting global allocator after the build minus before it (fixed allocations included) |
| mapped | tables mapped outside the allocator (dir-24-8 `mmap` roots and groups, from its stats) |
| entry table | prefix + payload array a competitor needs to return a full result (dir-24-8 stores them inside) |
| **footprint** | structure heap + mapped + entry table; **bytes per entry** = footprint / entries stored |
| RSS growth | `VmRSS` after the build minus before it |
| build peak | allocator peak during the build (+ mapped) and `VmHWM` growth (reset with `/proc/self/clear_refs` before the build) |
| lookup peak | `VmHWM` growth over all lookup phases (worker stacks, buffers, page faults) and heap growth |
| inputs, buffers | route list, entry table, queries, expected results; per-worker output and sample buffers (excluded from the footprint) |

### Limits, duration and statuses

* `MEM_LIMIT_MIB` (default: none, every size is attempted). With a limit
  (`MEM_LIMIT_MIB=<MiB>`, or `auto` = 70 % of `MemAvailable` at the start)
  each measuring process runs with `RLIMIT_DATA` = limit + its inputs, so an
  oversized structure fails to allocate instead of swapping (`memory limit`),
  and larger sizes are skipped when the extrapolation of the two previous
  sizes exceeds the limit. Without a limit, a structure larger than the free
  memory makes the machine swap: its measurements slow down and are reported
  as measured; a process killed by the kernel (out of memory) is `failed`.
  Sizes whose extrapolated build time exceeds `TIMEOUT` are skipped.
* `TIMEOUT` (600 s per process), `BUDGET` (3600 s for the whole run): runs
  beyond them are reported as `timeout` / `time budget`.
* Statuses: `ok`, `not applicable` (family or CPU feature), `memory limit`,
  `timeout`, `time budget`, `invalid`, `failed`. A configuration without a
  measurement is never drawn as zero: the charts leave a gap, the tables show
  the status and reason.
* Duration: the binary is compiled once (`cargo build`, then it re-executes
  itself); datasets are generated once and cached; each structure is built
  once per size and reused for all operations, distributions and thread
  counts. Progress lines show every run with its elapsed time.

### Outputs (`benchmarks/reports/`)

* `results.json`: everything (environment, settings, catalog, datasets,
  every run and measurement); rewritten after each run, so an interrupted run
  keeps its partial results. Input of `REPORT_ONLY=1`.
* `results.csv`: one row per measurement (and one per run without
  measurements, with its status).
* `report.html`: standalone page (dark theme, no external resource) with
  filters (family, operation, distribution, entries, threads, batch size,
  axis scales, structures), candlestick percentile ranks (body p50-p95, left
  wick min-p50, right wick p95-p99 with a p99 marker, ranked by p99), the
  throughput ranking, line charts over the measured sizes (throughput, p50/p95/
  p99, build time, memory, bytes per entry), throughput versus threads with
  the speedup over one thread, batch throughput versus batch size (every
  `BATCH` and `BATCH_SWEEP` size) with the SIMD / scalar ratio of
  dir-24-8 per size, sortable tables (results, memory, not
  executed, datasets), methodology, environment and settings.

Not measured on purpose: emulated or cross-compiled targets, and SIMD
kernels other than the one detected on the CPU (the library has no
runtime kernel selection; the unit tests compare every supported kernel
with the scalar one).
