//! Corpora of `bench-compare-lib`: generated once per (family, size, seed,
//! query count), cached, and read by every runner (Rust, Go, C) in the same
//! language-neutral binary layout (see `benchmarks/compare-lib/README.md`):
//!
//! * `prefixes.bin`: the input networks with their record ids (`index + 1`:
//!   unique, deterministic, non-zero);
//! * `q-<scenario>.bin`: query addresses with the expected record id of the
//!   longest match (0: no match), computed by the independent oracle of the
//!   shared generator (`benches/support/data.rs`);
//! * `corpus.json`: metadata (hashes, hit rates, length distribution).
//!
//! Routes come from the shared generator (distinct BGP-like prefixes,
//! nested networks, host routes, no `/0`). Default routes and duplicates are
//! only in the dedicated edge corpora ([`ensure_edge`]).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::dataset::Family;
use crate::dataset::data::{self, Addr, Rng, Workload};

/// Bump when the corpus content or layout changes (part of the cache key).
pub const CORPUS_VERSION: u32 = 1;
const PREFIX_MAGIC: &[u8; 8] = b"DLPCPF01";
const QUERY_MAGIC: &[u8; 8] = b"DLPCQF01";
/// Measured query scenarios.
pub const SCENARIOS: [&str; 4] = ["mixed", "hits", "misses", "local"];
/// Locality scenario: share of queries drawn from the hot set.
pub const HOT_PERMILLE: u64 = 900;
/// Locality scenario: distinct addresses in the hot set.
pub const HOT_SET: usize = 1024;
/// Sampled prefixes whose boundaries go into `q-validate.bin`.
const VALIDATE_PREFIXES: usize = 8192;
/// Corpora up to this size are also checked against a linear-scan oracle.
const NAIVE_CHECK_MAX: usize = 20_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub records: usize,
    pub bytes: u64,
    /// FNV-1a 64 of the file, hex.
    pub fnv64: String,
    /// Share of queries with a match (query files).
    pub hit_rate: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CorpusInfo {
    /// Cache directory name, also the corpus identifier.
    pub id: String,
    pub family: Family,
    /// Requested and actual number of input prefixes.
    pub size: usize,
    pub prefixes: usize,
    pub seed: u64,
    pub queries: usize,
    /// `"main"` or `"edge:<case>"`.
    pub kind: String,
    pub corpus_version: u32,
    pub generator_version: u32,
    pub files: Vec<FileInfo>,
    /// `(prefix length, count)`.
    pub lengths: Vec<(u8, usize)>,
    /// `/32` or `/128` routes.
    pub host_routes: usize,
    /// Prefixes contained in another prefix of the corpus.
    pub nested: usize,
    pub nested_sampled: bool,
    /// Queries also checked against the linear-scan oracle (0: none).
    pub naive_checked: usize,
    pub generate_s: f64,
    #[serde(default)]
    pub cached: bool,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl CorpusInfo {
    pub fn file(&self, name: &str) -> Option<&FileInfo> {
        self.files.iter().find(|f| f.name == name)
    }
    /// Hash of the input prefixes (identity of the table).
    pub fn prefix_hash(&self) -> &str {
        self.file("prefixes.bin").map_or("", |f| &f.fnv64)
    }
}

pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn header(magic: &[u8; 8], family: Family, count: usize, record: usize) -> Vec<u8> {
    let mut h = Vec::with_capacity(32);
    h.extend_from_slice(magic);
    h.extend(1u32.to_le_bytes());
    h.extend(fam_num(family).to_le_bytes());
    h.extend((count as u64).to_le_bytes());
    h.extend((record as u32).to_le_bytes());
    h.extend(0u32.to_le_bytes());
    h
}

fn fam_num(f: Family) -> u32 {
    match f {
        Family::Ipv4 => 4,
        Family::Ipv6 => 6,
    }
}

/// Network-order bytes of an address.
fn addr_bytes<A: Addr>(a: A, out: &mut Vec<u8>) {
    let w = usize::from(A::BITS / 8);
    let v = a.to_u128() << (128 - u32::from(A::BITS));
    out.extend_from_slice(&v.to_be_bytes()[..w]);
}

fn write_file(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    hit_rate: Option<f64>,
    records: usize,
) -> io::Result<FileInfo> {
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(name))?;
    Ok(FileInfo {
        name: name.into(),
        records,
        bytes: bytes.len() as u64,
        fnv64: format!("{:016x}", fnv64(bytes)),
        hit_rate,
    })
}

