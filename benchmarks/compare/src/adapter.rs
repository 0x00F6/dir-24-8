//! One monomorphized measurement interface over every structure.
//!
//! All engines produce the same public result, `Option<LookupMatch<'_,
//! u32>>` (winning prefix + borrowed payload). Competitors map an address
//! to a route index and resolve it through the shared entry table
//! ([`Resolved`]: one random read, the same cost class as dir-24-8's route
//! slot read); that table is counted in their memory footprint because
//! dir-24-8 stores the same prefixes and payloads inside its structure.

use std::marker::PhantomData;
use std::net::{Ipv4Addr, Ipv6Addr};

use dir_24_8::{IpNet, IpTable, Ipv4Net, Ipv6Net, LookupMatch};
use serde::{Deserialize, Serialize};

use crate::core::{Key, LpmEngine, MembershipEngine, Route, RouteId};
use dir_24_8::Replace;

/// How an engine performs an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Api {
    /// The structure's own API for this operation.
    Native,
    /// No batch API: the harness calls the single lookup in a loop.
    Loop,
    /// Dedicated existence API (no result materialized).
    Dedicated,
    /// Existence derived from the lookup (`lookup(ip).is_some()`).
    Derived,
    /// The structure cannot answer this operation.
    Unsupported,
}

/// Public form of a route: what a full lookup returns.
#[derive(Clone, Copy, Debug)]
pub struct Resolved {
    pub prefix: IpNet,
    pub payload: Option<u32>,
}

impl Resolved {
    #[inline(always)]
    pub fn as_match(&self) -> LookupMatch<'_, u32> {
        LookupMatch {
            prefix: self.prefix,
            data: self.payload.as_ref(),
        }
    }
}

pub trait Engine<K: Key>: Sized + Sync {
    /// Lookup support (`Native` or `Unsupported`).
    const LOOKUP: Api;
    /// Batch support (`Native`, `Loop` or `Unsupported`).
    const BATCH: Api;
    /// Existence support (`Dedicated` or `Derived`).
    const EXISTS: Api;
    /// Results are materialized through the shared entry table.
    const ENTRY_TABLE: bool;

    fn name() -> String;
    /// `Err(reason)` when the engine cannot run here (missing CPU feature,
    /// family not supported).
    fn applicable() -> Result<(), String> {
        Ok(())
    }
    fn build(routes: &[Route<K>]) -> Self;
    /// The structure's own accounting of its memory (cross-check only),
    /// `None` when it has none.
    fn reported_bytes(&self) -> Option<usize>;
    /// Bytes mapped outside the global allocator (`mmap`).
    fn mapped_bytes(&self) -> usize {
        0
    }
    /// Kernels actually used (dir-24-8 only).
    fn kernel_detail(&self) -> Option<String> {
        None
    }
    fn lookup<'a>(&'a self, key: K, res: &'a [Resolved]) -> Option<LookupMatch<'a, u32>>;
    fn lookup_batch<'a>(
        &'a self,
        keys: &[K],
        res: &'a [Resolved],
        out: &mut [Option<LookupMatch<'a, u32>>],
    );
    fn exists(&self, key: K) -> bool;
}

#[inline(always)]
fn resolve(id: Option<RouteId>, res: &[Resolved]) -> Option<LookupMatch<'_, u32>> {
    id.map(|i| res[i as usize].as_match())
}

/// Keys per call of a competitor's `lookup_batch` (stack buffer).
const ID_CHUNK: usize = 256;

/// A longest-prefix-match competitor.
pub struct Lpm<E>(E);

impl<K: Key, E: LpmEngine<K> + Sync> Engine<K> for Lpm<E> {
    const LOOKUP: Api = Api::Native;
    const BATCH: Api = Api::Loop;
    const EXISTS: Api = Api::Derived;
    const ENTRY_TABLE: bool = true;

    fn name() -> String {
        E::name()
    }
    fn build(routes: &[Route<K>]) -> Self {
        Lpm(E::build(routes))
    }
    fn reported_bytes(&self) -> Option<usize> {
        E::REPORTS_MEMORY.then(|| self.0.memory_bytes())
    }
    #[inline]
    fn lookup<'a>(&'a self, key: K, res: &'a [Resolved]) -> Option<LookupMatch<'a, u32>> {
        resolve(self.0.lookup(key), res)
    }
    /// The engine's `lookup_batch` (a loop of single lookups for every
    /// competitor, some hoist their CPU-feature dispatch out of it) through
    /// a bounded stack buffer, then the ids are resolved.
    #[inline]
    fn lookup_batch<'a>(
        &'a self,
        keys: &[K],
        res: &'a [Resolved],
        out: &mut [Option<LookupMatch<'a, u32>>],
    ) {
        let mut ids = [None; ID_CHUNK];
        for (kc, oc) in keys.chunks(ID_CHUNK).zip(out.chunks_mut(ID_CHUNK)) {
            self.0.lookup_batch(kc, &mut ids[..kc.len()]);
            for (o, &id) in oc.iter_mut().zip(&ids[..kc.len()]) {
                *o = resolve(id, res);
            }
        }
    }
    #[inline]
    fn exists(&self, key: K) -> bool {
        self.0.lookup(key).is_some()
    }
}

/// A membership-only competitor (bitmaps).
pub struct Member<E>(E);

