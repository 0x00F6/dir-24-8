//! Route, group and memory statistics.

use core::fmt;

/// Statistics of one address family.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FamilyStats {
    /// `true` once the family's root table has been allocated (first insert).
    pub allocated: bool,
    /// Number of distinct normalized CIDRs.
    pub routes: usize,
    /// Routes carrying a payload (`Some(T)`).
    pub routes_with_data: usize,
    /// Secondary 256-entry groups currently referenced by the table.
    pub groups_in_use: usize,
    /// Released groups kept for reuse.
    pub groups_free: usize,
    /// Groups the current `tbl8` buffer can hold without growing.
    pub groups_capacity: usize,
    /// Configured limit of groups in use.
    pub groups_limit: usize,
    /// Bytes reserved for the 2^24-entry root table (64 MiB once allocated).
    pub tbl24_bytes: usize,
    /// Bytes reserved for the secondary group buffer.
    pub tbl8_bytes: usize,
    /// Bytes of the route slot array (prefix + `Option<T>` inline, heap owned
    /// by `T` itself is not included).
    pub route_store_bytes: usize,
    /// Approximate bytes of the exact-match prefix index used for updates.
    pub index_bytes: usize,
    /// `true` when the tables are `mmap`ed with a huge-page advice.
    pub huge_pages: bool,
    /// Bytes of tables mapped outside the global allocator (`mmap`).
    pub mapped_bytes: usize,
}

impl FamilyStats {
    /// Lookup-structure bytes (`tbl24 + tbl8`).
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// let v4 = table.stats().ipv4;
    /// assert_eq!(v4.table_bytes(), v4.tbl24_bytes + v4.tbl8_bytes);
    /// assert_eq!(v4.tbl24_bytes, 64 << 20); // the 2^24-cell root
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn table_bytes(&self) -> usize {
        self.tbl24_bytes + self.tbl8_bytes
    }

    /// All bytes accounted for by the family.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("2001:db8::/48".parse::<IpNet>()?, Some(1), Replace)?;
    /// let v6 = table.stats().ipv6;
    /// assert_eq!(v6.total_bytes(), v6.table_bytes() + v6.route_store_bytes + v6.index_bytes);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn total_bytes(&self) -> usize {
        self.table_bytes() + self.route_store_bytes + self.index_bytes
    }
}

/// Statistics of a whole [`IpTable`](crate::IpTable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableStats {
    /// IPv4 engine.
    pub ipv4: FamilyStats,
    /// IPv6 engine.
    pub ipv6: FamilyStats,
    /// Kernel of the batch lookups: `"scalar"`, or with the `batch-simd`
    /// feature the one detected on this CPU (`"avx512"`, `"avx2"`,
    /// `"neon"`, or `"scalar"` when the CPU has none of them). Single
    /// lookups and `exist` always use the scalar kernels.
    pub batch_kernel: &'static str,
}

impl TableStats {
    /// Total routes of both families.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(2), Replace)?;
    /// assert_eq!(table.stats().routes(), 2);
    /// assert_eq!(table.stats().routes(), table.len());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn routes(&self) -> usize {
        self.ipv4.routes + self.ipv6.routes
    }

    /// Total accounted bytes of both families.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
    /// let stats = table.stats();
    /// assert_eq!(stats.total_bytes(), stats.ipv4.total_bytes() + stats.ipv6.total_bytes());
    /// println!("{:.1} MiB", stats.total_bytes() as f64 / (1 << 20) as f64);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn total_bytes(&self) -> usize {
        self.ipv4.total_bytes() + self.ipv6.total_bytes()
    }
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

impl fmt::Display for FamilyStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.allocated {
            return f.write_str("not allocated");
        }
        write!(
            f,
            "{} routes ({} with data), {} groups in use ({} free, capacity {}), \
             tbl24 {:.1} MiB, tbl8 {:.2} MiB, store {:.2} MiB, index {:.2} MiB{}",
            self.routes,
            self.routes_with_data,
            self.groups_in_use,
            self.groups_free,
            self.groups_capacity,
            mib(self.tbl24_bytes),
            mib(self.tbl8_bytes),
            mib(self.route_store_bytes),
            mib(self.index_bytes),
            if self.huge_pages { ", huge pages" } else { "" }
        )
    }
}

impl fmt::Display for TableStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "batch kernel: {}", self.batch_kernel)?;
        writeln!(f, "IPv4: {}", self.ipv4)?;
        write!(f, "IPv6: {}", self.ipv6)
    }
}
