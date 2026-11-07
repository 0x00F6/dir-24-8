//! Snapshot measurements: compressed size, save time, load time and memory
//! (feature `snapshot`, custom harness: compression runs take seconds, so
//! each configuration is timed a few times and the median is reported).
//!
//! ```text
//! make bench BENCHES=snapshot
//! make bench BENCHES=snapshot SNAPSHOT_SIZES=100k,1M SNAPSHOT_PROFILES=9e,9e+sorted,6 SNAPSHOT_REPS=3
//! ```
//!
//! For every family, size and profile: the shared generator builds a
//! BGP-like table (`benches/support/data.rs`, payload `Option<u32>`: three
//! routes out of four carry their index), the table is saved to a
//! `Vec<u8>` and loaded back `SNAPSHOT_REPS` times, and the restored table
//! is checked against the original (routes and 20 000 lookups) before the
//! numbers are printed.
//!
//! Memory (Linux, from `/proc/self/status`; before each operation freed heap
//! memory is returned with `malloc_trim` and the peak is reset through
//! `/proc/self/clear_refs`; the largest value over the repetitions is
//! reported):
//!
//! * `table` - resident memory of the table built by inserting the routes
//!   (`build` is the time of those insertions, for comparison with `load`);
//! * `save +peak` - peak resident memory added by the save, on top of the
//!   table: encoder state and the compressed output;
//! * `build +peak` - transient memory of building the table by insertion,
//!   above the finished table (the group buffers grow by reallocation);
//! * `load +peak` - transient memory of the load above the restored table:
//!   the same growth plus the decoder state and buffers.
//!
//! Profiles (`SNAPSHOT_PROFILES`, `+`-separated tokens): `<level>` (0-9),
//! `<level>e` (with `PRESET_EXTREME`), `sorted` (`EntryOrder::Sorted`),
//! `t<n>` (threads), `b<MiB>` (block size). Default: `9e,9e+sorted,6`.
//! Other variables: `SNAPSHOT_SIZES` (default `10k,100k,1M`),
//! `SNAPSHOT_FAMILIES` (`ipv4,ipv6`), `SNAPSHOT_REPS` (default 3). A
//! command-line argument filters the configurations by name.

#[path = "support/data.rs"]
mod data;

use std::hint::black_box;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use data::{Addr, Workload};
use dir_24_8::{EntryOrder, IpNet, IpTable, LoadOptions, Replace, SnapshotOptions};

fn env(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn parse_size(s: &str) -> usize {
    let s = s.trim().to_ascii_lowercase();
    let (digits, mult) = match s.strip_suffix('m') {
        Some(d) => (d, 1_000_000),
        None => match s.strip_suffix('k') {
            Some(d) => (d, 1_000),
            None => (s.as_str(), 1),
        },
    };
    digits.parse::<usize>().expect("size such as 10k or 1M") * mult
}

fn parse_profile(spec: &str) -> SnapshotOptions {
    let mut o = SnapshotOptions::new();
    for token in spec.split('+') {
        if token == "sorted" {
            o = o.order(EntryOrder::Sorted);
        } else if let Some(n) = token.strip_prefix('t') {
            o = o.threads(n.parse().expect("t<threads>"));
        } else if let Some(n) = token.strip_prefix('b') {
            o = o.block_size(n.parse::<u64>().expect("b<MiB>") << 20);
        } else if let Some(level) = token.strip_suffix('e') {
            o = o.level(level.parse().expect("<level>e")).extreme(true);
        } else {
            o = o.level(token.parse().expect("<level>")).extreme(false);
        }
    }
    o
}

/// `(VmRSS, VmHWM)` in bytes (Linux).
fn rss() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|l| l.starts_with(name))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

/// Returns freed heap memory to the system (so that resident memory
/// deltas are not hidden by the allocator's caches), then resets the peak.
fn reset_peak() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only releases free heap pages; it has no
    // preconditions.
    unsafe {
        libc::malloc_trim(0);
    }
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "n/a".into(),
        |b| format!("{:.1} MiB", b as f64 / (1 << 20) as f64),
    )
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn secs(d: Duration) -> String {
    if d.as_secs_f64() >= 1.0 {
        format!("{:.2} s", d.as_secs_f64())
    } else {
        format!("{:.1} ms", d.as_secs_f64() * 1e3)
    }
}

fn to_ip<A: Addr>(a: A) -> IpAddr {
    let v = a.to_u128();
    if A::BITS == 32 {
        IpAddr::V4((v as u32).into())
    } else {
        IpAddr::V6(v.into())
    }
}

struct Fixture {
    table: IpTable<u32>,
    queries: Vec<IpAddr>,
    build: Duration,
    table_rss: Option<u64>,
    build_peak: Option<u64>,
}

