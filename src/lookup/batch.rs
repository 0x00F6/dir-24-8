//! Batch drivers: key representations, chunking, mixed-family partitioning
//! and result materialization.
//!
//! Batches are processed in chunks of [`CHUNK`] keys with fixed-size stack
//! buffers (no heap allocation):
//!
//! 1. keys are converted into the kernel representation (`u32` numeric for
//!    IPv4, network-order `[u8; 16]` for IPv6). `&[u32]` and `&[[u8; 16]]`
//!    inputs skip this copy and are passed to the kernels directly;
//! 2. mixed `&[IpAddr]` chunks are partitioned per family, remembering the
//!    original position of every key;
//! 3. the kernel writes raw cells into a stack buffer;
//! 4. a sink converts each cell (`LookupMatch`, `bool` or `MatchId`) and
//!    writes it at the original position, so the output order always matches
//!    the input order.

use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::dispatch::Kernel;
use super::result::{LookupMatch, MATCH_V6};
use crate::ipv4::Ipv4Engine;
use crate::ipv6::Ipv6Engine;

/// Keys per chunk of homogeneous batches.
pub(crate) const CHUNK: usize = 64;
/// Keys per chunk of mixed batches (each family typically gets a few full
/// SIMD blocks); positions inside a chunk fit in a `u16`.
pub(crate) const MIXED_CHUNK: usize = 256;

/// Key types accepted by the batch APIs.
///
/// | Type        | Family | Representation                                   |
/// |-------------|--------|--------------------------------------------------|
/// | `IpAddr`    | mixed  | partitioned per family                           |
/// | `Ipv4Addr`  | IPv4   | `u32::from_be_bytes(octets)`                     |
/// | `Ipv6Addr`  | IPv6   | `octets()` (network order)                       |
/// | `u32`       | IPv4   | numeric, `u32::from_be_bytes(octets)` (zero copy)|
/// | `[u8; 4]`   | IPv4   | network-order octets                             |
/// | `u128`      | IPv6   | numeric, `u128::from_be_bytes(octets)`           |
/// | `[u8; 16]`  | IPv6   | network-order octets (zero copy)                 |
///
/// The trait is sealed.
///
/// ```
/// use std::net::{IpAddr, Ipv4Addr};
/// use dir_24_8::{IpNet, IpTable, Replace};
///
/// let mut table = IpTable::new();
/// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
/// // The same address as a mixed-family key, a typed key, a number and octets.
/// let a = table.lookup_array(&[IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]);
/// let b = table.lookup_array(&[Ipv4Addr::new(10, 0, 0, 1)]);
/// let c = table.lookup_array(&[0x0A00_0001u32]);
/// let d = table.lookup_array(&[[10u8, 0, 0, 1]]);
/// assert!([a[0], b[0], c[0], d[0]].iter().all(|m| m.and_then(|m| m.data) == Some(&1)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait BatchKey: Copy + private::Sealed {}

impl<K: Copy + private::Sealed> BatchKey for K {}

pub(crate) mod private {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum KeyFamily {
        V4,
        V6,
        Mixed,
    }

    pub enum Split {
        V4(u32),
        V6([u8; 16]),
    }

    pub trait Sealed: Copy {
        const FAMILY: KeyFamily;
        fn split(self) -> Split;
        #[inline(always)]
        fn v4_slice(_keys: &[Self]) -> Option<&[u32]> {
            None
        }
        #[inline(always)]
        fn v6_slice(_keys: &[Self]) -> Option<&[[u8; 16]]> {
            None
        }
    }

    impl Sealed for IpAddr {
        const FAMILY: KeyFamily = KeyFamily::Mixed;
        #[inline(always)]
        fn split(self) -> Split {
            match self {
                IpAddr::V4(a) => Split::V4(u32::from(a)),
                IpAddr::V6(a) => Split::V6(a.octets()),
            }
        }
    }

    impl Sealed for Ipv4Addr {
        const FAMILY: KeyFamily = KeyFamily::V4;
        #[inline(always)]
        fn split(self) -> Split {
            Split::V4(u32::from(self))
        }
    }

    impl Sealed for u32 {
        const FAMILY: KeyFamily = KeyFamily::V4;
        #[inline(always)]
        fn split(self) -> Split {
            Split::V4(self)
        }
        #[inline(always)]
        fn v4_slice(keys: &[Self]) -> Option<&[u32]> {
            Some(keys)
        }
    }

    impl Sealed for [u8; 4] {
        const FAMILY: KeyFamily = KeyFamily::V4;
        #[inline(always)]
        fn split(self) -> Split {
            Split::V4(u32::from_be_bytes(self))
        }
    }

    impl Sealed for Ipv6Addr {
        const FAMILY: KeyFamily = KeyFamily::V6;
        #[inline(always)]
        fn split(self) -> Split {
            Split::V6(self.octets())
        }
    }

