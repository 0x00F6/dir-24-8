//! Compressed snapshots of a table (feature `snapshot`): save the logical
//! state of an [`IpTable`] to a `Vec<u8>`, a file or any writer, and rebuild
//! an equivalent table from it.
//!
//! ```
//! use dir_24_8::{IpNet, IpTable, Replace};
//!
//! let mut table = IpTable::new();
//! table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(String::from("corp")), Replace)?;
//! table.insert("10.1.0.0/16".parse::<IpNet>()?, None, Replace)?; // route without payload
//! table.insert("2001:db8::/32".parse::<IpNet>()?, Some(String::from("doc")), Replace)?;
//!
//! // In memory: the encoder writes straight into the returned `Vec<u8>`.
//! let bytes: Vec<u8> = table.snapshot()?;
//! let restored: IpTable<String> = IpTable::load(&bytes)?; // borrows `bytes`
//! assert_eq!(restored.lookup_data("10.2.3.4".parse()?), Some(&"corp".to_string()));
//! assert_eq!(restored.get("10.1.0.0/16".parse::<IpNet>()?), Some(None)); // present, no payload
//!
//! // To a file: streamed through a temporary file, then renamed.
//! let path = std::env::temp_dir().join(format!("dir-24-8-doc-{}.snap", std::process::id()));
//! table.snapshot_to_file(&path)?;
//! let from_file: IpTable<String> = IpTable::load_from_file(&path)?;
//! assert_eq!(from_file.len(), 3);
//! # std::fs::remove_file(&path)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What is saved
//!
//! The snapshot holds the logical state, read from the route stores (one
//! slot per inserted CIDR), not from the lookup tables:
//!
//! * every IPv4 and IPv6 route with its prefix length, including routes
//!   fully masked by more specific ones (the lookup tables only hold
//!   winners, the route stores hold every route);
//! * every payload, and the difference between a route without payload
//!   (`get` returns `Some(None)`) and an absent route (`None`);
//! * the [`Config`]: huge-page advice and capacity limits.
//!
//! The lookup tables (`tbl24`, groups) and the prefix index are rebuilt on
//! load by inserting the routes, and the batch kernel is detected again, so
//! the restored table runs the kernels of the loading build and CPU, and is
//! a regular table: every lookup, `exist`, batch, insertion (any [insert
//! mode](crate::mode)) and removal behaves as on the original. A capacity
//! limit the routes exceed fails with [`SnapshotError::Table`];
//! [`LoadOptions::config`] replaces the stored configuration. Families without routes are not
//! allocated after a load, as in a new table.
//!
//! # Payloads
//!
//! Payloads are encoded with serde as CBOR (RFC 8949, `ciborium`), a
//! self-describing format: every serde representation round-trips,
//! including `#[serde(skip_serializing_if)]`, `#[serde(flatten)]`, untagged
//! enums and `serde_json::Value`. Field names are stored, but they repeat
//! and the compressor removes most of that cost (about +8 % on a compressed
//! column of small structs compared with postcard, a non-self-describing
//! format). Only these methods carry bounds: `T: Serialize` to
//! save, `T: DeserializeOwned` to load; no `Clone` bound is needed.
//!
//! # Compression
//!
//! The body is compressed as one XZ stream (LZMA2) through liblzma, with a
//! CRC64 integrity check. The default profile, `9 | PRESET_EXTREME`,
//! favors the size; [`SnapshotOptions`] documents the time and memory
//! trade-offs, threads, the encoder memory budget and its policy, and
//! [`LoadOptions`] the decoder limits. Neither the uncompressed snapshot
//! nor a list of the entries is ever materialized:
//!
//! * saving serializes route after route into the encoder, whose output is
//!   the returned `Vec<u8>` or the (buffered) file; the extra memory is the
//!   compressed output and the encoder's working memory (a 64 KiB buffer of
//!   prefix lengths, one payload encoding at a time);
//! * loading reads the compressed source through the decoder and inserts
//!   route after route ([`load`](IpTable::load) borrows the given bytes,
//!   files are read through a 64 KiB buffer); the extra memory is the
//!   decoder state and fixed buffers.
//!
//! Before writing, the payloads are serialized once more to measure the
//! uncompressed size (stopping at the dictionary size): the dictionary is
//! reduced to that size, which leaves the ratio unchanged and saves encoder
//! and decoder memory on small tables.
//!
//! # Format (version 1)
//!
//! All integers are unsigned; `u16`/`u32` are little-endian, `varint` is
//! LEB128 (minimal, at most 10 bytes), addresses are big-endian.
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | signature `89 44 49 52 32 34 38 0A` (`\x89DIR248\n`) |
//! | 8 | 2 | format version, `u16` = 1 |
//! | 10 | 1 | compression codec: 1 = XZ |
//! | 11 | 1 | payload codec: 1 = CBOR via serde |
//! | 12 | 4 | CRC-32 (IEEE) of bytes 0..12, `u32` |
//! | 16 | | one XZ stream (integrity check required), then nothing |
//!
//! The decompressed body:
//!
//! 1. configuration, 15 bytes: two retired backend bytes, flags (bit 0:
//!    huge pages, others 0), `max_routes`, `max_ipv4_groups`,
//!    `max_ipv6_groups` (`u32` each). The retired bytes held the requested
//!    batch and single-lookup backends when tables could force one. Writers
//!    store `0` and `0xFF` (the former defaults: automatic SIMD, no
//!    single-lookup override), so every v1 reader rebuilds the default
//!    configuration; readers accept the codes earlier writers stored (`0` to
//!    `4`, and `0xFF` in the second byte) and ignore them;
//! 2. IPv4 section, then IPv6 section, each:
//!    * route count `n`, `u32`;
//!    * prefix blocks of up to 65 536 routes: the block's prefix lengths
//!      (one byte each), then the block's network addresses, each truncated
//!      to `ceil(length / 8)` bytes with the bits past the length zero;
//!    * `n` payloads in the same order: `varint` 0 for no payload, or
//!      `varint` `k + 1` followed by one CBOR data item of `k` bytes;
//! 3. end marker `END\n`.
//!
//! The layout is columnar because it compresses better: lengths, addresses
//! and payloads each have their own statistics (25-45 % smaller than one
//! record per route on the generated benchmark tables). Blocks bound the
//! loader's length buffer to 64 KiB, for 1-4 % more than a single length
//! column, which the loader would have to buffer whole (one byte per
//! route). Addresses are truncated, not delta-coded: XOR with the previous
//! address compressed worse.
//!
//! Validation and policies, checked before any value is used:
//!
//! * signature, header checksum, version and codecs, in that order. The
//!   16-byte header layout is the same in every version; a reader rejects
//!   versions it does not know ([`SnapshotError::UnsupportedVersion`]). Any
//!   change of the body layout or of the meaning of a field increases the
//!   version;
//! * counts against the format maximum (2^31 - 1 per family), the restored
//!   capacity limits and [`LoadOptions::max_entries`]; lengths against 32 or
//!   128; host bits must be zero; payload sizes against
//!   [`LoadOptions::max_value_bytes`]; every payload must use exactly its
//!   declared bytes;
//! * a prefix present twice, unknown flags or retired backend codes, a missing end
//!   marker, data after the end marker or bytes after the XZ stream are
//!   [`SnapshotError::Malformed`];
//! * the XZ integrity check and index are verified when the stream ends,
//!   before the table is returned. A truncated input is
//!   [`SnapshotError::Truncated`], damaged data [`SnapshotError::Corrupt`]
//!   (when parsing fails first, the rest of the stream is decompressed to
//!   tell damaged data from an intact but invalid snapshot).
//!
//! Entries are written in the order of the route stores
//! ([`EntryOrder::Storage`], deterministic for a given history) or sorted
//! ([`EntryOrder::Sorted`], the same for every history). Two snapshots of
//! the same table with the same options and liblzma version are
//! byte-identical only when `T`'s serialization is deterministic (a
//! `HashMap` payload, for example, is not).
//!
//! # Consistency and concurrency
//!
//! A snapshot borrows the table (`&self`) for its whole duration, so the
//! borrow checker guarantees that no mutation happens meanwhile and the
//! snapshot is a consistent state. Lookups may run concurrently. Behind a
//! `RwLock`, the snapshot holds the read guard and writers wait until it
//! ends, compression included; to let writers proceed, snapshot an
//! immutable generation (for example an `Arc<IpTable<T>>` replaced by
//! writers) instead of the live table.
//!
//! # Files
//!
//! [`snapshot_to_file`](IpTable::snapshot_to_file) streams into a temporary
//! file created in the destination directory, finishes the XZ stream,
//! flushes, copies the permissions of the file it replaces, optionally
//! syncs ([`FileSync`]) and renames it over the destination: readers see
//! the previous snapshot or the complete new one, never a partial file. On
//! any error the temporary file is removed and the previous snapshot is
//! untouched. The rename is atomic on POSIX file systems and uses
//! `MoveFileEx` replacement on Windows; durability after a power loss is a
//! separate guarantee, chosen with [`FileSync`].

