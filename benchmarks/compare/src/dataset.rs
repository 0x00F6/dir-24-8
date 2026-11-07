//! Datasets: routes, query sets and expected results, generated once per
//! (family, size, parameters) and cached on disk.
//!
//! Generation uses the shared generator (`benches/support/data.rs`, also
//! used by the Criterion benches) and its oracle. A cache file is keyed by
//! the generator version and every parameter, so a run reuses it only when
//! nothing changed. Each measuring process loads the file instead of
//! regenerating (and re-verifying) the data.
//!
//! File layout (little endian): magic `D248DS01`, `u32` header length, JSON
//! header ([`DatasetMeta`]), routes (`key`, `len: u8`, `payload: u32`,
//! `u32::MAX` = none), then for each distribution `queries` keys followed
//! by `queries` expected route indexes (`u32::MAX` = no match).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[path = "../../../benches/support/data.rs"]
#[allow(clippy::all)]
pub mod data;

use crate::adapter::Resolved;
use crate::core::{Key, Route};
use dir_24_8::{IpNet, Ipv4Net, Ipv6Net};

const MAGIC: &[u8; 8] = b"D248DS01";
/// "No route" marker of the expected results.
pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Ipv4,
    Ipv6,
}

impl Family {
    pub const ALL: [Family; 2] = [Family::Ipv4, Family::Ipv6];
    pub fn name(self) -> &'static str {
        match self {
            Family::Ipv4 => "ipv4",
            Family::Ipv6 => "ipv6",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Family::Ipv4 => "IPv4",
            Family::Ipv6 => "IPv6",
        }
    }
    pub fn parse(s: &str) -> Option<Family> {
        Self::ALL.into_iter().find(|f| f.name() == s)
    }
}

/// Query distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dist {
    /// Hits and misses shuffled (`hit_permille` hits).
    Random,
    /// Only addresses covered by a route.
    Hits,
    /// Only addresses covered by no route.
    Misses,
}

impl Dist {
    pub const ALL: [Dist; 3] = [Dist::Random, Dist::Hits, Dist::Misses];
    pub fn name(self) -> &'static str {
        match self {
            Dist::Random => "random",
            Dist::Hits => "hits",
            Dist::Misses => "misses",
        }
    }
    pub fn parse(s: &str) -> Option<Dist> {
        Self::ALL.into_iter().find(|d| d.name() == s)
    }
    pub fn index(self) -> usize {
        self as usize
    }
    fn workload(self, hit_permille: u32) -> data::Workload {
        match self {
            Dist::Random => data::Workload::Random { hit_permille },
            Dist::Hits => data::Workload::Hits,
            Dist::Misses => data::Workload::Misses,
        }
    }
}

/// Generation parameters (part of the cache key).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Params {
    pub seed: u64,
    pub queries: usize,
    pub hit_permille: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatasetMeta {
    pub family: Family,
    /// Requested number of entries.
    pub size: usize,
    /// Distinct prefixes actually stored.
    pub entries: usize,
    /// Queries per distribution.
    pub queries: usize,
    pub hit_permille: u32,
    pub seed: u64,
    pub generator_version: u32,
    /// Measured share of covered addresses per distribution (from the
    /// oracle's expected results), in `Dist::ALL` order.
    pub hit_ratio: Vec<f64>,
    /// `(length, routes)` for every present prefix length.
    pub lengths: Vec<(u8, usize)>,
    /// Routes without payload.
    pub without_payload: usize,
    /// Generation + verification time (seconds).
    pub generate_s: f64,
    pub file_bytes: u64,
    /// `true` when this run reused the cache file (set by the orchestrator).
    #[serde(default)]
    pub cached: bool,
    /// Load time of the file in the measuring process (set by the
    /// orchestrator from the first run).
    #[serde(default)]
    pub load_s: Option<f64>,
}

/// A loaded dataset.
pub struct Dataset<K> {
    pub meta: DatasetMeta,
    pub routes: Vec<Route<K>>,
    /// Public form of every route (prefix + payload), indexed by route id.
    pub resolved: Vec<Resolved>,
    /// Query keys per distribution (`Dist::ALL` order).
    pub queries: Vec<Vec<K>>,
    /// Expected route index per query (`NONE` = no match).
    pub expected: Vec<Vec<u32>>,
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xCBF2_9CE4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3)
    })
}