    impl Sealed for u128 {
        const FAMILY: KeyFamily = KeyFamily::V6;
        #[inline(always)]
        fn split(self) -> Split {
            Split::V6(self.to_be_bytes())
        }
    }

    impl Sealed for [u8; 16] {
        const FAMILY: KeyFamily = KeyFamily::V6;
        #[inline(always)]
        fn split(self) -> Split {
            Split::V6(self)
        }
        #[inline(always)]
        fn v6_slice(keys: &[Self]) -> Option<&[[u8; 16]]> {
            Some(keys)
        }
    }
}

use private::{KeyFamily, Split};

/// Receives raw cells at their original batch position.
pub(crate) trait Sink {
    /// Called for every cell of a chunk one chunk before `put4`, so that
    /// payload reads overlap with the next chunk's table accesses.
    #[inline(always)]
    fn prefetch4(&self, _cell: u32) {}
    #[inline(always)]
    fn prefetch6(&self, _cell: u32) {}
    fn put4(&mut self, pos: usize, cell: u32);
    fn put6(&mut self, pos: usize, cell: u32);
    /// The key's family has no table yet.
    fn absent(&mut self, pos: usize);
}

pub(crate) struct MatchSink<'o, 'a, T> {
    pub(crate) out: &'o mut [Option<LookupMatch<'a, T>>],
    pub(crate) v4: Option<&'a Ipv4Engine<T>>,
    pub(crate) v6: Option<&'a Ipv6Engine<T>>,
}

impl<T> Sink for MatchSink<'_, '_, T> {
    #[inline(always)]
    fn prefetch4(&self, cell: u32) {
        if let Some(e) = self.v4 {
            e.prefetch(cell);
        }
    }
    #[inline(always)]
    fn prefetch6(&self, cell: u32) {
        if let Some(e) = self.v6 {
            e.prefetch(cell);
        }
    }
    #[inline(always)]
    fn put4(&mut self, pos: usize, cell: u32) {
        self.out[pos] = self.v4.and_then(|e| e.resolve(cell));
    }
    #[inline(always)]
    fn put6(&mut self, pos: usize, cell: u32) {
        self.out[pos] = self.v6.and_then(|e| e.resolve(cell));
    }
    #[inline(always)]
    fn absent(&mut self, pos: usize) {
        self.out[pos] = None;
    }
}

pub(crate) struct ExistSink<'o> {
    pub(crate) out: &'o mut [bool],
}

impl Sink for ExistSink<'_> {
    #[inline(always)]
    fn put4(&mut self, pos: usize, cell: u32) {
        self.out[pos] = cell != 0;
    }
    #[inline(always)]
    fn put6(&mut self, pos: usize, cell: u32) {
        self.out[pos] = cell != 0;
    }
    #[inline(always)]
    fn absent(&mut self, pos: usize) {
        self.out[pos] = false;
    }
}

pub(crate) struct IdSink<'o> {
    pub(crate) out: &'o mut [u32],
}

impl Sink for IdSink<'_> {
    #[inline(always)]
    fn put4(&mut self, pos: usize, cell: u32) {
        self.out[pos] = cell;
    }
    #[inline(always)]
    fn put6(&mut self, pos: usize, cell: u32) {
        self.out[pos] = if cell != 0 { cell | MATCH_V6 } else { 0 };
    }
    #[inline(always)]
    fn absent(&mut self, pos: usize) {
        self.out[pos] = 0;
    }
}

/// Engines and kernel a batch runs against.
pub(crate) struct BatchCtx<'a, T> {
    pub(crate) kernel: Kernel,
    pub(crate) v4: Option<&'a Ipv4Engine<T>>,
    pub(crate) v6: Option<&'a Ipv6Engine<T>>,
}

