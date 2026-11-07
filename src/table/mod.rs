//! Public facade: [`IpTable`], configuration and statistics.

pub(crate) mod buffer;
mod config;
pub(crate) mod engine;
pub(crate) mod groups;
pub(crate) mod stats;

use core::fmt;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;

pub use config::Config;
pub use stats::{FamilyStats, TableStats};

use crate::error::Error;
use crate::ipv4::Ipv4Engine;
use crate::ipv6::Ipv6Engine;
use crate::lookup::batch::{BatchCtx, BatchKey, ExistSink, IdSink, MatchSink};
use crate::lookup::dispatch::Kernel;
use crate::lookup::result::{MATCH_V6, ids_as_cells};
use crate::lookup::{LookupMatch, MatchId};
use crate::merge::{MergeOutcome, merge_payload};
use crate::mode::InsertMode;
use crate::simd::scalar;
use engine::EngineLimits;

/// Longest-prefix-match table for IPv4 and IPv6 CIDRs with optional
/// payloads.
///
/// * IPv4 routes live in a DIR-24-8 engine (one or two memory accesses per
///   lookup), IPv6 routes in a 24/8 multi-level trie (one access per level,
///   up to 14). Each family's 64 MiB root is allocated on the first insert
///   of that family.
/// * IPv4 and IPv6 are separate key spaces: `192.0.2.1` and its
///   IPv4-mapped form `::ffff:192.0.2.1` are different keys.
/// * Lookups borrow (`&self`), never allocate, never clone `T` and take no
///   lock: any number of threads can look up concurrently when `T: Sync`.
/// * Single lookups and `exist` run the scalar kernels. Batch lookups
///   (`lookup_batch_into`, `lookup_array`, `exist_batch_into`,
///   `exist_array`, `lookup_ids_into`) run them too, unless the
///   `batch-simd` feature is enabled: they then use AVX-512 or AVX2
///   (x86-64) or NEON (AArch64) when the CPU has it, detected once when the
///   table is built.
///
/// ```
/// use dir_24_8::{IpNet, IpTable, Replace};
///
/// let mut table = IpTable::new();
/// table.insert("10.0.0.0/8".parse::<IpNet>().unwrap(), Some("corp"), Replace).unwrap();
/// table.insert("10.1.0.0/16".parse::<IpNet>().unwrap(), None, Replace).unwrap();
///
/// let m = table.lookup("10.2.3.4".parse().unwrap()).unwrap();
/// assert_eq!(m.prefix.to_string(), "10.0.0.0/8");
/// assert_eq!(m.data, Some(&"corp"));
///
/// // The more specific route wins even without payload.
/// let m = table.lookup("10.1.3.4".parse().unwrap()).unwrap();
/// assert_eq!(m.prefix.to_string(), "10.1.0.0/16");
/// assert_eq!(m.data, None);
///
/// assert!(table.exist("10.1.3.4".parse().unwrap()));
/// assert!(table.lookup("192.0.2.1".parse().unwrap()).is_none());
/// ```
pub struct IpTable<T> {
    config: Config,
    batch: Kernel,
    v4: Option<Ipv4Engine<T>>,
    v6: Option<Ipv6Engine<T>>,
}

impl<T> Default for IpTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for IpTable<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpTable")
            .field("batch_kernel", &self.batch.name())
            .field("ipv4_routes", &self.len_ipv4())
            .field("ipv6_routes", &self.len_ipv6())
            .finish_non_exhaustive()
    }
}

#[inline]
fn check_len(input: usize, output: usize) -> Result<(), Error> {
    if input == output {
        Ok(())
    } else {
        Err(Error::LengthMismatch { input, output })
    }
}