/// Cache file of a dataset.
pub fn cache_path(dir: &Path, family: Family, size: usize, p: &Params) -> PathBuf {
    let key = format!(
        "v{};{};{};seed={};q={};hit={}",
        data::GENERATOR_VERSION,
        family.name(),
        size,
        p.seed,
        p.queries,
        p.hit_permille
    );
    dir.join(format!("{}-{}-{:016x}.bin", family.name(), size, fnv(&key)))
}

/// Generates, verifies and writes a dataset.
pub fn prepare(family: Family, size: usize, p: &Params, path: &Path) -> std::io::Result<()> {
    match family {
        Family::Ipv4 => prepare_k::<u32>(family, size, p, path),
        Family::Ipv6 => prepare_k::<u128>(family, size, p, path),
    }
}

fn prepare_k<A: data::Addr + Key>(
    family: Family,
    size: usize,
    p: &Params,
    path: &Path,
) -> std::io::Result<()> {
    let t0 = Instant::now();
    let table = data::table::<A>(size, p.seed);
    let mut queries = Vec::new();
    let mut expected = Vec::new();
    let mut hit_ratio = Vec::new();
    for d in Dist::ALL {
        let q = data::queries(&table, d.workload(p.hit_permille), p.queries, p.seed);
        let e: Vec<u32> = q
            .iter()
            .map(|&a| table.oracle.lookup(a).unwrap_or(NONE))
            .collect();
        let hits = e.iter().filter(|&&x| x != NONE).count();
        match d {
            Dist::Hits => assert_eq!(hits, q.len(), "every hit must be covered"),
            Dist::Misses => assert_eq!(hits, 0, "no miss may be covered"),
            Dist::Random => {}
        }
        hit_ratio.push(hits as f64 / q.len().max(1) as f64);
        queries.push(q);
        expected.push(e);
    }
    let mut counts = vec![0usize; usize::from(<A as data::Addr>::BITS) + 1];
    for &(_, l) in &table.routes {
        counts[usize::from(l)] += 1;
    }
    let mut meta = DatasetMeta {
        family,
        size,
        entries: table.routes.len(),
        queries: p.queries,
        hit_permille: p.hit_permille,
        seed: p.seed,
        generator_version: data::GENERATOR_VERSION,
        hit_ratio,
        lengths: counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(l, &c)| (l as u8, c))
            .collect(),
        without_payload: (0..table.routes.len())
            .filter(|&i| data::payload(i).is_none())
            .count(),
        generate_s: 0.0,
        file_bytes: 0,
        cached: false,
        load_s: None,
    };
    meta.generate_s = t0.elapsed().as_secs_f64();

    let kb = (<A as Key>::BITS / 8) as usize;
    let mut body =
        Vec::with_capacity(table.routes.len() * (kb + 5) + Dist::ALL.len() * p.queries * (kb + 4));
    let put_key = |v: &mut Vec<u8>, a: A| v.extend_from_slice(&Key::to_u128(a).to_le_bytes()[..kb]);
    for (i, &(net, len)) in table.routes.iter().enumerate() {
        put_key(&mut body, net);
        body.push(len);
        body.extend_from_slice(&data::payload(i).unwrap_or(NONE).to_le_bytes());
    }
    for (q, e) in queries.iter().zip(&expected) {
        for &a in q {
            put_key(&mut body, a);
        }
        for &x in e {
            body.extend_from_slice(&x.to_le_bytes());
        }
    }
    let header_len = serde_json::to_vec(&meta).expect("serializable").len();
    meta.file_bytes = (MAGIC.len() + 4 + header_len + body.len()) as u64;
    // The header length may change by a few digits once `file_bytes` is
    // set; recompute until stable.
    let mut header = serde_json::to_vec(&meta).expect("serializable");
    while (MAGIC.len() + 4 + header.len() + body.len()) as u64 != meta.file_bytes {
        meta.file_bytes = (MAGIC.len() + 4 + header.len() + body.len()) as u64;
        header = serde_json::to_vec(&meta).expect("serializable");
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    f.write_all(MAGIC)?;
    f.write_all(&(header.len() as u32).to_le_bytes())?;
    f.write_all(&header)?;
    f.write_all(&body)?;
    f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Reads only the header of a cache file.
pub fn read_meta(path: &Path) -> std::io::Result<DatasetMeta> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head)?;
    if &head[..8] != MAGIC {
        return Err(std::io::Error::other("not a dataset file"));
    }
    let n = u32::from_le_bytes(head[8..12].try_into().expect("4 bytes")) as usize;
    let mut h = vec![0u8; n];
    f.read_exact(&mut h)?;
    serde_json::from_slice(&h).map_err(std::io::Error::other)
}