mod error;
mod file;
pub(crate) mod format;
mod options;
mod xz;

use core::cell::Cell;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

use liblzma::bufread::XzDecoder;
use liblzma::write::XzEncoder;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use error::{BoxError, SnapshotError, SnapshotLimit};
pub use options::{EntryOrder, FileSync, LoadOptions, OverBudget, SnapshotOptions};

#[cfg(doc)]
use crate::Config;
use crate::IpTable;

/// What [`IpTable::snapshot_to_writer`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SnapshotSummary {
    /// IPv4 routes saved.
    pub ipv4_routes: usize,
    /// IPv6 routes saved.
    pub ipv6_routes: usize,
    /// Uncompressed body size, in bytes.
    pub uncompressed_bytes: u64,
    /// Bytes written to the destination (header and XZ stream).
    pub compressed_bytes: u64,
    /// LZMA2 dictionary size used, in bytes.
    pub dict_size: u32,
    /// Encoder threads used.
    pub threads: u32,
    /// Encoder memory computed by liblzma for these settings, in bytes.
    pub encoder_memory: u64,
}

/// Destination adapter: counts compressed bytes and refuses every write
/// once the snapshot failed, so that the encoder's drop does not append the
/// end of a stream after an error.
struct Sink<'p, W> {
    inner: W,
    written: u64,
    aborted: &'p Cell<bool>,
}

