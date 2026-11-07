//! Snapshot and load settings.

use crate::table::Config;

use super::error::SnapshotError;

/// Order of the routes inside a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum EntryOrder {
    /// Order of the route store (default): insertion order, with removed
    /// routes' slots reused by later insertions. Streams the table without
    /// any temporary array. A table restored from a snapshot keeps the order
    /// of that snapshot.
    #[default]
    Storage,
    /// Sorted by network address, then prefix length. Two tables with the
    /// same routes produce the same entry order whatever their history.
    /// Sorted prefixes compress better when the table was not built in
    /// address order (the prefix data of the generated benchmark tables
    /// shrinks about 2x), but payloads move with their routes: payloads
    /// that follow the insertion order can compress worse, and the total
    /// may grow (see the snapshot benchmark). Costs a temporary array of 4
    /// bytes per route and a sort before writing.
    Sorted,
}

/// What to do when the encoder needs more memory than
/// [`SnapshotOptions::memory_budget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OverBudget {
    /// Return [`SnapshotError::LimitExceeded`] (default): the compression
    /// settings are never lowered silently.
    #[default]
    Fail,
    /// Halve the LZMA2 dictionary until the encoder fits the budget (down to
    /// 4 KiB, then fail). A smaller dictionary than the uncompressed
    /// snapshot can reduce the compression ratio.
    ShrinkDictionary,
}

/// Durability of [`IpTable::snapshot_to_file`](crate::IpTable::snapshot_to_file).
///
/// The snapshot is always written to a temporary file of the destination
/// directory and published with a rename, so readers see either the
/// previous file or the complete new one. This setting only decides what
/// survives a power loss or an OS crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum FileSync {
    /// No `fsync`: after a crash the destination may hold the previous
    /// snapshot, the new one or, on some file systems, an empty or partial
    /// file.
    None,
    /// `fsync` the temporary file before the rename (default): after a crash
    /// the destination holds the previous or the new snapshot, complete.
    #[default]
    File,
    /// [`FileSync::File`], then `fsync` the directory after the rename
    /// (Unix only; ignored elsewhere): once the call returns, the new
    /// snapshot survives a crash.
    FileAndDirectory,
}

