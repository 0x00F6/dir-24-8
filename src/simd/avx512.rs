//! AVX-512 kernels (x86-64, `avx512f` + `avx512bw` + `avx512vl`): 16
//! lookups per 512-bit vector.
//!
//! * IPv4: `vpsrld` + `vpgatherdd` on `tbl24`; the extended lanes are a
//!   `__mmask16` obtained with one signed compare (`vpcmpd`), the secondary
//!   level is a masked `vpgatherdd`. Tails use masked loads/stores
//!   (`vmovdqu32{k}`), which never touch masked-off memory.
//! * IPv6: 16 addresses (four 512-bit loads) are transposed with
//!   `vpunpck{l,h}{dq,qdq}` into four vectors of 32-bit words, the root index
//!   is a `vpshufb` byte swap (as in DPDK `trie_avx512.c`), and four vectors
//!   (64 addresses) walk the levels in lock step with masked gathers.
//!   Finished lanes keep their terminal cell; the loop ends when every mask
//!   is empty.
//! * Secondary indices are clamped with `vpminud` to the last `tbl8` cell.

use core::arch::x86_64::*;

use super::tables::Tables;

const LANES: usize = 16;
const BLOCK: usize = 128;

#[inline]
#[target_feature(enable = "avx512f")]
fn gather_root(t: &Tables<'_>, idx: __m512i) -> __m512i {
    // SAFETY: callers pass indices `< 2^24`; `tbl24` has `2^24` cells.
    unsafe { _mm512_i32gather_epi32::<4>(idx, t.tbl24_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx512f")]
fn gather_root_masked(t: &Tables<'_>, k: __mmask16, idx: __m512i) -> __m512i {
    // SAFETY: as `gather_root`; masked-off lanes are not loaded.
    unsafe {
        _mm512_mask_i32gather_epi32::<4>(_mm512_setzero_si512(), k, idx, t.tbl24_ptr().cast())
    }
}

/// Mask of lanes holding a group cell (sign bit set).
#[inline]
#[target_feature(enable = "avx512f")]
fn groups(cell: __m512i) -> __mmask16 {
    _mm512_cmplt_epi32_mask(cell, _mm512_setzero_si512())
}

#[inline]
#[target_feature(enable = "avx512f")]
fn gather_group(
    t: &Tables<'_>,
    cell: __m512i,
    m: __mmask16,
    byte: __m512i,
    max8: __m512i,
) -> __m512i {
    let idx = _mm512_or_si512(_mm512_slli_epi32::<8>(cell), byte);
    let idx = _mm512_min_epu32(idx, max8);
    // SAFETY: only lanes in `m` are loaded; indices are clamped to
    // `tbl8.len() - 1 < 2^31`.
    unsafe { _mm512_mask_i32gather_epi32::<4>(cell, m, idx, t.tbl8_ptr().cast()) }
}

/// Batch IPv4 lookup. `keys.len() == out.len()`.
#[target_feature(enable = "avx512f,avx512bw,avx512vl")]
pub(crate) fn v4_batch(t: &Tables<'_>, keys: &[u32], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    let max8 = _mm512_set1_epi32(t.tbl8_max() as i32);
    let low = _mm512_set1_epi32(0xFF);
    let n = keys.len().min(out.len());
    let kp = keys.as_ptr();
    let op = out.as_mut_ptr();
    let mut start = 0;
    while start < n {
        let end = (start + BLOCK).min(n);
        // Phase 1: root gathers for the whole block (masked final vector).
        let mut i = start;
        while i < end {
            let k: __mmask16 = if end - i >= LANES {
                0xFFFF
            } else {
                (1u16 << (end - i)) - 1
            };
            // SAFETY: lanes in `k` are `< end - i` elements past `i`, all in
            // bounds of `keys`/`out`; masked lanes are not accessed.
            unsafe {
                let a = _mm512_maskz_loadu_epi32(k, kp.add(i).cast());
                let c = gather_root_masked(t, k, _mm512_srli_epi32::<8>(a));
                _mm512_mask_storeu_epi32(op.add(i).cast(), k, c);
            }
            i += LANES;
        }
        // Phase 2: secondary level for the extended lanes only.
        let mut i = start;
        while i < end {
            let k: __mmask16 = if end - i >= LANES {
                0xFFFF
            } else {
                (1u16 << (end - i)) - 1
            };
            // SAFETY: same bounds argument as phase 1.
            unsafe {
                let c = _mm512_maskz_loadu_epi32(k, op.add(i).cast::<i32>());
                let m = groups(c) & k;
                if m != 0 {
                    let a = _mm512_maskz_loadu_epi32(k, kp.add(i).cast());
                    let c = gather_group(t, c, m, _mm512_and_si512(a, low), max8);
                    _mm512_mask_storeu_epi32(op.add(i).cast(), m, c);
                }
            }
            i += LANES;
        }
        start = end;
    }
}

/// Per 32-bit lane `[o2, o1, o0, 0]`: the 24-bit root index of a word that
/// holds octets 0..3 in memory order.
#[inline]
#[target_feature(enable = "avx512f,avx512bw")]
fn root_index(w0: __m512i) -> __m512i {
    let ctrl = _mm512_set4_epi32(
        0x800C_0D0E_u32 as i32,
        0x8008_090A_u32 as i32,
        0x8004_0506_u32 as i32,
        0x8000_0102_u32 as i32,
    );
    _mm512_shuffle_epi8(w0, ctrl)
}

/// Loads 16 addresses as four vectors of 32-bit words. Lane `4b + j`
/// holds address `b + 4j`.
#[inline]
#[target_feature(enable = "avx512f")]
fn transpose16(keys: &[[u8; 16]]) -> [__m512i; 4] {
    debug_assert!(keys.len() >= LANES);
    let p = keys.as_ptr().cast::<__m512i>();
    // SAFETY: 16 contiguous `[u8; 16]` are 256 bytes = four 512-bit loads.
    let (r0, r1, r2, r3) = unsafe {
        (
            _mm512_loadu_si512(p),
            _mm512_loadu_si512(p.add(1)),
            _mm512_loadu_si512(p.add(2)),
            _mm512_loadu_si512(p.add(3)),
        )
    };
    let t0 = _mm512_unpacklo_epi32(r0, r1);
    let t1 = _mm512_unpackhi_epi32(r0, r1);
    let t2 = _mm512_unpacklo_epi32(r2, r3);
    let t3 = _mm512_unpackhi_epi32(r2, r3);
    [
        _mm512_unpacklo_epi64(t0, t2),
        _mm512_unpackhi_epi64(t0, t2),
        _mm512_unpacklo_epi64(t1, t3),
        _mm512_unpackhi_epi64(t1, t3),
    ]
}

#[inline]
#[target_feature(enable = "avx512f")]
fn octet<const S: u32>(w: __m512i) -> __m512i {
    _mm512_and_si512(_mm512_srli_epi32::<S>(w), _mm512_set1_epi32(0xFF))
}

/// Lock-step walk of `V` vectors (`16 * V` addresses) in input order.
///
/// All lanes of all vectors advance one level per step, so up to `16 * V`
/// independent secondary loads are in flight; finished lanes are masked off
/// and keep their terminal cell.
// Several parallel per-vector arrays are indexed by `v`.
#[allow(clippy::needless_range_loop)]
#[inline]
#[target_feature(enable = "avx512f,avx512bw,avx512vl")]
fn v6_block<const V: usize>(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32], max8: __m512i) {
    debug_assert!(keys.len() >= V * LANES && out.len() >= V * LANES);
    let zero = _mm512_setzero_si512();
    let mut w = [[zero; 4]; V];
    let mut c = [zero; V];
    let mut m = [0 as __mmask16; V];
    for v in 0..V {
        w[v] = transpose16(&keys[v * LANES..]);
        c[v] = gather_root(t, root_index(w[v][0]));
        m[v] = groups(c[v]);
    }
    macro_rules! step {
        ($l:lifetime, $w:literal, $s:literal) => {
            if m.iter().fold(0, |a, &x| a | x) == 0 {
                break $l;
            }
            for v in 0..V {
                if m[v] != 0 {
                    c[v] = gather_group(t, c[v], m[v], octet::<$s>(w[v][$w]), max8);
                    m[v] = groups(c[v]);
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
    // Lane 4b + j holds address b + 4j: the inverse is the same 4x4 swap.
    let perm = _mm512_setr_epi32(0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15);
    for v in 0..V {
        // SAFETY: `out` holds at least `16 * V` cells.
        unsafe {
            _mm512_storeu_si512(
                out.as_mut_ptr().add(v * LANES).cast(),
                _mm512_permutexvar_epi32(perm, c[v]),
            );
        }
    }
}

/// Vectors walked together by the IPv6 batch kernel. Measured on Zen 4
/// (100k BGP-like routes, random hits): 2 vectors 49 ns/IP, 4 vectors
/// 40 ns/IP, 8 vectors 45 ns/IP.
const V6_VECTORS: usize = 4;

/// Batch IPv6 lookup. `keys.len() == out.len()`.
///
/// Full blocks of `16 * V6_VECTORS` addresses, then single vectors, then one
/// zero-padded vector for the last `< 16` addresses.
#[target_feature(enable = "avx512f,avx512bw,avx512vl")]
pub(crate) fn v6_batch(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    const STEP: usize = V6_VECTORS * LANES;
    let max8 = _mm512_set1_epi32(t.tbl8_max() as i32);
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
