//! Snapshot errors.

use core::fmt;
use std::io;

use crate::error::Error;

/// Boxed error of a serializer, a deserializer or the compression library.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Resource bounded by [`SnapshotOptions`](super::SnapshotOptions) or
/// [`LoadOptions`](super::LoadOptions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SnapshotLimit {
    /// Memory of the XZ encoder, as computed by liblzma for the selected
    /// settings ([`SnapshotOptions::memory_budget`](super::SnapshotOptions::memory_budget)).
    EncoderMemory,
    /// Memory of the XZ decoder, enforced by liblzma
    /// ([`LoadOptions::decoder_memory_limit`](super::LoadOptions::decoder_memory_limit)).
    DecoderMemory,
    /// Decompressed bytes read from the snapshot
    /// ([`LoadOptions::max_decompressed_bytes`](super::LoadOptions::max_decompressed_bytes)).
    DecompressedBytes,
    /// Routes of both families together
    /// ([`LoadOptions::max_entries`](super::LoadOptions::max_entries)).
    Entries,
    /// Encoded size of one payload
    /// ([`LoadOptions::max_value_bytes`](super::LoadOptions::max_value_bytes)).
    ValueBytes,
}

impl fmt::Display for SnapshotLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EncoderMemory => "encoder memory",
            Self::DecoderMemory => "decoder memory",
            Self::DecompressedBytes => "decompressed bytes",
            Self::Entries => "entries",
            Self::ValueBytes => "payload bytes",
        })
    }
}

/// Errors of [`IpTable::snapshot`](crate::IpTable::snapshot),
/// [`IpTable::load`](crate::IpTable::load) and their variants.
///
/// A failed load never returns a partially restored table. A failed file
/// snapshot leaves the previous file untouched and removes its temporary
/// file.
///
/// | Category | Variants |
/// |---|---|
/// | I/O | [`Io`](Self::Io) |
/// | payload serialization | [`Serialize`](Self::Serialize), [`Deserialize`](Self::Deserialize) |
/// | compression library | [`Compression`](Self::Compression) |
/// | format | [`NotASnapshot`](Self::NotASnapshot), [`Truncated`](Self::Truncated), [`Corrupt`](Self::Corrupt), [`Malformed`](Self::Malformed), [`UnsupportedCodec`](Self::UnsupportedCodec) |
/// | version | [`UnsupportedVersion`](Self::UnsupportedVersion) |
/// | limits | [`LimitExceeded`](Self::LimitExceeded) |
/// | caller | [`InvalidOptions`](Self::InvalidOptions), [`Table`](Self::Table) |
///
/// ```
/// use dir_24_8::{IpTable, SnapshotError};
///
/// let err = IpTable::<u32>::load(b"definitely not a snapshot").unwrap_err();
/// assert!(matches!(err, SnapshotError::NotASnapshot));
/// let err = IpTable::<u32>::load(&[]).unwrap_err();
/// assert!(matches!(err, SnapshotError::Truncated));
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Reading the source or writing the destination failed (including the
    /// temporary file, rename and sync steps of a file snapshot).
    Io(io::Error),
    /// A payload could not be serialized.
    Serialize(BoxError),
    /// A payload could not be decoded into `T`: the snapshot was written
    /// with an incompatible payload type, or (when the checksum still
    /// matches) with a different serde representation.
    Deserialize(BoxError),
    /// The compression library failed for a reason other than the input
    /// data: unsupported settings, out of memory, internal error.
    Compression(BoxError),
    /// The input does not start with the snapshot signature.
    NotASnapshot,
    /// The input ends before the end of the snapshot.
    Truncated,
    /// The data is damaged: header checksum or XZ integrity check
    /// mismatch, or undecodable compressed data.
    Corrupt(&'static str),
    /// The data is intact (or cannot be proven damaged) but does not follow
    /// the format: invalid CIDR, duplicate prefix, bad counts or markers,
    /// data after the end of the snapshot...
    Malformed(String),
    /// The snapshot uses a format version this build cannot read.
    UnsupportedVersion {
        /// Version found in the header.
        found: u16,
        /// Version this build reads and writes.
        supported: u16,
    },
    /// The snapshot uses a compression or payload codec this build does not
    /// know.
    UnsupportedCodec {
        /// Compression codec identifier from the header.
        compression: u8,
        /// Payload codec identifier from the header.
        payload: u8,
    },
    /// A configured limit was exceeded.
    LimitExceeded {
        /// The limited resource.
        limit: SnapshotLimit,
        /// The configured maximum.
        max: u64,
        /// The amount that was needed or declared, when known.
        requested: Option<u64>,
    },
    /// An option value is out of range.
    InvalidOptions(&'static str),
    /// Rebuilding the table failed: a capacity limit of the restored
    /// configuration is exceeded, or an allocation failed.
    /// [`LoadOptions::config`](super::LoadOptions::config) replaces the
    /// stored configuration.
    Table(Error),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "snapshot I/O error: {e}"),
            Self::Serialize(e) => write!(f, "payload serialization failed: {e}"),
            Self::Deserialize(e) => write!(f, "payload deserialization failed: {e}"),
            Self::Compression(e) => write!(f, "compression library error: {e}"),
            Self::NotASnapshot => f.write_str("not a dir-24-8 snapshot (bad signature)"),
            Self::Truncated => f.write_str("snapshot is truncated"),
            Self::Corrupt(what) => write!(f, "snapshot is corrupt: {what}"),
            Self::Malformed(what) => write!(f, "malformed snapshot: {what}"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "snapshot format version {found} is not supported (this build reads version {supported})"
            ),
            Self::UnsupportedCodec {
                compression,
                payload,
            } => write!(
                f,
                "unsupported snapshot codec (compression {compression}, payload {payload})"
            ),
            Self::LimitExceeded {
                limit,
                max,
                requested,
            } => match requested {
                Some(r) => write!(f, "{limit} limit exceeded: {r} needed, limit {max}"),
                None => write!(f, "{limit} limit exceeded: limit {max}"),
            },
            Self::InvalidOptions(what) => write!(f, "invalid snapshot options: {what}"),
            Self::Table(e) => write!(f, "cannot rebuild the table: {e}"),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Serialize(e) | Self::Deserialize(e) | Self::Compression(e) => Some(e.as_ref()),
            Self::Table(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Error> for SnapshotError {
    fn from(e: Error) -> Self {
        Self::Table(e)
    }
}
