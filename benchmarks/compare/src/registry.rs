//! Every comparable structure, in a fixed order (the order also fixes each
//! structure's color in the report). The default selection keeps one
//! representative per algorithm family; `--engines all` runs every entry.

use serde::{Deserialize, Serialize};

use crate::adapter::{Api, Engine, Lpm, Member, OURS_SCALAR, Ours, ScalarBatches, Simd};
use crate::bench::{self, RunCfg, RunResult};
use crate::dataset::{Dataset, Family};
use crate::engines::{
    BinTrie, DenseBitmap, HashLpmFx, HashLpmStd, HeadBin16, HeadBin18, HeadBin20, HeadBin24,
    IpNetworkTableEngine, IptrieLc, IptrieRadix, Patricia, PatriciaTreeCrate, Poptrie16, Poptrie18,
    PrefixTrieEngine, Radix4, Radix8, RadixTrieCrate, RangeMapEngine, Roaring, SortedIntervals,
    TreeBitmap, TreeBitmapCrate,
};

type Run4 = fn(&Dataset<u32>, &RunCfg) -> RunResult;
type Run6 = fn(&Dataset<u128>, &RunCfg) -> RunResult;

/// Catalog row (also serialized into the report).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineInfo {
    pub name: String,
    /// Position in the catalog (color slot in the report).
    pub index: usize,
    pub default: bool,
    pub families: Vec<Family>,
    pub lookup: Api,
    pub batch: Api,
    pub exists: Api,
    /// Results resolved through the shared entry table (counted in memory).
    pub entry_table: bool,
    pub algorithm: String,
    pub config: String,
    pub source: String,
    pub updates: String,
}

pub struct Entry {
    pub info: EngineInfo,
    run4: Run4,
    run6: Run6,
}

impl Entry {
    fn new<A: Engine<u32>, B: Engine<u128>>(default: bool, families: &[Family]) -> Entry {
        let name = A::name();
        let (algorithm, config, source, updates) = describe(&name);
        Entry {
            info: EngineInfo {
                name,
                index: 0,
                default,
                families: families.to_vec(),
                lookup: A::LOOKUP,
                batch: A::BATCH,
                exists: A::EXISTS,
                entry_table: A::ENTRY_TABLE,
                algorithm,
                config,
                source,
                updates,
            },
            run4: bench::run::<u32, A>,
            run6: bench::run::<u128, B>,
        }
    }

    pub fn run4(&self, d: &Dataset<u32>, c: &RunCfg) -> RunResult {
        (self.run4)(d, c)
    }

    pub fn run6(&self, d: &Dataset<u128>, c: &RunCfg) -> RunResult {
        (self.run6)(d, c)
    }

    /// Reason this entry cannot run on this machine / family.
    pub fn unavailable(&self, family: Family) -> Option<String> {
        if !self.info.families.contains(&family) {
            return Some(format!(
                "{}-only structure",
                self.info
                    .families
                    .iter()
                    .map(|f| f.label())
                    .collect::<Vec<_>>()
                    .join("/")
            ));
        }
        if self.info.name == OURS_SCALAR && crate::adapter::batch_kernel() != "scalar" {
            // Measured by the scalar build of this binary.
            if let Err(reason) = scalar_exe() {
                return Some(reason);
            }
        }
        None
    }
}

fn describe(name: &str) -> (String, String, String, String) {
    if name.starts_with("dir-24-8") {
        let kernels = if name == OURS_SCALAR {
            "built without the batch-simd feature: scalar kernels for batches (two-phase \
             blocks) and single lookups"
                .to_string()
        } else {
            format!(
                "batch-simd feature: batches through the kernel detected on this CPU ({}), \
                 single lookups through the scalar kernels",
                crate::adapter::batch_kernel()
            )
        };
        return (
            "DIR-24-8 (IPv4) + 24/8 multi-level trie (IPv6)".into(),
            format!("{kernels}; IpTable<u32>, public API, default configuration"),
            "this crate".into(),
            "incremental".into(),
        );
    }
    match crate::engines::catalog()
        .into_iter()
        .find(|c| c.name == name)
    {
        Some(c) => (
            c.algorithm.to_string(),
            c.config,
            c.source.to_string(),
            c.updates.to_string(),
        ),
        None => (String::new(), String::new(), String::new(), String::new()),
    }
}

const BOTH: &[Family] = &[Family::Ipv4, Family::Ipv6];
const V4: &[Family] = &[Family::Ipv4];

