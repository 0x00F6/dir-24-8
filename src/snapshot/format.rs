//! Snapshot format, version 1: outer header, body writer and body parser.
//!
//! The layout is specified in the [module documentation](super); every
//! integer has an explicit width and byte order, nothing depends on the
//! memory layout of Rust types.

use std::io::{self, BufReader, Read, Write};

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{AddressFamily, Error, Resource};
use crate::route::prefix::Prefix;
use crate::route::store::{RouteStore, Slot};
use crate::table::{Config, IpTable};

use super::error::{SnapshotError, SnapshotLimit};
use super::options::{EntryOrder, LoadOptions};
use super::xz;

/// Signature: a non-ASCII byte (detects 7-bit channels), the name, and a
/// line feed (detects newline conversion).
pub(crate) const MAGIC: [u8; 8] = *b"\x89DIR248\n";
/// Format version written by this build, and the only one it reads.
pub(crate) const FORMAT_VERSION: u16 = 1;
/// Compression codec identifier: one XZ stream (LZMA2 filter).
pub(crate) const CODEC_XZ: u8 = 1;
/// Payload codec identifier: one CBOR data item (RFC 8949) per payload,
/// produced by serde through `ciborium`.
pub(crate) const PAYLOAD_CBOR: u8 = 1;
/// Length of the outer header.
pub(crate) const HEADER_LEN: usize = 16;
/// End-of-body marker.
pub(crate) const TRAILER: [u8; 4] = *b"END\n";
/// Routes per prefix block (lengths then addresses).
pub(crate) const BLOCK: usize = 1 << 16;
/// Size of the configuration record at the start of the body.
const CONFIG_LEN: usize = 15;
/// Retired backend fields of the configuration record (bytes 0 and 1):
/// written as the former defaults (`auto-simd`, no single-lookup
/// override), read back as any code earlier writers stored, then ignored.
const RETIRED_BATCH_DEFAULT: u8 = 0;
const RETIRED_SINGLE_DEFAULT: u8 = 0xFF;
/// Highest backend code earlier writers stored (`neon`).
const RETIRED_MAX_CODE: u8 = 4;
/// Configuration flag: huge pages requested.
const FLAG_HUGE_PAGES: u8 = 1;
/// Buffer size of the I/O adapters (decompressed side and files).
pub(crate) const IO_BUF: usize = 64 << 10;

// ----------------------------------------------------------------------
// Primitive encodings
// ----------------------------------------------------------------------

/// CRC-32 (IEEE 802.3, as in zlib, gzip and xz) lookup table.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |c, &b| {
        CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8)
    })
}

/// The 16-byte outer header of a version 1 snapshot.
pub(crate) fn header() -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(&MAGIC);
    h[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h[10] = CODEC_XZ;
    h[11] = PAYLOAD_CBOR;
    let crc = crc32(&h[..12]);
    h[12..].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Unsigned LEB128.
fn write_varint<W: Write>(w: &mut W, mut v: u64) -> io::Result<()> {
    let mut buf = [0u8; 10];
    let mut n = 0;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            buf[n] = byte;
            n += 1;
            break;
        }
        buf[n] = byte | 0x80;
        n += 1;
    }
    w.write_all(&buf[..n])
}

const fn varint_len(v: u64) -> u64 {
    let bits = 64 - (v | 1).leading_zeros() as u64;
    bits.div_ceil(7)
}

/// Mask of the `len` leading bits of a left-aligned 128-bit address.
const fn mask(len: u8) -> u128 {
    if len == 0 {
        0
    } else {
        u128::MAX << (128 - len as u32)
    }
}

/// Bytes of the address column for a prefix of `len` bits.
const fn addr_bytes(len: u8) -> usize {
    (len as usize).div_ceil(8)
}

// ----------------------------------------------------------------------
// Families
// ----------------------------------------------------------------------

/// What the snapshot code needs from each address family.
pub(crate) trait Family: Prefix + Into<IpNet> + core::fmt::Display {
    const FAMILY: AddressFamily;

    /// Route store of `table`, `None` before the first insertion.
    fn store<T>(table: &IpTable<T>) -> Option<&RouteStore<Self, T>>;

