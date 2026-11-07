//! Public error type.

use core::fmt;

/// Address family of a route, table or resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressFamily {
    /// IPv4 (32-bit keys, DIR-24-8 engine).
    Ipv4,
    /// IPv6 (128-bit keys, 24/8 multi-level trie engine).
    Ipv6,
}

impl fmt::Display for AddressFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ipv4 => "IPv4",
            Self::Ipv6 => "IPv6",
        })
    }
}

/// Bounded resource whose limit was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    /// Route slots (one per distinct normalized CIDR).
    Routes,
    /// Secondary 256-entry groups (`tbl8` groups).
    Groups,
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Routes => "route slots",
            Self::Groups => "secondary groups",
        })
    }
}

/// Errors returned by table construction, mutation and batch lookups.
///
/// Every mutation that returns an error leaves the table unchanged: capacity
/// and allocation checks happen before the first table cell is written.
///
/// ```
/// use dir_24_8::{Config, Error, IpNet, IpTable, Replace, Resource};
///
/// let mut table = IpTable::with_config(Config::new().max_routes(1))?;
/// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
/// let err = table.insert("11.0.0.0/8".parse::<IpNet>()?, Some(2), Replace).unwrap_err();
/// assert!(matches!(err, Error::CapacityExceeded { resource: Resource::Routes, .. }));
/// assert_eq!(table.len(), 1); // unchanged
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A configured limit (route slots or secondary groups) would be exceeded.
    CapacityExceeded {
        /// Family whose engine is full.
        family: AddressFamily,
        /// Exhausted resource.
        resource: Resource,
        /// Configured limit for this resource.
        limit: u64,
    },
    /// The system allocator (or `mmap`) refused an allocation.
    AllocationFailed {
        /// Family whose engine attempted the allocation.
        family: AddressFamily,
        /// Requested size in bytes.
        bytes: usize,
    },
    /// The output buffer of a batch lookup does not have the input length.
    LengthMismatch {
        /// Number of input keys.
        input: usize,
        /// Number of output slots.
        output: usize,
    },
    /// An invalid configuration value.
    InvalidConfig(&'static str),
    /// A fallible merge (for example the serde adapter) failed. The existing
    /// payload is left unchanged.
    MergeFailed(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded {
                family,
                resource,
                limit,
            } => write!(
                f,
                "{family} table is full: limit of {limit} {resource} reached"
            ),
            Self::AllocationFailed { family, bytes } => {
                write!(f, "{family} table allocation of {bytes} bytes failed")
            }
            Self::LengthMismatch { input, output } => write!(
                f,
                "batch output has {output} slots but {input} keys were given"
            ),
            Self::InvalidConfig(msg) => write!(f, "invalid configuration: {msg}"),
            Self::MergeFailed(err) => write!(f, "merge failed: {err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MergeFailed(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}
