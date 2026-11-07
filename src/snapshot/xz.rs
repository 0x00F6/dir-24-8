//! XZ (LZMA2) codec setup through liblzma: encoder planning (dictionary,
//! memory budget), encoder and decoder streams, error classification.

use std::io;

use liblzma::stream::{Check, Error as LzmaError, Filters, LzmaOptions, MtStreamBuilder, Stream};

use super::error::{SnapshotError, SnapshotLimit};
use super::options::{LoadOptions, OverBudget, SnapshotOptions};

/// Encoder settings resolved from [`SnapshotOptions`] and the uncompressed
/// size of the snapshot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EncoderPlan {
    pub(crate) preset: u32,
    pub(crate) dict_size: u32,
    pub(crate) threads: u32,
    pub(crate) block_size: Option<u64>,
    pub(crate) memory: u64,
}

fn preset_of(options: &SnapshotOptions) -> u32 {
    options.level
        | if options.extreme {
            liblzma::stream::PRESET_EXTREME
        } else {
            0
        }
}

/// LZMA2 options of `preset` and the dictionary size of `preset`, read from
/// liblzma (`lzma_lzma_preset`).
fn preset_options(preset: u32) -> Result<liblzma_sys::lzma_options_lzma, SnapshotError> {
    // SAFETY: `lzma_options_lzma` is a plain C struct (integers, enums
    // represented as integers, raw pointers) for which all-zero bytes are a
    // valid value; liblzma's own `LzmaOptions::new_preset` initializes it
    // the same way. `lzma_lzma_preset` only writes to the struct, which
    // outlives the call.
    unsafe {
        let mut opts: liblzma_sys::lzma_options_lzma = core::mem::zeroed();
        if liblzma_sys::lzma_lzma_preset(&raw mut opts, preset) != 0 {
            return Err(SnapshotError::InvalidOptions("unsupported XZ preset"));
        }
        Ok(opts)
    }
}

/// Memory needed by a single-threaded LZMA2 encoder with `preset` and
/// `dict_size`, as computed by liblzma (`lzma_raw_encoder_memusage`).
fn single_thread_memusage(preset: u32, dict_size: u32) -> Result<u64, SnapshotError> {
    let mut opts = preset_options(preset)?;
    opts.dict_size = dict_size;
    let filters = [
        liblzma_sys::lzma_filter {
            id: liblzma_sys::LZMA_FILTER_LZMA2,
            options: (&raw mut opts).cast(),
        },
        liblzma_sys::lzma_filter {
            id: liblzma_sys::LZMA_VLI_UNKNOWN,
            options: core::ptr::null_mut(),
        },
    ];
    // SAFETY: `filters` is a valid filter chain terminated by
    // `LZMA_VLI_UNKNOWN`; it and the options it points to outlive the call,
    // which only reads them.
    let bytes = unsafe { liblzma_sys::lzma_raw_encoder_memusage(filters.as_ptr()) };
    if bytes == u64::MAX {
        return Err(SnapshotError::InvalidOptions(
            "unsupported LZMA2 settings (dictionary size)",
        ));
    }
    Ok(bytes)
}

fn filters(preset: u32, dict_size: u32) -> Result<Filters, SnapshotError> {
    let mut opts = LzmaOptions::new_preset(preset).map_err(compression)?;
    opts.dict_size(dict_size);
    let mut filters = Filters::new();
    filters.lzma2(&opts);
    Ok(filters)
}

fn mt_builder(plan: &EncoderPlan) -> Result<MtStreamBuilder, SnapshotError> {
    let mut builder = MtStreamBuilder::new();
    builder
        .threads(plan.threads)
        .filters(filters(plan.preset, plan.dict_size)?)
        .check(Check::Crc64);
    if let Some(block) = plan.block_size {
        builder.block_size(block);
    }
    Ok(builder)
}

fn memusage(plan: &EncoderPlan) -> Result<u64, SnapshotError> {
    if plan.threads == 1 {
        return single_thread_memusage(plan.preset, plan.dict_size);
    }
    match mt_builder(plan)?.memusage() {
        u64::MAX => Err(SnapshotError::InvalidOptions(
            "unsupported multithreaded settings (threads, block size)",
        )),
        bytes => Ok(bytes),
    }
}

/// Dictionary size requested by `options` (the preset's unless
/// overridden), before fitting it to the snapshot.
pub(crate) fn wanted_dict_size(options: &SnapshotOptions) -> Result<u32, SnapshotError> {
    match options.dict_size {
        Some(d) => Ok(d),
        None => Ok(preset_options(preset_of(options))?.dict_size),
    }
}