fn fixture<A: Addr>(n: usize, nets: impl Fn(&[(A, u8)]) -> Vec<IpNet>) -> Fixture {
    let t = data::table::<A>(n, 0x5eed);
    let queries = data::queries(&t, Workload::Random { hit_permille: 500 }, 20_000, 7)
        .into_iter()
        .map(to_ip)
        .collect();
    let nets = nets(&t.routes);
    reset_peak();
    let before = rss();
    let t0 = Instant::now();
    let mut table = IpTable::new();
    for (i, net) in nets.into_iter().enumerate() {
        table.insert(net, data::payload(i), Replace).unwrap();
    }
    let build = t0.elapsed();
    let after = rss();
    Fixture {
        table,
        queries,
        build,
        table_rss: before.zip(after).map(|(b, a)| a.0.saturating_sub(b.0)),
        build_peak: after.map(|a| a.1.saturating_sub(a.0)),
    }
}

fn check(original: &IpTable<u32>, restored: &IpTable<u32>, queries: &[IpAddr]) {
    assert_eq!(original.len_ipv4(), restored.len_ipv4());
    assert_eq!(original.len_ipv6(), restored.len_ipv6());
    for (prefix, data) in original.iter().step_by(7) {
        assert_eq!(restored.get(prefix), Some(data), "{prefix}");
    }
    for &q in queries {
        assert_eq!(original.lookup(q), restored.lookup(q), "{q}");
    }
}

fn main() {
    let filter: Option<String> = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let sizes: Vec<usize> = env("BENCH_SNAPSHOT_SIZES", "10k,100k,1M")
        .split(',')
        .map(parse_size)
        .collect();
    let profiles: Vec<String> = env("BENCH_SNAPSHOT_PROFILES", "9e,9e+sorted,6")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let families = env("BENCH_SNAPSHOT_FAMILIES", "ipv4,ipv6");
    let reps: usize = env("BENCH_SNAPSHOT_REPS", "3")
        .parse()
        .expect("SNAPSHOT_REPS");
    let reps = reps.max(1);

    println!(
        "| family | routes | profile | raw | compressed | B/route | ratio | save | load | build | encoder mem | save +peak | table | build +peak | load +peak |"
    );
    println!("|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for family in families.split(',').map(str::trim) {
        for &n in &sizes {
            let names: Vec<String> = profiles
                .iter()
                .map(|p| format!("{family}/{n}/{p}"))
                .filter(|name| filter.as_ref().is_none_or(|f| name.contains(f.as_str())))
                .collect();
            if names.is_empty() {
                continue;
            }
            let fx = match family {
                "ipv4" => {
                    fixture::<u32>(n, |r| data::v4_nets(r).into_iter().map(IpNet::V4).collect())
                }
                "ipv6" => {
                    fixture::<u128>(n, |r| data::v6_nets(r).into_iter().map(IpNet::V6).collect())
                }
                other => panic!("unknown family {other}"),
            };
            for profile in profiles
                .iter()
                .filter(|p| names.iter().any(|n| n.ends_with(&format!("/{p}"))))
            {
                let options = parse_profile(profile);
                let mut save_times = Vec::new();
                let mut load_times = Vec::new();
                let mut save_peak = None;
                let mut load_peak = None;
                let mut summary = None;
                let mut bytes = Vec::new();
                for rep in 0..reps {
                    drop(std::mem::take(&mut bytes));
                    reset_peak();
                    let before = rss();
                    let t0 = Instant::now();
                    let mut out = Vec::new();
                    let s = fx.table.snapshot_to_writer(&mut out, &options).unwrap();
                    save_times.push(t0.elapsed());
                    let after = rss();
                    let peak = before.zip(after).map(|(b, a)| a.1.saturating_sub(b.0));
                    save_peak = save_peak.max(peak);
                    summary = Some(s);
                    bytes = out;

                    reset_peak();
                    let t0 = Instant::now();
                    let restored: IpTable<u32> =
                        IpTable::load_with_options(black_box(&bytes), &LoadOptions::new()).unwrap();
                    load_times.push(t0.elapsed());
                    let after = rss();
                    load_peak = load_peak.max(after.map(|a| a.1.saturating_sub(a.0)));
                    if rep == 0 {
                        check(&fx.table, &restored, &fx.queries);
                    }
                    drop(restored);
                }
                let s = summary.expect("at least one repetition");
                let routes = fx.table.len();
                println!(
                    "| {family} | {routes} | {profile} | {} | {} | {:.2} | {:.1} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    s.uncompressed_bytes,
                    s.compressed_bytes,
                    s.compressed_bytes as f64 / routes as f64,
                    s.uncompressed_bytes as f64 / s.compressed_bytes as f64,
                    secs(median(save_times)),
                    secs(median(load_times)),
                    secs(fx.build),
                    mib(Some(s.encoder_memory)),
                    mib(save_peak),
                    mib(fx.table_rss),
                    mib(fx.build_peak),
                    mib(load_peak),
                );
            }
        }
    }
}