impl<T> IpTable<T> {
    /// Empty table with the default configuration. No table memory is
    /// allocated until the first insertion.
    ///
    /// ```
    /// use dir_24_8::IpTable;
    ///
    /// let table: IpTable<String> = IpTable::new();
    /// assert!(table.is_empty());
    /// assert_eq!(table.stats().total_bytes(), 0); // nothing allocated before the first insertion
    /// ```
    pub fn new() -> Self {
        Self::with_config(Config::new()).expect("the default configuration is always valid")
    }

    /// Empty table with an explicit configuration. Fails when a limit is
    /// out of range.
    ///
    /// ```
    /// use dir_24_8::{Config, Error, IpTable};
    ///
    /// let table: IpTable<u8> = IpTable::with_config(Config::new().max_routes(1_000))?;
    /// assert!(table.is_empty());
    /// let err = IpTable::<u8>::with_config(Config::new().max_routes(u32::MAX)).unwrap_err();
    /// assert!(matches!(err, Error::InvalidConfig(_)));
    /// # Ok::<(), dir_24_8::Error>(())
    /// ```
    pub fn with_config(config: Config) -> Result<Self, Error> {
        config.validate()?;
        Ok(Self {
            config,
            batch: Kernel::detect(),
            v4: None,
            v6: None,
        })
    }

    /// Configuration the table was built with.
    ///
    /// ```
    /// use dir_24_8::{Config, IpTable};
    ///
    /// let config = Config::new().max_routes(1_000);
    /// let table: IpTable<u32> = IpTable::with_config(config.clone())?;
    /// assert_eq!(table.config(), &config);
    /// # Ok::<(), dir_24_8::Error>(())
    /// ```
    pub fn config(&self) -> &Config {
        &self.config
    }

    fn limits(&self) -> EngineLimits {
        EngineLimits {
            max_routes: self.config.max_routes,
            max_ipv4_groups: self.config.max_ipv4_groups,
            max_ipv6_groups: self.config.max_ipv6_groups,
            huge_pages: self.config.huge_pages,
        }
    }

    fn v4_mut(&mut self) -> Result<&mut Ipv4Engine<T>, Error> {
        if self.v4.is_none() {
            self.v4 = Some(Ipv4Engine::new(&self.limits())?);
        }
        Ok(self.v4.as_mut().expect("just allocated"))
    }

    fn v6_mut(&mut self) -> Result<&mut Ipv6Engine<T>, Error> {
        if self.v6.is_none() {
            self.v6 = Some(Ipv6Engine::new(&self.limits())?);
        }
        Ok(self.v6.as_mut().expect("just allocated"))
    }

    // ------------------------------------------------------------------
    // Mutation
    // ------------------------------------------------------------------

    /// Inserts a CIDR (normalized: host bits are cleared) with an optional
    /// payload. `mode` decides what happens when the CIDR is already present:
    ///
    /// * [`Replace`](crate::Replace): its payload is replaced (including by
    ///   `None`); returns the previous payload, or `None` for a new CIDR.
    /// * [`Merge`](crate::Merge): its payload is deep merged
    ///   (`T: `[`DeepMerge`](crate::DeepMerge)); returns a
    ///   [`MergeOutcome`](crate::MergeOutcome).
    /// * [`MergeWith`](crate::MergeWith)`(f)`: merged by your function.
    /// * `SerdeMerge` (feature `serde`): deep merged through
    ///   `serde_json::Value`.
    ///
    /// See [`crate::mode`] for the full table. On error the table is
    /// unchanged.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Merge, MergeOutcome, Replace};
    ///
    /// let mut table: IpTable<Vec<u32>> = IpTable::new();
    /// let net: IpNet = "10.1.2.3/16".parse()?; // stored as 10.1.0.0/16
    ///
    /// assert_eq!(table.insert(net, Some(vec![1]), Replace)?, None);
    /// assert_eq!(table.insert(net, Some(vec![2]), Merge)?, MergeOutcome::Merged);
    /// assert_eq!(table.lookup("10.1.200.1".parse()?).unwrap().data, Some(&vec![1, 2]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// Allocations: the family's root (64 MiB, lazily zeroed) on its first
    /// insertion, possibly one slot/index growth, and new secondary groups
    /// (1 KiB each; the group buffer grows geometrically).
    pub fn insert<M: InsertMode<T>>(
        &mut self,
        net: impl Into<IpNet>,
        data: Option<T>,
        mode: M,
    ) -> Result<M::Output, Error> {
        mode.apply(self, net.into(), data)
    }