/// `prefixes.bin` from `(network, length, id)`.
fn prefix_file<A: Addr>(
    dir: &Path,
    family: Family,
    routes: &[(A, u8, u32)],
) -> io::Result<FileInfo> {
    let w = usize::from(A::BITS / 8);
    let mut b = header(PREFIX_MAGIC, family, routes.len(), w + 8);
    b.reserve(routes.len() * (w + 8));
    for &(net, len, id) in routes {
        addr_bytes(net, &mut b);
        b.extend([len, 0, 0, 0]);
        b.extend(id.to_le_bytes());
    }
    write_file(dir, "prefixes.bin", &b, None, routes.len())
}

/// `q-<name>.bin` from addresses and expected ids.
fn query_file<A: Addr>(
    dir: &Path,
    family: Family,
    name: &str,
    qs: &[A],
    expected: &[u32],
) -> io::Result<FileInfo> {
    let w = usize::from(A::BITS / 8);
    let mut b = header(QUERY_MAGIC, family, qs.len(), w + 4);
    b.reserve(qs.len() * (w + 4));
    for (&a, &e) in qs.iter().zip(expected) {
        addr_bytes(a, &mut b);
        b.extend(e.to_le_bytes());
    }
    let hits = expected.iter().filter(|&&e| e != 0).count();
    let rate = if qs.is_empty() {
        0.0
    } else {
        hits as f64 / qs.len() as f64
    };
    write_file(dir, &format!("q-{name}.bin"), &b, Some(rate), qs.len())
}

fn last_of<A: Addr>(net: A, len: u8) -> A {
    let all = u128::MAX >> (128 - u32::from(A::BITS));
    let host = if len >= A::BITS { 0 } else { all >> len };
    A::from_u128(net.to_u128() | host)
}

/// Longest match by linear scan; among equal prefixes the last one wins.
fn naive<A: Addr>(routes: &[(A, u8, u32)], a: A) -> u32 {
    let mut best: Option<(u8, u32)> = None;
    for &(net, len, id) in routes {
        if data::network(a, len) == net && best.is_none_or(|(l, _)| len >= l) {
            best = Some((len, id));
        }
    }
    best.map_or(0, |b| b.1)
}

/// First, last, previous and next address of sampled prefixes, plus the
/// first and last address of the family.
fn boundary_queries<A: Addr>(routes: &[(A, u8, u32)], limit: usize) -> Vec<A> {
    let step = routes.len().div_ceil(limit.max(1)).max(1);
    let max = u128::MAX >> (128 - u32::from(A::BITS));
    let mut out = vec![A::from_u128(0), A::from_u128(max)];
    for &(net, len, _) in routes.iter().step_by(step) {
        let first = net.to_u128();
        let last = last_of(net, len).to_u128();
        out.push(net);
        out.push(A::from_u128(last));
        if first > 0 {
            out.push(A::from_u128(first - 1));
        }
        if last < max {
            out.push(A::from_u128(last + 1));
        }
    }
    out
}

/// 90 % of the queries from a hot set of 1 024 addresses (drawn like the
/// mixed scenario), 10 % from the mixed stream.
fn locality<A: Addr>(t: &data::Table<A>, count: usize, seed: u64) -> Vec<A> {
    let hot = data::queries(t, Workload::RANDOM, HOT_SET, seed ^ 0x10CA_1000);
    let cold = data::queries(t, Workload::RANDOM, count, seed ^ 0x10CA_2000);
    let mut rng = Rng::new(seed ^ 0x10CA_3000);
    cold.into_iter()
        .map(|c| {
            if rng.below(1000) < HOT_PERMILLE {
                hot[rng.below(HOT_SET as u64) as usize]
            } else {
                c
            }
        })
        .collect()
}

