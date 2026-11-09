//! Membership engine (IPv4 only): "is the key covered by any route?".
//!
//! [`DenseBitmap`] (`bitmap-dense(2^32-bit)`): one bit per IPv4 address,
//! `2^26` `u64` words (512 MiB) from a zeroed allocation (`calloc`, so the
//! pages are mapped lazily by the OS until written); each route sets its bit
//! range with whole-word fills; `contains` is a single bit test. For IPv6 it
//! reports "not applicable" (2^128 bits cannot be represented densely).

use std::marker::PhantomData;

use crate::core::{Key, MembershipEngine, Route};

/// Words of the dense IPv4 bitmap.
const WORDS: usize = 1 << 26;

const NOT_V4: &str = "not applicable: IPv4-only structure (2^32-bit address space)";

/// Inclusive `[first, last]` address range of a route as `u32` (IPv4 only).
fn v4_range<K: Key>(r: &Route<K>) -> (u32, u32) {
    (
        r.net.to_u128() as u32,
        K::last(r.net, r.len).to_u128() as u32,
    )
}

/// `bitmap-dense(2^32-bit)`.
pub struct DenseBitmap<K = u32> {
    /// `None` for IPv6 (not applicable). A fixed-size array so that the
    /// word index `key >> 6` needs no bounds check.
    words: Option<Box<[u64; WORDS]>>,
    _key: PhantomData<K>,
}

impl<K: Key> DenseBitmap<K> {
    fn set_range(words: &mut [u64; WORDS], first: u32, last: u32) {
        let (fw, lw) = ((first >> 6) as usize, (last >> 6) as usize);
        let fmask = u64::MAX << (first & 63);
        let lmask = u64::MAX >> (63 - (last & 63));
        if fw == lw {
            words[fw] |= fmask & lmask;
        } else {
            words[fw] |= fmask;
            words[fw + 1..lw].fill(u64::MAX);
            words[lw] |= lmask;
        }
    }
}

impl<K: Key> MembershipEngine<K> for DenseBitmap<K> {
    fn name() -> String {
        "bitmap-dense(2^32-bit)".into()
    }

    fn applicable(_routes: &[Route<K>]) -> Result<(), &'static str> {
        if K::BITS == 32 { Ok(()) } else { Err(NOT_V4) }
    }

    fn build(routes: &[Route<K>]) -> Self {
        if K::BITS != 32 {
            return Self {
                words: None,
                _key: PhantomData,
            };
        }
        // `vec![0; n]` uses a zeroed allocation (lazily mapped pages); the
        // boxed slice has exactly WORDS elements, so the conversion succeeds.
        let mut words: Box<[u64; WORDS]> = vec![0u64; WORDS]
            .into_boxed_slice()
            .try_into()
            .expect("exact length");
        for r in routes {
            let (first, last) = v4_range(r);
            Self::set_range(&mut words, first, last);
        }
        Self {
            words: Some(words),
            _key: PhantomData,
        }
    }

    #[inline]
    fn contains(&self, key: K) -> bool {
        match &self.words {
            Some(w) => {
                let k = key.to_u128() as u32;
                (w[(k >> 6) as usize] >> (k & 63)) & 1 != 0
            }
            None => false,
        }
    }

    fn memory_bytes(&self) -> usize {
        self.words
            .as_ref()
            .map_or(0, |_| WORDS * std::mem::size_of::<u64>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{heavy_lock, membership_suite, route};

    #[test]
    fn dense_bitmap() {
        // Allocates 512 MiB per build: serialized with the other big tests,
        // and only one bitmap alive at a time.
        let _g = heavy_lock();
        membership_suite::<u32, DenseBitmap<u32>>();
        membership_suite::<u128, DenseBitmap<u128>>();
    }

    #[test]
    fn dense_bitmap_word_edges() {
        let _g = heavy_lock();
        let routes = [route(0u32, 32), route(63u32, 32), route(0xFFFF_FFC0u32, 26)];
        let b = <DenseBitmap<u32> as MembershipEngine<u32>>::build(&routes);
        assert!(b.contains(0) && b.contains(63) && !b.contains(1) && !b.contains(64));
        assert!(b.contains(u32::MAX) && b.contains(0xFFFF_FFC0) && !b.contains(0xFFFF_FFBF));
        assert_eq!(b.memory_bytes(), 512 << 20);
    }
}