    /// Inserts a single address as a host route (`/32` or `/128`), with the
    /// same `mode` semantics as [`insert`](Self::insert).
    ///
    /// ```
    /// use std::net::Ipv4Addr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert_addr(Ipv4Addr::new(192, 0, 2, 1), Some("host"), Replace)?;
    /// assert_eq!(table.get("192.0.2.1/32".parse::<IpNet>()?), Some(Some(&"host")));
    /// assert_eq!(table.lookup_data("192.0.2.1".parse()?), Some(&"host"));
    /// assert_eq!(table.lookup_data("192.0.2.2".parse()?), None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn insert_addr<M: InsertMode<T>>(
        &mut self,
        ip: impl Into<IpAddr>,
        data: Option<T>,
        mode: M,
    ) -> Result<M::Output, Error> {
        self.insert(IpNet::from(ip.into()), data, mode)
    }

    /// Inserts every `(cidr, payload)` pair with `mode`, stopping at the
    /// first error (the routes inserted before it stay).
    ///
    /// ```
    /// use dir_24_8::{IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.try_extend(
    ///     [("10.0.0.0/8".parse()?, Some(1)), ("2001:db8::/32".parse()?, Some(2))],
    ///     Replace,
    /// )?;
    /// assert_eq!(table.len(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_extend<I, M>(&mut self, routes: I, mode: M) -> Result<(), Error>
    where
        I: IntoIterator<Item = (IpNet, Option<T>)>,
        M: InsertMode<T> + Clone,
    {
        for (net, data) in routes {
            self.insert(net, data, mode.clone())?;
        }
        Ok(())
    }

    /// [`Replace`](crate::Replace) mode.
    pub(crate) fn insert_replacing(
        &mut self,
        net: IpNet,
        data: Option<T>,
    ) -> Result<Option<Option<T>>, Error> {
        match net {
            IpNet::V4(n) => self.v4_mut()?.insert(n, data),
            IpNet::V6(n) => self.v6_mut()?.insert(n, data),
        }
    }

    /// Merging modes: payload rules of [`crate::merge`]; on error the
    /// existing payload must be left unchanged by `merge` and the error is
    /// returned as [`Error::MergeFailed`].
    pub(crate) fn insert_merging<E>(
        &mut self,
        net: IpNet,
        data: Option<T>,
        merge: impl FnOnce(&mut T, T) -> Result<(), E>,
    ) -> Result<MergeOutcome, Error>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let net = net.trunc();
        match self.get_mut(net) {
            Some(existing) => {
                merge_payload(existing, data, merge).map_err(|e| Error::MergeFailed(Box::new(e)))
            }
            None => self
                .insert_replacing(net, data)
                .map(|_| MergeOutcome::Inserted),
        }
    }

    /// Replaces the payload of an existing CIDR without touching the lookup
    /// tables. Returns the previous payload, or `None` (and drops `data`)
    /// when the CIDR is absent.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let net: IpNet = "10.0.0.0/8".parse()?;
    /// let mut table = IpTable::new();
    /// assert_eq!(table.replace(net, Some(1)), None); // absent: nothing inserted
    /// table.insert(net, Some(1), Replace)?;
    /// assert_eq!(table.replace(net, Some(2)), Some(Some(1)));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn replace(&mut self, net: impl Into<IpNet>, data: Option<T>) -> Option<Option<T>> {
        let slot = self.get_mut(net)?;
        Some(core::mem::replace(slot, data))
    }

    /// Removes a CIDR. Addresses it covered fall back to the best remaining
    /// less specific route. Returns the removed payload, or `None` when the
    /// CIDR is absent. Never allocates.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some("wide"), Replace)?;
    /// table.insert("10.1.0.0/16".parse::<IpNet>()?, Some("narrow"), Replace)?;
    /// assert_eq!(table.remove("10.1.0.0/16".parse::<IpNet>()?), Some(Some("narrow")));
    /// // The addresses of the removed route fall back to the parent.
    /// assert_eq!(table.lookup_data("10.1.2.3".parse()?), Some(&"wide"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn remove(&mut self, net: impl Into<IpNet>) -> Option<Option<T>> {
        match net.into() {
            IpNet::V4(n) => self.v4.as_mut()?.remove(n),
            IpNet::V6(n) => self.v6.as_mut()?.remove(n),
        }
    }

    /// Removes every route and releases all table memory.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.clear();
    /// assert!(table.is_empty());
    /// assert_eq!(table.stats().total_bytes(), 0); // the 64 MiB root is released too
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn clear(&mut self) {
        self.v4 = None;
        self.v6 = None;
    }

    // ------------------------------------------------------------------
    // Exact-CIDR access (not LPM)
    // ------------------------------------------------------------------

    /// Payload of an exact (normalized) CIDR: `None` when the CIDR is not a
    /// route, `Some(None)` for a route without payload.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// assert_eq!(table.get("10.9.9.9/8".parse::<IpNet>()?), Some(Some(&1))); // normalized
    /// assert_eq!(table.get("10.0.0.0/16".parse::<IpNet>()?), None); // exact match only, not LPM
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get(&self, net: impl Into<IpNet>) -> Option<Option<&T>> {
        match net.into() {
            IpNet::V4(n) => self.v4.as_ref()?.get(n).map(|s| s.data.as_ref()),
            IpNet::V6(n) => self.v6.as_ref()?.get(n).map(|s| s.data.as_ref()),
        }
    }

    /// Mutable payload of an exact CIDR. Changing it never touches the
    /// lookup tables.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let net: IpNet = "10.0.0.0/8".parse()?;
    /// let mut table = IpTable::new();
    /// table.insert(net, Some(1), Replace)?;
    /// if let Some(Some(counter)) = table.get_mut(net) {
    ///     *counter += 1;
    /// }
    /// assert_eq!(table.lookup_data("10.9.9.9".parse()?), Some(&2));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_mut(&mut self, net: impl Into<IpNet>) -> Option<&mut Option<T>> {
        match net.into() {
            IpNet::V4(n) => self.v4.as_mut()?.get_mut(n),
            IpNet::V6(n) => self.v6.as_mut()?.get_mut(n),
        }
    }

    /// `true` when this exact (normalized) CIDR is a route. Unlike
    /// [`exist`](Self::exist) this is not a longest-prefix match.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table: IpTable<()> = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, None, Replace)?;
    /// assert!(table.contains_prefix("10.0.0.0/8".parse::<IpNet>()?));
    /// assert!(!table.contains_prefix("10.0.0.0/9".parse::<IpNet>()?)); // covered, but not a route
    /// assert!(table.exist("10.0.0.1".parse()?));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn contains_prefix(&self, net: impl Into<IpNet>) -> bool {
        self.get(net).is_some()
    }

    /// Number of routes (both families).
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(2), Replace)?; // same CIDR
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(3), Replace)?;
    /// assert_eq!(table.len(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn len(&self) -> usize {
        self.len_ipv4() + self.len_ipv6()
    }

    /// `true` without any route.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// assert!(table.is_empty());
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// assert!(!table.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of IPv4 routes.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("::ffff:10.0.0.0/104".parse::<IpNet>()?, Some(2), Replace)?; // IPv6 (mapped)
    /// assert_eq!((table.len_ipv4(), table.len_ipv6()), (1, 1));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn len_ipv4(&self) -> usize {
        self.v4.as_ref().map_or(0, |e| e.len())
    }

    /// Number of IPv6 routes.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("2001:db8::/48".parse::<IpNet>()?, Some(2), Replace)?;
    /// assert_eq!((table.len_ipv4(), table.len_ipv6()), (0, 2));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn len_ipv6(&self) -> usize {
        self.v6.as_ref().map_or(0, |e| e.len())
    }

    /// Iterates over all routes (IPv4 first, unspecified order inside a
    /// family).
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, None, Replace)?;
    /// let mut routes: Vec<String> = table.iter().map(|(net, _)| net.to_string()).collect();
    /// routes.sort();
    /// assert_eq!(routes, ["10.0.0.0/8", "2001:db8::/32"]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (IpNet, Option<&T>)> {
        let v4 = self
            .v4
            .iter()
            .flat_map(|e| e.iter().map(|s| (IpNet::V4(s.net), s.data.as_ref())));
        let v6 = self
            .v6
            .iter()
            .flat_map(|e| e.iter().map(|s| (IpNet::V6(s.net), s.data.as_ref())));
        v4.chain(v6)
    }

    /// Iterates over all routes with mutable payloads.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(2), Replace)?;
    /// for (_, payload) in table.iter_mut() {
    ///     if let Some(v) = payload {
    ///         *v *= 10;
    ///     }
    /// }
    /// assert_eq!(table.lookup_data("2001:db8::1".parse()?), Some(&20));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (IpNet, &mut Option<T>)> {
        let v4 = self
            .v4
            .iter_mut()
            .flat_map(|e| e.iter_mut().map(|s| (IpNet::V4(s.net), &mut s.data)));
        let v6 = self
            .v6
            .iter_mut()
            .flat_map(|e| e.iter_mut().map(|s| (IpNet::V6(s.net), &mut s.data)));
        v4.chain(v6)
    }

    /// Route, group and memory statistics.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.1.2.0/25".parse::<IpNet>()?, Some(()), Replace)?;
    /// let stats = table.stats();
    /// assert_eq!(stats.ipv4.routes, 1);
    /// assert_eq!(stats.ipv4.groups_in_use, 1); // a /25 needs one secondary group
    /// assert!(!stats.ipv6.allocated);           // no IPv6 route: no IPv6 root
    /// println!("{stats}");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn stats(&self) -> TableStats {
        TableStats {
            ipv4: self.v4.as_ref().map(|e| e.stats()).unwrap_or_default(),
            ipv6: self.v6.as_ref().map(|e| e.stats()).unwrap_or_default(),
            batch_kernel: self.batch.name(),
        }
    }

    // ------------------------------------------------------------------
    // Single lookups
    // ------------------------------------------------------------------

    /// Longest-prefix match of one address.
    ///
    /// * `None`: no route covers `ip`;
    /// * `Some(m)` with `m.data == None`: the winning route has no payload;
    /// * `Some(m)` with `m.data == Some(&t)`: the winning route's payload.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some("doc"), Replace)?;
    /// let m = table.lookup("2001:db8::1".parse()?).unwrap();
    /// assert_eq!((m.prefix.to_string(), m.data), ("2001:db8::/32".to_string(), Some(&"doc")));
    /// assert!(table.lookup("2001:db9::1".parse()?).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn lookup(&self, ip: IpAddr) -> Option<LookupMatch<'_, T>> {
        match ip {
            IpAddr::V4(a) => self.lookup_ipv4(a),
            IpAddr::V6(a) => self.lookup_ipv6(a),
        }
    }

    /// Longest-prefix match of an IPv4 address (one or two table accesses).
    ///
    /// ```
    /// use std::net::Ipv4Addr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.1.0.0/16".parse::<IpNet>()?, Some("site"), Replace)?;
    /// let m = table.lookup_ipv4(Ipv4Addr::new(10, 1, 2, 3)).unwrap();
    /// assert_eq!((m.prefix.to_string(), m.data), ("10.1.0.0/16".to_string(), Some(&"site")));
    /// assert!(table.lookup_ipv4(Ipv4Addr::new(10, 2, 0, 1)).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn lookup_ipv4(&self, ip: Ipv4Addr) -> Option<LookupMatch<'_, T>> {
        let e = self.v4.as_ref()?;
        e.resolve(scalar::v4_one(&e.tables(), u32::from(ip)))
    }

    /// Longest-prefix match of an IPv6 address (one access per level).
    ///
    /// ```
    /// use std::net::Ipv6Addr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("2001:db8:1200::/41".parse::<IpNet>()?, Some(41), Replace)?; // any length
    /// let ip: Ipv6Addr = "2001:db8:1234::1".parse()?;
    /// assert_eq!(table.lookup_ipv6(ip).unwrap().data, Some(&41));
    /// assert!(table.lookup_ipv6("2001:db8:ff00::1".parse()?).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn lookup_ipv6(&self, ip: Ipv6Addr) -> Option<LookupMatch<'_, T>> {
        let e = self.v6.as_ref()?;
        e.resolve(scalar::v6_one(&e.tables(), u128::from(ip)))
    }

    /// Payload of the longest-prefix match. Convenience over
    /// [`lookup`](Self::lookup): it returns `None` both when nothing matches
    /// and when the winning route has no payload (it does not fall back to
    /// a less specific route with data).
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(8), Replace)?;
    /// table.insert("10.1.0.0/16".parse::<IpNet>()?, None, Replace)?;
    /// assert_eq!(table.lookup_data("10.2.0.1".parse()?), Some(&8));
    /// // The /16 wins and has no payload: no fallback to the /8.
    /// assert_eq!(table.lookup_data("10.1.0.1".parse()?), None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn lookup_data(&self, ip: IpAddr) -> Option<&T> {
        self.lookup(ip)?.data
    }

    /// `true` when at least one route (with or without payload) contains
    /// `ip`. Never reads route slots or payloads.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table: IpTable<()> = IpTable::new();
    /// table.insert("192.0.2.0/24".parse::<IpNet>()?, None, Replace)?; // no payload needed
    /// assert!(table.exist("192.0.2.200".parse()?));
    /// assert!(!table.exist("198.51.100.1".parse()?));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn exist(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(a) => self.exist_ipv4(a),
            IpAddr::V6(a) => self.exist_ipv6(a),
        }
    }

    /// [`exist`](Self::exist) for an IPv4 address.
    ///
    /// ```
    /// use std::net::Ipv4Addr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table: IpTable<()> = IpTable::new();
    /// table.insert("192.0.2.0/24".parse::<IpNet>()?, None, Replace)?;
    /// assert!(table.exist_ipv4(Ipv4Addr::new(192, 0, 2, 77)));
    /// assert!(!table.exist_ipv4(Ipv4Addr::new(198, 51, 100, 1)));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn exist_ipv4(&self, ip: Ipv4Addr) -> bool {
        self.v4
            .as_ref()
            .is_some_and(|e| scalar::v4_one(&e.tables(), u32::from(ip)) != 0)
    }

    /// [`exist`](Self::exist) for an IPv6 address.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table: IpTable<()> = IpTable::new();
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, None, Replace)?;
    /// assert!(table.exist_ipv6("2001:db8::42".parse()?));
    /// assert!(!table.exist_ipv6("::ffff:192.0.2.1".parse()?)); // separate key space
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn exist_ipv6(&self, ip: Ipv6Addr) -> bool {
        self.v6
            .as_ref()
            .is_some_and(|e| scalar::v6_one(&e.tables(), u128::from(ip)) != 0)
    }

    // ------------------------------------------------------------------
    // Batch lookups
    // ------------------------------------------------------------------

    fn ctx(&self) -> BatchCtx<'_, T> {
        BatchCtx {
            kernel: self.batch,
            v4: self.v4.as_ref(),
            v6: self.v6.as_ref(),
        }
    }

    /// Batch longest-prefix match into a caller-provided buffer.
    ///
    /// `keys` may be `&[IpAddr]` (mixed families), `&[Ipv4Addr]`,
    /// `&[Ipv6Addr]`, or the numeric fast paths `&[u32]` / `&[u128]` /
    /// `&[[u8; 4]]` / `&[[u8; 16]]` (see [`BatchKey`]). `output` must have
    /// exactly `keys.len()` slots; it is validated before any write and
    /// result `i` corresponds to key `i`.
    ///
    /// No allocation: keys are processed in chunks of 64 through stack
    /// buffers. With the `batch-simd` feature the chunks run through the
    /// SIMD kernel detected on this CPU (AVX-512, AVX2 or NEON), otherwise
    /// through the scalar one; the results are the same.
    ///
    /// ```
    /// use std::net::Ipv4Addr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some("a"), Replace)?;
    /// let keys = [Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(192, 0, 2, 1)];
    /// let mut out = [None; 2];
    /// table.lookup_batch_into(&keys, &mut out)?;
    /// assert_eq!(out[0].unwrap().data, Some(&"a"));
    /// assert!(out[1].is_none());
    /// // The output length must match the input length.
    /// assert!(table.lookup_batch_into(&keys, &mut [None; 3]).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn lookup_batch_into<'a, K: BatchKey>(
        &'a self,
        keys: &[K],
        output: &mut [Option<LookupMatch<'a, T>>],
    ) -> Result<(), Error> {
        check_len(keys.len(), output.len())?;
        let mut sink = MatchSink {
            out: output,
            v4: self.v4.as_ref(),
            v6: self.v6.as_ref(),
        };
        self.ctx().run(keys, &mut sink);
        Ok(())
    }

    /// Batch lookup of a fixed-size array, returned by value.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// // Numeric IPv4 keys: `u32::from(Ipv4Addr)`.
    /// let [a, b] = table.lookup_array(&[0x0A00_0001u32, 0xC000_0201]);
    /// assert_eq!((a.and_then(|m| m.data), b.is_none()), (Some(&1), true));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn lookup_array<K: BatchKey, const N: usize>(
        &self,
        keys: &[K; N],
    ) -> [Option<LookupMatch<'_, T>>; N] {
        let mut out = [None; N];
        let mut sink = MatchSink {
            out: &mut out,
            v4: self.v4.as_ref(),
            v6: self.v6.as_ref(),
        };
        self.ctx().run(keys, &mut sink);
        out
    }

    /// Batch [`exist`](Self::exist) into a caller-provided buffer.
    ///
    /// ```
    /// use std::net::IpAddr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table: IpTable<()> = IpTable::new();
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, None, Replace)?;
    /// let keys: [IpAddr; 2] = ["2001:db8::1".parse()?, "10.0.0.1".parse()?];
    /// let mut out = [false; 2];
    /// table.exist_batch_into(&keys, &mut out)?;
    /// assert_eq!(out, [true, false]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn exist_batch_into<K: BatchKey>(
        &self,
        keys: &[K],
        output: &mut [bool],
    ) -> Result<(), Error> {
        check_len(keys.len(), output.len())?;
        self.ctx().run(keys, &mut ExistSink { out: output });
        Ok(())
    }

    /// Batch [`exist`](Self::exist) of a fixed-size array.
    ///
    /// ```
    /// use std::net::Ipv4Addr;
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table: IpTable<()> = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, None, Replace)?;
    /// let covered = table.exist_array(&[Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(192, 0, 2, 1)]);
    /// assert_eq!(covered, [true, false]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn exist_array<K: BatchKey, const N: usize>(&self, keys: &[K; N]) -> [bool; N] {
        let mut out = [false; N];
        self.ctx().run(keys, &mut ExistSink { out: &mut out });
        out
    }

    /// Batch lookup returning compact [`MatchId`]s instead of borrowed
    /// results: the kernels' intermediate output, without touching route
    /// slots. With `&[u32]` keys the kernel writes straight into `output`.
    /// Use [`resolve`](Self::resolve) to obtain a [`LookupMatch`].
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, MatchId, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some("a"), Replace)?;
    /// let keys = [0x0A00_0001u32, 0xC000_0201];
    /// let mut ids = [MatchId::NONE; 2];
    /// table.lookup_ids_into(&keys, &mut ids)?; // 4-byte identifiers, no payload access
    /// assert!(ids[0].is_match() && !ids[1].is_match());
    /// assert_eq!(table.resolve(ids[0]).unwrap().data, Some(&"a"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn lookup_ids_into<'a, K: BatchKey>(
        &'a self,
        keys: &[K],
        output: &mut [MatchId<'a>],
    ) -> Result<(), Error> {
        check_len(keys.len(), output.len())?;
        let cells = ids_as_cells(output);
        if let Some(raw) = K::v4_slice(keys) {
            match &self.v4 {
                Some(e) => self.batch.v4_batch(&e.tables(), raw, cells),
                None => cells.fill(0),
            }
            return Ok(());
        }
        if let Some(raw) = K::v6_slice(keys) {
            match &self.v6 {
                Some(e) => {
                    self.batch.v6_batch(&e.tables(), raw, cells);
                    for c in cells.iter_mut() {
                        if *c != 0 {
                            *c |= MATCH_V6;
                        }
                    }
                }
                None => cells.fill(0),
            }
            return Ok(());
        }
        self.ctx().run(keys, &mut IdSink { out: cells });
        Ok(())
    }

    /// Converts a [`MatchId`] into a borrowed result (one slot read).
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, MatchId, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some("corp"), Replace)?;
    /// let mut ids = [MatchId::NONE; 1];
    /// table.lookup_ids_into(&[[10u8, 1, 2, 3]], &mut ids)?;
    /// let m = table.resolve(ids[0]).unwrap();
    /// assert_eq!((m.prefix.to_string(), m.data), ("10.0.0.0/8".to_string(), Some(&"corp")));
    /// assert!(table.resolve(MatchId::NONE).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn resolve<'a>(&'a self, id: MatchId<'a>) -> Option<LookupMatch<'a, T>> {
        let raw = id.raw();
        if raw & MATCH_V6 != 0 {
            self.v6.as_ref()?.resolve_live(raw & !MATCH_V6)
        } else {
            self.v4.as_ref()?.resolve_live(raw)
        }
    }
}

#[cfg(any(feature = "snapshot", all(test, feature = "batch-simd")))]
impl<T> IpTable<T> {
    /// IPv4 engine, `None` before the first IPv4 insertion.
    pub(crate) fn ipv4_engine(&self) -> Option<&Ipv4Engine<T>> {
        self.v4.as_ref()
    }

    /// IPv6 engine, `None` before the first IPv6 insertion.
    pub(crate) fn ipv6_engine(&self) -> Option<&Ipv6Engine<T>> {
        self.v6.as_ref()
    }
}

#[cfg(feature = "snapshot")]
impl<T> IpTable<T> {
    pub(crate) fn ipv4_engine_mut(&mut self) -> Option<&mut Ipv4Engine<T>> {
        self.v4.as_mut()
    }

    pub(crate) fn ipv6_engine_mut(&mut self) -> Option<&mut Ipv6Engine<T>> {
        self.v6.as_mut()
    }
}

impl<T> IpTable<T> {
    /// Secondary groups in use per family (IPv4, IPv6). Test helper.
    #[doc(hidden)]
    pub fn groups_in_use_for_tests(&self) -> (u32, u32) {
        let s = self.stats();
        (s.ipv4.groups_in_use as u32, s.ipv6.groups_in_use as u32)
    }
}