fn length_stats<A: Addr>(routes: &[(A, u8, u32)]) -> (Vec<(u8, usize)>, usize, usize, bool) {
    let mut counts = vec![0usize; usize::from(A::BITS) + 1];
    for r in routes {
        counts[usize::from(r.1)] += 1;
    }
    let lengths: Vec<(u8, usize)> = counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(l, c)| (l as u8, *c))
        .collect();
    let hosts = counts[usize::from(A::BITS)];
    let set: HashSet<(u128, u8)> = routes.iter().map(|r| (r.0.to_u128(), r.1)).collect();
    let present: Vec<u8> = lengths.iter().map(|l| l.0).collect();
    let sample = 200_000;
    let sampled = routes.len() > sample;
    let step = if sampled { routes.len() / sample } else { 1 };
    let mut nested = 0;
    let mut seen = 0;
    for &(net, len, _) in routes.iter().step_by(step) {
        seen += 1;
        if present
            .iter()
            .any(|&l| l < len && set.contains(&(data::network(net, l).to_u128(), l)))
        {
            nested += 1;
        }
    }
    if sampled {
        nested = nested * routes.len() / seen.max(1);
    }
    (lengths, hosts, nested, sampled)
}

fn generate<A: Addr>(
    dir: &Path,
    family: Family,
    size: usize,
    seed: u64,
    queries: usize,
) -> io::Result<CorpusInfo> {
    let t0 = Instant::now();
    let t = data::table::<A>(size, seed);
    let routes: Vec<(A, u8, u32)> = t
        .routes
        .iter()
        .enumerate()
        .map(|(i, &(net, len))| (net, len, i as u32 + 1))
        .collect();
    let expect = |qs: &[A]| -> Vec<u32> {
        qs.iter()
            .map(|&a| t.oracle.lookup(a).map_or(0, |i| i + 1))
            .collect()
    };
    let mut files = vec![prefix_file(dir, family, &routes)?];
    let mut naive_checked = 0;
    let mut scenario = |name: &str, qs: Vec<A>, files: &mut Vec<FileInfo>| -> io::Result<()> {
        let e = expect(&qs);
        match name {
            "hits" => assert!(e.iter().all(|&x| x != 0), "a hit query has no match"),
            "misses" => assert!(e.iter().all(|&x| x == 0), "a miss query is covered"),
            _ => {}
        }
        if routes.len() <= NAIVE_CHECK_MAX {
            let n = if name == "validate" {
                qs.len()
            } else {
                qs.len().min(2048)
            };
            for i in 0..n {
                assert_eq!(
                    naive(&routes, qs[i]),
                    e[i],
                    "oracles disagree on {:?}",
                    qs[i]
                );
            }
            naive_checked += n;
        }
        files.push(query_file(dir, family, name, &qs, &e)?);
        Ok(())
    };
    scenario(
        "mixed",
        data::queries(&t, Workload::RANDOM, queries, seed),
        &mut files,
    )?;
    scenario(
        "hits",
        data::queries(&t, Workload::Hits, queries, seed),
        &mut files,
    )?;
    scenario(
        "misses",
        data::queries(&t, Workload::Misses, queries, seed),
        &mut files,
    )?;
    scenario("local", locality(&t, queries, seed), &mut files)?;
    scenario(
        "validate",
        boundary_queries(&routes, VALIDATE_PREFIXES),
        &mut files,
    )?;
    let (lengths, host_routes, nested, nested_sampled) = length_stats(&routes);
    Ok(CorpusInfo {
        id: String::new(),
        family,
        size,
        prefixes: routes.len(),
        seed,
        queries,
        kind: "main".into(),
        corpus_version: CORPUS_VERSION,
        generator_version: data::GENERATOR_VERSION,
        files,
        lengths,
        host_routes,
        nested,
        nested_sampled,
        naive_checked,
        generate_s: t0.elapsed().as_secs_f64(),
        cached: false,
        dir: dir.to_path_buf(),
    })
}

/// Reuses a cached corpus when its metadata and every file hash match.
fn reuse(dir: &Path) -> Option<CorpusInfo> {
    let meta: CorpusInfo =
        serde_json::from_slice(&std::fs::read(dir.join("corpus.json")).ok()?).ok()?;
    for f in &meta.files {
        let b = std::fs::read(dir.join(&f.name)).ok()?;
        if format!("{:016x}", fnv64(&b)) != f.fnv64 {
            return None;
        }
    }
    Some(CorpusInfo {
        cached: true,
        dir: dir.to_path_buf(),
        ..meta
    })
}