impl<W: Write> Write for Sink<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.aborted.get() {
            return Err(io::Error::other("snapshot aborted"));
        }
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.aborted.get() {
            return Ok(());
        }
        self.inner.flush()
    }
}

/// Counts the uncompressed bytes written to the encoder.
struct Counter<W> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for Counter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "snapshot")))]
impl<T: Serialize> IpTable<T> {
    /// Compressed snapshot of the table in a new `Vec<u8>`, with the default
    /// [`SnapshotOptions`] (XZ `9 | PRESET_EXTREME`, CRC64). The encoder
    /// writes into the returned vector as it goes; no uncompressed copy is
    /// built. See the [`snapshot` module](crate::snapshot) for the format.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("192.0.2.0/24".parse::<IpNet>()?, Some(64500u32), Replace)?;
    /// let bytes = table.snapshot()?;
    /// assert_eq!(&bytes[..4], b"\x89DIR");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        self.snapshot_with_options(&SnapshotOptions::new())
    }

    /// [`snapshot`](Self::snapshot) with explicit settings.
    ///
    /// ```
    /// use dir_24_8::{EntryOrder, IpNet, IpTable, Replace, SnapshotOptions};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("192.0.2.0/24".parse::<IpNet>()?, Some(64500u32), Replace)?;
    /// let options = SnapshotOptions::new().level(6).order(EntryOrder::Sorted);
    /// let bytes = table.snapshot_with_options(&options)?;
    /// assert_eq!(IpTable::<u32>::load(&bytes)?.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn snapshot_with_options(
        &self,
        options: &SnapshotOptions,
    ) -> Result<Vec<u8>, SnapshotError> {
        let mut out = Vec::new();
        self.snapshot_to_writer(&mut out, options)?;
        out.shrink_to_fit();
        Ok(out)
    }

    /// Writes a snapshot to `path` with the default [`SnapshotOptions`]:
    /// streamed into a temporary file of the same directory, then renamed
    /// over `path` (see [Files](crate::snapshot#files)). The previous file
    /// is kept on any error.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, Some(String::from("doc")), Replace)?;
    /// let path = std::env::temp_dir().join(format!("dir-24-8-save-{}.snap", std::process::id()));
    /// table.snapshot_to_file(&path)?;
    /// assert!(std::fs::metadata(&path)?.len() > 16);
    /// # std::fs::remove_file(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn snapshot_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), SnapshotError> {
        self.snapshot_to_file_with_options(path, &SnapshotOptions::new())
    }

    /// [`snapshot_to_file`](Self::snapshot_to_file) with explicit settings
    /// (compression and [`FileSync`]).
    ///
    /// ```
    /// use dir_24_8::{FileSync, IpNet, IpTable, Replace, SnapshotOptions};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let path = std::env::temp_dir().join(format!("dir-24-8-sync-{}.snap", std::process::id()));
    /// let options = SnapshotOptions::new().file_sync(FileSync::FileAndDirectory);
    /// table.snapshot_to_file_with_options(&path, &options)?;
    /// assert_eq!(IpTable::<u32>::load_from_file(&path)?.len(), 1);
    /// # std::fs::remove_file(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn snapshot_to_file_with_options<P: AsRef<Path>>(
        &self,
        path: P,
        options: &SnapshotOptions,
    ) -> Result<(), SnapshotError> {
        options.validate()?;
        file::write_atomic(path.as_ref(), options.file_sync, |w| {
            self.snapshot_to_writer(w, options).map(drop)
        })
    }

    /// Streams a snapshot into any writer and returns what was written. This
    /// is the core of the other snapshot methods. Nothing is written after
    /// an error, but what was written before it is an incomplete snapshot.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace, SnapshotOptions};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, None, Replace)?;
    /// let mut out = Vec::new();
    /// let summary = table.snapshot_to_writer(&mut out, &SnapshotOptions::new())?;
    /// assert_eq!((summary.ipv4_routes, summary.ipv6_routes), (1, 1));
    /// assert_eq!(summary.compressed_bytes, out.len() as u64);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn snapshot_to_writer<W: Write>(
        &self,
        writer: W,
        options: &SnapshotOptions,
    ) -> Result<SnapshotSummary, SnapshotError> {
        options.validate()?;
        let mut scratch = Vec::new();
        // Uncompressed size, measured up to the dictionary size: the
        // dictionary is fitted to smaller snapshots.
        let cap = u64::from(xz::wanted_dict_size(options)?);
        let estimate = format::body_size(self, cap, &mut scratch)?;
        let plan = xz::plan(options, estimate)?;

        let aborted = Cell::new(false);
        let mut sink = Sink {
            inner: writer,
            written: 0,
            aborted: &aborted,
        };
        sink.write_all(&format::header())
            .map_err(xz::classify_encoder_error)?;
        let encoder = XzEncoder::new_stream(&mut sink, xz::encoder(&plan)?);
        let mut body = Counter {
            inner: BufWriter::with_capacity(format::IO_BUF, encoder),
            written: 0,
        };
        if let Err(e) = format::write_body(self, &mut body, options.order, &mut scratch) {
            aborted.set(true);
            return Err(e);
        }
        let Counter {
            inner: buffered,
            written: uncompressed_bytes,
        } = body;
        let encoder = match buffered.into_inner() {
            Ok(encoder) => encoder,
            Err(e) => {
                aborted.set(true);
                return Err(xz::classify_encoder_error(e.into_error()));
            }
        };
        let sink = encoder.finish().map_err(xz::classify_encoder_error)?;
        sink.flush().map_err(xz::classify_encoder_error)?;
        Ok(SnapshotSummary {
            ipv4_routes: self.len_ipv4(),
            ipv6_routes: self.len_ipv6(),
            uncompressed_bytes,
            compressed_bytes: sink.written,
            dict_size: plan.dict_size,
            threads: plan.threads,
            encoder_memory: plan.memory,
        })
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "snapshot")))]
impl<T: DeserializeOwned> IpTable<T> {
    /// Rebuilds a table from a snapshot held in memory, with the default
    /// [`LoadOptions`]. Accepts `&[u8]`, `&Vec<u8>`, `Vec<u8>`, arrays...:
    /// the bytes are only borrowed, never copied, and decompressed as the
    /// routes are inserted.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(vec![1u16, 2]), Replace)?;
    /// let bytes: Vec<u8> = table.snapshot()?;
    ///
    /// let borrowed: IpTable<Vec<u16>> = IpTable::load(&bytes)?; // &Vec<u8>
    /// let sliced: IpTable<Vec<u16>> = IpTable::load(&bytes[..])?; // &[u8]
    /// let owned: IpTable<Vec<u16>> = IpTable::load(bytes)?; // Vec<u8>, dropped after loading
    /// assert_eq!(borrowed.lookup_data("10.1.1.1".parse()?), Some(&vec![1, 2]));
    /// assert_eq!(sliced.len() + owned.len(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn load<B: AsRef<[u8]>>(bytes: B) -> Result<Self, SnapshotError> {
        Self::load_with_options(bytes, &LoadOptions::new())
    }

    /// [`load`](Self::load) with explicit limits and settings.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, LoadOptions, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let bytes = table.snapshot()?;
    /// let options = LoadOptions::new().max_entries(10).max_value_bytes(16);
    /// assert_eq!(IpTable::<u32>::load_with_options(&bytes, &options)?.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn load_with_options<B: AsRef<[u8]>>(
        bytes: B,
        options: &LoadOptions,
    ) -> Result<Self, SnapshotError> {
        Self::load_from_reader(bytes.as_ref(), options)
    }

    /// Rebuilds a table from a snapshot file, with the default
    /// [`LoadOptions`]. The file is read through a 64 KiB buffer and
    /// decompressed as the routes are inserted.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let path = std::env::temp_dir().join(format!("dir-24-8-load-{}.snap", std::process::id()));
    /// table.snapshot_to_file(&path)?;
    /// let restored: IpTable<u32> = IpTable::load_from_file(&path)?;
    /// assert_eq!(restored.lookup_data("10.9.9.9".parse()?), Some(&1));
    /// # std::fs::remove_file(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, SnapshotError> {
        Self::load_from_file_with_options(path, &LoadOptions::new())
    }

    /// [`load_from_file`](Self::load_from_file) with explicit limits and
    /// settings.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, LoadOptions, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("2001:db8::/32".parse::<IpNet>()?, None::<u8>, Replace)?;
    /// let path = std::env::temp_dir().join(format!("dir-24-8-loadopt-{}.snap", std::process::id()));
    /// table.snapshot_to_file(&path)?;
    /// let options = LoadOptions::new().decoder_memory_limit(16 << 20).max_entries(1);
    /// let restored: IpTable<u8> = IpTable::load_from_file_with_options(&path, &options)?;
    /// assert_eq!(restored.get("2001:db8::/32".parse::<IpNet>()?), Some(None));
    /// # std::fs::remove_file(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn load_from_file_with_options<P: AsRef<Path>>(
        path: P,
        options: &LoadOptions,
    ) -> Result<Self, SnapshotError> {
        let file = File::open(path).map_err(SnapshotError::Io)?;
        Self::load_from_reader(BufReader::with_capacity(format::IO_BUF, file), options)
    }

    /// Rebuilds a table from a snapshot read from any buffered reader (wrap
    /// unbuffered sources in a `BufReader`). This is the core of the other
    /// load methods. The reader must end with the snapshot: trailing bytes
    /// are an error.
    ///
    /// ```
    /// use std::io::BufReader;
    /// use dir_24_8::{IpNet, IpTable, LoadOptions, Replace};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let bytes = table.snapshot()?;
    /// let reader = BufReader::new(std::io::Cursor::new(bytes));
    /// let restored: IpTable<u32> = IpTable::load_from_reader(reader, &LoadOptions::new())?;
    /// assert_eq!(restored.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn load_from_reader<R: BufRead>(
        mut reader: R,
        options: &LoadOptions,
    ) -> Result<Self, SnapshotError> {
        options.validate()?;
        format::read_header(&mut reader)?;
        let stream_header = format::read_xz_stream_header(&mut reader)?;
        let decoder =
            XzDecoder::new_stream((&stream_header[..]).chain(reader), xz::decoder(options)?);
        let mut body = format::Body::new(decoder, options);
        let table = match format::parse_body(&mut body, options) {
            Ok(table) => table,
            Err(e) => return Err(body.explain(e)),
        };
        if let Err(e) = body.finish() {
            return Err(body.explain(e));
        }
        // The XZ stream ended (integrity check and index verified); nothing
        // may follow it.
        let source = body.decoder_mut().get_mut();
        let rest = loop {
            match source.fill_buf() {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => break other.map_err(SnapshotError::Io)?,
            }
        };
        if !rest.is_empty() {
            return Err(SnapshotError::Malformed(
                "trailing bytes after the compressed stream".into(),
            ));
        }
        Ok(table)
    }
}
