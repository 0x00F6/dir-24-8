//! Criterion benchmarks of the lookup paths.
//!
//! One table is measured per build: single lookups always run the scalar
//! kernels (benchmark id `scalar`), batches the kernel of the build (id
//! `scalar`, or with `--features batch-simd` the CPU's SIMD kernel, e.g.
//! `avx512`). `make bench` enables `batch-simd`; `make bench SIMD=0`
//! measures the scalar batches.
//!
//! Groups (filter with `make bench FILTER=<regex>`):
//!
//! * `single/{ipv4,ipv6}/{pattern}`: one `lookup_ipv4`/`lookup_ipv6` call
//!   per iteration (independent back-to-back calls).
//! * `single-latency/{ipv4,ipv6}`: same, but each address depends on the
//!   previous result, which serializes calls and exposes latency.
//! * `exist/...`: single and batch `exist`.
//! * `batch/{ipv4,ipv6,mixed}/{pattern}/{size}`: `lookup_batch_into` with
//!   typed keys (`Ipv4Addr`, `Ipv6Addr`, `IpAddr`). Throughput is per
//!   address, so Criterion reports the amortized time per IP.
//! * `batch-repr/...`: numeric fast paths (`u32`, `u128`), identifiers only
//!   (`lookup_ids_into`) versus full `LookupMatch` results, and a loop of
//!   single calls for comparison.
//! * `huge-pages/...`: the same batch with and without huge pages.
//!
//! Workloads (`random` = 50 % hits, `hits`, `misses`, `deep`, `sequential`,
//! `local`) and route tables come from `benches/support/data.rs`, the
//! generator `bench-compare` uses too; misses are covered by no prefix.
//! Every window of keys comes from a pool of 2^20 pre-generated queries
//! that the timed loop walks through, so successive iterations do not reuse
//! cache-hot addresses. Batch results are checked against the (scalar)
//! single lookups before any measurement.
//!
//! Environment: `BENCH_V4_ROUTES` (default 200000), `BENCH_V6_ROUTES`
//! (default 100000), `BENCH_TIME` (seconds per benchmark, default 1.5).

#[path = "support/data.rs"]
mod data;

use std::hint::black_box;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use data::Workload;
use dir_24_8::Replace;
use dir_24_8::{Config, IpNet, IpTable, LookupMatch, MatchId};

const POOL: usize = 1 << 20;
const SIZES: [usize; 7] = [1, 8, 16, 32, 64, 256, 1024];