    /// Payloads of the routes of `table`, in slot order.
    fn for_each_slot_mut<T, E>(
        table: &mut IpTable<T>,
        f: impl FnMut(&mut Slot<Self, T>) -> Result<(), E>,
    ) -> Result<usize, E>;

    /// Network address, left-aligned on 128 bits.
    fn bits(&self) -> u128;

    /// Prefix from a left-aligned network address (`len` and host bits
    /// already validated).
    fn from_bits(bits: u128, len: u8) -> Self;
}

fn each_slot_mut<'a, N: 'a, T: 'a, E>(
    slots: impl Iterator<Item = &'a mut Slot<N, T>>,
    mut f: impl FnMut(&mut Slot<N, T>) -> Result<(), E>,
) -> Result<usize, E> {
    let mut n = 0;
    for slot in slots {
        f(slot)?;
        n += 1;
    }
    Ok(n)
}

impl Family for Ipv4Net {
    const FAMILY: AddressFamily = AddressFamily::Ipv4;

    fn store<T>(table: &IpTable<T>) -> Option<&RouteStore<Self, T>> {
        table.ipv4_engine().map(|e| e.routes())
    }

    fn for_each_slot_mut<T, E>(
        table: &mut IpTable<T>,
        f: impl FnMut(&mut Slot<Self, T>) -> Result<(), E>,
    ) -> Result<usize, E> {
        match table.ipv4_engine_mut() {
            Some(e) => each_slot_mut(e.iter_mut(), f),
            None => Ok(0),
        }
    }

    fn bits(&self) -> u128 {
        u128::from(u32::from(self.network())) << 96
    }

    fn from_bits(bits: u128, len: u8) -> Self {
        Ipv4Net::new(((bits >> 96) as u32).into(), len).expect("length validated")
    }
}

impl Family for Ipv6Net {
    const FAMILY: AddressFamily = AddressFamily::Ipv6;

    fn store<T>(table: &IpTable<T>) -> Option<&RouteStore<Self, T>> {
        table.ipv6_engine().map(|e| e.routes())
    }

    fn for_each_slot_mut<T, E>(
        table: &mut IpTable<T>,
        f: impl FnMut(&mut Slot<Self, T>) -> Result<(), E>,
    ) -> Result<usize, E> {
        match table.ipv6_engine_mut() {
            Some(e) => each_slot_mut(e.iter_mut(), f),
            None => Ok(0),
        }
    }

    fn bits(&self) -> u128 {
        u128::from(self.network())
    }

    fn from_bits(bits: u128, len: u8) -> Self {
        Ipv6Net::new(bits.into(), len).expect("length validated")
    }
}

// ----------------------------------------------------------------------
// Configuration record
// ----------------------------------------------------------------------

fn config_record(c: &Config) -> [u8; CONFIG_LEN] {
    let mut r = [0u8; CONFIG_LEN];
    r[0] = RETIRED_BATCH_DEFAULT;
    r[1] = RETIRED_SINGLE_DEFAULT;
    r[2] = if c.huge_pages { FLAG_HUGE_PAGES } else { 0 };
    r[3..7].copy_from_slice(&c.max_routes.to_le_bytes());
    r[7..11].copy_from_slice(&c.max_ipv4_groups.to_le_bytes());
    r[11..15].copy_from_slice(&c.max_ipv6_groups.to_le_bytes());
    r
}

fn parse_config(r: &[u8; CONFIG_LEN]) -> Result<Config, SnapshotError> {
    let u32_at = |i: usize| u32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]);
    if r[2] & !FLAG_HUGE_PAGES != 0 {
        return Err(SnapshotError::Malformed(format!(
            "unknown configuration flags {:#04x}",
            r[2]
        )));
    }
    for (code, none_allowed) in [(r[0], false), (r[1], true)] {
        if code > RETIRED_MAX_CODE && !(none_allowed && code == RETIRED_SINGLE_DEFAULT) {
            return Err(SnapshotError::Malformed(format!(
                "unknown backend code {code}"
            )));
        }
    }
    let (max_routes, max_v4, max_v6) = (u32_at(3), u32_at(7), u32_at(11));
    if max_routes > Config::MAX_ROUTES || max_v4 > Config::MAX_GROUPS || max_v6 > Config::MAX_GROUPS
    {
        return Err(SnapshotError::Malformed(
            "configuration limit above the encoding maximum".into(),
        ));
    }
    Ok(Config::new()
        .huge_pages(r[2] & FLAG_HUGE_PAGES != 0)
        .max_routes(max_routes)
        .max_ipv4_groups(max_v4)
        .max_ipv6_groups(max_v6))
}

