//! Native Rust runner of the libraries part (`bench-compare runner --plan
//! <file>`): prefix-trie and dir-24-8. Same plan protocol,
//! same JSON-lines records and same measurement steps as the Go and C
//! runners (see `benchmarks/compare-lib/README.md`).
//!
//! The harness is generic over the library and the family and is
//! monomorphized: every lookup is a direct, inlinable call.

use std::hint::black_box;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::time::Instant;

use dir_24_8::{IpTable, Replace};
use ipnet::{Ipv4Net, Ipv6Net};
use serde_json::{Value, json};

use crate::{measure, sys};

pub const CONTAINS: usize = 0;
pub const LOOKUP: usize = 1;
pub const OP_NAMES: [&str; 2] = ["contains", "lookup"];

fn emit(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn fail(msg: impl std::fmt::Display) -> ! {
    emit(json!({"type": "error", "message": msg.to_string()}));
    std::process::exit(2)
}

// ------------------------------------------------------------ plan

pub struct Task {
    pub latency: bool,
    pub op: usize,
    pub scenario: String,
    pub samples: usize,
    /// Worker threads of a throughput measurement.
    pub threads: usize,
}

pub struct Plan {
    pub lib: String,
    pub corpus: String,
    pub target_ms: u64,
    pub warmup: usize,
    pub prewarm: usize,
    pub edge: bool,
    pub validate: Vec<(String, usize)>,
    pub tasks: Vec<Task>,
}

fn parse_op(s: &str) -> usize {
    match s {
        "contains" => CONTAINS,
        "lookup" => LOOKUP,
        _ => fail(format!("unknown operation {s:?}")),
    }
}

pub fn read_plan(path: &Path) -> Plan {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(format!("plan: {e}")));
    let mut p = Plan {
        lib: String::new(),
        corpus: String::new(),
        target_ms: 100,
        warmup: 1 << 18,
        prewarm: 0,
        edge: false,
        validate: Vec::new(),
        tasks: Vec::new(),
    };
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.is_empty() || w[0].starts_with('#') {
            continue;
        }
        let num = |i: usize| -> usize {
            w.get(i)
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| fail(format!("plan line {line:?}")))
        };
        match w[0] {
            "lib" => p.lib = w[1].into(),
            "corpus" => p.corpus = w[1].into(),
            "target_ms" => p.target_ms = num(1) as u64,
            "warmup" => p.warmup = num(1),
            "prewarm_ms" => p.prewarm = num(1),
            "edge" => p.edge = true,
            "validate" => p.validate.push((w[1].into(), num(2))),
            "throughput" => p.tasks.push(Task {
                latency: false,
                op: parse_op(w[1]),
                scenario: w[2].into(),
                samples: 0,
                threads: if w.len() > 3 { num(3).max(1) } else { 1 },
            }),
            "latency" => p.tasks.push(Task {
                latency: true,
                op: parse_op(w[1]),
                scenario: w[2].into(),
                samples: num(3),
                threads: 1,
            }),
            "mmdb" => {}
            d => fail(format!("unknown plan directive {d:?}")),
        }
    }
    p
}

// ------------------------------------------------------------ corpus

struct Queries {
    width: usize,
    addrs: Vec<u8>,
    expected: Vec<u32>,
}

fn read_corpus_file(path: &Path, magic: &[u8; 8]) -> (u32, usize, usize, Vec<u8>) {
    let b = std::fs::read(path).unwrap_or_else(|e| fail(format!("{}: {e}", path.display())));
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    if b.len() < 32 || &b[..8] != magic || u32_at(8) != 1 {
        fail(format!("{}: bad header", path.display()));
    }
    let family = u32_at(12);
    let count = u64::from_le_bytes(b[16..24].try_into().unwrap()) as usize;
    let rec = u32_at(24) as usize;
    if b.len() != 32 + count * rec {
        fail(format!("{}: bad size", path.display()));
    }
    (family, count, rec, b)
}

