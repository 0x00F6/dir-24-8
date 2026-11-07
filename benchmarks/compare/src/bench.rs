//! Measurement of one structure on one dataset (runs in its own process).
//!
//! Sequence: build (timed, memory probes around it), validation of every
//! measured operation against the oracle's expected results, then for each
//! operation x distribution x thread count: a calibrated warm-up, timed
//! throughput repetitions, and latency sampling. The structure is built once
//! and shared read-only by the workers.

use std::hint::black_box;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dir_24_8::LookupMatch;
use serde::{Deserialize, Serialize};

use crate::adapter::{Api, Engine, Resolved};
use crate::core::Key;
use crate::dataset::{Dataset, Dist, Family, NONE};
use crate::measure;
use crate::sys;
use crate::timer::{self, Stamp, Summary, Tsc};

/// Measured operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// One address per call, full result (prefix + payload).
    Lookup,
    /// An array of addresses per call, full results.
    Batch,
    /// One address per call, boolean "covered by a prefix".
    Exists,
}

impl Op {
    pub const ALL: [Op; 3] = [Op::Lookup, Op::Batch, Op::Exists];
    pub fn name(self) -> &'static str {
        match self {
            Op::Lookup => "lookup",
            Op::Batch => "batch",
            Op::Exists => "exists",
        }
    }
    pub fn parse(s: &str) -> Option<Op> {
        Self::ALL.into_iter().find(|o| o.name() == s)
    }
}

/// Settings of one measuring process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunCfg {
    pub family: Family,
    pub size: usize,
    pub engine: String,
    pub data: std::path::PathBuf,
    pub ops: Vec<Op>,
    pub dists: Vec<Dist>,
    pub threads: Vec<usize>,
    pub batch: Vec<usize>,
    /// Extra batch sizes measured for native batch APIs with the first
    /// thread count only.
    #[serde(default)]
    pub batch_sweep: Vec<usize>,
    pub warmup_ms: u64,
    pub window_ms: u64,
    pub reps: usize,
    pub lat_samples: usize,
    pub lat_batches: usize,
    /// Upper bound of one latency pass (ms, single-thread equivalent); the
    /// sample count is reduced for slow structures, never below
    /// `MIN_LAT_SAMPLES` / `MIN_LAT_BATCHES`.
    #[serde(default = "default_lat_max_ms")]
    pub lat_max_ms: u64,
    /// Queries per distribution validated before timing (0 = all).
    pub validate: usize,
    pub ticks_per_s: f64,
    pub mem_limit: usize,
    pub pin: bool,
}

fn default_lat_max_ms() -> u64 {
    100
}

/// Floors of the latency sample counts (p99 needs enough samples).
pub const MIN_LAT_SAMPLES: usize = 2_000;
pub const MIN_LAT_BATCHES: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Ok,
    /// The structure does not support this family / CPU.
    NotApplicable,
    /// Estimated or actual memory above the limit.
    MemoryLimit,
    /// The process exceeded its time limit.
    Timeout,
    /// The global time budget was exhausted before this run.
    Budget,
    /// Results disagree with the oracle (not measured).
    Invalid,
    /// The process failed for another reason.
    Failed,
}

/// Memory accounting of one structure (bytes). See the report's
/// methodology section for the definitions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Memory {
    /// Live heap bytes allocated by the build and still owned by the
    /// structure (counting global allocator).
    pub struct_heap: usize,
    /// Tables mapped outside the allocator (dir-24-8 `mmap` tables).
    pub struct_mapped: usize,
    /// Shared entry table needed to return prefix + payload (competitors).
    pub entry_table: usize,
    /// `struct_heap + struct_mapped + entry_table`.
    pub footprint: usize,
    pub bytes_per_entry: f64,
    /// The structure's own accounting (`None`: the structure has none).
    pub reported: Option<usize>,
    /// Resident set growth over the build (`VmRSS` after - before).
    pub rss_delta: Option<i64>,
    /// Peak heap above the pre-build level during the build, plus mapped
    /// tables.
    pub build_peak_heap: usize,
    /// Peak resident growth during the build (`VmHWM` reset before the
    /// build).
    pub build_peak_rss: Option<usize>,
    /// Inputs held outside the structure: route list, entry table, queries
    /// and expected results.
    pub inputs: usize,
    /// Per-worker output and latency-sample buffers.
    pub buffers: usize,
    /// Peak resident growth during all lookups (worker stacks, page faults,
    /// any allocation; `VmHWM` reset before the lookups).
    pub lookup_peak_rss: Option<usize>,
    /// Heap allocated during lookups above the pre-lookup level (thread
    /// bookkeeping; the engines themselves do not allocate).
    pub lookup_peak_heap: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Throughput {
    /// Addresses per second (aggregate over the workers): median, min, max
    /// over the repetitions.
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub reps: usize,
    /// Addresses processed per repetition (all workers).
    pub addrs_per_rep: u64,
    /// Mean wall time of a repetition (ns).
    pub rep_ns: f64,
}