// ----------------------------------------------------------------------
// Writer
// ----------------------------------------------------------------------

/// Serializes one payload into `scratch` (cleared first).
fn encode_value<T: Serialize>(value: &T, scratch: &mut Vec<u8>) -> Result<(), SnapshotError> {
    scratch.clear();
    ciborium::into_writer(value, &mut *scratch).map_err(|e| SnapshotError::Serialize(Box::new(e)))
}

/// Exact size of the body, or a value of at least `cap` (the walk stops
/// there: a dictionary is never larger than the preset's). Serializes the
/// payloads it walks, so serialization errors surface before any output.
pub(crate) fn body_size<T: Serialize>(
    table: &IpTable<T>,
    cap: u64,
    scratch: &mut Vec<u8>,
) -> Result<u64, SnapshotError> {
    fn family<N: Family, T: Serialize>(
        store: Option<&RouteStore<N, T>>,
        size: &mut u64,
        cap: u64,
        scratch: &mut Vec<u8>,
    ) -> Result<(), SnapshotError> {
        *size += 4;
        let Some(store) = store else { return Ok(()) };
        for s in store.iter() {
            *size += 1 + addr_bytes(s.net.len()) as u64;
        }
        for s in store.iter() {
            if *size >= cap {
                return Ok(());
            }
            *size += match &s.data {
                None => 1,
                Some(v) => {
                    encode_value(v, scratch)?;
                    let n = scratch.len() as u64;
                    varint_len(n + 1) + n
                }
            };
        }
        Ok(())
    }
    let mut size = (CONFIG_LEN + TRAILER.len()) as u64;
    family(Ipv4Net::store(table), &mut size, cap, scratch)?;
    family(Ipv6Net::store(table), &mut size, cap, scratch)?;
    Ok(size)
}

/// Writes the whole body (configuration, both families, end marker).
pub(crate) fn write_body<T: Serialize, W: Write>(
    table: &IpTable<T>,
    w: &mut W,
    order: EntryOrder,
    scratch: &mut Vec<u8>,
) -> Result<(), SnapshotError> {
    w.write_all(&config_record(table.config()))
        .map_err(xz::classify_encoder_error)?;
    let mut lens = Vec::with_capacity(BLOCK.min(table.len()));
    write_family(w, Ipv4Net::store(table), order, scratch, &mut lens)?;
    write_family(w, Ipv6Net::store(table), order, scratch, &mut lens)?;
    w.write_all(&TRAILER).map_err(xz::classify_encoder_error)
}

fn write_family<N: Family, T: Serialize, W: Write>(
    w: &mut W,
    store: Option<&RouteStore<N, T>>,
    order: EntryOrder,
    scratch: &mut Vec<u8>,
    lens: &mut Vec<u8>,
) -> Result<(), SnapshotError> {
    let count = store.map_or(0, |s| s.len());
    // The route store never holds more than `Config::MAX_ROUTES` routes.
    let count = u32::try_from(count).expect("route count fits the format");
    w.write_all(&count.to_le_bytes())
        .map_err(xz::classify_encoder_error)?;
    let Some(store) = store else { return Ok(()) };
    match order {
        EntryOrder::Storage => write_entries(w, store.iter(), scratch, lens),
        EntryOrder::Sorted => {
            let slot = |i: u32| store.get(i).expect("live slot");
            let mut order: Vec<u32> = store.live_slots().collect();
            order.sort_unstable_by_key(|&i| {
                let net = slot(i).net;
                (net.bits(), net.len())
            });
            write_entries(w, order.iter().map(|&i| slot(i)), scratch, lens)
        }
    }
}