/// Resolves the encoder settings for a snapshot of `body_size` uncompressed
/// bytes (`body_size` may stop at the preset dictionary size: a larger
/// snapshot uses the full dictionary anyway).
pub(crate) fn plan(
    options: &SnapshotOptions,
    body_size: u64,
) -> Result<EncoderPlan, SnapshotError> {
    let preset = preset_of(options);
    let wanted = wanted_dict_size(options)?;
    // A dictionary larger than the data it compresses is never used: fit it
    // (the ratio is unchanged, the encoder and decoder memory shrink).
    let fitted = u64::from(wanted)
        .min(body_size)
        .max(u64::from(SnapshotOptions::MIN_DICT_SIZE)) as u32;
    let mut plan = EncoderPlan {
        preset,
        dict_size: fitted,
        threads: options.threads,
        block_size: options.block_size,
        memory: 0,
    };
    loop {
        plan.memory = memusage(&plan)?;
        let Some(budget) = options.memory_budget else {
            return Ok(plan);
        };
        if plan.memory <= budget {
            return Ok(plan);
        }
        let over = SnapshotError::LimitExceeded {
            limit: SnapshotLimit::EncoderMemory,
            max: budget,
            requested: Some(plan.memory),
        };
        match options.over_budget {
            OverBudget::Fail => return Err(over),
            OverBudget::ShrinkDictionary => {
                if plan.dict_size <= SnapshotOptions::MIN_DICT_SIZE {
                    return Err(over);
                }
                plan.dict_size = (plan.dict_size / 2).max(SnapshotOptions::MIN_DICT_SIZE);
            }
        }
    }
}

/// XZ encoder stream (CRC64 integrity check) for `plan`.
pub(crate) fn encoder(plan: &EncoderPlan) -> Result<Stream, SnapshotError> {
    if plan.threads == 1 {
        Stream::new_stream_encoder(&filters(plan.preset, plan.dict_size)?, Check::Crc64)
            .map_err(compression)
    } else {
        mt_builder(plan)?.encoder().map_err(compression)
    }
}

/// XZ decoder stream for one `.xz` stream (no concatenation), with the
/// memory limit of `options`.
pub(crate) fn decoder(options: &LoadOptions) -> Result<Stream, SnapshotError> {
    let limit = options.decoder_memory_limit;
    let stream = if options.threads == 1 {
        Stream::new_stream_decoder(limit, 0)
    } else {
        MtStreamBuilder::new()
            .threads(options.threads)
            .memlimit_threading(limit)
            .memlimit_stop(limit)
            .decoder()
    };
    stream.map_err(|e| match e {
        LzmaError::MemLimit => SnapshotError::LimitExceeded {
            limit: SnapshotLimit::DecoderMemory,
            max: limit,
            requested: None,
        },
        e => compression(e),
    })
}

fn compression(e: LzmaError) -> SnapshotError {
    SnapshotError::Compression(Box::new(e))
}

/// Classifies an error returned by the XZ decoder (or by the source it
/// reads through it).
pub(crate) fn classify_decoder_error(e: io::Error, decoder_memory_limit: u64) -> SnapshotError {
    if let Some(lzma) = e.get_ref().and_then(|i| i.downcast_ref::<LzmaError>()) {
        return match lzma {
            LzmaError::Data | LzmaError::Format => {
                SnapshotError::Corrupt("compressed data or integrity check mismatch")
            }
            LzmaError::MemLimit => SnapshotError::LimitExceeded {
                limit: SnapshotLimit::DecoderMemory,
                max: decoder_memory_limit,
                requested: None,
            },
            LzmaError::NoCheck | LzmaError::UnsupportedCheck => SnapshotError::Malformed(
                "compressed stream without a supported integrity check".into(),
            ),
            _ => SnapshotError::Compression(Box::new(*lzma)),
        };
    }
    match e.kind() {
        // liblzma's reader: the input ended before the end of the stream.
        io::ErrorKind::UnexpectedEof => SnapshotError::Truncated,
        // liblzma's reader: the decoder made no progress on its input.
        io::ErrorKind::InvalidData => SnapshotError::Corrupt("undecodable compressed data"),
        _ => SnapshotError::Io(e),
    }
}

/// Classifies an error returned by the XZ encoder or the destination.
pub(crate) fn classify_encoder_error(e: io::Error) -> SnapshotError {
    match e.get_ref().and_then(|i| i.downcast_ref::<LzmaError>()) {
        Some(lzma) => SnapshotError::Compression(Box::new(*lzma)),
        None => SnapshotError::Io(e),
    }
}
