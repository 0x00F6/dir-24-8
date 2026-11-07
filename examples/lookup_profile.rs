//! Profiling workload of every public lookup method, IPv4 and IPv6, on the
//! BGP-like tables of the benchmarks (`benches/support/data.rs`): `make
//! flamegraph` records it with `perf` and draws the flame graph.
//!
//! ```text
//! cargo run --release --example lookup_profile                         # short run, checks results
//! cargo run --release --features batch-simd --example lookup_profile -- 2.0  # 2 s per method
//! ```
//!
//! Each method runs in its own non-inlined function so that it gets its own
//! frame in the profile; batch results are checked against single lookups
//! before anything is timed.

#[path = "../benches/support/data.rs"]
mod data;

use std::hint::black_box;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use dir_24_8::{IpTable, LookupMatch, MatchId, Replace};

const QUERIES: usize = 1 << 16;
const BATCH: usize = 256;

struct Queries {
    v4: Vec<Ipv4Addr>,
    v6: Vec<Ipv6Addr>,
    n4: Vec<u32>,
    n6: Vec<u128>,
    mixed: Vec<IpAddr>,
}

/// Runs `f` for `budget`, at least once; returns the number of calls.
fn repeat(budget: Duration, f: &mut dyn FnMut()) -> u64 {
    let start = Instant::now();
    let mut calls = 0;
    loop {
        f();
        calls += 1;
        if start.elapsed() >= budget {
            return calls;
        }
    }
}

#[inline(never)]
fn single_lookup(t: &IpTable<u32>, q: &Queries) -> usize {
    q.mixed
        .iter()
        .filter(|&&ip| black_box(t.lookup(ip)).is_some())
        .count()
}

#[inline(never)]
fn single_lookup_ipv4(t: &IpTable<u32>, q: &Queries) -> usize {
    q.v4.iter()
        .filter(|&&a| black_box(t.lookup_ipv4(a)).is_some())
        .count()
}

#[inline(never)]
fn single_lookup_ipv6(t: &IpTable<u32>, q: &Queries) -> usize {
    q.v6.iter()
        .filter(|&&a| black_box(t.lookup_ipv6(a)).is_some())
        .count()
}

#[inline(never)]
fn single_lookup_data(t: &IpTable<u32>, q: &Queries) -> usize {
    q.mixed
        .iter()
        .filter(|&&ip| black_box(t.lookup_data(ip)).is_some())
        .count()
}

#[inline(never)]
fn single_exist(t: &IpTable<u32>, q: &Queries) -> usize {
    q.mixed.iter().filter(|&&ip| black_box(t.exist(ip))).count()
}

#[inline(never)]
fn batch_lookup_ipv4<'a>(
    t: &'a IpTable<u32>,
    q: &Queries,
    out: &mut [Option<LookupMatch<'a, u32>>],
) {
    for keys in q.v4.chunks(BATCH) {
        t.lookup_batch_into(keys, &mut out[..keys.len()]).unwrap();
        black_box(&*out);
    }
}

#[inline(never)]
fn batch_lookup_ipv6<'a>(
    t: &'a IpTable<u32>,
    q: &Queries,
    out: &mut [Option<LookupMatch<'a, u32>>],
) {
    for keys in q.v6.chunks(BATCH) {
        t.lookup_batch_into(keys, &mut out[..keys.len()]).unwrap();
        black_box(&*out);
    }
}

#[inline(never)]
fn batch_lookup_mixed<'a>(
    t: &'a IpTable<u32>,
    q: &Queries,
    out: &mut [Option<LookupMatch<'a, u32>>],
) {
    for keys in q.mixed.chunks(BATCH) {
        t.lookup_batch_into(keys, &mut out[..keys.len()]).unwrap();
        black_box(&*out);
    }
}

#[inline(never)]
fn batch_lookup_array(t: &IpTable<u32>, q: &Queries) -> usize {
    q.n4.as_chunks::<64>()
        .0
        .iter()
        .map(|keys| {
            black_box(t.lookup_array(keys))
                .iter()
                .filter(|m| m.is_some())
                .count()
        })
        .sum()
}

#[inline(never)]
fn batch_exist(t: &IpTable<u32>, q: &Queries, out: &mut [bool]) {
    for keys in q.mixed.chunks(BATCH) {
        t.exist_batch_into(keys, &mut out[..keys.len()]).unwrap();
        black_box(&*out);
    }
}

#[inline(never)]
fn batch_exist_array(t: &IpTable<u32>, q: &Queries) -> usize {
    q.n6.as_chunks::<64>()
        .0
        .iter()
        .map(|keys| {
            black_box(t.exist_array(keys))
                .iter()
                .filter(|&&e| e)
                .count()
        })
        .sum()
}