/// Prefix blocks (lengths, then addresses), then the payload column.
fn write_entries<'a, N: Family + 'a, T: Serialize + 'a, W: Write>(
    w: &mut W,
    slots: impl Iterator<Item = &'a Slot<N, T>> + Clone,
    scratch: &mut Vec<u8>,
    lens: &mut Vec<u8>,
) -> Result<(), SnapshotError> {
    let io = xz::classify_encoder_error;
    let mut rest = slots.clone();
    loop {
        lens.clear();
        lens.extend(rest.clone().take(BLOCK).map(|s| s.net.len()));
        if lens.is_empty() {
            break;
        }
        w.write_all(lens).map_err(io)?;
        for s in rest.by_ref().take(lens.len()) {
            let len = s.net.len();
            w.write_all(&s.net.bits().to_be_bytes()[..addr_bytes(len)])
                .map_err(io)?;
        }
    }
    for s in slots {
        match &s.data {
            None => w.write_all(&[0]).map_err(io)?,
            Some(v) => {
                encode_value(v, scratch)?;
                write_varint(w, scratch.len() as u64 + 1).map_err(io)?;
                w.write_all(scratch).map_err(io)?;
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------
// Reader
// ----------------------------------------------------------------------

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, SnapshotError> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(SnapshotError::Io(e)),
        }
    }
    Ok(n)
}

/// Reads and validates the outer header.
pub(crate) fn read_header<R: Read>(r: &mut R) -> Result<(), SnapshotError> {
    let mut h = [0u8; HEADER_LEN];
    let got = read_up_to(r, &mut h)?;
    let m = got.min(MAGIC.len());
    if h[..m] != MAGIC[..m] {
        return Err(SnapshotError::NotASnapshot);
    }
    if got < HEADER_LEN {
        return Err(SnapshotError::Truncated);
    }
    if crc32(&h[..12]) != u32::from_le_bytes([h[12], h[13], h[14], h[15]]) {
        return Err(SnapshotError::Corrupt("header checksum mismatch"));
    }
    let version = u16::from_le_bytes([h[8], h[9]]);
    if version != FORMAT_VERSION {
        return Err(SnapshotError::UnsupportedVersion {
            found: version,
            supported: FORMAT_VERSION,
        });
    }
    if h[10] != CODEC_XZ || h[11] != PAYLOAD_CBOR {
        return Err(SnapshotError::UnsupportedCodec {
            compression: h[10],
            payload: h[11],
        });
    }
    Ok(())
}

/// Reads the 12-byte XZ stream header (magic, flags, CRC-32 of the flags)
/// and requires an integrity check (CRC32, CRC64 or SHA-256).
pub(crate) fn read_xz_stream_header<R: Read>(r: &mut R) -> Result<[u8; 12], SnapshotError> {
    const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
    let mut h = [0u8; 12];
    if read_up_to(r, &mut h)? < h.len() {
        return Err(SnapshotError::Truncated);
    }
    if h[..6] != XZ_MAGIC
        || crc32(&h[6..8]) != u32::from_le_bytes([h[8], h[9], h[10], h[11]])
        || h[6] != 0
        || h[7] & 0xF0 != 0
    {
        return Err(SnapshotError::Corrupt("bad XZ stream header"));
    }
    match h[7] {
        0x01 | 0x04 | 0x0A => Ok(h),
        0x00 => Err(SnapshotError::Malformed(
            "compressed stream without integrity check".into(),
        )),
        id => Err(SnapshotError::Malformed(format!(
            "unsupported integrity check {id:#04x}"
        ))),
    }
}

const FAILED: &str = "snapshot stream failed";

/// Decompressed-side adapter: counts bytes against
/// [`LoadOptions::max_decompressed_bytes`] and remembers why the decoder
/// failed, so that errors seen through `BufReader` and `ciborium` can be
/// reported precisely.
pub(crate) struct Limited<R> {
    inner: R,
    read: u64,
    max: Option<u64>,
    decoder_memory_limit: u64,
    failure: Option<SnapshotError>,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other(FAILED));
        }
        match self.inner.read(buf) {
            Ok(n) => {
                self.read += n as u64;
                if let Some(max) = self.max
                    && self.read > max
                {
                    self.failure = Some(SnapshotError::LimitExceeded {
                        limit: SnapshotLimit::DecompressedBytes,
                        max,
                        requested: None,
                    });
                    return Err(io::Error::other(FAILED));
                }
                Ok(n)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(e),
            Err(e) => {
                self.failure = Some(xz::classify_decoder_error(e, self.decoder_memory_limit));
                Err(io::Error::other(FAILED))
            }
        }
    }
}

