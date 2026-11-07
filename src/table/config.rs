//! Table configuration.

use crate::error::Error;
use crate::route::entry::{GROUP_LIMIT, ROUTE_LIMIT};

/// Table configuration (builder style).
///
/// There is no kernel choice: single lookups run the scalar kernels, and
/// batch lookups too unless the `batch-simd` feature is enabled, in which
/// case they use the SIMD kernel detected on the CPU.
///
/// ```
/// use dir_24_8::{Config, IpTable};
///
/// let table: IpTable<u32> =
///     IpTable::with_config(Config::new().max_ipv4_groups(1 << 16).huge_pages(true))?;
/// assert!(table.is_empty());
/// # Ok::<(), dir_24_8::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub(crate) huge_pages: bool,
    pub(crate) max_ipv4_groups: u32,
    pub(crate) max_ipv6_groups: u32,
    pub(crate) max_routes: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    /// Upper bound of groups per family imposed by the cell encoding
    /// (`2^23 - 1`, i.e. 8 GiB of 1 KiB groups).
    pub const MAX_GROUPS: u32 = GROUP_LIMIT - 1;
    /// Upper bound of routes per family imposed by the cell encoding.
    pub const MAX_ROUTES: u32 = ROUTE_LIMIT;

    /// Default configuration: no huge-page advice, limits at the encoding
    /// maxima (memory grows only with actual use).
    ///
    /// ```
    /// use dir_24_8::{Config, IpTable};
    ///
    /// let config = Config::new();
    /// assert_eq!(config, Config::default());
    /// let table: IpTable<u32> = IpTable::with_config(config)?;
    /// assert!(table.is_empty());
    /// # Ok::<(), dir_24_8::Error>(())
    /// ```
    pub const fn new() -> Self {
        Self {
            huge_pages: false,
            max_ipv4_groups: Self::MAX_GROUPS,
            max_ipv6_groups: Self::MAX_GROUPS,
            max_routes: Self::MAX_ROUTES,
        }
    }

    /// Advise the 64 MiB roots (and group buffers of 2 MiB or more) for
    /// 2 MiB transparent huge pages (Linux, `MADV_HUGEPAGE`, 2 MiB aligned
    /// mappings). Ignored on other systems. Default: `false`.
    ///
    /// Huge pages remove most TLB misses of random lookups when the kernel
    /// can provide them, but with THP `defrag=madvise` every first touch of
    /// a 2 MiB region may run synchronous compaction: on a machine with
    /// fragmented memory this made table construction 15-25x slower while
    /// no lookup gain could be measured (see `docs/performance.md`). Enable
    /// it for long-lived, lookup-heavy tables on hosts with free huge pages.
    ///
    /// ```
    /// use dir_24_8::{Config, IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::with_config(Config::new().huge_pages(true))?;
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// assert_eq!(table.lookup_data("10.1.2.3".parse()?), Some(&1));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn huge_pages(mut self, enabled: bool) -> Self {
        self.huge_pages = enabled;
        self
    }

    /// Maximum number of IPv4 secondary groups in use (1 KiB each).
    ///
    /// ```
    /// use dir_24_8::{Config, Error, IpNet, IpTable, Replace, Resource};
    ///
    /// let mut table = IpTable::with_config(Config::new().max_ipv4_groups(1))?;
    /// table.insert("10.0.0.0/25".parse::<IpNet>()?, Some(1), Replace)?; // longer than /24: one group
    /// let err = table.insert("10.0.1.0/25".parse::<IpNet>()?, Some(2), Replace).unwrap_err();
    /// assert!(matches!(err, Error::CapacityExceeded { resource: Resource::Groups, .. }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn max_ipv4_groups(mut self, groups: u32) -> Self {
        self.max_ipv4_groups = groups;
        self
    }

    /// Maximum number of IPv6 secondary groups in use (1 KiB each).
    ///
    /// ```
    /// use dir_24_8::{Config, Error, IpNet, IpTable, Replace, Resource};
    ///
    /// let mut table = IpTable::with_config(Config::new().max_ipv6_groups(1))?;
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(1), Replace)?; // longer than /24: one group
    /// let err = table.insert("2002::/32".parse::<IpNet>()?, Some(2), Replace).unwrap_err();
    /// assert!(matches!(err, Error::CapacityExceeded { resource: Resource::Groups, .. }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn max_ipv6_groups(mut self, groups: u32) -> Self {
        self.max_ipv6_groups = groups;
        self
    }

    /// Maximum number of routes per family.
    ///
    /// ```
    /// use dir_24_8::{Config, Error, IpNet, IpTable, Replace, Resource};
    ///
    /// let mut table = IpTable::with_config(Config::new().max_routes(1))?;
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// let err = table.insert("11.0.0.0/8".parse::<IpNet>()?, Some(2), Replace).unwrap_err();
    /// assert!(matches!(err, Error::CapacityExceeded { resource: Resource::Routes, .. }));
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(3), Replace)?; // the limit is per family
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn max_routes(mut self, routes: u32) -> Self {
        self.max_routes = routes;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.max_ipv4_groups > Self::MAX_GROUPS || self.max_ipv6_groups > Self::MAX_GROUPS {
            return Err(Error::InvalidConfig("group limit above 2^23 - 1"));
        }
        if self.max_routes > Self::MAX_ROUTES {
            return Err(Error::InvalidConfig("route limit above 2^31 - 1"));
        }
        Ok(())
    }
}
