//! Criterion benchmarks of construction and updates, plus a memory
//! footprint report (printed before the measurements).
//!
//! Groups:
//!
//! * `build/{ipv4,ipv6}/{n}`: empty table to `n` BGP-like routes, including
//!   the lazily zeroed 64 MiB root of the family.
//! * `insert/...`, `remove/...`: one route of a given shape into / out of a
//!   populated table (timed with `iter_custom`, the inverse operation is
//!   done outside the timed section).
//! * `replace/...`: payload replacement of an existing CIDR.
//! * `merge/...`: typed deep merge (and serde deep merge with
//!   `--features serde`).
//!
//! Environment: `BENCH_V4_ROUTES`, `BENCH_V6_ROUTES`, `BENCH_TIME`.

#[path = "support/data.rs"]
mod data;

use std::collections::BTreeMap;
use std::hint::black_box;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dir_24_8::{IpNet, IpTable, Ipv4Net, Ipv6Net};
use dir_24_8::{Merge, Replace};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn configure() -> Criterion {
    let secs: f64 = std::env::var("BENCH_TIME")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.5);
    Criterion::default()
        .warm_up_time(Duration::from_secs_f64((secs / 3.0).max(0.2)))
        .measurement_time(Duration::from_secs_f64(secs))
        .sample_size(20)
        .configure_from_args()
}

fn build_v4(routes: &[Ipv4Net]) -> IpTable<u32> {
    let mut t = IpTable::new();
    for (i, n) in routes.iter().enumerate() {
        t.insert(*n, data::payload(i), Replace).unwrap();
    }
    t
}

fn build_v6(routes: &[Ipv6Net]) -> IpTable<u32> {
    let mut t = IpTable::new();
    for (i, n) in routes.iter().enumerate() {
        t.insert(*n, data::payload(i), Replace).unwrap();
    }
    t
}

fn mib(b: usize) -> f64 {
    b as f64 / (1 << 20) as f64
}

fn footprint() {
    eprintln!("memory footprint (allocated/reserved bytes; tbl24 is lazily zeroed):");
    eprintln!(
        "{:<8} {:>8} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "family",
        "routes",
        "groups",
        "tbl24 MiB",
        "tbl8 MiB",
        "store MiB",
        "index MiB",
        "total MiB"
    );
    for n in [1_000usize, 10_000, 100_000, 200_000] {
        let t = build_v4(&data::ipv4_routes(n, 1));
        let s = t.stats().ipv4;
        eprintln!(
            "{:<8} {:>8} {:>8} {:>10.1} {:>10.2} {:>10.2} {:>10.2} {:>10.1}",
            "ipv4",
            s.routes,
            s.groups_in_use,
            mib(s.tbl24_bytes),
            mib(s.tbl8_bytes),
            mib(s.route_store_bytes),
            mib(s.index_bytes),
            mib(s.total_bytes())
        );
    }
    for n in [1_000usize, 10_000, 100_000] {
        let t = build_v6(&data::ipv6_routes(n, 2));
        let s = t.stats().ipv6;
        eprintln!(
            "{:<8} {:>8} {:>8} {:>10.1} {:>10.2} {:>10.2} {:>10.2} {:>10.1}",
            "ipv6",
            s.routes,
            s.groups_in_use,
            mib(s.tbl24_bytes),
            mib(s.tbl8_bytes),
            mib(s.route_store_bytes),
            mib(s.index_bytes),
            mib(s.total_bytes())
        );
    }
}

/// Times `op` on `count` fresh routes, undoing the effect untimed.
fn timed_pairs(t: &mut IpTable<u32>, nets: &[IpNet], iters: u64, insert_timed: bool) -> Duration {
    let mut total = Duration::ZERO;
    let mut i = 0usize;
    while (i as u64) < iters {
        let chunk = &nets[..nets.len().min((iters - i as u64) as usize)];
        if insert_timed {
            let start = Instant::now();
            for n in chunk {
                black_box(t.insert(*n, Some(1), Replace).unwrap());
            }
            total += start.elapsed();
            for n in chunk {
                t.remove(*n);
            }
        } else {
            for n in chunk {
                t.insert(*n, Some(1), Replace).unwrap();
            }
            let start = Instant::now();
            for n in chunk {
                black_box(t.remove(*n));
            }
            total += start.elapsed();
        }
        i += chunk.len();
    }
    total
}

