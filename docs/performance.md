# Performance notes and measurements

All numbers in this file were measured on one machine and only describe it:

* AMD Ryzen 7 PRO 7840U (Zen 4, 8 cores / 16 threads, 16 MiB L3, AVX2 and
  AVX-512 F/BW/VL/DQ/VBMI), laptop, `performance` governor, boost on;
* Linux 7.0, THP `madvise` mode; the machine had ~7 GiB free RAM and a
  heavily used swap during the measurements, so **huge pages were mostly
  unavailable** (see below) and run-to-run noise was around ±10-20 %;
* Rust 1.98.1, `--release` with `codegen-units = 1`, `lto = "thin"`, no
  `target-cpu` flag (runtime dispatch).

No AArch64 machine was available: the NEON batch kernel is compiled and
type-checked (`make check-aarch64`) but **not measured and not executed**
here. CI runs the test suite natively on an AArch64 runner.

## How to measure

* `make bench` - Criterion (`benches/lookup.rs`, `benches/update.rs`).
  Every key window is drawn from a pool of 2^20 pre-generated queries so that
  iterations do not reuse cache-hot keys; batch results are checked against
  the (scalar) single lookups before measuring. `make bench` measures the
  SIMD batch kernel (`batch-simd`), `make bench SIMD=0` the scalar one. `single/*` measures one call per
  iteration (independent back-to-back calls); `single-latency/*` makes each
  address depend on the previous result (latency chain). Batch throughputs
  are per address (amortized), the batch time is the Criterion time.
* `make bench-compare` - the comparison harness (`benchmarks/compare`):
  latency percentiles from isolated calls timed with the core cycle counter,
  throughput with 1/4/8 threads, memory and build time over 1k-5M entries;
  see `benchmarks/README.md`. Options are Makefile variables (`SIZES`,
  `THREADS`, `ENGINES`, ...); there are no other benchmark targets.
* For A/B decisions on this noisy laptop a min-of-7 harness was used
  (1 M lookups per repetition, 200 k IPv4 + 100 k IPv6 BGP-like routes,
  random hits), alternating the variants over several rounds.

Default fixture (`make bench`): 200 000 IPv4 routes (BGP-like mix, 10 515
groups, tbl8 16 MiB) and 100 000 IPv6 routes (181 029 groups, tbl8 272 MiB).

## Optimizations kept, with their measurements

All values are nanoseconds per address (lower is better) unless noted.

### 1. Slot prefetch pipeline in batch materialization

The kernels return raw cells; building `LookupMatch` reads one route slot
per result, a second random access. Prefetching the slots of chunk `i` and
materializing it only after the kernel ran on chunk `i + 1` overlaps those
reads with the next table walk.

| IPv4, batch 256, random hits | before | after |
|---|---|---|
| AVX-512 | 15.6 | 8.05 |
| AVX2 | 15.6 | 8.77 |
| scalar | 10.7 | 9.97 |

(identifiers only, without slot reads: 4-5 ns for every backend.)

### 2. IPv6 lock-step width

Lanes of all vectors of a block advance one level per step, so more vectors
mean more independent misses in flight. Three interleaved rounds:

| AVX-512 vectors per block | 2 (32 lanes) | 4 (64 lanes) | 8 (128 lanes) |
|---|---|---|---|
| random hits | 49.1 | **39.9** | 44.8 |
| deep (long prefixes) | 28.1 | **23.2** | 24.5 |

| AVX2 vectors per block | 2 (16 lanes) | 4 (32 lanes) |
|---|---|---|
| random hits | 70.0 | **46.3** |

Tails use single-vector blocks so short batches do not pay for 64 lanes.

### 3. Mixed batches: larger chunks, cross-family overlap

Mixed `&[IpAddr]` chunks went from 64 to 256 keys (each family gets several
full SIMD blocks), and both families' kernels run before materialization so
slot prefetches of one family overlap the other family's walk.

| mixed (1/3 IPv6), batch 256 | before | after |
|---|---|---|
| AVX-512 | 46.5 | 38.4 |
| AVX2 | 53.1 | 40.6 |
| scalar | 55.9 | 48.9 |

### 4. Single lookups: gathers versus scalar loads

Single lookups are latency chains. Three SIMD designs were measured
(Criterion, 2 s per benchmark, `Ipv6Addr`/`Ipv4Addr` keys):

| single lookup | one-lane gather per level | vector index + scalar loads | scalar |
|---|---|---|---|
| IPv6, random hits | 297 | 147 | 159 |
| IPv6, deep | 211 | 89 | 146 |
| IPv4, random hits | 59 | 61 | 52 |
| IPv4, cache-resident | 7.1-8.2 | 6.5-6.9 (compiles to scalar code) | 3.9 |

Chains of one-lane `vpgatherdd` do not overlap across back-to-back lookups
on Zen 4, so the IPv6 SIMD single path does not gather.