impl<T> BatchCtx<'_, T> {
    /// Runs `keys` through the kernels and feeds every cell to `sink`.
    /// The caller guarantees the sink accepts `keys.len()` positions.
    #[inline]
    pub(crate) fn run<K: BatchKey, S: Sink>(&self, keys: &[K], sink: &mut S) {
        match K::FAMILY {
            KeyFamily::V4 => self.run_v4(keys, sink),
            KeyFamily::V6 => self.run_v6(keys, sink),
            KeyFamily::Mixed => self.run_mixed(keys, sink),
        }
    }

    fn run_v4<K: BatchKey, S: Sink>(&self, keys: &[K], sink: &mut S) {
        let Some(engine) = self.v4 else {
            (0..keys.len()).for_each(|i| sink.absent(i));
            return;
        };
        let t = engine.tables();
        // Double-buffered cells: chunk `i` is materialized after the kernel
        // ran on chunk `i + 1`, so its prefetched slots have arrived.
        let mut cells = [[0u32; CHUNK]; 2];
        let mut buf = [0u32; CHUNK];
        let mut pending: Option<(usize, usize)> = None;
        let mut cur = 0usize;
        for (ci, chunk) in keys.chunks(CHUNK).enumerate() {
            let n = chunk.len();
            let raw: &[u32] = match K::v4_slice(chunk) {
                Some(raw) => raw,
                None => {
                    for (b, k) in buf.iter_mut().zip(chunk) {
                        *b = match k.split() {
                            Split::V4(x) => x,
                            Split::V6(_) => 0,
                        };
                    }
                    &buf[..n]
                }
            };
            self.kernel.v4_batch(&t, raw, &mut cells[cur][..n]);
            cells[cur][..n].iter().for_each(|&c| sink.prefetch4(c));
            if let Some((base, pn)) = pending {
                for (j, &c) in cells[cur ^ 1][..pn].iter().enumerate() {
                    sink.put4(base + j, c);
                }
            }
            pending = Some((ci * CHUNK, n));
            cur ^= 1;
        }
        if let Some((base, pn)) = pending {
            for (j, &c) in cells[cur ^ 1][..pn].iter().enumerate() {
                sink.put4(base + j, c);
            }
        }
    }

    fn run_v6<K: BatchKey, S: Sink>(&self, keys: &[K], sink: &mut S) {
        let Some(engine) = self.v6 else {
            (0..keys.len()).for_each(|i| sink.absent(i));
            return;
        };
        let t = engine.tables();
        // Double-buffered cells: chunk `i` is materialized after the kernel
        // ran on chunk `i + 1`, so its prefetched slots have arrived.
        let mut cells = [[0u32; CHUNK]; 2];
        let mut buf = [[0u8; 16]; CHUNK];
        let mut pending: Option<(usize, usize)> = None;
        let mut cur = 0usize;
        for (ci, chunk) in keys.chunks(CHUNK).enumerate() {
            let n = chunk.len();
            let raw: &[[u8; 16]] = match K::v6_slice(chunk) {
                Some(raw) => raw,
                None => {
                    for (b, k) in buf.iter_mut().zip(chunk) {
                        *b = match k.split() {
                            Split::V6(x) => x,
                            Split::V4(_) => [0; 16],
                        };
                    }
                    &buf[..n]
                }
            };
            self.kernel.v6_batch(&t, raw, &mut cells[cur][..n]);
            cells[cur][..n].iter().for_each(|&c| sink.prefetch6(c));
            if let Some((base, pn)) = pending {
                for (j, &c) in cells[cur ^ 1][..pn].iter().enumerate() {
                    sink.put6(base + j, c);
                }
            }
            pending = Some((ci * CHUNK, n));
            cur ^= 1;
        }
        if let Some((base, pn)) = pending {
            for (j, &c) in cells[cur ^ 1][..pn].iter().enumerate() {
                sink.put6(base + j, c);
            }
        }
    }

    fn run_mixed<K: BatchKey, S: Sink>(&self, keys: &[K], sink: &mut S) {
        let t4 = self.v4.map(|e| e.tables());
        let t6 = self.v6.map(|e| e.tables());
        let mut k4 = [0u32; MIXED_CHUNK];
        let mut p4 = [0u16; MIXED_CHUNK];
        let mut k6 = [[0u8; 16]; MIXED_CHUNK];
        let mut p6 = [0u16; MIXED_CHUNK];
        let mut c4 = [0u32; MIXED_CHUNK];
        let mut c6 = [0u32; MIXED_CHUNK];
        for (ci, chunk) in keys.chunks(MIXED_CHUNK).enumerate() {
            let (mut n4, mut n6) = (0usize, 0usize);
            for (j, k) in chunk.iter().enumerate() {
                match k.split() {
                    Split::V4(x) => {
                        k4[n4] = x;
                        p4[n4] = j as u16;
                        n4 += 1;
                    }
                    Split::V6(x) => {
                        k6[n6] = x;
                        p6[n6] = j as u16;
                        n6 += 1;
                    }
                }
            }
            let base = ci * MIXED_CHUNK;
            // Both kernels run before any materialization so that the slot
            // prefetches of one family overlap the other family's walk.
            if let Some(t) = &t4 {
                self.kernel.v4_batch(t, &k4[..n4], &mut c4[..n4]);
                c4[..n4].iter().for_each(|&c| sink.prefetch4(c));
            }
            if let Some(t) = &t6 {
                self.kernel.v6_batch(t, &k6[..n6], &mut c6[..n6]);
                c6[..n6].iter().for_each(|&c| sink.prefetch6(c));
            }
            for (&p, &c) in p4[..n4].iter().zip(&c4[..n4]) {
                let pos = base + usize::from(p);
                if t4.is_some() {
                    sink.put4(pos, c)
                } else {
                    sink.absent(pos)
                }
            }
            for (&p, &c) in p6[..n6].iter().zip(&c6[..n6]) {
                let pos = base + usize::from(p);
                if t6.is_some() {
                    sink.put6(pos, c)
                } else {
                    sink.absent(pos)
                }
            }
        }
    }
}
