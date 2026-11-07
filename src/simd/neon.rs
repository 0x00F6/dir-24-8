//! NEON kernels (AArch64): 4 lookups per 128-bit vector.
//!
//! NEON has no gather from arbitrary memory. Table lookups (`vqtbl*`) only
//! select bytes from at most 64 bytes held in registers, which cannot
//! replace a scattered access to a 64 MiB `tbl24`. The kernels therefore
//! vectorize everything around the memory accesses and keep the accesses
//! themselves as one scalar load per lane:
//!
//! * IPv4: `vshrq_n_u32` computes 4 root indices, the 4 cells are loaded per
//!   lane, `vcltzq_s32` builds the extended-lane mask, `vmaxvq_u32` tests it,
//!   the group indices are `vorrq(vshlq_n(cell, 8), vandq(key, 0xFF))`
//!   clamped with `vminq_u32`, and only the extended lanes are loaded.
//! * IPv6: `vld4q_u32` de-interleaves 4 addresses into their four 32-bit
//!   words in one instruction (a free transpose), `vrev32q_u8` + shift
//!   builds the root indices, and the lock-step walk extracts each level's
//!   octet with immediate shifts; per-lane loads happen only for lanes that
//!   are still active.
//!
//! These kernels are compiled and type-checked for `aarch64` in CI, but no
//! AArch64 hardware was available while writing them: see the README for
//! the status of native measurements.

use core::arch::aarch64::*;

use super::tables::Tables;

const LANES: usize = 4;
const BLOCK: usize = 64;

#[inline(always)]
fn root_cell(t: &Tables<'_>, idx: u32) -> u32 {
    t.root(idx)
}

/// Loads the secondary cell of each lane of `idx` whose `mask` lane is set.
#[inline]
#[target_feature(enable = "neon")]
fn load_groups(t: &Tables<'_>, cell: uint32x4_t, idx: uint32x4_t, mask: uint32x4_t) -> uint32x4_t {
    let mut c = [0u32; LANES];
    let mut i = [0u32; LANES];
    let mut m = [0u32; LANES];
    // SAFETY: each array holds exactly 4 `u32`.
    unsafe {
        vst1q_u32(c.as_mut_ptr(), cell);
        vst1q_u32(i.as_mut_ptr(), idx);
        vst1q_u32(m.as_mut_ptr(), mask);
    }
    for lane in 0..LANES {
        if m[lane] != 0 {
            // Indices were clamped with `vminq_u32` to the last valid cell;
            // `group` still bounds-checks.
            c[lane] = t.group(i[lane] as usize);
        }
    }
    // SAFETY: `c` holds 4 `u32`.
    unsafe { vld1q_u32(c.as_ptr()) }
}

#[inline]
#[target_feature(enable = "neon")]
fn group_mask(cell: uint32x4_t) -> uint32x4_t {
    vcltzq_s32(vreinterpretq_s32_u32(cell))
}

#[inline]
#[target_feature(enable = "neon")]
fn roots4(t: &Tables<'_>, idx: uint32x4_t) -> uint32x4_t {
    let c = [
        root_cell(t, vgetq_lane_u32::<0>(idx)),
        root_cell(t, vgetq_lane_u32::<1>(idx)),
        root_cell(t, vgetq_lane_u32::<2>(idx)),
        root_cell(t, vgetq_lane_u32::<3>(idx)),
    ];
    // SAFETY: `c` holds 4 `u32`.
    unsafe { vld1q_u32(c.as_ptr()) }
}