/// Advances a window of `size` keys through the pool.
#[inline(always)]
fn window(off: &mut usize, size: usize) -> usize {
    *off += size;
    if *off + size > POOL {
        *off = 0;
    }
    *off
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Benchmark id of the single lookups (always the scalar kernels).
const SINGLE: &str = "scalar";

struct Fixture {
    t4: data::Table<u32>,
    t6: data::Table<u128>,
    v4: Vec<dir_24_8::Ipv4Net>,
    v6: Vec<dir_24_8::Ipv6Net>,
}

impl Fixture {
    fn new() -> Self {
        let t4 = data::table::<u32>(env_usize("BENCH_V4_ROUTES", 200_000), 1);
        let t6 = data::table::<u128>(env_usize("BENCH_V6_ROUTES", 100_000), 2);
        let v4 = data::v4_nets(&t4.routes);
        let v6 = data::v6_nets(&t6.routes);
        Self { t4, t6, v4, v6 }
    }

    fn table(&self, config: Config) -> IpTable<u32> {
        let mut t = IpTable::with_config(config).expect("valid configuration");
        for (i, n) in self.v4.iter().enumerate() {
            t.insert(*n, data::payload(i), Replace).unwrap();
        }
        for (i, n) in self.v6.iter().enumerate() {
            t.insert(*n, data::payload(i), Replace).unwrap();
        }
        t
    }
}

struct Queries {
    v4: Vec<Ipv4Addr>,
    v6: Vec<Ipv6Addr>,
    mixed: Vec<IpAddr>,
    n4: Vec<u32>,
    n6: Vec<u128>,
}

fn queries(f: &Fixture, p: Workload) -> Queries {
    let n4 = data::queries(&f.t4, p, POOL, 7);
    let n6 = data::queries(&f.t6, p, POOL, 8);
    let v4: Vec<Ipv4Addr> = n4.iter().map(|&a| Ipv4Addr::from(a)).collect();
    let v6: Vec<Ipv6Addr> = n6.iter().map(|&a| Ipv6Addr::from(a)).collect();
    let mut rng = data::Rng::new(9);
    let mixed = (0..POOL)
        .map(|i| {
            if rng.below(2) == 0 {
                IpAddr::V4(v4[i])
            } else {
                IpAddr::V6(v6[i])
            }
        })
        .collect();
    Queries {
        v4,
        v6,
        mixed,
        n4,
        n6,
    }
}

fn key(m: Option<LookupMatch<'_, u32>>) -> Option<(IpNet, Option<u32>)> {
    m.map(|m| (m.prefix, m.data.copied()))
}

/// Batch lookups (the build's batch kernel) must agree with single lookups
/// (scalar kernels) before anything is measured.
fn validate(t: &IpTable<u32>, q: &Queries) {
    let n = 20_000;
    let mut out = vec![None; n];
    t.lookup_batch_into(&q.mixed[..n], &mut out).unwrap();
    for (o, &ip) in out.iter().zip(&q.mixed[..n]) {
        assert_eq!(key(*o), key(t.lookup(ip)), "mixed batch {ip}");
    }
    let mut out4 = vec![None; n];
    t.lookup_batch_into(&q.v4[..n], &mut out4).unwrap();
    for (o, &a) in out4.iter().zip(&q.v4[..n]) {
        assert_eq!(key(*o), key(t.lookup_ipv4(a)), "IPv4 batch {a}");
    }
    let mut out6 = vec![None; n];
    t.lookup_batch_into(&q.v6[..n], &mut out6).unwrap();
    for (o, &a) in out6.iter().zip(&q.v6[..n]) {
        assert_eq!(key(*o), key(t.lookup_ipv6(a)), "IPv6 batch {a}");
    }
}

fn configure() -> Criterion {
    let secs: f64 = std::env::var("BENCH_TIME")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.5);
    Criterion::default()
        .warm_up_time(Duration::from_secs_f64((secs / 3.0).max(0.2)))
        .measurement_time(Duration::from_secs_f64(secs))
        .sample_size(30)
        .configure_from_args()
}

fn hit_ratio(t: &IpTable<u32>, q: &Queries) -> f64 {
    let n = 50_000;
    let hits = q.v4[..n].iter().filter(|&&a| t.exist_ipv4(a)).count()
        + q.v6[..n].iter().filter(|&&a| t.exist_ipv6(a)).count();
    hits as f64 / (2 * n) as f64
}

