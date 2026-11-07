//! Borrowed lookup results and compact match identifiers.

use core::fmt;
use core::marker::PhantomData;

use ipnet::IpNet;

/// Result of a longest-prefix match.
///
/// * `prefix` is the normalized CIDR that won the match;
/// * `data` borrows the payload stored with that CIDR, or is `None` when the
///   route was inserted without payload.
///
/// A lookup returns `Option<LookupMatch>`: `None` means that no route covers
/// the address. A more specific route without payload still wins the match
/// (`Some(LookupMatch { data: None, .. })`), it never falls back to a less
/// specific route that has data.
///
/// `LookupMatch` is `Copy` for every `T`: it only holds a prefix and a
/// reference, the payload itself is never cloned.
///
/// ```
/// use dir_24_8::{IpNet, IpTable, LookupMatch, Replace};
///
/// let mut table = IpTable::new();
/// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(String::from("corp")), Replace)?;
/// match table.lookup("10.20.30.40".parse()?) {
///     Some(LookupMatch { prefix, data: Some(name) }) => println!("{prefix}: {name}"),
///     Some(LookupMatch { prefix, data: None }) => println!("{prefix}: no payload"),
///     None => println!("no route"),
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct LookupMatch<'a, T> {
    /// Winning normalized CIDR.
    pub prefix: IpNet,
    /// Payload of the winning CIDR, borrowed from the table.
    pub data: Option<&'a T>,
}

// Manual impls: derives would require `T: Clone`/`T: Copy`/`T: PartialEq`
// although only a reference to `T` is stored.
impl<T> Clone for LookupMatch<'_, T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for LookupMatch<'_, T> {}

impl<T: PartialEq> PartialEq for LookupMatch<'_, T> {
    fn eq(&self, other: &Self) -> bool {
        self.prefix == other.prefix && self.data == other.data
    }
}

impl<T: Eq> Eq for LookupMatch<'_, T> {}

impl<T: fmt::Debug> fmt::Debug for LookupMatch<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LookupMatch")
            .field("prefix", &self.prefix)
            .field("data", &self.data)
            .finish()
    }
}

impl<'a, T> LookupMatch<'a, T> {
    /// `true` when the winning route carries a payload.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("10.1.0.0/16".parse::<IpNet>()?, None, Replace)?;
    /// assert!(table.lookup("10.2.0.1".parse()?).unwrap().has_data());
    /// assert!(!table.lookup("10.1.0.1".parse()?).unwrap().has_data()); // the /16 has no payload
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn has_data(&self) -> bool {
        self.data.is_some()
    }
}

/// Bit marking an IPv6 identifier (terminal cells never use the sign bit).
pub(crate) const MATCH_V6: u32 = 0x8000_0000;

/// Compact result of [`IpTable::lookup_ids_into`](crate::IpTable::lookup_ids_into).
///
/// It is the raw terminal cell produced by the lookup kernels (plus a family
/// bit), i.e. the intermediate result the batch lookups materialize into
/// [`LookupMatch`]. Converting it back is done with
/// [`IpTable::resolve`](crate::IpTable::resolve), which costs one slot read.
///
/// The lifetime ties identifiers to the shared borrow of the table that
/// produced them, so they cannot survive a mutation. Resolving an identifier
/// on a different table is a logic error that returns an unspecified route
/// or `None`, never undefined behavior.
///
/// ```
/// use dir_24_8::{IpNet, IpTable, MatchId, Replace};
///
/// let mut table = IpTable::new();
/// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(7), Replace)?;
/// let mut ids = [MatchId::NONE; 2];
/// table.lookup_ids_into(&[u128::from_be_bytes([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]), 1], &mut ids)?;
/// assert!(ids[0].is_match() && ids[0].is_ipv6());
/// assert_eq!(table.resolve(ids[0]).and_then(|m| m.data), Some(&7));
/// assert!(table.resolve(ids[1]).is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[repr(transparent)]
pub struct MatchId<'t> {
    raw: u32,
    _table: PhantomData<&'t ()>,
}

impl<'t> MatchId<'t> {
    /// Identifier meaning "no route matched".
    pub const NONE: Self = Self::from_raw(0);

    #[inline(always)]
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self {
            raw,
            _table: PhantomData,
        }
    }

    #[inline(always)]
    pub(crate) const fn raw(self) -> u32 {
        self.raw
    }

    /// `true` when a route matched.
    ///
    /// ```
    /// use std::net::Ipv4Addr;
    /// use dir_24_8::{IpNet, IpTable, MatchId, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(()), Replace)?;
    /// let mut ids = [MatchId::NONE; 2];
    /// table.lookup_ids_into(&[Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(192, 0, 2, 1)], &mut ids)?;
    /// assert!(ids[0].is_match());
    /// assert!(!ids[1].is_match());
    /// assert!(!MatchId::NONE.is_match());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline(always)]
    pub const fn is_match(self) -> bool {
        self.raw != 0
    }

    /// `true` when the identifier refers to an IPv6 route.
    ///
    /// ```
    /// use std::net::IpAddr;
    /// use dir_24_8::{IpNet, IpTable, MatchId, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(4), Replace)?;
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(6), Replace)?;
    /// let keys: [IpAddr; 2] = ["10.0.0.1".parse()?, "2001:db8::1".parse()?];
    /// let mut ids = [MatchId::NONE; 2];
    /// table.lookup_ids_into(&keys, &mut ids)?;
    /// assert!(!ids[0].is_ipv6());
    /// assert!(ids[1].is_ipv6());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline(always)]
    pub const fn is_ipv6(self) -> bool {
        self.raw & MATCH_V6 != 0
    }
}

impl Clone for MatchId<'_> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for MatchId<'_> {}

impl PartialEq for MatchId<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for MatchId<'_> {}

impl core::hash::Hash for MatchId<'_> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl Default for MatchId<'_> {
    fn default() -> Self {
        Self::NONE
    }
}

impl fmt::Debug for MatchId<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_match() {
            return f.write_str("MatchId(none)");
        }
        let family = if self.is_ipv6() { "v6" } else { "v4" };
        write!(f, "MatchId({family}:{})", self.raw & !MATCH_V6)
    }
}

/// Reinterprets an identifier buffer as raw cells for the kernels.
#[inline(always)]
pub(crate) fn ids_as_cells<'s>(ids: &'s mut [MatchId<'_>]) -> &'s mut [u32] {
    // SAFETY: `MatchId` is `repr(transparent)` over `u32` (the PhantomData
    // field is a 1-aligned ZST), so both slices have identical layout, and
    // every `u32` bit pattern is a valid `MatchId`.
    unsafe { core::slice::from_raw_parts_mut(ids.as_mut_ptr().cast::<u32>(), ids.len()) }
}