/// Batch IPv4 lookup. `keys.len() == out.len()`.
#[target_feature(enable = "neon")]
pub(crate) fn v4_batch(t: &Tables<'_>, keys: &[u32], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    let n = keys.len().min(out.len());
    let full = n - n % LANES;
    let max8 = vdupq_n_u32(t.tbl8_max());
    let low = vdupq_n_u32(0xFF);
    let mut start = 0;
    while start < full {
        let end = (start + BLOCK).min(full);
        let mut i = start;
        while i < end {
            // SAFETY: `i + 4 <= full <= keys.len() == out.len()`.
            unsafe {
                let k = vld1q_u32(keys.as_ptr().add(i));
                vst1q_u32(out.as_mut_ptr().add(i), roots4(t, vshrq_n_u32::<8>(k)));
            }
            i += LANES;
        }
        let mut i = start;
        while i < end {
            // SAFETY: as above.
            unsafe {
                let c = vld1q_u32(out.as_ptr().add(i));
                let m = group_mask(c);
                if vmaxvq_u32(m) != 0 {
                    let k = vld1q_u32(keys.as_ptr().add(i));
                    let idx = vminq_u32(vorrq_u32(vshlq_n_u32::<8>(c), vandq_u32(k, low)), max8);
                    vst1q_u32(out.as_mut_ptr().add(i), load_groups(t, c, idx, m));
                }
            }
            i += LANES;
        }
        start = end;
    }
    for i in full..n {
        out[i] = super::scalar::v4_one(t, keys[i]);
    }
}

#[inline]
#[target_feature(enable = "neon")]
fn octet<const S: i32>(w: uint32x4_t) -> uint32x4_t {
    // Register shift (negative = right) because `vshrq_n_u32` rejects 0.
    vandq_u32(vshlq_u32(w, vdupq_n_s32(-S)), vdupq_n_u32(0xFF))
}

/// Lock-step walk of 4 addresses.
#[inline]
#[target_feature(enable = "neon")]
fn v6_x4(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32], max8: uint32x4_t) {
    debug_assert!(keys.len() >= LANES && out.len() >= LANES);
    // Copy the (1-aligned) octets into a `u32`-aligned buffer so `LD4` reads
    // properly aligned elements; this compiles to four 128-bit moves.
    let mut words = [0u32; 4 * LANES];
    for (dst, key) in words.as_chunks_mut::<4>().0.iter_mut().zip(keys) {
        for (d, src) in dst.iter_mut().zip(key.as_chunks::<4>().0) {
            *d = u32::from_le_bytes(*src);
        }
    }
    // SAFETY: `words` holds 16 aligned `u32`; LD4 de-interleaves them so that
    // `w.j` lane `i` is word `j` of address `i`.
    let w = unsafe { vld4q_u32(words.as_ptr()) };
    // Lanes hold octets in memory order (little-endian words): reverse the
    // bytes of word 0 to get `o0 << 24 | o1 << 16 | o2 << 8 | o3`.
    let be0 = vreinterpretq_u32_u8(vrev32q_u8(vreinterpretq_u8_u32(w.0)));
    let mut c = roots4(t, vshrq_n_u32::<8>(be0));
    macro_rules! step {
        ($l:lifetime, $w:expr, $s:literal) => {
            let m = group_mask(c);
            if vmaxvq_u32(m) == 0 {
                break $l;
            }
            let idx = vminq_u32(vorrq_u32(vshlq_n_u32::<8>(c), octet::<$s>($w)), max8);
            c = load_groups(t, c, idx, m);
        };
    }
    'walk: {
        step!('walk, w.0, 24);
        step!('walk, w.1, 0);
        step!('walk, w.1, 8);
        step!('walk, w.1, 16);
        step!('walk, w.1, 24);
        step!('walk, w.2, 0);
        step!('walk, w.2, 8);
        step!('walk, w.2, 16);
        step!('walk, w.2, 24);
        step!('walk, w.3, 0);
        step!('walk, w.3, 8);
        step!('walk, w.3, 16);
        step!('walk, w.3, 24);
    }
    // SAFETY: `out` holds at least 4 cells.
    unsafe { vst1q_u32(out.as_mut_ptr(), c) };
}

/// Batch IPv6 lookup. `keys.len() == out.len()`.
#[target_feature(enable = "neon")]
pub(crate) fn v6_batch(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    let n = keys.len().min(out.len());
    let full = n - n % LANES;
    let max8 = vdupq_n_u32(t.tbl8_max());
    let mut i = 0;
    while i < full {
        v6_x4(t, &keys[i..i + LANES], &mut out[i..i + LANES], max8);
        i += LANES;
    }
    if full < n {
        let rest = n - full;
        let mut k = [[0u8; 16]; LANES];
        k[..rest].copy_from_slice(&keys[full..n]);
        let mut r = [0u32; LANES];
        v6_x4(t, &k, &mut r, max8);
        out[full..n].copy_from_slice(&r[..rest]);
    }
}