fn to_net<K: Key>(net: K, len: u8) -> IpNet {
    if K::BITS == 32 {
        IpNet::V4(Ipv4Net::new((net.to_u128() as u32).into(), len).expect("valid length"))
    } else {
        IpNet::V6(Ipv6Net::new(net.to_u128().into(), len).expect("valid length"))
    }
}

/// Loads a dataset of key type `K`.
pub fn load<K: Key>(path: &Path) -> std::io::Result<Dataset<K>> {
    let bytes = std::fs::read(path)?;
    let bad = |m: &str| std::io::Error::other(format!("{}: {m}", path.display()));
    if bytes.len() < 12 || &bytes[..8] != MAGIC {
        return Err(bad("not a dataset file"));
    }
    let hl = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")) as usize;
    let meta: DatasetMeta =
        serde_json::from_slice(&bytes[12..12 + hl]).map_err(std::io::Error::other)?;
    let kb = (K::BITS / 8) as usize;
    let mut at = 12 + hl;
    let key = |b: &[u8]| {
        let mut w = [0u8; 16];
        w[..kb].copy_from_slice(b);
        K::from_u128(u128::from_le_bytes(w))
    };
    let need = meta.entries * (kb + 5) + Dist::ALL.len() * meta.queries * (kb + 4);
    if bytes.len() != at + need {
        return Err(bad("truncated or inconsistent file"));
    }
    let mut routes = Vec::with_capacity(meta.entries);
    let mut resolved = Vec::with_capacity(meta.entries);
    for _ in 0..meta.entries {
        let net = key(&bytes[at..at + kb]);
        let len = bytes[at + kb];
        let pl = u32::from_le_bytes(bytes[at + kb + 1..at + kb + 5].try_into().expect("4"));
        at += kb + 5;
        let payload = (pl != NONE).then_some(pl);
        routes.push(Route { net, len, payload });
        resolved.push(Resolved {
            prefix: to_net(net, len),
            payload,
        });
    }
    let mut queries = Vec::new();
    let mut expected = Vec::new();
    for _ in Dist::ALL {
        let q: Vec<K> = bytes[at..at + meta.queries * kb]
            .chunks_exact(kb)
            .map(key)
            .collect();
        at += meta.queries * kb;
        let e: Vec<u32> = bytes[at..at + meta.queries * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        at += meta.queries * 4;
        queries.push(q);
        expected.push(e);
    }
    Ok(Dataset {
        meta,
        routes,
        resolved,
        queries,
        expected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_expected_results() {
        let dir = std::env::temp_dir().join(format!("d248-ds-{}", std::process::id()));
        let p = Params {
            seed: 3,
            queries: 4096,
            hit_permille: 500,
        };
        for fam in Family::ALL {
            let path = cache_path(&dir, fam, 2000, &p);
            prepare(fam, 2000, &p, &path).unwrap();
            let meta = read_meta(&path).unwrap();
            assert_eq!(meta.entries, 2000);
            assert_eq!(meta.file_bytes, std::fs::metadata(&path).unwrap().len());
            assert_eq!(meta.hit_ratio[Dist::Hits.index()], 1.0);
            assert_eq!(meta.hit_ratio[Dist::Misses.index()], 0.0);
            let r = meta.hit_ratio[Dist::Random.index()];
            assert!((r - 0.5).abs() < 0.01, "{r}");
            match fam {
                Family::Ipv4 => check::<u32>(&path),
                Family::Ipv6 => check::<u128>(&path),
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    fn check<K: Key + data::Addr>(path: &Path) {
        let d = load::<K>(path).unwrap();
        let routes: Vec<(K, u8)> = d.routes.iter().map(|r| (r.net, r.len)).collect();
        let oracle = data::Oracle::from_routes(&routes);
        for (q, e) in d.queries.iter().zip(&d.expected) {
            assert_eq!(q.len(), 4096);
            for (&k, &x) in q.iter().zip(e) {
                assert_eq!(oracle.lookup(k).unwrap_or(NONE), x);
            }
        }
        for (i, r) in d.routes.iter().enumerate() {
            assert_eq!(r.payload, data::payload(i));
            assert_eq!(d.resolved[i].prefix, to_net(r.net, r.len));
        }
    }
}
