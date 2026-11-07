//! AVX2 kernels (x86-64): 8 lookups per 256-bit vector.
//!
//! * IPv4: `vpsrld` builds the 24-bit indices, `vpgatherdd` loads `tbl24`,
//!   the sign bits of the gathered cells *are* the gather mask for the
//!   secondary level (`vpgatherdd` with a vector mask), and the group index
//!   `(cell << 8) | (key & 0xFF)` is built with `vpslld`/`vpand`/`vpor`.
//! * IPv6: blocks of 4 vectors (32 addresses); each 8 addresses (four 256-bit loads) are transposed with
//!   `vpunpck{l,h}{dq,qdq}` into four vectors holding the 32-bit words of
//!   each address; the root index is a `vpshufb` byte swap. The walk is
//!   lock-step: at iteration `k` every active lane consumes octet `2 + k`, so
//!   the octet is extracted with an immediate shift of the right word and
//!   finished lanes keep their terminal cell through the masked gather.
//! * Every secondary index is clamped with `vpminud` to the last valid
//!   `tbl8` cell, so no lane can address memory outside the allocation even
//!   if a table invariant were broken.

use core::arch::x86_64::*;

use super::tables::Tables;

const LANES: usize = 8;
/// Keys per phase block (roots of a whole block are gathered first).
const BLOCK: usize = 64;

