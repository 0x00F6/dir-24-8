//! Encoding of the 32-bit table cells shared by the IPv4 and IPv6 engines.
//!
//! Every cell of `tbl24` and of the secondary (`tbl8`) groups is a `u32`:
//!
//! | Value                              | Meaning                                     |
//! |------------------------------------|---------------------------------------------|
//! | `0x0000_0000`                      | no match ([`EMPTY`])                        |
//! | `0x0000_0001 ..= 0x7FFF_FFFF`      | terminal route, route slot = `value - 1`    |
//! | `0x8000_0000 \| g`, `g < 2^23`     | secondary group `g` ([`GROUP_FLAG`])        |
//!
//! Design notes:
//!
//! * `0` means "no match" so that freshly mapped, lazily zeroed memory is a
//!   valid empty table and `exist` is a single comparison.
//! * The group flag is the sign bit. AVX2 masked gathers use the sign bit of
//!   each lane as their mask, AVX-512 obtains the mask with one signed
//!   compare and NEON with `vcltzq_s32`, so "is extended" never needs an
//!   extra shift or AND.
//! * `(cell << 8) | byte` drops the flag and yields `g * 256 + byte`. Group
//!   indices are bounded by [`GROUP_LIMIT`] (`2^23`), so this index is always
//!   below `2^31` and therefore non-negative when interpreted as the signed
//!   32-bit index used by x86 gathers.
//! * Terminal cells store a route slot, never a payload: every cell covered
//!   by one route shares the same slot and therefore the same `T`.

/// Cell value meaning "no route covers this address".
pub(crate) const EMPTY: u32 = 0;
/// Sign bit marking a cell that points to a secondary group.
pub(crate) const GROUP_FLAG: u32 = 0x8000_0000;
/// Exclusive upper bound of group indices (keeps `g * 256 + byte < 2^31`).
pub(crate) const GROUP_LIMIT: u32 = 1 << 23;
/// Maximum number of route slots per family (`1..=0x7FFF_FFFF` encodable).
pub(crate) const ROUTE_LIMIT: u32 = 0x7FFF_FFFF;
/// Number of cells in one secondary group.
pub(crate) const GROUP_SIZE: usize = 256;
/// Number of cells in the 24-bit root table.
pub(crate) const TBL24_SIZE: usize = 1 << 24;

/// Returns `true` when `cell` references a secondary group.
#[inline(always)]
pub(crate) const fn is_group(cell: u32) -> bool {
    cell & GROUP_FLAG != 0
}

/// Group index stored in a group cell.
#[inline(always)]
pub(crate) const fn group_index(cell: u32) -> u32 {
    cell & !GROUP_FLAG
}

/// Cell value pointing to group `g`.
#[inline(always)]
pub(crate) const fn group_cell(g: u32) -> u32 {
    debug_assert!(g < GROUP_LIMIT);
    g | GROUP_FLAG
}

/// Cell value for route slot `slot`.
#[inline(always)]
pub(crate) const fn route_cell(slot: u32) -> u32 {
    debug_assert!(slot < ROUTE_LIMIT);
    slot + 1
}

/// Route slot stored in a terminal, non-empty cell.
#[inline(always)]
pub(crate) const fn route_slot(cell: u32) -> u32 {
    cell.wrapping_sub(1)
}

/// Index into the flat `tbl8` array for group cell `cell` and next key byte.
#[inline(always)]
pub(crate) const fn tbl8_index(cell: u32, byte: u32) -> usize {
    ((cell << 8) | (byte & 0xFF)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_round_trips() {
        assert!(!is_group(EMPTY));
        assert!(!is_group(route_cell(0)));
        assert!(!is_group(route_cell(ROUTE_LIMIT - 1)));
        assert_eq!(route_slot(route_cell(41)), 41);
        let g = group_cell(GROUP_LIMIT - 1);
        assert!(is_group(g));
        assert!((g as i32) < 0, "group flag must be the sign bit");
        assert_eq!(group_index(g), GROUP_LIMIT - 1);
        let idx = tbl8_index(g, 0xFF);
        assert_eq!(idx, ((GROUP_LIMIT - 1) as usize) * GROUP_SIZE + 0xFF);
        assert!(idx < (1usize << 31), "gather index must stay non-negative");
    }
}