fn read_queries(path: &Path) -> Queries {
    let (family, count, rec, b) = read_corpus_file(path, b"DLPCQF01");
    let width = if family == 4 { 4 } else { 16 };
    assert_eq!(rec, width + 4);
    let mut q = Queries {
        width,
        addrs: Vec::with_capacity(count * width),
        expected: Vec::with_capacity(count),
    };
    for r in b[32..].chunks_exact(rec) {
        q.addrs.extend_from_slice(&r[..width]);
        q.expected
            .push(u32::from_le_bytes(r[width..].try_into().unwrap()));
    }
    q
}

/// `(family, [(network bytes, length, id)])`.
fn read_prefixes(path: &Path) -> (u32, Vec<([u8; 16], u8, u32)>) {
    let (family, _, rec, b) = read_corpus_file(path, b"DLPCPF01");
    let width = if family == 4 { 4 } else { 16 };
    assert_eq!(rec, width + 8);
    let out = b[32..]
        .chunks_exact(rec)
        .map(|r| {
            let mut a = [0u8; 16];
            a[..width].copy_from_slice(&r[..width]);
            (
                a,
                r[width],
                u32::from_le_bytes(r[width + 4..].try_into().unwrap()),
            )
        })
        .collect();
    (family, out)
}

// ------------------------------------------------------------ families

pub trait Fam: 'static + Sync {
    type Addr: Copy + Sync;
    type Net: Copy + Sync + prefix_trie::Prefix;
    const NUM: u32;
    fn addr(b: &[u8]) -> Self::Addr;
    fn net(b: &[u8; 16], len: u8) -> Self::Net;
    fn host(a: Self::Addr) -> Self::Net;
    fn dir_exist(t: &IpTable<u32>, a: Self::Addr) -> bool;
    fn dir_lookup(t: &IpTable<u32>, a: Self::Addr) -> u32;
    fn dir_groups(t: &IpTable<u32>) -> usize;
}

pub struct V4;
pub struct V6;

impl Fam for V4 {
    type Addr = Ipv4Addr;
    type Net = Ipv4Net;
    const NUM: u32 = 4;
    #[inline(always)]
    fn addr(b: &[u8]) -> Ipv4Addr {
        Ipv4Addr::from(<[u8; 4]>::try_from(b).unwrap())
    }
    fn net(b: &[u8; 16], len: u8) -> Ipv4Net {
        Ipv4Net::new_assert(Ipv4Addr::new(b[0], b[1], b[2], b[3]), len)
    }
    #[inline(always)]
    fn host(a: Ipv4Addr) -> Ipv4Net {
        Ipv4Net::new_assert(a, 32)
    }
    #[inline(always)]
    fn dir_exist(t: &IpTable<u32>, a: Ipv4Addr) -> bool {
        t.exist_ipv4(a)
    }
    #[inline(always)]
    fn dir_lookup(t: &IpTable<u32>, a: Ipv4Addr) -> u32 {
        t.lookup_ipv4(a).and_then(|m| m.data.copied()).unwrap_or(0)
    }
    fn dir_groups(t: &IpTable<u32>) -> usize {
        t.stats().ipv4.groups_in_use
    }
}

impl Fam for V6 {
    type Addr = Ipv6Addr;
    type Net = Ipv6Net;
    const NUM: u32 = 6;
    #[inline(always)]
    fn addr(b: &[u8]) -> Ipv6Addr {
        Ipv6Addr::from(<[u8; 16]>::try_from(b).unwrap())
    }
    fn net(b: &[u8; 16], len: u8) -> Ipv6Net {
        Ipv6Net::new_assert(Ipv6Addr::from(*b), len)
    }
    #[inline(always)]
    fn host(a: Ipv6Addr) -> Ipv6Net {
        Ipv6Net::new_assert(a, 128)
    }
    #[inline(always)]
    fn dir_exist(t: &IpTable<u32>, a: Ipv6Addr) -> bool {
        t.exist_ipv6(a)
    }
    #[inline(always)]
    fn dir_lookup(t: &IpTable<u32>, a: Ipv6Addr) -> u32 {
        t.lookup_ipv6(a).and_then(|m| m.data.copied()).unwrap_or(0)
    }
    fn dir_groups(t: &IpTable<u32>) -> usize {
        t.stats().ipv6.groups_in_use
    }
}

// ------------------------------------------------------------ libraries