A second problem appeared in the comparison harness, whose keys are `u128`:
the first IPv6 design loaded the address from memory as one 16-byte vector
right after the caller had written it with two 8-byte scalar stores. The
store-to-load forwarding failure made even a root-only lookup (a guaranteed
miss) cost **15.9 ns instead of 3.4 ns**, and 66-91 ns instead of 22 ns for
hits on 10k routes. The single-lookup kernels now take the key **by value
(`u128` in registers)**: the SIMD path extracts the root index with
`vmovq` + `vpshufb` and walks the levels with register shifts and scalar
loads. Measured in the harness (`u128` keys, ns per lookup):

| IPv6 single | before | after | scalar |
|---|---|---|---|
| 10k routes, hits | 66.0 (AVX-512) / 58.4 (AVX2) | 22.5 / 22.5 | 22.1 |
| 100k routes, hits | 421.6 / 501.2 | 222.7 / 148.8 | 115.7 |
| 100k routes, misses | 15.9 / 16.4 | 6.3 / 8.1 | 5.8 |

(100k-route hits are DRAM-bound and very noisy on this machine.)

For IPv4, the "vector index" variant is turned into plain scalar code by
LLVM, so the SIMD IPv4 single path is a genuine one-lane masked gather. It
costs about 1.5x the scalar time on cache-resident tables (harness, 100k
routes: 28 ns vs 20 ns for hits, 3.7 vs 2.0 ns for misses).

**Consequence**: on this CPU the scalar kernels are the fastest for single
lookups (`bench-compare-lib` measured the same, 1.78-2.4x the throughput of
the AVX-512 single path). The SIMD single paths (one-lane gathers for
IPv4, `vpshufb` root index for IPv6, NEON index math) were **removed**:
single lookups and `exist` always run the scalar kernels, and the SIMD
kernels only serve batches, compiled with the `batch-simd` feature and
detected at table construction. The runtime `Backend` selection
(`Config::backend`, `single_lookup_backend`) was removed with them.

### 5. AVX-512 versus AVX2 for the `batch-simd` detection order

Register width alone does not decide: on Zen 4 512-bit operations are
executed as two 256-bit halves and gathers are microcoded. Measured:

| batch 256 | AVX-512 | AVX2 | scalar |
|---|---|---|---|
| IPv4 random hits | 8.05-8.65 | 8.6-8.8 | 10.0-10.8 |
| IPv6 random hits | 35-40 | 45-54 | 65-68 |
| mixed random hits | 38.4 | 40.6 | 48.9 |
| IPv4 cache-resident ("local") | 2.18 | 2.14 | 2.40 |
| IPv6 cache-resident | 5.0 | 5.7 | 8.3 |

AVX-512 wins or ties everywhere measured, so the `batch-simd` detection
prefers AVX-512, then AVX2. On Intel CPUs with the Gather Data Sampling
(Downfall) microcode mitigation, gathers are much slower: compare
`dir-24-8` and `dir-24-8[scalar]` in `make bench-compare` there, and build
without `batch-simd` if the scalar batches win.

### 6. Huge pages (opt-in)

Tables of 2 MiB and more are anonymous `mmap` mappings (lazily zeroed by the
kernel). With `Config::huge_pages(true)` they are 2 MiB aligned and advised
with `MADV_HUGEPAGE`. On this machine (THP `enabled=madvise`,
`defrag=madvise`, fragmented memory):

* only 4 MiB of a fully touched 64 MiB root actually received huge pages
  (`AnonHugePages` in `/proc/self/smaps_rollup`);
* batch lookups with and without the advice were within noise;
* building a 10 000-route IPv4 table took **84-137 ms with the advice versus
  4.7-6.3 ms without** (best of 5, three rounds): every first touch of a
  2 MiB region ran synchronous compaction.

The advice is therefore off by default. It remains available for
long-lived, lookup-heavy tables on hosts where huge pages are available
(where it removes most TLB misses of random lookups over the 64 MiB root and
large group buffers).

### Measured and not kept

* Scalar IPv6 lock-step block of 16, 32 or 64 lanes: differences within
  noise; 32 kept.
* Single-vector IPv6 x86 kernels (the DPDK-style 16x2 layout): replaced by
  the 4-vector blocks above.
* SIMD single lookups (one-lane gathers, vector root index): slower than
  the scalar walk or at parity (section 4); removed, single lookups are
  always scalar.

## Snapshot format and codec choices

Measured on tables of the shared generator (`benches/support/data.rs`)
compressed with liblzma `9 | PRESET_EXTREME` (scratch experiments, 200 000
and 1 000 000 routes; compressed bytes, lower is better).