/// Settings of [`IpTable::snapshot_with_options`](crate::IpTable::snapshot_with_options)
/// and the other snapshot methods (builder style).
///
/// The default profile favors the compressed size: XZ preset 9 with the
/// extreme flag (`9 | liblzma::stream::PRESET_EXTREME`, 64 MiB
/// dictionary), one thread, CRC64 integrity check, routes in storage order.
///
/// Trade-offs:
///
/// * a higher level (and `extreme`) costs compression time, not
///   decompression time;
/// * streaming keeps the uncompressed snapshot out of memory, but the
///   encoder has its own working memory: about 10x the dictionary for
///   levels 7-9. The dictionary is reduced to the uncompressed size when
///   the snapshot is smaller (no effect on the ratio), so small tables need
///   little encoder memory;
/// * threads split the stream into independently compressed blocks: more
///   memory (one encoder and block buffers per thread) and usually a lower
///   ratio. With the default block size (3x the dictionary) a snapshot is
///   only split when it is larger than that;
/// * the maximal preset of a codec is not the best setting for every input:
///   compare with [`level`](Self::level) and
///   [`dict_size`](Self::dict_size) on your own data when size matters.
///
/// ```
/// use dir_24_8::{EntryOrder, OverBudget, SnapshotOptions};
///
/// // Faster, bounded compression for a periodic background snapshot.
/// let options = SnapshotOptions::new()
///     .level(6)
///     .extreme(false)
///     .threads(4)
///     .memory_budget(256 << 20)
///     .over_budget(OverBudget::ShrinkDictionary)
///     .order(EntryOrder::Sorted);
/// # let _ = options;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotOptions {
    pub(crate) level: u32,
    pub(crate) extreme: bool,
    pub(crate) dict_size: Option<u32>,
    pub(crate) threads: u32,
    pub(crate) block_size: Option<u64>,
    pub(crate) memory_budget: Option<u64>,
    pub(crate) over_budget: OverBudget,
    pub(crate) order: EntryOrder,
    pub(crate) file_sync: FileSync,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotOptions {
    /// Default XZ preset level.
    pub const DEFAULT_LEVEL: u32 = 9;
    /// Smallest LZMA2 dictionary (4 KiB).
    pub const MIN_DICT_SIZE: u32 = 4096;
    /// Largest LZMA2 dictionary accepted by the encoder (1.5 GiB).
    pub const MAX_DICT_SIZE: u32 = 1536 << 20;
    /// Largest number of encoder or decoder threads (liblzma's limit).
    pub const MAX_THREADS: u32 = 16384;

    /// The default profile: level 9, extreme, one thread, no memory budget,
    /// storage order, [`FileSync::File`].
    ///
    /// ```
    /// use dir_24_8::SnapshotOptions;
    ///
    /// assert_eq!(SnapshotOptions::new(), SnapshotOptions::default());
    /// ```
    pub const fn new() -> Self {
        Self {
            level: Self::DEFAULT_LEVEL,
            extreme: true,
            dict_size: None,
            threads: 1,
            block_size: None,
            memory_budget: None,
            over_budget: OverBudget::Fail,
            order: EntryOrder::Storage,
            file_sync: FileSync::File,
        }
    }

    /// XZ preset level, `0..=9` (default 9). Lower levels compress faster
    /// with a smaller dictionary.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace, SnapshotOptions};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let fast = table.snapshot_with_options(&SnapshotOptions::new().level(1).extreme(false))?;
    /// assert_eq!(IpTable::<u32>::load(&fast)?.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn level(mut self, level: u32) -> Self {
        self.level = level;
        self
    }

    /// Adds `liblzma::stream::PRESET_EXTREME` to the level (default `true`):
    /// a slower, more thorough match search for a few percent smaller
    /// output. Decompression speed and memory are unchanged.
    ///
    /// ```
    /// use dir_24_8::SnapshotOptions;
    ///
    /// let options = SnapshotOptions::new().extreme(false); // plain preset 9
    /// # let _ = options;
    /// ```
    pub const fn extreme(mut self, extreme: bool) -> Self {
        self.extreme = extreme;
        self
    }

    /// LZMA2 dictionary size in bytes, overriding the preset's
    /// (`MIN_DICT_SIZE..=MAX_DICT_SIZE`). Encoder memory grows about 10x
    /// the dictionary, decoder memory about 1x. It is still reduced to the
    /// uncompressed size of small snapshots.
    ///
    /// ```
    /// use dir_24_8::SnapshotOptions;
    ///
    /// let options = SnapshotOptions::new().dict_size(16 << 20); // 16 MiB
    /// # let _ = options;
    /// ```
    pub const fn dict_size(mut self, bytes: u32) -> Self {
        self.dict_size = Some(bytes);
        self
    }

    /// Encoder threads (default 1). More than one uses liblzma's
    /// multithreaded encoder, which compresses blocks of
    /// [`block_size`](Self::block_size) bytes independently.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, LoadOptions, Replace, SnapshotOptions};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let bytes = table.snapshot_with_options(&SnapshotOptions::new().threads(2))?;
    /// let restored: IpTable<u32> = IpTable::load_with_options(&bytes, &LoadOptions::new().threads(2))?;
    /// assert_eq!(restored.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn threads(mut self, threads: u32) -> Self {
        self.threads = threads;
        self
    }

    /// Uncompressed bytes per block in multithreaded mode (default: 3x the
    /// dictionary, at least 1 MiB). Smaller blocks give more parallelism
    /// and a lower ratio. Ignored with one thread.
    ///
    /// ```
    /// use dir_24_8::SnapshotOptions;
    ///
    /// let options = SnapshotOptions::new().threads(8).block_size(8 << 20);
    /// # let _ = options;
    /// ```
    pub const fn block_size(mut self, bytes: u64) -> Self {
        self.block_size = Some(bytes);
        self
    }

    /// Upper bound of the encoder memory, in bytes, as computed by liblzma
    /// for the selected settings (all threads included). It does not cover
    /// the table, the destination (the `Vec<u8>` of
    /// [`IpTable::snapshot`](crate::IpTable::snapshot)) nor the small I/O
    /// buffers. See [`over_budget`](Self::over_budget) for what happens
    /// when the settings do not fit. Default: no budget.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, Replace, SnapshotError, SnapshotLimit, SnapshotOptions};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// // Even the smallest dictionary needs more than 1 KiB of encoder memory.
    /// let err = table.snapshot_with_options(&SnapshotOptions::new().memory_budget(1024)).unwrap_err();
    /// assert!(matches!(err, SnapshotError::LimitExceeded { limit: SnapshotLimit::EncoderMemory, .. }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn memory_budget(mut self, bytes: u64) -> Self {
        self.memory_budget = Some(bytes);
        self
    }

    /// Policy when the settings exceed the [memory budget](Self::memory_budget)
    /// (default [`OverBudget::Fail`]).
    ///
    /// ```
    /// use dir_24_8::{OverBudget, SnapshotOptions};
    ///
    /// let options = SnapshotOptions::new()
    ///     .memory_budget(64 << 20)
    ///     .over_budget(OverBudget::ShrinkDictionary);
    /// # let _ = options;
    /// ```
    pub const fn over_budget(mut self, policy: OverBudget) -> Self {
        self.over_budget = policy;
        self
    }

    /// Order of the routes in the snapshot (default [`EntryOrder::Storage`]).
    ///
    /// ```
    /// use dir_24_8::{EntryOrder, IpNet, IpTable, Replace, SnapshotOptions};
    ///
    /// let sorted = SnapshotOptions::new().order(EntryOrder::Sorted);
    /// let (mut a, mut b) = (IpTable::new(), IpTable::new());
    /// for net in ["10.0.0.0/8", "192.0.2.0/24"] {
    ///     a.insert(net.parse::<IpNet>()?, Some(1u8), Replace)?;
    /// }
    /// for net in ["192.0.2.0/24", "10.0.0.0/8"] {
    ///     b.insert(net.parse::<IpNet>()?, Some(1u8), Replace)?;
    /// }
    /// // Same routes, different history: identical sorted snapshots.
    /// assert_eq!(a.snapshot_with_options(&sorted)?, b.snapshot_with_options(&sorted)?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn order(mut self, order: EntryOrder) -> Self {
        self.order = order;
        self
    }

    /// Durability of file snapshots (default [`FileSync::File`]).
    ///
    /// ```
    /// use dir_24_8::{FileSync, SnapshotOptions};
    ///
    /// let options = SnapshotOptions::new().file_sync(FileSync::FileAndDirectory);
    /// # let _ = options;
    /// ```
    pub const fn file_sync(mut self, sync: FileSync) -> Self {
        self.file_sync = sync;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), SnapshotError> {
        if self.level > 9 {
            return Err(SnapshotError::InvalidOptions("level above 9"));
        }
        if let Some(d) = self.dict_size
            && !(Self::MIN_DICT_SIZE..=Self::MAX_DICT_SIZE).contains(&d)
        {
            return Err(SnapshotError::InvalidOptions(
                "dictionary size outside 4 KiB..=1.5 GiB",
            ));
        }
        if !(1..=Self::MAX_THREADS).contains(&self.threads) {
            return Err(SnapshotError::InvalidOptions("threads outside 1..=16384"));
        }
        if self.block_size == Some(0) {
            return Err(SnapshotError::InvalidOptions("block size of 0 bytes"));
        }
        Ok(())
    }
}