/// Scratch buffer of the CBOR decoder (short strings and byte strings).
const CBOR_SCRATCH: usize = 4096;

/// The decompressed body of a snapshot being parsed.
pub(crate) struct Body<R> {
    inner: BufReader<Limited<R>>,
    /// Reused by every payload (`ciborium::from_reader` would zero a fresh
    /// 4 KiB buffer per payload).
    scratch: Box<[u8; CBOR_SCRATCH]>,
}

impl<R: Read> Body<R> {
    pub(crate) fn new(decoder: R, options: &LoadOptions) -> Self {
        Self {
            inner: BufReader::with_capacity(
                IO_BUF,
                Limited {
                    inner: decoder,
                    read: 0,
                    max: options.max_decompressed_bytes,
                    decoder_memory_limit: options.decoder_memory_limit,
                    failure: None,
                },
            ),
            scratch: Box::new([0; CBOR_SCRATCH]),
        }
    }

    /// The decoder (to inspect what follows the compressed stream).
    pub(crate) fn decoder_mut(&mut self) -> &mut R {
        &mut self.inner.get_mut().inner
    }

    /// Converts an I/O error seen while parsing: the decoder's own failure
    /// when there is one, otherwise the body ended early.
    fn fail(&mut self, e: io::Error) -> SnapshotError {
        if let Some(f) = self.inner.get_mut().failure.take() {
            return f;
        }
        match e.kind() {
            io::ErrorKind::UnexpectedEof => {
                SnapshotError::Malformed("the snapshot body ends early".into())
            }
            _ => SnapshotError::Io(e),
        }
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), SnapshotError> {
        self.inner.read_exact(buf).map_err(|e| self.fail(e))
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Unsigned LEB128, at most 10 bytes, minimal encoding.
    fn varint(&mut self) -> Result<u64, SnapshotError> {
        let mut v = 0u64;
        for i in 0..10 {
            let mut b = [0u8; 1];
            self.read_exact(&mut b)?;
            let byte = b[0];
            if i == 9 && byte > 1 {
                break;
            }
            v |= u64::from(byte & 0x7F) << (7 * i);
            if byte & 0x80 == 0 {
                if byte == 0 && i > 0 {
                    break;
                }
                return Ok(v);
            }
        }
        Err(SnapshotError::Malformed(
            "invalid variable-length integer".into(),
        ))
    }

    /// One payload: `0` for no payload, `n + 1` then `n` bytes of CBOR.
    fn value<T: DeserializeOwned>(&mut self, max: Option<u64>) -> Result<Option<T>, SnapshotError> {
        let tag = self.varint()?;
        if tag == 0 {
            return Ok(None);
        }
        let len = tag - 1;
        if let Some(max) = max
            && len > max
        {
            return Err(SnapshotError::LimitExceeded {
                limit: SnapshotLimit::ValueBytes,
                max,
                requested: Some(len),
            });
        }
        let mut frame = (&mut self.inner).take(len);
        let decoded = ciborium::from_reader_with_buffer::<T, _>(&mut frame, &mut self.scratch[..]);
        let left = frame.limit();
        match decoded {
            Ok(v) if left == 0 => Ok(Some(v)),
            Ok(_) => Err(SnapshotError::Malformed(format!(
                "payload of {len} bytes has {left} unused bytes"
            ))),
            Err(ciborium::de::Error::Io(e)) => {
                if left == 0 && self.inner.get_ref().failure.is_none() {
                    Err(SnapshotError::Malformed(format!(
                        "payload longer than its {len} declared bytes"
                    )))
                } else {
                    Err(self.fail(e))
                }
            }
            Err(e) => Err(SnapshotError::Deserialize(Box::new(e))),
        }
    }

    /// After the end marker: the decompressed stream must end there. Reading
    /// its end makes liblzma verify the integrity check and the index.
    pub(crate) fn finish(&mut self) -> Result<(), SnapshotError> {
        let mut extra = [0u8; 1];
        loop {
            return match self.inner.read(&mut extra) {
                Ok(0) => Ok(()),
                Ok(_) => Err(SnapshotError::Malformed("data after the end marker".into())),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => Err(self.fail(e)),
            };
        }
    }

    /// Refines a parse error: when the body looks malformed, decompress the
    /// rest of the stream so that a damaged snapshot is reported as
    /// corrupt or truncated (the checksum is only verified at the end).
    pub(crate) fn explain(&mut self, e: SnapshotError) -> SnapshotError {
        if !matches!(
            e,
            SnapshotError::Malformed(_) | SnapshotError::Deserialize(_)
        ) {
            return e;
        }
        match io::copy(&mut self.inner, &mut io::sink()) {
            Ok(_) => e,
            Err(io) => match self.fail(io) {
                damaged @ (SnapshotError::Corrupt(_) | SnapshotError::Truncated) => damaged,
                _ => e,
            },
        }
    }
}

/// Parses the body and rebuilds the table.
pub(crate) fn parse_body<T: DeserializeOwned, R: Read>(
    body: &mut Body<R>,
    options: &LoadOptions,
) -> Result<IpTable<T>, SnapshotError> {
    let mut record = [0u8; CONFIG_LEN];
    body.read_exact(&mut record)?;
    let stored = parse_config(&record)?;
    let mut table = IpTable::with_config(options.config.clone().unwrap_or(stored))?;
    let mut entries = 0u64;
    let mut lens = Vec::new();
    read_family::<Ipv4Net, T, R>(body, &mut table, options, &mut entries, &mut lens)?;
    read_family::<Ipv6Net, T, R>(body, &mut table, options, &mut entries, &mut lens)?;
    let mut trailer = [0u8; TRAILER.len()];
    body.read_exact(&mut trailer)?;
    if trailer != TRAILER {
        return Err(SnapshotError::Malformed("missing end marker".into()));
    }
    Ok(table)
}

fn read_family<N: Family, T: DeserializeOwned, R: Read>(
    body: &mut Body<R>,
    table: &mut IpTable<T>,
    options: &LoadOptions,
    entries: &mut u64,
    lens: &mut Vec<u8>,
) -> Result<(), SnapshotError> {
    let count = body.u32()?;
    if count > Config::MAX_ROUTES {
        return Err(SnapshotError::Malformed(format!(
            "{} route count {count} above the format maximum",
            N::FAMILY
        )));
    }
    *entries += u64::from(count);
    if let Some(max) = options.max_entries
        && *entries > max
    {
        return Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::Entries,
            max,
            requested: Some(*entries),
        });
    }
    let max_routes = table.config().max_routes;
    if count > max_routes {
        return Err(SnapshotError::Table(Error::CapacityExceeded {
            family: N::FAMILY,
            resource: Resource::Routes,
            limit: u64::from(max_routes),
        }));
    }
    let count = count as usize;
    let mut done = 0;
    while done < count {
        let m = BLOCK.min(count - done);
        lens.resize(m, 0);
        body.read_exact(lens)?;
        for &len in lens.iter() {
            if len > N::MAX_LEN {
                return Err(SnapshotError::Malformed(format!(
                    "{} prefix length {len} above {}",
                    N::FAMILY,
                    N::MAX_LEN
                )));
            }
            let mut bytes = [0u8; 16];
            body.read_exact(&mut bytes[..addr_bytes(len)])?;
            let bits = u128::from_be_bytes(bytes);
            if bits & !mask(len) != 0 {
                return Err(SnapshotError::Malformed(format!(
                    "{} prefix with host bits set (length {len})",
                    N::FAMILY
                )));
            }
            let net = N::from_bits(bits, len);
            if table.insert_replacing(net.into(), None)?.is_some() {
                return Err(SnapshotError::Malformed(format!("duplicate prefix {net}")));
            }
        }
        done += m;
    }
    // A fresh table allocates route slots in insertion order and nothing was
    // removed, so slot order is the order of the prefix column.
    let max_value = options.max_value_bytes;
    let filled = N::for_each_slot_mut(table, |slot| {
        slot.data = body.value(max_value)?;
        Ok::<(), SnapshotError>(())
    })?;
    debug_assert_eq!(filled, count);
    Ok(())
}