| Choice | Kept | Alternatives measured |
|---|---|---|
| layout | columns: lengths, truncated addresses, payloads (200k routes, `Option<u32>` payloads: IPv4 3.50 B/route, IPv6 5.96) | one record per route: 5.24 / 7.85 B/route with truncated addresses, 4.18 / 7.78 with full addresses |
| length column | blocks of 65 536 routes (bounded 64 KiB loader buffer) | one global column: 1-4 % smaller at 1M routes, but the loader must buffer one byte per route; blocks of 1 024-16 384 routes: 1-10 % larger than one column at 200k |
| payloads | after all prefix blocks of the family | interleaved block by block: up to 24 % larger |
| addresses | truncated to `ceil(len / 8)` bytes | XOR with the previous address: 3-11 % larger, sorted or not |
| payload codec | CBOR (`ciborium`), self-describing | postcard 3.89 B/route on a small struct, MessagePack arrays 4.03, MessagePack maps 4.21, CBOR 4.22: postcard is about 8 % smaller but cannot decode `skip_serializing_if`, untagged enums, `flatten` or `serde_json::Value` |
| LZMA2 `pb` | preset (2) | `pb = 0`: +0.7 to +1.9 % on the kept layout |
| order | storage order by default, `EntryOrder::Sorted` on request | sorting shrinks the prefix columns about 2x, but payloads move with their routes: totals measured from -10 % to +2 % depending on the payloads (see the README) |

Kept, with an A/B measurement (3 alternated rounds, median of 5 loads of
1M IPv4 routes, level 1): decoding the payloads with
`ciborium::from_reader_with_buffer` and one reused 4 KiB scratch buffer
instead of `ciborium::from_reader`, which zeroes a new 4 KiB stack buffer
for every payload. Load: 864 / 831 / 806 ms before, 793 / 800 / 800 ms
after (about -4 %).

The dictionary is reduced to the uncompressed size when the snapshot is
smaller (as `xz` does for files): the output size is unchanged, while the
encoder of a 100k-route IPv4 snapshot needs 10.2 MiB instead of the
preset's 674 MiB.

## Measurement choices of `bench-compare`

Kept and rejected after measuring on this machine (AMD Ryzen 7 PRO 7840U,
background load average around 4):

* **Latency clock.** The TSC of this CPU advances in steps of 33 ticks
  (10 ns at 3.3 GHz): consecutive `rdtsc` reads differ by 0 or 33. With it,
  single lookups (3-40 ns) came out as 0, 10, 20 or 30 ns, and subtracting
  the 20-30 ns timer cost produced minima of 0 ns. Kept: the core cycle
  counter read with `rdpmc` from a per-thread `perf_event` (user-mode
  cycles, allowed with `perf_event_paranoid` <= 2), 1-cycle resolution, an
  empty timed section costs a stable 49 cycles; cycles are converted with
  the frequency measured over the same sampling pass (cycles / TSC time). The
  TSC remains the fallback when perf events are not permitted.
* **Coordinator waiting.** A coordinator spinning with `yield_now` while
  one worker ran made 1-thread throughput unstable (it could share the
  worker's CPU); it now parks and the last worker to finish unparks it.
* **Worker pinning.** Pinned (distinct physical cores first, CPU 0 last)
  and unpinned runs showed the same spread under background load; pinning is
  kept for repeatable core assignment with 4 and 8 threads (`PIN=0`
  disables it).
* **Query set size.** With 65 536 queries per distribution, repeating the
  same configuration (dir-24-8, 100k and 250k IPv4 entries, random, 1
  thread) gave 11-89 M lookups/s across processes: the queries' working set
  (~12 MB) competed for the 16 MB L3 with the background applications.
  With 2^20 queries (working set well above the L3) the same experiment gave
  15-43 M/s; kept as the default (`QUERIES`).
* **Replayed addresses.** The first version restarted every repetition at
  the worker's offset: a slow structure (few lookups per 10 ms window)
  replayed the same ~25 000 addresses six times and the latency pass reused
  them, so its working set stayed in cache (Tree Bitmap crate, 10M IPv4
  entries: p50 25 ns). Each worker now keeps a cursor that advances over the
  2^20 queries across repetitions and into the latency pass (same
  configuration: p50 525 ns, the cost of its memory accesses).
* **Single-thread memory latency.** Fenced latencies of memory-bound
  lookups are higher with one active thread than with four (Tree Bitmap,
  100k IPv4: p50 100 ns at 1 thread, 81 ns at 4): the mobile memory fabric
  clocks down at low load. This is reported as measured, not corrected.

## Safety mechanisms on the hot path

* Every secondary gather index is clamped with `vpminud`/`vminq_u32` to the
  last valid `tbl8` cell (one instruction per vector), and group 0 is
  reserved so the buffer is never empty: an active lane can never read
  outside the allocation, even with corrupted cells.
* Root indices are 24-bit by construction (`>> 8` or a 3-byte swap) and
  `tbl24` has exactly 2^24 cells.
* Scalar kernels use bounds-checked `tbl8` reads; only the root read uses
  an unchecked index justified by the 24-bit mask.