/// Settings of [`IpTable::load_with_options`](crate::IpTable::load_with_options)
/// and [`IpTable::load_from_file_with_options`](crate::IpTable::load_from_file_with_options)
/// (builder style).
///
/// Whatever the limits, every count, length and CIDR of the snapshot is
/// validated before it is used, and no allocation is sized from a declared
/// value: memory only grows with data actually decoded. The limits bound
/// that data for untrusted inputs:
///
/// | Limit | Default | Scope |
/// |---|---|---|
/// | [`decoder_memory_limit`](Self::decoder_memory_limit) | 256 MiB | liblzma decoder state only (dictionary, buffers); not the table, not the process |
/// | [`max_decompressed_bytes`](Self::max_decompressed_bytes) | none | total decompressed snapshot bytes |
/// | [`max_entries`](Self::max_entries) | none | routes of both families (the restored [`Config::max_routes`] also applies per family) |
/// | [`max_value_bytes`](Self::max_value_bytes) | none | encoded size of each payload |
///
/// The default decoder limit is about four times what the default
/// snapshot profile needs (64 MiB dictionary).
///
/// ```
/// use dir_24_8::{IpNet, IpTable, LoadOptions, Replace, SnapshotError, SnapshotLimit};
///
/// let mut table = IpTable::new();
/// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(String::from("corp")), Replace)?;
/// table.insert("2001:db8::/32".parse::<IpNet>()?, None, Replace)?;
/// let bytes = table.snapshot()?;
///
/// let untrusted = LoadOptions::new()
///     .decoder_memory_limit(64 << 20)
///     .max_decompressed_bytes(1 << 30)
///     .max_entries(1_000_000)
///     .max_value_bytes(4096);
/// assert_eq!(IpTable::<String>::load_with_options(&bytes, &untrusted)?.len(), 2);
///
/// let err = IpTable::<String>::load_with_options(&bytes, &LoadOptions::new().max_entries(1)).unwrap_err();
/// assert!(matches!(err, SnapshotError::LimitExceeded { limit: SnapshotLimit::Entries, .. }));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOptions {
    pub(crate) decoder_memory_limit: u64,
    pub(crate) max_decompressed_bytes: Option<u64>,
    pub(crate) max_entries: Option<u64>,
    pub(crate) max_value_bytes: Option<u64>,
    pub(crate) threads: u32,
    pub(crate) config: Option<Config>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadOptions {
    /// Default [`decoder_memory_limit`](Self::decoder_memory_limit): 256 MiB.
    pub const DEFAULT_DECODER_MEMORY_LIMIT: u64 = 256 << 20;

    /// Default settings: decoder memory limit of 256 MiB, no other limit, one
    /// decoder thread, configuration taken from the snapshot.
    ///
    /// ```
    /// use dir_24_8::LoadOptions;
    ///
    /// assert_eq!(LoadOptions::new(), LoadOptions::default());
    /// ```
    pub const fn new() -> Self {
        Self {
            decoder_memory_limit: Self::DEFAULT_DECODER_MEMORY_LIMIT,
            max_decompressed_bytes: None,
            max_entries: None,
            max_value_bytes: None,
            threads: 1,
            config: None,
        }
    }

    /// Memory limit of the XZ decoder, enforced by liblzma (`u64::MAX`: no
    /// limit). It covers the decoder's dictionary and buffers only. A
    /// snapshot compressed with a larger dictionary fails with
    /// [`SnapshotLimit::DecoderMemory`](super::SnapshotLimit::DecoderMemory).
    ///
    /// ```
    /// use dir_24_8::LoadOptions;
    ///
    /// let options = LoadOptions::new().decoder_memory_limit(u64::MAX); // trusted input
    /// # let _ = options;
    /// ```
    pub const fn decoder_memory_limit(mut self, bytes: u64) -> Self {
        self.decoder_memory_limit = bytes;
        self
    }

    /// Maximum number of decompressed bytes (default: no limit). Bounds the
    /// work spent on a highly compressible input.
    ///
    /// ```
    /// use dir_24_8::{IpNet, IpTable, LoadOptions, Replace, SnapshotError};
    ///
    /// let mut table = IpTable::new();
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let bytes = table.snapshot()?;
    /// let err = IpTable::<u32>::load_with_options(&bytes, &LoadOptions::new().max_decompressed_bytes(8))
    ///     .unwrap_err();
    /// assert!(matches!(err, SnapshotError::LimitExceeded { .. }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn max_decompressed_bytes(mut self, bytes: u64) -> Self {
        self.max_decompressed_bytes = Some(bytes);
        self
    }

    /// Maximum number of routes, both families together (default: no
    /// limit). Checked against the declared counts before any route is
    /// inserted.
    ///
    /// ```
    /// use dir_24_8::LoadOptions;
    ///
    /// let options = LoadOptions::new().max_entries(2_000_000);
    /// # let _ = options;
    /// ```
    pub const fn max_entries(mut self, entries: u64) -> Self {
        self.max_entries = Some(entries);
        self
    }

    /// Maximum encoded size of one payload, in bytes (default: no limit).
    /// Checked against each payload's declared size before it is decoded.
    ///
    /// ```
    /// use dir_24_8::LoadOptions;
    ///
    /// let options = LoadOptions::new().max_value_bytes(64 << 10);
    /// # let _ = options;
    /// ```
    pub const fn max_value_bytes(mut self, bytes: u64) -> Self {
        self.max_value_bytes = Some(bytes);
        self
    }

    /// Decoder threads (default 1). Only snapshots written with several
    /// encoder threads (several blocks) decode in parallel; liblzma falls
    /// back to fewer threads when the [decoder memory
    /// limit](Self::decoder_memory_limit) would be exceeded.
    ///
    /// ```
    /// use dir_24_8::LoadOptions;
    ///
    /// let options = LoadOptions::new().threads(4);
    /// # let _ = options;
    /// ```
    pub const fn threads(mut self, threads: u32) -> Self {
        self.threads = threads;
        self
    }

    /// Builds the table with this configuration instead of the one stored
    /// in the snapshot: for example to raise capacity limits or change the
    /// huge-page advice.
    ///
    /// ```
    /// use dir_24_8::{Config, IpNet, IpTable, LoadOptions, Replace};
    ///
    /// let mut table = IpTable::with_config(Config::new().max_routes(10))?;
    /// table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1u32), Replace)?;
    /// let bytes = table.snapshot()?;
    ///
    /// assert_eq!(IpTable::<u32>::load(&bytes)?.config(), &Config::new().max_routes(10)); // stored
    /// let options = LoadOptions::new().config(Config::new()); // default limits
    /// assert_eq!(IpTable::<u32>::load_with_options(&bytes, &options)?.config(), &Config::new());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub(crate) fn validate(&self) -> Result<(), SnapshotError> {
        if !(1..=SnapshotOptions::MAX_THREADS).contains(&self.threads) {
            return Err(SnapshotError::InvalidOptions("threads outside 1..=16384"));
        }
        if self.decoder_memory_limit == 0 {
            return Err(SnapshotError::InvalidOptions("decoder memory limit of 0"));
        }
        Ok(())
    }
}