pub trait Lib<F: Fam>: Sized + Sync {
    /// Native query type, prepared before any timing.
    type Q: Copy + Sync;
    const LOOKUP: bool;
    fn info() -> Value;
    fn build(ps: &[([u8; 16], u8, u32)]) -> Self;
    /// `(count, unit name)` of the structure when it exposes one.
    fn units(&self) -> Option<(usize, &'static str)>;
    fn query(a: F::Addr) -> Self::Q;
    fn contains(&self, q: &Self::Q) -> bool;
    /// Record id of the longest match, 0 for none.
    fn lookup(&self, q: &Self::Q) -> u32;
}

/// tiborschneider/prefix-trie: `PrefixMap<Ipv4Net | Ipv6Net, u32>`.
pub struct PrefixTrie<F: Fam> {
    map: prefix_trie::PrefixMap<F::Net, u32>,
}

impl<F: Fam> Lib<F> for PrefixTrie<F> {
    type Q = F::Net;
    const LOOKUP: bool = true;
    fn info() -> Value {
        json!({"id": "prefix-trie", "crate": "prefix-trie", "backend": "PrefixMap<Ipv4Net | Ipv6Net, u32> (binary prefix trie)",
            "contains": "adapted: get_lpm(host prefix).is_some() (no address membership API)",
            "lookup": "native get_lpm(host /32 or /128 prefix)"})
    }
    fn build(ps: &[([u8; 16], u8, u32)]) -> Self {
        let mut map = prefix_trie::PrefixMap::new();
        for (a, l, id) in ps {
            map.insert(F::net(a, *l), *id);
        }
        Self { map }
    }
    fn units(&self) -> Option<(usize, &'static str)> {
        Some((self.map.len(), "prefixes (len)"))
    }
    #[inline(always)]
    fn query(a: F::Addr) -> F::Net {
        F::host(a)
    }
    #[inline(always)]
    fn contains(&self, q: &F::Net) -> bool {
        self.map.get_lpm(q).is_some()
    }
    #[inline(always)]
    fn lookup(&self, q: &F::Net) -> u32 {
        self.map.get_lpm(q).map_or(0, |(_, &v)| v)
    }
}

/// This project: `IpTable<u32>` with the default configuration. Single
/// lookups and `exist` always run the scalar kernels.
pub struct Dir248<F: Fam> {
    table: IpTable<u32>,
    _f: std::marker::PhantomData<F>,
}

impl<F: Fam> Lib<F> for Dir248<F> {
    type Q = F::Addr;
    const LOOKUP: bool = true;
    fn info() -> Value {
        let t: IpTable<u32> = IpTable::new();
        json!({"id": "dir-24-8", "crate": "dir-24-8 (this repository)",
            "backend": format!("IpTable<u32>, default Config: scalar kernels for single lookups ({} for batches, not measured here)",
                t.stats().batch_kernel),
            "contains": "native exist_ipv4 / exist_ipv6", "lookup": "native lookup_ipv4 / lookup_ipv6 (payload)",
            "single_backend": "scalar"})
    }
    fn build(ps: &[([u8; 16], u8, u32)]) -> Self {
        let mut table = IpTable::new();
        for (a, l, id) in ps {
            let net: ipnet::IpNet = match F::NUM {
                4 => Ipv4Net::new_assert(Ipv4Addr::new(a[0], a[1], a[2], a[3]), *l).into(),
                _ => Ipv6Net::new_assert(Ipv6Addr::from(*a), *l).into(),
            };
            table
                .insert(net, Some(*id), Replace)
                .unwrap_or_else(|e| fail(format!("insert: {e}")));
        }
        Self {
            table,
            _f: std::marker::PhantomData,
        }
    }
    fn units(&self) -> Option<(usize, &'static str)> {
        Some((F::dir_groups(&self.table), "secondary groups (tbl8)"))
    }
    #[inline(always)]
    fn query(a: F::Addr) -> F::Addr {
        a
    }
    #[inline(always)]
    fn contains(&self, q: &F::Addr) -> bool {
        F::dir_exist(&self.table, *q)
    }
    #[inline(always)]
    fn lookup(&self, q: &F::Addr) -> u32 {
        F::dir_lookup(&self.table, *q)
    }
}

// ------------------------------------------------------------ harness

#[inline(always)]
fn op_value<F: Fam, L: Lib<F>>(l: &L, op: usize, q: &L::Q) -> u64 {
    if op == CONTAINS {
        u64::from(l.contains(q))
    } else {
        u64::from(l.lookup(q))
    }
}

/// `n` operations on queries `lo, lo + 1, ...` (index & mask).
#[inline(never)]
fn run_loop<F: Fam, L: Lib<F>>(l: &L, op: usize, qs: &[L::Q], lo: usize, n: usize) -> u64 {
    let mask = qs.len() - 1;
    let mut sum = 0u64;
    if op == CONTAINS {
        for i in lo..lo + n {
            sum += u64::from(l.contains(black_box(&qs[i & mask])));
        }
    } else {
        for i in lo..lo + n {
            sum += u64::from(l.lookup(black_box(&qs[i & mask])));
        }
    }
    black_box(sum)
}

/// `lookups` operations split over `threads` workers sharing `l` (read
/// only): worker k runs indices `[k * lookups / threads, (k + 1) * lookups
/// / threads)`. Workers are started before the clock and released
/// together; returns (checksum, wall ns from the release to the last
/// worker's end).
fn run_threads<F: Fam, L: Lib<F>>(
    l: &L,
    op: usize,
    qs: &[L::Q],
    lookups: usize,
    threads: usize,
) -> (u64, u64) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    if threads == 1 {
        let s0 = Instant::now();
        let sum = run_loop::<F, L>(l, op, qs, 0, lookups);
        return (sum, s0.elapsed().as_nanos() as u64);
    }
    let ready = AtomicUsize::new(0);
    let go = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|k| {
                let (ready, go) = (&ready, &go);
                scope.spawn(move || {
                    let lo = k * lookups / threads;
                    let hi = (k + 1) * lookups / threads;
                    ready.fetch_add(1, Ordering::AcqRel);
                    while !go.load(Ordering::Acquire) {
                        std::hint::spin_loop();
                    }
                    let sum = run_loop::<F, L>(l, op, qs, lo, hi - lo);
                    (sum, Instant::now())
                })
            })
            .collect();
        while ready.load(Ordering::Acquire) < threads {
            std::hint::spin_loop();
        }
        let t0 = Instant::now();
        go.store(true, Ordering::Release);
        let mut sum = 0u64;
        let mut last = t0;
        for w in workers {
            let (s, end) = w.join().expect("worker");
            sum += s;
            last = last.max(end);
        }
        (sum, (last - t0).as_nanos() as u64)
    })
}