#[inline(never)]
fn batch_ids_and_resolve<'a>(t: &'a IpTable<u32>, q: &Queries, ids: &mut [MatchId<'a>]) -> usize {
    let mut hits = 0;
    for keys in q.n4.chunks(BATCH) {
        let ids = &mut ids[..keys.len()];
        t.lookup_ids_into(keys, ids).unwrap();
        hits += ids
            .iter()
            .filter(|&&id| black_box(t.resolve(id)).is_some())
            .count();
    }
    hits
}

fn main() {
    let secs: f64 = std::env::args()
        .nth(1)
        .map_or(0.02, |s| s.parse().expect("seconds per method"));
    let budget = Duration::from_secs_f64(secs);

    let t4 = data::table::<u32>(100_000, 1);
    let t6 = data::table::<u128>(50_000, 2);
    let mut table = IpTable::new();
    for (i, n) in data::v4_nets(&t4.routes).into_iter().enumerate() {
        table.insert(n, data::payload(i), Replace).unwrap();
    }
    for (i, n) in data::v6_nets(&t6.routes).into_iter().enumerate() {
        table.insert(n, data::payload(i), Replace).unwrap();
    }
    let n4 = data::queries(&t4, data::Workload::RANDOM, QUERIES, 7);
    let n6 = data::queries(&t6, data::Workload::RANDOM, QUERIES, 8);
    let v4: Vec<Ipv4Addr> = n4.iter().map(|&a| Ipv4Addr::from(a)).collect();
    let v6: Vec<Ipv6Addr> = n6.iter().map(|&a| Ipv6Addr::from(a)).collect();
    let mixed = (0..QUERIES)
        .map(|i| {
            if i % 2 == 0 {
                IpAddr::V4(v4[i])
            } else {
                IpAddr::V6(v6[i])
            }
        })
        .collect();
    let q = Queries {
        v4,
        v6,
        n4,
        n6,
        mixed,
    };
    println!(
        "{} routes, batch kernel {}, {secs} s per method",
        table.len(),
        table.stats().batch_kernel
    );

    // Batches (the batch kernel) must agree with single lookups (scalar).
    let mut out = vec![None; BATCH];
    let mut exist = vec![false; BATCH];
    let mut ids = vec![MatchId::NONE; BATCH];
    for keys in q.mixed.chunks(BATCH).take(16) {
        table
            .lookup_batch_into(keys, &mut out[..keys.len()])
            .unwrap();
        table
            .exist_batch_into(keys, &mut exist[..keys.len()])
            .unwrap();
        for (i, &ip) in keys.iter().enumerate() {
            let single = table.lookup(ip);
            assert_eq!(
                out[i].map(|m| (m.prefix, m.data)),
                single.map(|m| (m.prefix, m.data))
            );
            assert_eq!(exist[i], single.is_some());
        }
    }
    let hits = single_lookup(&table, &q);
    assert!(
        hits > QUERIES / 4 && hits < QUERIES * 3 / 4,
        "about half hits: {hits}"
    );
    assert_eq!(
        batch_ids_and_resolve(&table, &q, &mut ids),
        single_lookup_ipv4(&table, &q)
    );

    let run = |name: &str, f: &mut dyn FnMut()| {
        let calls = repeat(budget, f);
        println!("  {name:<32} {calls} passes of {QUERIES} addresses");
    };
    run("lookup", &mut || {
        black_box(single_lookup(&table, &q));
    });
    run("lookup_ipv4", &mut || {
        black_box(single_lookup_ipv4(&table, &q));
    });
    run("lookup_ipv6", &mut || {
        black_box(single_lookup_ipv6(&table, &q));
    });
    run("lookup_data", &mut || {
        black_box(single_lookup_data(&table, &q));
    });
    run("exist", &mut || {
        black_box(single_exist(&table, &q));
    });
    run("lookup_batch_into (Ipv4Addr)", &mut || {
        batch_lookup_ipv4(&table, &q, &mut out)
    });
    run("lookup_batch_into (Ipv6Addr)", &mut || {
        batch_lookup_ipv6(&table, &q, &mut out)
    });
    run("lookup_batch_into (IpAddr)", &mut || {
        batch_lookup_mixed(&table, &q, &mut out)
    });
    run("lookup_array (u32)", &mut || {
        black_box(batch_lookup_array(&table, &q));
    });
    run("exist_batch_into (IpAddr)", &mut || {
        batch_exist(&table, &q, &mut exist)
    });
    run("exist_array (u128)", &mut || {
        black_box(batch_exist_array(&table, &q));
    });
    run("lookup_ids_into + resolve (u32)", &mut || {
        black_box(batch_ids_and_resolve(&table, &q, &mut ids));
    });
}