impl<K: Key, E: MembershipEngine<K> + Sync> Engine<K> for Member<E> {
    const LOOKUP: Api = Api::Unsupported;
    const BATCH: Api = Api::Unsupported;
    const EXISTS: Api = Api::Dedicated;
    const ENTRY_TABLE: bool = false;

    fn name() -> String {
        E::name()
    }
    fn applicable() -> Result<(), String> {
        E::applicable(&[]).map_err(str::to_string)
    }
    fn build(routes: &[Route<K>]) -> Self {
        Member(E::build(routes))
    }
    fn reported_bytes(&self) -> Option<usize> {
        E::REPORTS_MEMORY.then(|| self.0.memory_bytes())
    }
    fn lookup<'a>(&'a self, _: K, _: &'a [Resolved]) -> Option<LookupMatch<'a, u32>> {
        unreachable!("membership structures return no LPM result")
    }
    fn lookup_batch<'a>(
        &'a self,
        _: &[K],
        _: &'a [Resolved],
        _: &mut [Option<LookupMatch<'a, u32>>],
    ) {
        unreachable!("membership structures return no LPM result")
    }
    #[inline]
    fn exists(&self, key: K) -> bool {
        self.0.contains(key)
    }
}

/// Name of the dir-24-8 entry whose batches run the scalar kernel.
pub const OURS_SCALAR: &str = "dir-24-8[scalar]";

/// Batch kernel of this build of dir-24-8 on this CPU: `"scalar"` without
/// the `batch-simd` feature, otherwise the detected one (`"avx512"`, ...).
pub fn batch_kernel() -> &'static str {
    IpTable::<u32>::new().stats().batch_kernel
}

/// dir-24-8 variant (type level, so every engine is monomorphized). The
/// batch kernel is fixed when the library is compiled: `Simd` is this
/// binary's kernel (the CPU's SIMD kernel with the default `batch-simd`
/// feature), `ScalarBatches` requires a build without `batch-simd`
/// (`bench-compare-scalar`, run by the orchestrator for that entry).
pub trait Sel: Send + Sync + 'static {
    const NAME: &'static str;
    const SCALAR_BATCHES: bool;
}

pub struct Simd;
impl Sel for Simd {
    const NAME: &'static str = "dir-24-8";
    const SCALAR_BATCHES: bool = false;
}

pub struct ScalarBatches;
impl Sel for ScalarBatches {
    const NAME: &'static str = OURS_SCALAR;
    const SCALAR_BATCHES: bool = true;
}

/// dir-24-8 through its public API only, default configuration.
pub struct Ours<S> {
    table: IpTable<u32>,
    _s: PhantomData<S>,
}

fn net4(r: &Route<u32>) -> IpNet {
    IpNet::V4(Ipv4Net::new(Ipv4Addr::from(r.net), r.len).expect("valid length"))
}

fn net6(r: &Route<u128>) -> IpNet {
    IpNet::V6(Ipv6Net::new(Ipv6Addr::from(r.net), r.len).expect("valid length"))
}

macro_rules! ours_family {
    ($k:ty, $net:ident, $addr:ty, $lookup:ident, $exist:ident) => {
        impl<S: Sel> Engine<$k> for Ours<S> {
            const LOOKUP: Api = Api::Native;
            const BATCH: Api = Api::Native;
            const EXISTS: Api = Api::Dedicated;
            const ENTRY_TABLE: bool = false;

            fn name() -> String {
                S::NAME.into()
            }
            fn applicable() -> Result<(), String> {
                match batch_kernel() {
                    k if S::SCALAR_BATCHES && k != "scalar" => Err(format!(
                        "this build batches with {k}: the scalar entry needs a build without \
                         the batch-simd feature (bench-compare-scalar)"
                    )),
                    _ => Ok(()),
                }
            }
            fn build(routes: &[Route<$k>]) -> Self {
                let mut table = IpTable::new();
                for r in routes {
                    table.insert($net(r), r.payload, Replace).expect("insert");
                }
                Ours {
                    table,
                    _s: PhantomData,
                }
            }
            fn reported_bytes(&self) -> Option<usize> {
                Some(self.table.stats().total_bytes())
            }
            fn mapped_bytes(&self) -> usize {
                let s = self.table.stats();
                s.ipv4.mapped_bytes + s.ipv6.mapped_bytes
            }
            fn kernel_detail(&self) -> Option<String> {
                Some(format!(
                    "batch: {}, single: scalar",
                    self.table.stats().batch_kernel
                ))
            }
            #[inline]
            fn lookup<'a>(&'a self, key: $k, _: &'a [Resolved]) -> Option<LookupMatch<'a, u32>> {
                self.table.$lookup(<$addr>::from(key))
            }
            #[inline]
            fn lookup_batch<'a>(
                &'a self,
                keys: &[$k],
                _: &'a [Resolved],
                out: &mut [Option<LookupMatch<'a, u32>>],
            ) {
                self.table
                    .lookup_batch_into(keys, out)
                    .expect("same length");
            }
            #[inline]
            fn exists(&self, key: $k) -> bool {
                self.table.$exist(<$addr>::from(key))
            }
        }
    };
}

ours_family!(u32, net4, Ipv4Addr, lookup_ipv4, exist_ipv4);
ours_family!(u128, net6, Ipv6Addr, lookup_ipv6, exist_ipv6);