#[inline(never)]
fn run_latency<F: Fam, L: Lib<F>>(l: &L, op: usize, qs: &[L::Q], samples: &mut [u64]) -> u64 {
    let mask = qs.len() - 1;
    let mut sum = 0u64;
    for (i, s) in samples.iter_mut().enumerate() {
        let q = black_box(&qs[i & mask]);
        let t0 = Instant::now();
        let v = black_box(op_value::<F, L>(l, op, q));
        *s = t0.elapsed().as_nanos() as u64;
        sum += v;
    }
    sum
}

fn clock_stats() -> Value {
    const N: usize = 200_000;
    let mut d: Vec<u64> = (0..N)
        .map(|_| {
            let t0 = Instant::now();
            let t1 = Instant::now();
            (t1 - t0).as_nanos() as u64
        })
        .collect();
    d.sort_unstable();
    let mut res = u64::MAX;
    let base = Instant::now();
    let mut prev = 0u64;
    for _ in 0..2_000_000 {
        let t = base.elapsed().as_nanos() as u64;
        if t != prev {
            res = res.min(t - prev);
            prev = t;
        }
    }
    json!({"source": "std::time::Instant (CLOCK_MONOTONIC, vDSO)", "overhead_ns": d[N / 2], "overhead_min": d[0], "resolution_ns": res})
}

pub fn percentiles(s: &mut [u64]) -> Value {
    s.sort_unstable();
    let n = s.len();
    let rank = |p: f64| -> u64 {
        let k = ((p / 100.0 * n as f64) + 0.999_999_999) as usize;
        s[k.clamp(1, n) - 1]
    };
    let mean = s.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
    json!({"min": s[0], "p1": rank(1.0), "p5": rank(5.0), "p25": rank(25.0), "p50": rank(50.0), "p75": rank(75.0),
        "p90": rank(90.0), "p95": rank(95.0), "p99": rank(99.0), "max": s[n - 1], "mean": mean,
        "p99_9": if n >= 100_000 { json!(rank(99.9)) } else { Value::Null }})
}

