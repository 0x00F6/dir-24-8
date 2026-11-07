//! Normalized prefixes of one family.

use core::fmt::Debug;
use core::hash::Hash;

use ipnet::{Ipv4Net, Ipv6Net};

/// A normalized (host bits cleared) prefix of one address family.
pub(crate) trait Prefix: Copy + Eq + Hash + Debug {
    /// Maximum prefix length (32 or 128).
    const MAX_LEN: u8;
    /// Prefix length.
    fn len(&self) -> u8;
    /// The covering prefix of length `len` (`len <= self.len()`), normalized.
    fn truncated(&self, len: u8) -> Self;
}

impl Prefix for Ipv4Net {
    const MAX_LEN: u8 = 32;

    #[inline]
    fn len(&self) -> u8 {
        self.prefix_len()
    }

    #[inline]
    fn truncated(&self, len: u8) -> Self {
        debug_assert!(len <= Self::MAX_LEN);
        Ipv4Net::new(self.network(), len.min(Self::MAX_LEN))
            .map(|n| n.trunc())
            .unwrap_or(*self)
    }
}

impl Prefix for Ipv6Net {
    const MAX_LEN: u8 = 128;

    #[inline]
    fn len(&self) -> u8 {
        self.prefix_len()
    }

    #[inline]
    fn truncated(&self, len: u8) -> Self {
        debug_assert!(len <= Self::MAX_LEN);
        Ipv6Net::new(self.network(), len.min(Self::MAX_LEN))
            .map(|n| n.trunc())
            .unwrap_or(*self)
    }
}