fn benches(c: &mut Criterion) {
    let fixture = Fixture::new();
    let table = fixture.table(Config::new());
    let kernel = table.stats().batch_kernel;
    eprintln!(
        "fixture: {} IPv4 + {} IPv6 routes (BGP-like), batch kernel {kernel}, single lookups {SINGLE}\n{}",
        fixture.v4.len(),
        fixture.v6.len(),
        table.stats()
    );
    let t = &table;

    let patterns = [
        Workload::RANDOM,
        Workload::Hits,
        Workload::Misses,
        Workload::Deep,
        Workload::Sequential,
        Workload::Local,
    ];
    let all_queries: Vec<(Workload, Queries)> = patterns
        .iter()
        .map(|&p| (p, queries(&fixture, p)))
        .collect();
    for (p, q) in &all_queries {
        validate(t, q);
        eprintln!("pattern {:<10} hit ratio {:.3}", p.name(), hit_ratio(t, q));
    }
    let q_of = |p: Workload| &all_queries.iter().find(|(x, _)| *x == p).unwrap().1;

    // ---------------------------------------------------------------- single
    for p in [
        Workload::Hits,
        Workload::Misses,
        Workload::Deep,
        Workload::Local,
    ] {
        let q = q_of(p);
        for (fam, is_v4) in [("ipv4", true), ("ipv6", false)] {
            let mut g = c.benchmark_group(format!("single/{fam}/{}", p.name()));
            g.throughput(Throughput::Elements(1));
            let mut i = 0usize;
            g.bench_function(BenchmarkId::from_parameter(SINGLE), |bench| {
                if is_v4 {
                    bench.iter(|| {
                        i = (i + 1) & (POOL - 1);
                        black_box(t.lookup_ipv4(black_box(q.v4[i])))
                    })
                } else {
                    bench.iter(|| {
                        i = (i + 1) & (POOL - 1);
                        black_box(t.lookup_ipv6(black_box(q.v6[i])))
                    })
                }
            });
            g.finish();
        }
    }

    // Dependent chain: the next address depends on the previous result.
    {
        let q = q_of(Workload::Hits);
        for (fam, is_v4) in [("ipv4", true), ("ipv6", false)] {
            let mut g = c.benchmark_group(format!("single-latency/{fam}/hit"));
            g.throughput(Throughput::Elements(1));
            let mut i = 0usize;
            let mut dep = 0usize;
            g.bench_function(BenchmarkId::from_parameter(SINGLE), |bench| {
                bench.iter(|| {
                    i = (i + 1 + dep) & (POOL - 1);
                    let r = if is_v4 {
                        t.lookup_ipv4(q.v4[i])
                    } else {
                        t.lookup_ipv6(q.v6[i])
                    };
                    dep = r.map_or(0, |m| usize::from(m.prefix.prefix_len()) & 1);
                    black_box(dep)
                })
            });
            g.finish();
        }
    }

    // ----------------------------------------------------------------- exist
    for p in [Workload::Hits, Workload::Misses] {
        let q = q_of(p);
        let mut g = c.benchmark_group(format!("exist/single-ipv4/{}", p.name()));
        g.throughput(Throughput::Elements(1));
        let mut i = 0usize;
        g.bench_function(BenchmarkId::from_parameter(SINGLE), |bench| {
            bench.iter(|| {
                i = (i + 1) & (POOL - 1);
                black_box(t.exist(black_box(IpAddr::V4(q.v4[i]))))
            })
        });
        g.finish();
        let mut g = c.benchmark_group(format!("exist/batch-mixed/{}/256", p.name()));
        g.throughput(Throughput::Elements(256));
        let mut out = vec![false; 256];
        let mut off = 0usize;
        g.bench_function(BenchmarkId::from_parameter(kernel), |bench| {
            bench.iter(|| {
                window(&mut off, 256);
                t.exist_batch_into(&q.mixed[off..off + 256], &mut out)
                    .unwrap();
                black_box(&out);
            })
        });
        g.finish();
    }

    // ----------------------------------------------------------------- batch
    for (p, q) in &all_queries {
        let sizes: &[usize] = if *p == Workload::Hits { &SIZES } else { &[256] };
        for &size in sizes {
            for fam in ["ipv4", "ipv6", "mixed"] {
                let mut g = c.benchmark_group(format!("batch/{fam}/{}/{size}", p.name()));
                g.throughput(Throughput::Elements(size as u64));
                let mut out = vec![None; size];
                let mut off = 0usize;
                g.bench_function(BenchmarkId::from_parameter(kernel), |bench| {
                    bench.iter(|| {
                        window(&mut off, size);
                        let end = off + size;
                        match fam {
                            "ipv4" => t.lookup_batch_into(&q.v4[off..end], &mut out),
                            "ipv6" => t.lookup_batch_into(&q.v6[off..end], &mut out),
                            _ => t.lookup_batch_into(&q.mixed[off..end], &mut out),
                        }
                        .unwrap();
                        black_box(&out);
                    })
                });
                g.finish();
            }
        }
    }

    // ------------------------------------------------- batch representations
    {
        let q = q_of(Workload::Hits);
        let size = 256;
        for fam in ["ipv4", "ipv6"] {
            let mut g = c.benchmark_group(format!("batch-repr/{fam}/hit/{size}"));
            g.throughput(Throughput::Elements(size as u64));
            let mut out = vec![None; size];
            let mut ids = vec![MatchId::NONE; size];
            let mut off = 0usize;
            g.bench_function(BenchmarkId::new("typed-full", kernel), |bench| {
                bench.iter(|| {
                    window(&mut off, size);
                    if fam == "ipv4" {
                        t.lookup_batch_into(&q.v4[off..off + size], &mut out)
                            .unwrap();
                    } else {
                        t.lookup_batch_into(&q.v6[off..off + size], &mut out)
                            .unwrap();
                    }
                    black_box(&out);
                })
            });
            g.bench_function(BenchmarkId::new("numeric-full", kernel), |bench| {
                bench.iter(|| {
                    window(&mut off, size);
                    if fam == "ipv4" {
                        t.lookup_batch_into(&q.n4[off..off + size], &mut out)
                            .unwrap();
                    } else {
                        t.lookup_batch_into(&q.n6[off..off + size], &mut out)
                            .unwrap();
                    }
                    black_box(&out);
                })
            });
            g.bench_function(BenchmarkId::new("numeric-ids", kernel), |bench| {
                bench.iter(|| {
                    window(&mut off, size);
                    if fam == "ipv4" {
                        t.lookup_ids_into(&q.n4[off..off + size], &mut ids).unwrap();
                    } else {
                        t.lookup_ids_into(&q.n6[off..off + size], &mut ids).unwrap();
                    }
                    black_box(&ids);
                })
            });
            g.bench_function(BenchmarkId::new("loop-of-singles", SINGLE), |bench| {
                bench.iter(|| {
                    window(&mut off, size);
                    if fam == "ipv4" {
                        for (o, &a) in out.iter_mut().zip(&q.v4[off..off + size]) {
                            *o = t.lookup_ipv4(a);
                        }
                    } else {
                        for (o, &a) in out.iter_mut().zip(&q.v6[off..off + size]) {
                            *o = t.lookup_ipv6(a);
                        }
                    }
                    black_box(&out);
                })
            });
            g.finish();
        }
    }

    // ------------------------------------------------------------ huge pages
    {
        drop(table);
        let q = q_of(Workload::Hits);
        let size = 256;
        for huge in [true, false] {
            let t = fixture.table(Config::new().huge_pages(huge));
            let name = if huge { "on" } else { "off" };
            for fam in ["ipv4", "ipv6"] {
                let mut g = c.benchmark_group(format!("huge-pages/{fam}/hit/{size}"));
                g.throughput(Throughput::Elements(size as u64));
                let mut out = vec![None; size];
                let mut off = 0usize;
                g.bench_function(BenchmarkId::new(name, kernel), |bench| {
                    bench.iter(|| {
                        window(&mut off, size);
                        if fam == "ipv4" {
                            t.lookup_batch_into(&q.v4[off..off + size], &mut out)
                                .unwrap();
                        } else {
                            t.lookup_batch_into(&q.v6[off..off + size], &mut out)
                                .unwrap();
                        }
                        black_box(&out);
                    })
                });
                let mut i = 0usize;
                g.bench_function(
                    BenchmarkId::new(format!("{name}-single"), SINGLE),
                    |bench| {
                        bench.iter(|| {
                            i = (i + 1) & (POOL - 1);
                            if fam == "ipv4" {
                                black_box(t.lookup_ipv4(q.v4[i]).is_some())
                            } else {
                                black_box(t.lookup_ipv6(q.v6[i]).is_some())
                            }
                        })
                    },
                );
                g.finish();
            }
        }
    }
}

criterion_group! {
    name = lookup;
    config = configure();
    targets = benches
}
criterion_main!(lookup);