/// The full catalog, in color order.
pub fn catalog() -> Vec<Entry> {
    let mut v = vec![
        // Default selection (the first 8 get the solid colors, the next
        // ones the same hues dashed).
        Entry::new::<Ours<Simd>, Ours<Simd>>(true, BOTH),
        Entry::new::<Lpm<Poptrie18<u32>>, Lpm<Poptrie18<u128>>>(true, BOTH),
        Entry::new::<Lpm<IpNetworkTableEngine<u32>>, Lpm<IpNetworkTableEngine<u128>>>(true, BOTH),
        Entry::new::<Lpm<PrefixTrieEngine<u32>>, Lpm<PrefixTrieEngine<u128>>>(true, BOTH),
        Entry::new::<Lpm<SortedIntervals<u32>>, Lpm<SortedIntervals<u128>>>(true, BOTH),
        Entry::new::<Lpm<HashLpmFx<u32>>, Lpm<HashLpmFx<u128>>>(true, BOTH),
        Entry::new::<Member<Roaring<u32>>, Member<Roaring<u128>>>(true, V4),
        Entry::new::<Ours<ScalarBatches>, Ours<ScalarBatches>>(true, BOTH),
        Entry::new::<Lpm<Patricia<u32>>, Lpm<Patricia<u128>>>(true, BOTH),
        Entry::new::<Lpm<RadixTrieCrate<u32>>, Lpm<RadixTrieCrate<u128>>>(true, BOTH),
        Entry::new::<Lpm<IptrieLc<u32>>, Lpm<IptrieLc<u128>>>(true, BOTH),
        Entry::new::<Lpm<RangeMapEngine<u32>>, Lpm<RangeMapEngine<u128>>>(true, BOTH),
        Entry::new::<Lpm<PatriciaTreeCrate<u32>>, Lpm<PatriciaTreeCrate<u128>>>(true, BOTH),
        // On request (`--engines all` or by name).
        Entry::new::<Lpm<IptrieRadix<u32>>, Lpm<IptrieRadix<u128>>>(false, BOTH),
        Entry::new::<Lpm<TreeBitmapCrate<u32>>, Lpm<TreeBitmapCrate<u128>>>(false, BOTH),
        Entry::new::<Lpm<TreeBitmap<u32>>, Lpm<TreeBitmap<u128>>>(false, BOTH),
        Entry::new::<Lpm<Poptrie16<u32>>, Lpm<Poptrie16<u128>>>(false, BOTH),
        Entry::new::<Lpm<Radix4<u32>>, Lpm<Radix4<u128>>>(false, BOTH),
        Entry::new::<Lpm<Radix8<u32>>, Lpm<Radix8<u128>>>(false, BOTH),
        Entry::new::<Lpm<BinTrie<u32>>, Lpm<BinTrie<u128>>>(false, BOTH),
        Entry::new::<Lpm<HeadBin24<u32>>, Lpm<HeadBin24<u128>>>(false, BOTH),
        Entry::new::<Lpm<HeadBin20<u32>>, Lpm<HeadBin20<u128>>>(false, BOTH),
        Entry::new::<Lpm<HeadBin18<u32>>, Lpm<HeadBin18<u128>>>(false, BOTH),
        Entry::new::<Lpm<HeadBin16<u32>>, Lpm<HeadBin16<u128>>>(false, BOTH),
        Entry::new::<Lpm<HashLpmStd<u32>>, Lpm<HashLpmStd<u128>>>(false, BOTH),
        Entry::new::<Member<DenseBitmap<u32>>, Member<DenseBitmap<u128>>>(false, V4),
    ];
    for (i, e) in v.iter_mut().enumerate() {
        e.info.index = i;
    }
    v
}

/// Executable that measures the `dir-24-8[scalar]` entry when this binary
/// batches with SIMD: `BENCH_SCALAR_EXE`, or `bench-compare-scalar` next to
/// this executable (the Makefile builds it without the `batch-simd`
/// feature).
pub fn scalar_exe() -> Result<std::path::PathBuf, String> {
    let path = match std::env::var_os("BENCH_SCALAR_EXE") {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_exe()
            .map_err(|e| format!("current executable: {e}"))?
            .with_file_name(format!(
                "bench-compare-scalar{}",
                std::env::consts::EXE_SUFFIX
            )),
    };
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "scalar build {} missing (`make bench-compare` builds it)",
            path.display()
        ))
    }
}

/// Entries selected by `spec`: `default`, `all`, or comma-separated
/// substrings of names (may be combined with `default`).
pub fn select(spec: &[String]) -> Vec<Entry> {
    let all = catalog();
    if spec.is_empty() {
        return all.into_iter().filter(|e| e.info.default).collect();
    }
    all.into_iter()
        .filter(|e| {
            spec.iter().any(|s| match s.as_str() {
                "all" => true,
                "default" => e.info.default,
                s => e.info.name == s || e.info.name.contains(s),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both dir-24-8 variants (batch kernel of this build, scalar
    /// batches) are benchmarked by default.
    #[test]
    fn both_dir_24_8_variants_are_benchmarked() {
        let c = catalog();
        for name in ["dir-24-8", OURS_SCALAR] {
            let e = c
                .iter()
                .find(|e| e.info.name == name)
                .unwrap_or_else(|| panic!("no benchmark entry for {name}"));
            assert!(e.info.default, "{name} is not in the default selection");
            assert_eq!(e.info.batch, Api::Native);
        }
    }

    #[test]
    fn catalog_is_complete_and_ordered() {
        let c = catalog();
        let names: Vec<&str> = c.iter().map(|e| e.info.name.as_str()).collect();
        assert_eq!(names[0], "dir-24-8");
        let defaults = c.iter().filter(|e| e.info.default).count();
        assert_eq!(defaults, 13);
        assert!(c.iter().take(defaults).all(|e| e.info.default));
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
        // Every competitor of the engines module is registered.
        for e in crate::engines::catalog() {
            assert!(names.contains(&e.name.as_str()), "{} missing", e.name);
        }
        for e in &c {
            assert!(!e.info.algorithm.is_empty(), "{}", e.info.name);
        }
        assert_eq!(select(&[]).len(), 13);
        assert_eq!(select(&["all".into()]).len(), c.len());
        let s = select(&["default".into(), "iptrie-radix".into()]);
        assert_eq!(s.len(), 14);
    }
}