#[inline]
#[target_feature(enable = "avx2")]
fn load(p: &[u32]) -> __m256i {
    debug_assert!(p.len() >= LANES);
    // SAFETY: `p` holds at least 8 `u32`, unaligned load.
    unsafe { _mm256_loadu_si256(p.as_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx2")]
fn store(p: &mut [u32], v: __m256i) {
    debug_assert!(p.len() >= LANES);
    // SAFETY: `p` holds at least 8 `u32`, unaligned store.
    unsafe { _mm256_storeu_si256(p.as_mut_ptr().cast(), v) }
}

#[inline]
#[target_feature(enable = "avx2")]
fn gather_root(t: &Tables<'_>, idx: __m256i) -> __m256i {
    // SAFETY: callers pass indices `< 2^24` (`key >> 8` or a 24-bit byte
    // swap) and `tbl24` has exactly `2^24` cells.
    unsafe { _mm256_i32gather_epi32::<4>(t.tbl24_ptr().cast(), idx) }
}

/// Follows the lanes of `cell` whose sign bit is set into `tbl8`.
#[inline]
#[target_feature(enable = "avx2")]
fn gather_group(t: &Tables<'_>, cell: __m256i, byte: __m256i, max8: __m256i) -> __m256i {
    let idx = _mm256_or_si256(_mm256_slli_epi32::<8>(cell), byte);
    let idx = _mm256_min_epu32(idx, max8);
    // SAFETY: only lanes whose mask (the sign bit of `cell`) is set are
    // loaded, and their indices are clamped to `tbl8.len() - 1 < 2^31`.
    unsafe { _mm256_mask_i32gather_epi32::<4>(cell, t.tbl8_ptr().cast(), idx, cell) }
}

#[inline]
#[target_feature(enable = "avx2")]
fn any_group(cell: __m256i) -> bool {
    _mm256_movemask_ps(_mm256_castsi256_ps(cell)) != 0
}

/// Full-width IPv4 step for one vector of keys.
#[inline]
#[target_feature(enable = "avx2")]
fn v4_vec(t: &Tables<'_>, keys: __m256i, max8: __m256i) -> __m256i {
    let cell = gather_root(t, _mm256_srli_epi32::<8>(keys));
    if any_group(cell) {
        let byte = _mm256_and_si256(keys, _mm256_set1_epi32(0xFF));
        gather_group(t, cell, byte, max8)
    } else {
        cell
    }
}

/// Batch IPv4 lookup. `keys.len() == out.len()`.
#[target_feature(enable = "avx2")]
pub(crate) fn v4_batch(t: &Tables<'_>, keys: &[u32], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    let max8 = _mm256_set1_epi32(t.tbl8_max() as i32);
    let low = _mm256_set1_epi32(0xFF);
    let n = keys.len().min(out.len());
    let full = n - n % LANES;
    let mut start = 0;
    while start < full {
        let end = (start + BLOCK).min(full);
        // Phase 1: independent root gathers for the whole block.
        let mut i = start;
        while i < end {
            let k = load(&keys[i..]);
            store(&mut out[i..], gather_root(t, _mm256_srli_epi32::<8>(k)));
            i += LANES;
        }
        // Phase 2: secondary level, only for vectors with extended lanes.
        let mut i = start;
        while i < end {
            let cell = load(&out[i..]);
            if any_group(cell) {
                let byte = _mm256_and_si256(load(&keys[i..]), low);
                store(&mut out[i..], gather_group(t, cell, byte, max8));
            }
            i += LANES;
        }
        start = end;
    }
    if full < n {
        let rest = n - full;
        let mut k = [0u32; LANES];
        k[..rest].copy_from_slice(&keys[full..n]);
        let mut r = [0u32; LANES];
        store(&mut r, v4_vec(t, load(&k), max8));
        out[full..n].copy_from_slice(&r[..rest]);
    }
}

/// Control for `vpshufb`: per 32-bit lane, bytes `[o2, o1, o0, 0]`, i.e. the
/// 24-bit root index of the octets stored little-endian in the lane.
#[inline]
#[target_feature(enable = "avx2")]
fn root_shuffle() -> __m256i {
    _mm256_setr_epi8(
        2, 1, 0, -128, 6, 5, 4, -128, 10, 9, 8, -128, 14, 13, 12, -128, 2, 1, 0, -128, 6, 5, 4,
        -128, 10, 9, 8, -128, 14, 13, 12, -128,
    )
}

/// Loads 8 addresses and returns their four 32-bit words (octets in
/// little-endian order inside each lane) with lanes in the order
/// `0, 2, 4, 6, 1, 3, 5, 7`.
#[inline]
#[target_feature(enable = "avx2")]
fn transpose8(keys: &[[u8; 16]]) -> [__m256i; 4] {
    debug_assert!(keys.len() >= LANES);
    let p = keys.as_ptr().cast::<__m256i>();
    // SAFETY: 8 contiguous `[u8; 16]` are 128 bytes = four 256-bit loads.
    let (r0, r1, r2, r3) = unsafe {
        (
            _mm256_loadu_si256(p),
            _mm256_loadu_si256(p.add(1)),
            _mm256_loadu_si256(p.add(2)),
            _mm256_loadu_si256(p.add(3)),
        )
    };
    let t0 = _mm256_unpacklo_epi32(r0, r1);
    let t1 = _mm256_unpackhi_epi32(r0, r1);
    let t2 = _mm256_unpacklo_epi32(r2, r3);
    let t3 = _mm256_unpackhi_epi32(r2, r3);
    [
        _mm256_unpacklo_epi64(t0, t2),
        _mm256_unpackhi_epi64(t0, t2),
        _mm256_unpacklo_epi64(t1, t3),
        _mm256_unpackhi_epi64(t1, t3),
    ]
}

/// Octet at bit offset `S` of every lane of `w`.
#[inline]
#[target_feature(enable = "avx2")]
fn octet<const S: i32>(w: __m256i) -> __m256i {
    _mm256_and_si256(_mm256_srli_epi32::<S>(w), _mm256_set1_epi32(0xFF))
}

/// Lock-step IPv6 walk of `V` vectors (`8 * V` addresses), results in input
/// order.
// Several parallel per-vector arrays are indexed by `v`.
#[allow(clippy::needless_range_loop)]
#[inline]
#[target_feature(enable = "avx2")]
fn v6_block<const V: usize>(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32], max8: __m256i) {
    debug_assert!(keys.len() >= V * LANES && out.len() >= V * LANES);
    let zero = _mm256_setzero_si256();
    let shuf = root_shuffle();
    let mut w = [[zero; 4]; V];
    let mut c = [zero; V];
    for v in 0..V {
        w[v] = transpose8(&keys[v * LANES..]);
        c[v] = gather_root(t, _mm256_shuffle_epi8(w[v][0], shuf));
    }
    macro_rules! step {
        ($l:lifetime, $w:literal, $s:literal) => {
            let any = c.iter().fold(zero, |a, &x| _mm256_or_si256(a, x));
            if !any_group(any) {
                break $l;
            }
            for v in 0..V {
                if any_group(c[v]) {
                    c[v] = gather_group(t, c[v], octet::<$s>(w[v][$w]), max8);
                }
            }
        };
    }
    'walk: {
        step!('walk, 0, 24);
        step!('walk, 1, 0);
        step!('walk, 1, 8);
        step!('walk, 1, 16);
        step!('walk, 1, 24);
        step!('walk, 2, 0);
        step!('walk, 2, 8);
        step!('walk, 2, 16);
        step!('walk, 2, 24);
        step!('walk, 3, 0);
        step!('walk, 3, 8);
        step!('walk, 3, 16);
        step!('walk, 3, 24);
    }
    // Undo the transpose lane order (0,2,4,6,1,3,5,7).
    let perm = _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7);
    for v in 0..V {
        store(
            &mut out[v * LANES..],
            _mm256_permutevar8x32_epi32(c[v], perm),
        );
    }
}

/// Vectors walked together by the IPv6 batch kernel.
const V6_VECTORS: usize = 4;

/// Batch IPv6 lookup. `keys.len() == out.len()`.
#[target_feature(enable = "avx2")]
pub(crate) fn v6_batch(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    const STEP: usize = V6_VECTORS * LANES;
    let max8 = _mm256_set1_epi32(t.tbl8_max() as i32);
    let n = keys.len().min(out.len());
    let mut i = 0;
    while i + STEP <= n {
        v6_block::<V6_VECTORS>(t, &keys[i..i + STEP], &mut out[i..i + STEP], max8);
        i += STEP;
    }
    while i + LANES <= n {
        v6_block::<1>(t, &keys[i..i + LANES], &mut out[i..i + LANES], max8);
        i += LANES;
    }
    if i < n {
        let rest = n - i;
        let mut k = [[0u8; 16]; LANES];
        k[..rest].copy_from_slice(&keys[i..n]);
        let mut r = [0u32; LANES];
        v6_block::<1>(t, &k, &mut r, max8);
        out[i..n].copy_from_slice(&r[..rest]);
    }
}