fn store(dir: &Path, info: &mut CorpusInfo) -> io::Result<()> {
    info.id = dir.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(dir.join("corpus.json"), serde_json::to_vec_pretty(info)?)
}

/// The main corpus of `(family, size)`, generated or reused.
pub fn ensure(
    root: &Path,
    family: Family,
    size: usize,
    seed: u64,
    queries: usize,
) -> io::Result<CorpusInfo> {
    assert!(
        queries.is_power_of_two(),
        "the query count must be a power of two"
    );
    let id = format!(
        "{}-{size}-s{seed}-q{queries}-c{CORPUS_VERSION}g{}",
        family.name(),
        data::GENERATOR_VERSION
    );
    let dir = root.join(&id);
    if let Some(info) = reuse(&dir) {
        return Ok(info);
    }
    std::fs::create_dir_all(&dir)?;
    let mut info = match family {
        Family::Ipv4 => generate::<u32>(&dir, family, size, seed, queries)?,
        Family::Ipv6 => generate::<u128>(&dir, family, size, seed, queries)?,
    };
    store(&dir, &mut info)?;
    Ok(info)
}

/// Edge cases: `nested` (nested networks, first and last addresses, host
/// routes, misses), `default` (the same plus the family's `/0`, which covers
/// every address) and `duplicates` (two CIDRs inserted twice with different
/// ids: the last insertion is expected to win).
pub const EDGE_CASES: [&str; 3] = ["nested", "default", "duplicates"];

fn edge_routes(family: Family, case: &str) -> Vec<(u128, u8, u32)> {
    let v4 = |s: &str| u128::from(u32::from(s.parse::<std::net::Ipv4Addr>().unwrap()));
    let v6 = |s: &str| u128::from(s.parse::<std::net::Ipv6Addr>().unwrap());
    let mut r: Vec<(u128, u8, u32)> = match family {
        Family::Ipv4 => [
            ("10.0.0.0", 8),
            ("10.1.0.0", 16),
            ("10.1.2.0", 24),
            ("10.1.2.3", 32),
            ("10.1.2.128", 25),
            ("192.0.2.0", 24),
            ("192.0.2.255", 32),
            ("203.0.113.0", 31),
            ("0.0.0.0", 32),
            ("255.255.255.255", 32),
            ("172.16.0.0", 12),
            ("172.31.255.0", 24),
        ]
        .iter()
        .map(|&(a, l)| (v4(a), l))
        .zip(1..)
        .map(|((a, l), id)| (a, l, id))
        .collect(),
        Family::Ipv6 => [
            ("2001:db8::", 32),
            ("2001:db8:1::", 48),
            ("2001:db8:1:2::", 64),
            ("2001:db8:1:2::3", 128),
            ("2001:db8:1:2:8000::", 65),
            ("2400:cb00::", 32),
            ("2400:cb00:ffff:ffff:ffff:ffff:ffff:ffff", 128),
            ("2a00::", 12),
            ("::", 128),
            ("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", 128),
            ("fe80::", 10),
            ("2001:db8:1:2:ffff::", 80),
        ]
        .iter()
        .map(|&(a, l)| (v6(a), l))
        .zip(1..)
        .map(|((a, l), id)| (a, l, id))
        .collect(),
    };
    let next = r.len() as u32 + 1;
    match case {
        "default" => r.push((0, 0, next)),
        "duplicates" => {
            let (a, l, _) = r[1];
            let (b, m, _) = r[6];
            r.push((a, l, next));
            r.push((b, m, next + 1));
        }
        _ => {}
    }
    r
}

fn edge_misses(family: Family) -> Vec<u128> {
    match family {
        Family::Ipv4 => ["11.0.0.1", "8.8.8.8", "192.0.3.0", "100.64.0.1"]
            .iter()
            .map(|s| u128::from(u32::from(s.parse::<std::net::Ipv4Addr>().unwrap())))
            .collect(),
        Family::Ipv6 => ["2001:db9::1", "3000::1", "::1", "fc00::1"]
            .iter()
            .map(|s| u128::from(s.parse::<std::net::Ipv6Addr>().unwrap()))
            .collect(),
    }
}