fn fresh_v4(t: &IpTable<u32>, len: u8, count: usize, seed: u64) -> Vec<IpNet> {
    let mut rng = data::Rng::new(seed);
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let a = (1 + rng.below(223) as u32) << 24 | (rng.next_u64() as u32 & 0x00FF_FFFF);
        let n = IpNet::V4(Ipv4Net::new(Ipv4Addr::from(a), len).unwrap().trunc());
        if !t.contains_prefix(n) && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

fn fresh_v6(t: &IpTable<u32>, len: u8, count: usize, seed: u64) -> Vec<IpNet> {
    let mut rng = data::Rng::new(seed);
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let a = (0x2u128 << 124) | (rng.u128() >> 3);
        let n = IpNet::V6(Ipv6Net::new(Ipv6Addr::from(a), len).unwrap().trunc());
        if !t.contains_prefix(n) && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

fn benches(c: &mut Criterion) {
    footprint();
    let n4 = env_usize("BENCH_V4_ROUTES", 200_000);
    let n6 = env_usize("BENCH_V6_ROUTES", 100_000);

    // ----------------------------------------------------------------- build
    for n in [10_000usize, n4.min(100_000)] {
        let routes = data::ipv4_routes(n, 1);
        let mut g = c.benchmark_group("build/ipv4");
        g.throughput(Throughput::Elements(routes.len() as u64));
        g.sample_size(10);
        g.bench_function(BenchmarkId::from_parameter(n), |b| {
            b.iter(|| black_box(build_v4(&routes)))
        });
        g.finish();
    }
    for n in [10_000usize, n6.min(100_000)] {
        let routes = data::ipv6_routes(n, 2);
        let mut g = c.benchmark_group("build/ipv6");
        g.throughput(Throughput::Elements(routes.len() as u64));
        g.sample_size(10);
        g.bench_function(BenchmarkId::from_parameter(n), |b| {
            b.iter(|| black_box(build_v6(&routes)))
        });
        g.finish();
    }

    // ------------------------------------------------------ insert / remove
    let mut t = build_v4(&data::ipv4_routes(n4, 1));
    for (i, n) in data::ipv6_routes(n6, 2).iter().enumerate() {
        t.insert(*n, data::payload(i), Replace).unwrap();
    }
    let shapes: Vec<(&str, Vec<IpNet>)> = vec![
        ("ipv4-/24", fresh_v4(&t, 24, 256, 10)),
        ("ipv4-/16", fresh_v4(&t, 16, 64, 11)),
        ("ipv4-/28-new-group", fresh_v4(&t, 28, 256, 12)),
        ("ipv4-/8", fresh_v4(&t, 8, 8, 13)),
        ("ipv6-/48", fresh_v6(&t, 48, 256, 14)),
        ("ipv6-/64", fresh_v6(&t, 64, 256, 15)),
        ("ipv6-/128", fresh_v6(&t, 128, 256, 16)),
        ("ipv6-/20", fresh_v6(&t, 20, 16, 17)),
    ];
    for (name, nets) in &shapes {
        let mut g = c.benchmark_group(format!("insert/{name}"));
        g.throughput(Throughput::Elements(1));
        g.bench_function("populated", |b| {
            b.iter_custom(|iters| timed_pairs(&mut t, nets, iters, true))
        });
        g.finish();
        let mut g = c.benchmark_group(format!("remove/{name}"));
        g.throughput(Throughput::Elements(1));
        g.bench_function("populated", |b| {
            b.iter_custom(|iters| timed_pairs(&mut t, nets, iters, false))
        });
        g.finish();
    }

    // --------------------------------------------------------------- replace
    {
        let existing: Vec<IpNet> = t.iter().map(|(n, _)| n).step_by(97).take(1024).collect();
        let mut g = c.benchmark_group("replace");
        g.throughput(Throughput::Elements(1));
        let mut i = 0usize;
        g.bench_function("existing-cidr", |b| {
            b.iter(|| {
                i = (i + 1) % existing.len();
                black_box(t.replace(existing[i], Some(i as u32)))
            })
        });
        g.finish();
    }
    drop(t);

    // ----------------------------------------------------------------- merge
    {
        let routes = data::ipv4_routes(10_000, 3);
        let mut t: IpTable<BTreeMap<u32, Vec<u32>>> = IpTable::new();
        for (i, n) in routes.iter().enumerate() {
            t.insert(
                *n,
                Some(BTreeMap::from([(0, vec![i as u32]), (1, vec![])])),
                Replace,
            )
            .unwrap();
        }
        let mut g = c.benchmark_group("merge");
        g.throughput(Throughput::Elements(1));
        let mut i = 0usize;
        let mut j = 0u32;
        g.bench_function("typed-btreemap", |b| {
            b.iter_batched(
                || {
                    j = j.wrapping_add(1);
                    BTreeMap::from([(1, Vec::new()), (2 + j % 4, Vec::new())])
                },
                |incoming| {
                    i = (i + 1) % routes.len();
                    black_box(t.insert(routes[i], Some(incoming), Merge).unwrap())
                },
                BatchSize::SmallInput,
            )
        });
        g.bench_function("incoming-none-unchanged", |b| {
            b.iter(|| {
                i = (i + 1) % routes.len();
                black_box(t.insert(routes[i], None, Merge).unwrap())
            })
        });
        g.finish();
    }
    #[cfg(feature = "serde")]
    {
        let routes = data::ipv4_routes(10_000, 4);
        let mut t: IpTable<serde_json::Value> = IpTable::new();
        for (i, n) in routes.iter().enumerate() {
            t.insert(
                *n,
                Some(serde_json::json!({"asn": i, "tags": ["a"], "geo": {"cc": "FR"}})),
                Replace,
            )
            .unwrap();
        }
        let mut g = c.benchmark_group("merge");
        g.throughput(Throughput::Elements(1));
        let mut i = 0usize;
        g.bench_function("serde-json-value", |b| {
            b.iter_batched(
                || serde_json::json!({"asn": 7, "geo": {"city": "Lyon"}}),
                |incoming| {
                    i = (i + 1) % routes.len();
                    black_box(t.insert(routes[i], Some(incoming), Merge).unwrap())
                },
                BatchSize::SmallInput,
            )
        });
        g.finish();
    }
}

criterion_group! {
    name = update;
    config = configure();
    targets = benches
}
criterion_main!(update);