/// Returns freed heap memory to the system before reading the RSS.
fn settle() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only releases free heap pages.
    unsafe {
        libc::malloc_trim(0);
    }
}

fn run<F: Fam, L: Lib<F>>(p: &Plan) {
    emit(
        json!({"type": "meta", "runner": "rust", "lib": p.lib, "impl": L::info(),
        "build": {"rustc": env!("COMPARE_RUSTC_RELEASE"), "profile": env!("COMPARE_PROFILE"),
            "opt_level": env!("COMPARE_OPT_LEVEL"), "target_features": env!("COMPARE_TARGET_FEATURES"),
            "lto": "thin (workspace release profile)", "codegen_units": 1},
        "clock": clock_stats()}),
    );
    let dir = Path::new(&p.corpus);
    let mut names: Vec<&str> = p.validate.iter().map(|v| v.0.as_str()).collect();
    names.extend(p.tasks.iter().map(|t| t.scenario.as_str()));
    names.sort_unstable();
    names.dedup();
    let mut raw = std::collections::HashMap::new();
    let mut native = std::collections::HashMap::new();
    for name in names {
        let q = read_queries(&dir.join(format!("q-{name}.bin")));
        if p.tasks.iter().any(|t| t.scenario == name) && !q.expected.len().is_power_of_two() {
            fail(format!(
                "measured scenario {name}: query count is not a power of two"
            ));
        }
        let n: Vec<L::Q> = q
            .addrs
            .chunks_exact(q.width)
            .map(|b| L::query(F::addr(b)))
            .collect();
        native.insert(name.to_string(), n);
        raw.insert(name.to_string(), q);
    }
    let (family, prefixes) = read_prefixes(&dir.join("prefixes.bin"));
    assert_eq!(family, F::NUM);

    settle();
    let heap0 = measure::heap_now();
    let rss0 = sys::rss();
    let t0 = Instant::now();
    let lib = L::build(&prefixes);
    let build_ns = t0.elapsed().as_nanos() as u64;
    settle();
    let heap1 = measure::heap_now();
    let rss1 = sys::rss();
    let (units, units_name) = lib.units().map_or((-1i64, ""), |(n, s)| (n as i64, s));
    emit(
        json!({"type": "build", "family": family, "prefixes": prefixes.len(), "build_ns": build_ns,
        "rss_before": rss0, "rss_after": rss1, "heap_before": heap0, "heap_after": heap1,
        "structure_bytes": heap1 as i64 - heap0 as i64,
        "structure_method": "Rust heap (counting global allocator) before/after build; memory mapped by dir-24-8 is only in the RSS",
        "units": units, "units_name": units_name}),
    );

    let mut invalid = [false; 2];
    for (scenario, limit) in &p.validate {
        let q = &raw[scenario];
        let qs = &native[scenario];
        let n = if *limit > 0 {
            (*limit).min(qs.len())
        } else {
            qs.len()
        };
        for op in [CONTAINS, LOOKUP] {
            if op == LOOKUP && !L::LOOKUP {
                continue;
            }
            let (mut mism, mut examples) = (0usize, Vec::new());
            for (i, query) in qs.iter().enumerate().take(n) {
                let mut want = q.expected[i];
                if op == CONTAINS && want != 0 {
                    want = 1;
                }
                let got = op_value::<F, L>(&lib, op, query) as u32;
                if got != want {
                    mism += 1;
                    if examples.len() < 5 {
                        let a = &q.addrs[i * q.width..(i + 1) * q.width];
                        let hex: String = a.iter().map(|b| format!("{b:02x}")).collect();
                        examples
                            .push(json!({"addr": hex, "want": want, "got": got, "error": false}));
                    }
                }
            }
            invalid[op] |= mism > 0;
            emit(
                json!({"type": "validation", "scenario": scenario, "op": OP_NAMES[op], "checked": n,
                "mismatches": mism, "errors": 0, "examples": examples}),
            );
        }
    }

    // Warm-up: one pass over each measured scenario per operation, stopped
    // after prewarm_ms (0: no time limit).
    let mut warmed = std::collections::HashSet::new();
    for t in &p.tasks {
        if warmed.insert(t.scenario.clone()) {
            let qs = &native[&t.scenario];
            for op in [CONTAINS, LOOKUP] {
                if op == LOOKUP && !L::LOOKUP {
                    continue;
                }
                let w0 = Instant::now();
                let mut done = 0;
                while done < qs.len()
                    && (p.prewarm == 0 || w0.elapsed().as_millis() < p.prewarm as u128)
                {
                    let c = 4096.min(qs.len() - done);
                    run_loop::<F, L>(&lib, op, qs, done, c);
                    done += c;
                }
                emit(
                    json!({"type": "warmup", "scenario": t.scenario, "op": OP_NAMES[op], "lookups": done}),
                );
            }
        }
    }

    let mut volume: std::collections::HashMap<(usize, String), usize> =
        std::collections::HashMap::new();
    for t in &p.tasks {
        let op = OP_NAMES[t.op];
        let kind = if t.latency { "latency" } else { "throughput" };
        if invalid[t.op] {
            emit(
                json!({"type": "skip", "task": kind, "op": op, "scenario": t.scenario, "threads": t.threads,
                "reason": "validation failed: results differ from the oracle"}),
            );
            continue;
        }
        let q = &raw[&t.scenario];
        let qs = &native[&t.scenario];
        let mask = qs.len() - 1;
        let expect = |n: usize| -> u64 {
            (0..n)
                .map(|i| {
                    let e = q.expected[i & mask];
                    if t.op == CONTAINS {
                        u64::from(e != 0)
                    } else {
                        u64::from(e)
                    }
                })
                .sum()
        };
        if t.latency {
            let mut samples = vec![0u64; t.samples];
            let sum = run_latency::<F, L>(&lib, t.op, qs, &mut samples);
            emit(
                json!({"type": "latency", "op": op, "scenario": t.scenario, "samples": t.samples, "checksum": sum,
                "expected_checksum": expect(t.samples), "errors": 0, "raw_ns": percentiles(&mut samples)}),
            );
        } else {
            // One single-thread calibration per (operation, scenario): every
            // thread count then runs the same total number of lookups.
            let lookups = *volume.entry((t.op, t.scenario.clone())).or_insert_with(|| {
                let warm = p.warmup.min(qs.len());
                let c0 = Instant::now();
                run_loop::<F, L>(&lib, t.op, qs, 0, warm);
                let per = c0.elapsed().as_nanos() as f64 / warm as f64;
                let lookups =
                    ((p.target_ms as f64 * 1e6 / per) as usize + 1).clamp(1 << 14, 1 << 27);
                lookups.div_ceil(1024) * 1024
            });
            let (sum, wall) = run_threads::<F, L>(&lib, t.op, qs, lookups, t.threads);
            emit(
                json!({"type": "throughput", "op": op, "scenario": t.scenario, "threads": t.threads, "lookups": lookups,
                "wall_ns": wall, "checksum": sum, "expected_checksum": expect(lookups), "errors": 0}),
            );
        }
    }
    if !p.tasks.is_empty() {
        settle();
        emit(
            json!({"type": "memory", "stage": "after_measurements", "rss": sys::rss(), "heap": measure::heap_now()}),
        );
    }
    black_box(&prefixes);
    emit(json!({"type": "end", "hwm": sys::hwm()}));
}

/// Entry point of `bench-compare runner --plan <file>`.
pub fn main(plan: &Path) {
    let p = read_plan(plan);
    let fam: u32 = {
        let (f, _, _, _) =
            read_corpus_file(&Path::new(&p.corpus).join("prefixes.bin"), b"DLPCPF01");
        f
    };
    match (p.lib.as_str(), fam) {
        ("prefix-trie", 4) => run::<V4, PrefixTrie<V4>>(&p),
        ("prefix-trie", 6) => run::<V6, PrefixTrie<V6>>(&p),
        ("dir-24-8", 4) => run::<V4, Dir248<V4>>(&p),
        ("dir-24-8", 6) => run::<V6, Dir248<V6>>(&p),
        (lib, f) => fail(format!("unknown library {lib:?} for family {f}")),
    }
}