fn generate_edge<A: Addr>(dir: &Path, family: Family, case: &str) -> io::Result<CorpusInfo> {
    let routes: Vec<(A, u8, u32)> = edge_routes(family, case)
        .into_iter()
        .map(|(a, l, id)| (A::from_u128(a), l, id))
        .collect();
    let mut qs = boundary_queries(&routes, usize::MAX);
    qs.extend(edge_misses(family).into_iter().map(A::from_u128));
    let expected: Vec<u32> = qs.iter().map(|&a| naive(&routes, a)).collect();
    let files = vec![
        prefix_file(dir, family, &routes)?,
        query_file(dir, family, "validate", &qs, &expected)?,
    ];
    let (lengths, host_routes, nested, nested_sampled) = length_stats(&routes);
    Ok(CorpusInfo {
        id: String::new(),
        family,
        size: routes.len(),
        prefixes: routes.len(),
        seed: 0,
        queries: qs.len(),
        kind: format!("edge:{case}"),
        corpus_version: CORPUS_VERSION,
        generator_version: data::GENERATOR_VERSION,
        files,
        lengths,
        host_routes,
        nested,
        nested_sampled,
        naive_checked: qs.len(),
        generate_s: 0.0,
        cached: false,
        dir: dir.to_path_buf(),
    })
}

/// The edge corpus `case` of `family` (always regenerated: it is tiny).
pub fn ensure_edge(root: &Path, family: Family, case: &str) -> io::Result<CorpusInfo> {
    let dir = root.join(format!("edge-{}-{case}-c{CORPUS_VERSION}", family.name()));
    std::fs::create_dir_all(&dir)?;
    let mut info = match family {
        Family::Ipv4 => generate_edge::<u32>(&dir, family, case)?,
        Family::Ipv6 => generate_edge::<u128>(&dir, family, case)?,
    };
    store(&dir, &mut info)?;
    Ok(info)
}

/// Canonical form of a textual address for the corpus: IPv4-mapped IPv6
/// addresses (`::ffff:a.b.c.d`) are IPv4 addresses.
pub fn normalize(addr: std::net::IpAddr) -> std::net::IpAddr {
    match addr {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(addr, std::net::IpAddr::V4),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_oracle_prefers_longest_then_last() {
        let r: Vec<(u32, u8, u32)> = vec![
            (0x0A00_0000, 8, 1),
            (0x0A01_0000, 16, 2),
            (0x0A01_0000, 16, 3),
        ];
        assert_eq!(naive(&r, 0x0A01_0203), 3);
        assert_eq!(naive(&r, 0x0A02_0000), 1);
        assert_eq!(naive(&r, 0x0B00_0000), 0);
    }

    #[test]
    fn normalization_unmaps_ipv4_mapped_addresses() {
        let mapped: std::net::IpAddr = "::ffff:10.1.2.3".parse().unwrap();
        assert_eq!(
            normalize(mapped),
            "10.1.2.3".parse::<std::net::IpAddr>().unwrap()
        );
        let v6: std::net::IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(normalize(v6), v6);
    }

    #[test]
    fn corpus_files_follow_the_layout() {
        let dir = std::env::temp_dir().join(format!("libcmp-corpus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let info = ensure(&dir, Family::Ipv4, 500, 7, 1024).unwrap();
        assert_eq!(info.prefixes, 500);
        let b = std::fs::read(info.dir.join("prefixes.bin")).unwrap();
        assert_eq!(&b[..8], PREFIX_MAGIC);
        assert_eq!(b.len(), 32 + 500 * 12);
        assert_eq!(
            u32::from_le_bytes(b[32 + 8..32 + 12].try_into().unwrap()),
            1,
            "first id"
        );
        assert_eq!(info.file("q-hits.bin").unwrap().hit_rate, Some(1.0));
        assert_eq!(info.file("q-misses.bin").unwrap().hit_rate, Some(0.0));
        let mixed = info.file("q-mixed.bin").unwrap().hit_rate.unwrap();
        assert!((0.45..0.55).contains(&mixed), "{mixed}");
        assert!(info.naive_checked > 0);
        // Reuse validates the hashes.
        assert!(ensure(&dir, Family::Ipv4, 500, 7, 1024).unwrap().cached);
        for case in EDGE_CASES {
            ensure_edge(&dir, Family::Ipv6, case).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
