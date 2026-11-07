//! Route bookkeeping shared by both engines: cell encoding, normalized
//! prefixes, the payload store and the prefix index used for maintenance.

pub(crate) mod entry;
pub(crate) mod index;
pub(crate) mod prefix;
pub(crate) mod store;
