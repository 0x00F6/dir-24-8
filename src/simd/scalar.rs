//! Portable scalar kernels: reference semantics, every single lookup, and
//! batches without `batch-simd` (or on CPUs without a SIMD kernel).
//!
//! Single lookups are the textbook walk. Batch lookups are restructured for
//! memory-level parallelism: all root cells of a block are loaded first
//! (independent loads the out-of-order core overlaps), then the secondary
//! levels are resolved with branch-free selects so that mixed hit/miss
//! patterns do not cause mispredictions that would squash in-flight loads.
//! Inactive lanes read cell 0 of the reserved group 0, which stays in L1.

use super::tables::Tables;
use crate::route::entry::{is_group, tbl8_index};

/// Keys per block in batch kernels (fits comfortably in L1 with outputs).
const BLOCK: usize = 64;

#[inline(always)]
fn group_mask(cell: u32) -> u32 {
    // All ones when the sign (group) bit is set.
    ((cell as i32) >> 31) as u32
}

/// One branch-free secondary step: follows `cell` when it is a group.
#[inline(always)]
fn step(t: &Tables<'_>, cell: u32, byte: u32) -> u32 {
    let m = group_mask(cell);
    let next = t.group(tbl8_index(cell, byte) & m as usize);
    (next & m) | (cell & !m)
}

#[inline(always)]
pub(crate) fn v4_one(t: &Tables<'_>, key: u32) -> u32 {
    let cell = t.root(key >> 8);
    if is_group(cell) {
        t.group(tbl8_index(cell, key))
    } else {
        cell
    }
}

pub(crate) fn v4_batch(t: &Tables<'_>, keys: &[u32], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    for (kb, ob) in keys.chunks(BLOCK).zip(out.chunks_mut(BLOCK)) {
        for (o, &k) in ob.iter_mut().zip(kb) {
            *o = t.root(k >> 8);
        }
        for (o, &k) in ob.iter_mut().zip(kb) {
            *o = step(t, *o, k);
        }
    }
}

#[inline(always)]
fn root6(t: &Tables<'_>, key: &[u8; 16]) -> u32 {
    t.root(u32::from(key[0]) << 16 | u32::from(key[1]) << 8 | u32::from(key[2]))
}

/// Single IPv6 lookup on the numeric key (`u128::from_be_bytes(octets)`),
/// kept in registers: octet `p` is `key >> (120 - 8p)`.
#[inline(always)]
pub(crate) fn v6_one(t: &Tables<'_>, key: u128) -> u32 {
    let mut cell = t.root((key >> 104) as u32);
    let mut shift = 96u32;
    while is_group(cell) {
        cell = t.group(tbl8_index(cell, (key >> shift) as u32));
        if shift == 0 {
            break;
        }
        shift -= 8;
    }
    cell
}

pub(crate) fn v6_batch(t: &Tables<'_>, keys: &[[u8; 16]], out: &mut [u32]) {
    debug_assert_eq!(keys.len(), out.len());
    const B: usize = 32;
    for (kb, ob) in keys.chunks(B).zip(out.chunks_mut(B)) {
        let mut any = 0u32;
        for (o, k) in ob.iter_mut().zip(kb) {
            *o = root6(t, k);
            any |= *o;
        }
        // Lock-step walk: every still-active lane is at the same depth.
        let mut pos = 3;
        while group_mask(any) != 0 && pos < 16 {
            any = 0;
            for (o, k) in ob.iter_mut().zip(kb) {
                *o = step(t, *o, u32::from(k[pos]));
                any |= *o;
            }
            pos += 1;
        }
    }
}
