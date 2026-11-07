//! Lookup results, batch drivers and batch kernel dispatch.

pub(crate) mod batch;
pub(crate) mod dispatch;
pub(crate) mod result;

pub use batch::BatchKey;
pub use result::{LookupMatch, MatchId};
