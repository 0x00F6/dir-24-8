//! Limits and error mapping shared by the two engines.

use crate::error::{AddressFamily, Error, Resource};
use crate::route::store::ReserveError;

use super::groups::PoolError;

/// Per-engine limits derived from [`Config`](super::Config).
#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineLimits {
    pub(crate) max_routes: u32,
    pub(crate) max_ipv4_groups: u32,
    pub(crate) max_ipv6_groups: u32,
    pub(crate) huge_pages: bool,
}

pub(crate) fn alloc_error(family: AddressFamily, bytes: usize) -> Error {
    Error::AllocationFailed { family, bytes }
}

pub(crate) fn pool_error(family: AddressFamily, e: PoolError) -> Error {
    match e {
        PoolError::Limit(limit) => Error::CapacityExceeded {
            family,
            resource: Resource::Groups,
            limit: u64::from(limit),
        },
        PoolError::Alloc(bytes) => alloc_error(family, bytes),
    }
}

pub(crate) fn store_error(family: AddressFamily, limit: u32, e: ReserveError) -> Error {
    match e {
        ReserveError::Limit => Error::CapacityExceeded {
            family,
            resource: Resource::Routes,
            limit: u64::from(limit),
        },
        ReserveError::Alloc(bytes) => alloc_error(family, bytes),
    }
}