/// One operation x distribution x threads (x batch size) measurement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measure {
    pub op: Op,
    pub dist: Dist,
    pub threads: usize,
    /// Addresses per call (`batch` only).
    pub batch: Option<usize>,
    pub throughput: Throughput,
    /// Mean time per address per worker: `wall x threads / addresses`
    /// (median repetition). For batches this is the amortized cost, not a
    /// latency distribution.
    pub amortized_ns: f64,
    /// `"op"`: one isolated call per sample; `"batch"`: one whole batch
    /// call per sample.
    pub latency_kind: String,
    pub latency: Summary,
    /// Latency clock: `"cycles"` (core cycle counter, converted with the
    /// measured frequency) or `"tsc"` (time-stamp counter).
    pub clock: String,
    /// Effective core frequency during the latency pass (cycle counter).
    pub clock_ghz: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub family: Family,
    pub size: usize,
    pub engine: String,
    pub status: Status,
    pub reason: Option<String>,
    pub entries: usize,
    /// Kernels used (dir-24-8 only), e.g. `batch: avx512, single: scalar`.
    #[serde(alias = "backend")]
    pub kernels: Option<String>,
    pub build_s: Option<f64>,
    pub load_s: Option<f64>,
    pub memory: Option<Memory>,
    pub measures: Vec<Measure>,
    /// Timer overhead subtracted from latency samples, ns `(min, median)`.
    pub timer_overhead_ns: Option<(f64, f64)>,
    /// Wall time of the measuring process.
    pub process_s: f64,
    /// 1-minute load average of the machine when the process started
    /// (background load is the main source of noise).
    #[serde(default)]
    pub load_avg: Option<f64>,
}

impl RunResult {
    pub fn status(cfg: &RunCfg, status: Status, reason: impl Into<String>) -> Self {
        RunResult {
            family: cfg.family,
            size: cfg.size,
            engine: cfg.engine.clone(),
            status,
            reason: Some(reason.into()),
            entries: 0,
            kernels: None,
            build_s: None,
            load_s: None,
            memory: None,
            measures: Vec::new(),
            timer_overhead_ns: None,
            process_s: 0.0,
            load_avg: crate::sys::loadavg(),
        }
    }
}

type Out<'a> = Option<LookupMatch<'a, u32>>;

#[inline]
fn same(m: Out<'_>, exp: u32, res: &[Resolved]) -> bool {
    match m {
        None => exp == NONE,
        Some(m) => {
            exp != NONE
                && m.prefix == res[exp as usize].prefix
                && m.data == res[exp as usize].payload.as_ref()
        }
    }
}

fn validate<K: Key, E: Engine<K>>(e: &E, ds: &Dataset<K>, cfg: &RunCfg) -> Result<(), String> {
    let res = &ds.resolved;
    for &d in &cfg.dists {
        let n = match cfg.validate {
            0 => ds.queries[d.index()].len(),
            v => v.min(ds.queries[d.index()].len()),
        };
        let (keys, exp) = (&ds.queries[d.index()][..n], &ds.expected[d.index()][..n]);
        let fail = |what: &str, i: usize, got: String| {
            Err(format!(
                "{what}/{}: query #{i} {:#x}: got {got}, expected {}",
                d.name(),
                keys[i].to_u128(),
                if exp[i] == NONE {
                    "no match".to_string()
                } else {
                    res[exp[i] as usize].prefix.to_string()
                }
            ))
        };
        if E::LOOKUP != Api::Unsupported && cfg.ops.contains(&Op::Lookup) {
            for (i, &k) in keys.iter().enumerate() {
                let m = e.lookup(k, res);
                if !same(m, exp[i], res) {
                    return fail("lookup", i, format!("{:?}", m.map(|m| m.prefix)));
                }
            }
        }
        if E::BATCH != Api::Unsupported && cfg.ops.contains(&Op::Batch) {
            // Every measured batch size, swept ones included.
            let sweep: &[usize] = if E::BATCH == Api::Native {
                &cfg.batch_sweep
            } else {
                &[]
            };
            for &b in cfg.batch.iter().chain(sweep) {
                let mut out: Vec<Out<'_>> = vec![None; b];
                for (c, kc) in keys.chunks(b).enumerate() {
                    e.lookup_batch(kc, res, &mut out[..kc.len()]);
                    for (j, m) in out[..kc.len()].iter().enumerate() {
                        if !same(*m, exp[c * b + j], res) {
                            return fail(
                                &format!("batch({b})"),
                                c * b + j,
                                format!("{:?}", m.map(|m| m.prefix)),
                            );
                        }
                    }
                }
            }
        }
        if cfg.ops.contains(&Op::Exists) {
            for (i, &k) in keys.iter().enumerate() {
                if e.exists(k) != (exp[i] != NONE) {
                    return fail("exists", i, e.exists(k).to_string());
                }
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ work loops

/// `n` single lookups starting at `start`, wrapping around `keys`.
#[inline(never)]
fn work_lookup<K: Key, E: Engine<K>>(e: &E, keys: &[K], res: &[Resolved], start: usize, n: u64) {
    let mut i = start;
    for _ in 0..n {
        black_box(e.lookup(keys[i], res));
        i += 1;
        if i == keys.len() {
            i = 0;
        }
    }
}

#[inline(never)]
fn work_exists<K: Key, E: Engine<K>>(e: &E, keys: &[K], start: usize, n: u64) {
    let mut i = start;
    for _ in 0..n {
        black_box(e.exists(keys[i]));
        i += 1;
        if i == keys.len() {
            i = 0;
        }
    }
}

/// `calls` batch calls of `b` addresses starting at chunk `start`.
#[inline(never)]
fn work_batch<'a, K: Key, E: Engine<K>>(
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    out: &mut [Out<'a>],
    b: usize,
    start: usize,
    calls: u64,
) {
    let chunks = keys.len() / b;
    let mut c = start;
    for _ in 0..calls {
        e.lookup_batch(&keys[c * b..c * b + b], res, &mut out[..b]);
        black_box(&mut *out);
        c += 1;
        if c == chunks {
            c = 0;
        }
    }
}

#[inline(never)]
fn sample_lookup<K: Key, E: Engine<K>, S: Stamp>(
    st: &S,
    e: &E,
    keys: &[K],
    res: &[Resolved],
    start: usize,
    n: usize,
    out: &mut Vec<u64>,
) {
    let mut i = start;
    let mask = st.mask();
    for _ in 0..n {
        let k = keys[i];
        let t0 = st.begin();
        black_box(e.lookup(k, res));
        let t1 = st.end();
        out.push(t1.wrapping_sub(t0) & mask);
        i += 1;
        if i == keys.len() {
            i = 0;
        }
    }
}

#[inline(never)]
fn sample_exists<K: Key, E: Engine<K>, S: Stamp>(
    st: &S,
    e: &E,
    keys: &[K],
    start: usize,
    n: usize,
    out: &mut Vec<u64>,
) {
    let mut i = start;
    let mask = st.mask();
    for _ in 0..n {
        let k = keys[i];
        let t0 = st.begin();
        black_box(e.exists(k));
        let t1 = st.end();
        out.push(t1.wrapping_sub(t0) & mask);
        i += 1;
        if i == keys.len() {
            i = 0;
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn sample_batch<'a, K: Key, E: Engine<K>, S: Stamp>(
    st: &S,
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    out: &mut [Out<'a>],
    b: usize,
    start: usize,
    n: usize,
    samples: &mut Vec<u64>,
) {
    let chunks = keys.len() / b;
    let mut c = start;
    let mask = st.mask();
    for _ in 0..n {
        let kc = &keys[c * b..c * b + b];
        let t0 = st.begin();
        e.lookup_batch(kc, res, &mut out[..b]);
        black_box(&mut *out);
        let t1 = st.end();
        samples.push(t1.wrapping_sub(t0) & mask);
        c += 1;
        if c == chunks {
            c = 0;
        }
    }
}

/// One timed unit of work: `units` operations (single ops) or batch calls.
#[derive(Clone, Copy)]
struct Task {
    op: Op,
    b: usize,
}

impl Task {
    /// Addresses per unit.
    fn addrs(self) -> u64 {
        if self.op == Op::Batch {
            self.b as u64
        } else {
            1
        }
    }
    /// Units in the key set (start offsets are spread over them).
    fn span(self, q: usize) -> usize {
        if self.op == Op::Batch { q / self.b } else { q }
    }
}

#[inline]
fn run_units<'a, K: Key, E: Engine<K>>(
    t: Task,
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    out: &mut [Out<'a>],
    start: usize,
    units: u64,
) {
    match t.op {
        Op::Lookup => work_lookup(e, keys, res, start, units),
        Op::Exists => work_exists(e, keys, start, units),
        Op::Batch => work_batch(e, keys, res, out, t.b, start, units),
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn sample_units<'a, K: Key, E: Engine<K>, S: Stamp>(
    st: &S,
    t: Task,
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    out: &mut [Out<'a>],
    start: usize,
    n: usize,
    samples: &mut Vec<u64>,
) {
    match t.op {
        Op::Lookup => sample_lookup(st, e, keys, res, start, n, samples),
        Op::Exists => sample_exists(st, e, keys, start, n, samples),
        Op::Batch => sample_batch(st, e, keys, res, out, t.b, start, n, samples),
    }
}

/// Calibrates the cost of one unit on the calling thread (doubles the work
/// until the warm-up time is spent) and returns ns per unit.
fn calibrate<'a, K: Key, E: Engine<K>>(
    t: Task,
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    out: &mut [Out<'a>],
    warmup: Duration,
) -> f64 {
    let begin = Instant::now();
    let mut units = 64u64;
    loop {
        let s = Instant::now();
        run_units(t, e, keys, res, out, 0, units);
        let el = s.elapsed();
        if begin.elapsed() >= warmup {
            return el.as_nanos().max(1) as f64 / units as f64;
        }
        units = units.saturating_mul(2);
    }
}

/// One latency pass with stamp `st`: samples are converted to picoseconds
/// (stamp overhead subtracted) in place. Returns the effective counter
/// frequency in GHz (cycles per ns) for cycle counters.
#[allow(clippy::too_many_arguments)]
fn latency_pass<'a, K: Key, E: Engine<K>, S: Stamp>(
    st: &S,
    ps_per_unit: Option<f64>,
    t: Task,
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    out: &mut [Out<'a>],
    start: usize,
    n: usize,
    samples: &mut Vec<u64>,
) -> Option<f64> {
    let overhead = timer::stamp_overhead(st);
    samples.clear();
    let w0 = Instant::now();
    let c0 = st.begin();
    sample_units(st, t, e, keys, res, out, start, n, samples);
    let c1 = st.end();
    let wall_ns = w0.elapsed().as_nanos().max(1) as f64;
    let (ps, ghz) = match ps_per_unit {
        Some(ps) => (ps, None),
        None => {
            let ghz = (c1.wrapping_sub(c0) & st.mask()) as f64 / wall_ns;
            (1000.0 / ghz, Some(ghz))
        }
    };
    for s in samples.iter_mut() {
        *s = (s.saturating_sub(overhead) as f64 * ps).round() as u64;
    }
    ghz
}

const MODE_THROUGHPUT: u8 = 0;
const MODE_LATENCY: u8 = 1;
const MODE_QUIT: u8 = 2;

/// Start/stop protocol shared by the workers of one configuration.
struct Control {
    generation: AtomicU32,
    mode: AtomicU8,
    ready: AtomicUsize,
    done: AtomicUsize,
    /// Per worker: start, end (ns since `origin`).
    stamps: Vec<(AtomicU64, AtomicU64)>,
}

impl Control {
    /// Blocks (without spinning on a CPU a worker may need) until `n`
    /// workers have checked in; the last one unparks the coordinator.
    fn wait_all(counter: &AtomicUsize, n: usize) {
        while counter.load(Ordering::Acquire) < n {
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }
    fn release(&self, mode: u8) {
        self.done.store(0, Ordering::Release);
        self.mode.store(mode, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

struct Outcome {
    walls_ns: Vec<f64>,
    /// Latency samples in picoseconds.
    samples_ps: Vec<u64>,
    /// `"cycles"` (all workers used the cycle counter) or `"tsc"`.
    clock: &'static str,
    /// Mean effective frequency of the cycle counters (GHz).
    ghz: Option<f64>,
}

/// Runs `reps + 1` timed repetitions (the first is discarded) and one
/// latency pass with `threads` pinned workers sharing `e`.
#[allow(clippy::too_many_arguments)]
fn run_workers<'a, K: Key, E: Engine<K>>(
    t: Task,
    e: &'a E,
    keys: &[K],
    res: &'a [Resolved],
    outs: &mut [Vec<Out<'a>>],
    sample_bufs: &mut [Vec<u64>],
    threads: usize,
    units: u64,
    reps: usize,
    lat_per_worker: usize,
    cpus: &[usize],
    tsc_ps_per_tick: f64,
) -> Outcome {
    let coordinator = std::thread::current();
    let ghz: Vec<AtomicU64> = (0..threads).map(|_| AtomicU64::new(0)).collect();
    let ctl = Control {
        generation: AtomicU32::new(0),
        mode: AtomicU8::new(MODE_THROUGHPUT),
        ready: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        stamps: (0..threads)
            .map(|_| (AtomicU64::new(0), AtomicU64::new(0)))
            .collect(),
    };
    let origin = Instant::now();
    let span = t.span(keys.len());
    let mut walls = Vec::with_capacity(reps);
    std::thread::scope(|s| {
        for (i, (out, smp)) in outs
            .iter_mut()
            .zip(sample_bufs.iter_mut())
            .take(threads)
            .enumerate()
        {
            let ctl = &ctl;
            let ghz = &ghz;
            let coordinator = coordinator.clone();
            let cpu = cpus.get(i % cpus.len().max(1)).copied();
            s.spawn(move || {
                if let Some(c) = cpu {
                    sys::pin(c);
                }
                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                let cycles = timer::pmc::Counter::open();
                #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
                let cycles: Option<Tsc> = None;
                let check_in = |c: &AtomicUsize| {
                    if c.fetch_add(1, Ordering::AcqRel) + 1 == threads {
                        coordinator.unpark();
                    }
                };
                // Disjoint start offsets: worker i starts at i/threads of the
                // key set. The cursor then advances across repetitions and
                // into the latency pass, so that no repetition replays
                // addresses this worker has just looked up (slow structures
                // would otherwise keep a small, cache-resident working set).
                let mut pos = i * span / threads;
                let advance = |pos: usize, by: u64| ((pos as u64 + by) % span as u64) as usize;
                check_in(&ctl.ready);
                let mut seen = 0u32;
                loop {
                    let g = loop {
                        let g = ctl.generation.load(Ordering::Acquire);
                        if g != seen {
                            break g;
                        }
                        std::hint::spin_loop();
                    };
                    seen = g;
                    match ctl.mode.load(Ordering::Acquire) {
                        MODE_QUIT => break,
                        MODE_LATENCY => {
                            let g = match &cycles {
                                Some(c) => latency_pass(
                                    c,
                                    None,
                                    t,
                                    e,
                                    keys,
                                    res,
                                    out,
                                    pos,
                                    lat_per_worker,
                                    smp,
                                ),
                                None => latency_pass(
                                    &Tsc,
                                    Some(tsc_ps_per_tick),
                                    t,
                                    e,
                                    keys,
                                    res,
                                    out,
                                    pos,
                                    lat_per_worker,
                                    smp,
                                ),
                            };
                            ghz[i].store(g.map_or(0, |g| (g * 1e6) as u64), Ordering::Relaxed);
                            pos = advance(pos, lat_per_worker as u64);
                        }
                        _ => {
                            let s0 = origin.elapsed().as_nanos() as u64;
                            run_units(t, e, keys, res, out, pos, units);
                            let s1 = origin.elapsed().as_nanos() as u64;
                            pos = advance(pos, units);
                            ctl.stamps[i].0.store(s0, Ordering::Relaxed);
                            ctl.stamps[i].1.store(s1, Ordering::Relaxed);
                        }
                    }
                    check_in(&ctl.done);
                }
            });
        }
        Control::wait_all(&ctl.ready, threads);
        for rep in 0..=reps {
            ctl.release(MODE_THROUGHPUT);
            Control::wait_all(&ctl.done, threads);
            if rep > 0 {
                let first = ctl.stamps.iter().map(|s| s.0.load(Ordering::Relaxed)).min();
                let last = ctl.stamps.iter().map(|s| s.1.load(Ordering::Relaxed)).max();
                walls.push((last.unwrap_or(0) - first.unwrap_or(0)).max(1) as f64);
            }
        }
        ctl.release(MODE_LATENCY);
        Control::wait_all(&ctl.done, threads);
        ctl.release(MODE_QUIT);
    });
    let mut samples = Vec::with_capacity(lat_per_worker * threads);
    for b in sample_bufs.iter().take(threads) {
        samples.extend_from_slice(b);
    }
    let g: Vec<f64> = ghz
        .iter()
        .map(|g| g.load(Ordering::Relaxed) as f64 / 1e6)
        .collect();
    let all_cycles = g.iter().all(|&x| x > 0.0);
    Outcome {
        walls_ns: walls,
        samples_ps: samples,
        clock: if all_cycles { "cycles" } else { "tsc" },
        ghz: all_cycles.then(|| g.iter().sum::<f64>() / g.len() as f64),
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Builds, validates and measures engine `E` on `ds`.
pub fn run<K: Key, E: Engine<K>>(ds: &Dataset<K>, cfg: &RunCfg) -> RunResult {
    let started = Instant::now();
    let mut r = RunResult::status(cfg, Status::Ok, "");
    r.reason = None;
    r.entries = ds.meta.entries;
    debug_assert_eq!(ds.meta.entries, ds.routes.len());
    if let Err(reason) = E::applicable() {
        r.status = Status::NotApplicable;
        r.reason = Some(reason);
        return r;
    }
    let ns_per_tick = 1e9 / cfg.ticks_per_s;
    let (ov_min, ov_med) = timer::overhead();
    r.timer_overhead_ns = Some((ov_min as f64 * ns_per_tick, ov_med as f64 * ns_per_tick));

    // ---------------------------------------------------------------- build
    let inputs = std::mem::size_of_val(&ds.routes[..])
        + std::mem::size_of_val(&ds.resolved[..])
        + ds.queries
            .iter()
            .map(|q| std::mem::size_of_val(&q[..]))
            .sum::<usize>()
        + ds.expected
            .iter()
            .map(|q| std::mem::size_of_val(&q[..]))
            .sum::<usize>();
    let heap0 = measure::heap_now();
    let rss0 = sys::rss();
    let hwm_ok = sys::reset_hwm();
    measure::reset_peak();
    let t = Instant::now();
    let engine = E::build(&ds.routes);
    let build = t.elapsed();
    let heap1 = measure::heap_now();
    let peak1 = measure::heap_peak();
    let rss1 = sys::rss();
    let hwm1 = sys::hwm();
    r.build_s = Some(build.as_secs_f64());
    r.kernels = engine.kernel_detail();

    let mapped = engine.mapped_bytes();
    let struct_heap = heap1.saturating_sub(heap0);
    let entry_table = if E::ENTRY_TABLE {
        std::mem::size_of_val(&ds.resolved[..])
    } else {
        0
    };
    let footprint = struct_heap + mapped + entry_table;
    let mut mem = Memory {
        struct_heap,
        struct_mapped: mapped,
        entry_table,
        footprint,
        bytes_per_entry: footprint as f64 / ds.routes.len().max(1) as f64,
        reported: engine.reported_bytes(),
        rss_delta: rss0.zip(rss1).map(|(a, b)| b as i64 - a as i64),
        build_peak_heap: peak1.saturating_sub(heap0) + mapped,
        build_peak_rss: if hwm_ok {
            rss0.zip(hwm1).map(|(a, b)| b.saturating_sub(a))
        } else {
            None
        },
        inputs,
        ..Memory::default()
    };

    // ----------------------------------------------------------- validation
    if let Err(msg) = validate(&engine, ds, cfg) {
        r.status = Status::Invalid;
        r.reason = Some(msg);
        r.memory = Some(mem);
        return r;
    }

    // -------------------------------------------------------------- buffers
    let max_t = cfg.threads.iter().copied().max().unwrap_or(1);
    let sweep: &[usize] = if E::BATCH == Api::Native {
        &cfg.batch_sweep
    } else {
        &[]
    };
    let max_b = cfg.batch.iter().chain(sweep).copied().max().unwrap_or(1);
    let lat_max = cfg.lat_samples.max(cfg.lat_batches);
    let res: &[Resolved] = &ds.resolved;
    let mut outs: Vec<Vec<Out<'_>>> = (0..max_t).map(|_| vec![None; max_b]).collect();
    let mut sample_bufs: Vec<Vec<u64>> = (0..max_t).map(|_| Vec::with_capacity(lat_max)).collect();
    mem.buffers = max_t * (max_b * std::mem::size_of::<Out<'_>>() + lat_max * 8);
    let cpus = if cfg.pin {
        sys::cpu_order()
    } else {
        Vec::new()
    };

    let heap2 = measure::heap_now();
    let rss2 = sys::rss();
    let hwm_ok = sys::reset_hwm();
    measure::reset_peak();

    // -------------------------------------------------------------- measure
    let window = Duration::from_millis(cfg.window_ms.max(1));
    let warmup = Duration::from_millis(cfg.warmup_ms.max(1));
    for &op in &cfg.ops {
        let supported = match op {
            Op::Lookup => E::LOOKUP != Api::Unsupported,
            Op::Batch => E::BATCH != Api::Unsupported,
            Op::Exists => E::EXISTS != Api::Unsupported,
        };
        if !supported {
            continue;
        }
        // `(batch size, swept)`: swept sizes run with the first thread count.
        let sizes: Vec<(Option<usize>, bool)> = if op == Op::Batch {
            let mut v: Vec<(Option<usize>, bool)> =
                cfg.batch.iter().map(|&b| (Some(b), false)).collect();
            v.extend(sweep.iter().map(|&b| (Some(b), true)));
            v.sort_by_key(|&(b, _)| b);
            v
        } else {
            vec![(None, false)]
        };
        for (b, swept) in sizes {
            let task = Task {
                op,
                b: b.unwrap_or(1),
            };
            for &d in &cfg.dists {
                let keys = &ds.queries[d.index()];
                let ns_per_unit = calibrate(task, &engine, keys, res, &mut outs[0], warmup);
                let units = ((window.as_nanos() as f64 / ns_per_unit).ceil() as u64).max(1);
                // Slow structures get fewer samples so that one pass stays
                // around `lat_max_ms`, with a floor that keeps p99 meaningful.
                let (want, floor) = if op == Op::Batch {
                    (cfg.lat_batches, MIN_LAT_BATCHES)
                } else {
                    (cfg.lat_samples, MIN_LAT_SAMPLES)
                };
                let fit = (cfg.lat_max_ms.max(1) as f64 * 1e6 / ns_per_unit) as usize;
                let lat_total = want.min(fit.max(floor));
                let threads_list = if swept {
                    &cfg.threads[..cfg.threads.len().min(1)]
                } else {
                    &cfg.threads[..]
                };
                for &threads in threads_list {
                    let lat_per_worker = lat_total.div_ceil(threads);
                    let mut o = run_workers(
                        task,
                        &engine,
                        keys,
                        res,
                        &mut outs,
                        &mut sample_bufs,
                        threads,
                        units,
                        cfg.reps.max(1),
                        lat_per_worker,
                        &cpus,
                        1e12 / cfg.ticks_per_s,
                    );
                    let addrs = units * task.addrs() * threads as u64;
                    let mut rates: Vec<f64> =
                        o.walls_ns.iter().map(|w| addrs as f64 / w * 1e9).collect();
                    let mut walls = o.walls_ns.clone();
                    let med_wall = median(&mut walls);
                    let tp = Throughput {
                        median: median(&mut rates),
                        min: rates.first().copied().unwrap_or(0.0),
                        max: rates.last().copied().unwrap_or(0.0),
                        reps: rates.len(),
                        addrs_per_rep: addrs,
                        rep_ns: o.walls_ns.iter().sum::<f64>() / o.walls_ns.len() as f64,
                    };
                    r.measures.push(Measure {
                        op,
                        dist: d,
                        threads,
                        batch: b,
                        throughput: tp,
                        amortized_ns: med_wall * threads as f64 / addrs as f64,
                        latency_kind: if op == Op::Batch { "batch" } else { "op" }.into(),
                        latency: timer::summarize(&mut o.samples_ps, 0, 1e-3),
                        clock: o.clock.into(),
                        clock_ghz: o.ghz,
                    });
                }
            }
        }
    }
    let heap3 = measure::heap_peak();
    let hwm3 = sys::hwm();
    mem.lookup_peak_heap = heap3.saturating_sub(heap2);
    mem.lookup_peak_rss = if hwm_ok {
        rss2.zip(hwm3).map(|(a, b)| b.saturating_sub(a))
    } else {
        None
    };
    r.memory = Some(mem);
    drop(outs);
    drop(engine);
    r.process_s = started.elapsed().as_secs_f64();
    r
}
