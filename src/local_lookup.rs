//! Local GIDX0002/GRTB0002 lookup and Web2 artifact conversion.
//!
//! The on-disk reader is intentionally independent of the Python lookup
//! service.  Endpoint and candidate files use the same framing as Web2.

use memmap2::{Mmap, MmapOptions};
use std::cmp::min;
use std::fs::File;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

const TABLE_HEADER_BYTES: usize = 256;
const PAGE_BYTES: usize = 4096;
const COUNT_BYTES: usize = 2;
const TABLE_VERSION: u32 = 2;
const PREFIX_ENTRIES: usize = (1 << 24) + 1;
const REQUIRED_FLAGS: u32 = 0xff;
const INTERNAL_QUERY_BYTES: usize = 24;
const INTERNAL_MATCH_BYTES: usize = 24;

pub const ENDPOINT_MAGIC: &[u8; 8] = b"NTLMEND1";
pub const ENDPOINT_HEADER_BYTES: usize = 32;
pub const ENDPOINT_RECORD_BYTES: usize = 8;
pub const CANDIDATE_MAGIC: &[u8; 8] = b"NTLMCAN1";
pub const CANDIDATE_HEADER_BYTES: usize = 40;
pub const CANDIDATE_RECORD_BYTES: usize = 16;

#[derive(Debug, Error)]
pub enum LocalLookupError {
    #[error("{0}")]
    Invalid(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, LocalLookupError>;

fn invalid(message: impl Into<String>) -> LocalLookupError {
    LocalLookupError::Invalid(message.into())
}

fn io_error(context: impl Into<String>, source: std::io::Error) -> LocalLookupError {
    LocalLookupError::Io {
        context: context.into(),
        source,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateRecord {
    pub ordinal: u64,
    pub start: u64,
}

#[derive(Clone, Debug)]
pub struct LocalLookupOptions {
    /// Base name with or without `.grtb`; shards are `<base>.0000.grtb`, etc.
    pub data_base: PathBuf,
    pub index_path: PathBuf,
    pub read_workers: usize,
    pub preload_index: bool,
    pub lock_index: bool,
}

impl LocalLookupOptions {
    pub fn new(data_base: impl Into<PathBuf>, index_path: impl Into<PathBuf>) -> Self {
        Self {
            data_base: data_base.into(),
            index_path: index_path.into(),
            read_workers: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            preload_index: false,
            lock_index: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableInfo {
    pub records: u64,
    pub blocks: u64,
    pub data_bytes: u64,
    pub index_bytes: u64,
    pub index_locked: bool,
    pub records_per_part: u64,
    pub parts: u32,
    pub start_bits: u32,
    pub rice_k: u32,
    pub min_endpoint: u64,
    pub max_endpoint: u64,
}

/// Thread-safe cancellation and progress state for a local lookup.
#[derive(Default)]
pub struct LookupControl {
    cancelled: AtomicBool,
    processed: AtomicU64,
}

impl LookupControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn processed(&self) -> u64 {
        self.processed.load(Ordering::Acquire)
    }
}

pub struct LocalTable {
    reader: Reader,
    read_workers: usize,
}

impl LocalTable {
    pub fn open(options: LocalLookupOptions) -> Result<Self> {
        if options.read_workers == 0 {
            return Err(invalid("lookup read worker count must be positive"));
        }
        Ok(Self {
            reader: Reader::open(
                options.data_base,
                &options.index_path,
                options.preload_index,
                options.lock_index,
            )?,
            read_workers: options.read_workers,
        })
    }

    pub fn info(&self) -> &TableInfo {
        &self.reader.info
    }

    pub fn lookup_endpoints(
        &self,
        endpoints: &[u64],
        control: Option<&LookupControl>,
    ) -> Result<Vec<CandidateRecord>> {
        if endpoints.is_empty() {
            return Err(invalid("endpoint batch must not be empty"));
        }
        let owned_control = LookupControl::default();
        let control = control.unwrap_or(&owned_control);
        let query_data = encode_internal_queries(endpoints)?;
        let prepared = self.reader.prepare_encoded(query_data, control)?;
        let (_, raw_matches) =
            self.reader
                .lookup_prepared(&prepared, self.read_workers, control)?;
        decode_internal_matches(&raw_matches, endpoints.len() as u64)
    }

    /// Consume a complete `NTLMEND1` blob and return a complete `NTLMCAN1` blob.
    pub fn lookup_endpoint_file(
        &self,
        endpoint_file: &[u8],
        control: Option<&LookupControl>,
    ) -> Result<Vec<u8>> {
        let endpoints = parse_endpoint_file(endpoint_file)?;
        let candidates = self.lookup_endpoints(&endpoints, control)?;
        encode_candidate_file(endpoints.len() as u64, &candidates)
    }
}

pub fn parse_endpoint_file(data: &[u8]) -> Result<Vec<u64>> {
    if data.len() < ENDPOINT_HEADER_BYTES {
        return Err(invalid("native endpoint file is truncated"));
    }
    if &data[..8] != ENDPOINT_MAGIC
        || read_le_u32(data, 8)? != 1
        || read_le_u32(data, 12)? != ENDPOINT_RECORD_BYTES as u32
        || read_le_u32(data, 24)? != 0
        || read_le_u32(data, 28)? != 0
    {
        return Err(invalid("unsupported native endpoint format"));
    }
    let count = read_le_u64(data, 16)?;
    if count == 0 || count > usize::MAX as u64 {
        return Err(invalid(
            "endpoint record count is outside the allowed range",
        ));
    }
    let payload = (count as usize)
        .checked_mul(ENDPOINT_RECORD_BYTES)
        .and_then(|size| ENDPOINT_HEADER_BYTES.checked_add(size))
        .ok_or_else(|| invalid("native endpoint file size overflows this platform"))?;
    if data.len() != payload {
        return Err(invalid(
            "native endpoint file size does not match record count",
        ));
    }
    let mut endpoints = Vec::with_capacity(count as usize);
    for offset in (ENDPOINT_HEADER_BYTES..data.len()).step_by(ENDPOINT_RECORD_BYTES) {
        endpoints.push(read_le_u64(data, offset)?);
    }
    Ok(endpoints)
}

pub fn encode_candidate_file(query_count: u64, candidates: &[CandidateRecord]) -> Result<Vec<u8>> {
    if candidates.iter().any(|item| item.ordinal >= query_count) {
        return Err(invalid(
            "candidate ordinal is outside the endpoint query batch",
        ));
    }
    let payload = candidates
        .len()
        .checked_mul(CANDIDATE_RECORD_BYTES)
        .and_then(|size| CANDIDATE_HEADER_BYTES.checked_add(size))
        .ok_or_else(|| invalid("native candidate file size overflows this platform"))?;
    let mut output = Vec::with_capacity(payload);
    output.extend_from_slice(CANDIDATE_MAGIC);
    output.extend_from_slice(&1u32.to_le_bytes());
    output.extend_from_slice(&(CANDIDATE_RECORD_BYTES as u32).to_le_bytes());
    output.extend_from_slice(&query_count.to_le_bytes());
    output.extend_from_slice(&(candidates.len() as u64).to_le_bytes());
    output.extend_from_slice(&0u64.to_le_bytes());
    for item in candidates {
        output.extend_from_slice(&item.ordinal.to_le_bytes());
        output.extend_from_slice(&item.start.to_le_bytes());
    }
    Ok(output)
}

pub fn validate_candidate_file(data: &[u8], expected_query_count: Option<u64>) -> Result<u64> {
    if data.len() < CANDIDATE_HEADER_BYTES {
        return Err(invalid("native candidate file is truncated"));
    }
    if &data[..8] != CANDIDATE_MAGIC
        || read_le_u32(data, 8)? != 1
        || read_le_u32(data, 12)? != CANDIDATE_RECORD_BYTES as u32
        || read_le_u64(data, 32)? != 0
    {
        return Err(invalid("unsupported native candidate format"));
    }
    let query_count = read_le_u64(data, 16)?;
    let match_count = read_le_u64(data, 24)?;
    if expected_query_count.is_some_and(|expected| query_count != expected) {
        return Err(invalid(
            "candidate query count does not match the endpoint batch",
        ));
    }
    if match_count > usize::MAX as u64 {
        return Err(invalid(
            "candidate match count is outside the allowed range",
        ));
    }
    let expected_len = (match_count as usize)
        .checked_mul(CANDIDATE_RECORD_BYTES)
        .and_then(|size| CANDIDATE_HEADER_BYTES.checked_add(size))
        .ok_or_else(|| invalid("native candidate file size overflows this platform"))?;
    if data.len() != expected_len {
        return Err(invalid(
            "native candidate file size does not match match count",
        ));
    }
    for offset in (CANDIDATE_HEADER_BYTES..data.len()).step_by(CANDIDATE_RECORD_BYTES) {
        if read_le_u64(data, offset)? >= query_count {
            return Err(invalid(
                "candidate ordinal is outside the endpoint query batch",
            ));
        }
    }
    Ok(match_count)
}

fn encode_internal_queries(endpoints: &[u64]) -> Result<Vec<u8>> {
    let capacity = endpoints
        .len()
        .checked_mul(INTERNAL_QUERY_BYTES)
        .ok_or_else(|| invalid("internal query buffer size overflow"))?;
    let mut output = Vec::with_capacity(capacity);
    for (ordinal, endpoint) in endpoints.iter().enumerate() {
        output.extend_from_slice(&(ordinal as u64).to_le_bytes());
        output.extend_from_slice(&0u64.to_le_bytes());
        output.extend_from_slice(&endpoint.to_le_bytes());
    }
    Ok(output)
}

fn decode_internal_matches(data: &[u8], query_count: u64) -> Result<Vec<CandidateRecord>> {
    if !data.len().is_multiple_of(INTERNAL_MATCH_BYTES) {
        return Err(invalid("native match buffer is inconsistent"));
    }
    let mut output = Vec::with_capacity(data.len() / INTERNAL_MATCH_BYTES);
    for offset in (0..data.len()).step_by(INTERNAL_MATCH_BYTES) {
        let ordinal = read_le_u64(data, offset)?;
        if read_le_u64(data, offset + 8)? != 0 || ordinal >= query_count {
            return Err(invalid("native match token is outside the query batch"));
        }
        output.push(CandidateRecord {
            ordinal,
            start: read_le_u64(data, offset + 16)?,
        });
    }
    Ok(output)
}

#[derive(Clone, Copy)]
struct Work {
    endpoint: u64,
    tag: usize,
    page: u64,
}

struct PreparedLookup {
    query_data: Vec<u8>,
    work: Vec<Work>,
    page_offsets: Vec<u32>,
    query_count: usize,
}

impl PreparedLookup {
    fn page_count(&self) -> usize {
        self.page_offsets.len().saturating_sub(1)
    }
}

struct DecodedPage {
    count: usize,
    start_bit: usize,
    endpoints: [u64; 1024],
}

struct BitReader<'a> {
    data: &'a [u8],
    bits: usize,
    position: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], position: usize) -> Self {
        Self {
            data,
            bits: data.len() * 8,
            position,
        }
    }

    fn read_bits(&mut self, count: u32) -> Result<u64> {
        let count = count as usize;
        if count > 64 || self.position > self.bits || count > self.bits - self.position {
            return Err(invalid("V2 packed page is truncated"));
        }
        if count == 0 {
            return Ok(0);
        }
        let byte = self.position >> 3;
        let bit = self.position & 7;
        let available_bytes = ((self.bits + 7) >> 3) - byte;
        let first = min(available_bytes, 8);
        let mut value = 0u64;
        for index in 0..first {
            value |= (self.data[byte + index] as u64) << (8 * index);
        }
        value >>= bit;
        if count > 64 - bit && available_bytes > 8 {
            value |= (self.data[byte + 8] as u64) << (64 - bit);
        }
        if count < 64 {
            value &= (1u64 << count) - 1;
        }
        self.position += count;
        Ok(value)
    }

    fn unary(&mut self) -> Result<u64> {
        let mut count = 0u64;
        while self.position < self.bits {
            let byte = self.position >> 3;
            let bit = self.position & 7;
            let remaining = self.bits - self.position;
            let available = min(remaining, 64 - bit);
            let bytes = ((self.bits + 7) >> 3) - byte;
            let take = min(bytes, 8);
            let mut word = 0u64;
            for index in 0..take {
                word |= (self.data[byte + index] as u64) << (8 * index);
            }
            word >>= bit;
            if available < 64 {
                word &= (1u64 << available) - 1;
            }
            if word != 0 {
                let zeroes = word.trailing_zeros() as usize;
                count += zeroes as u64;
                self.position += zeroes + 1;
                return Ok(count);
            }
            count += available as u64;
            self.position += available;
        }
        Err(invalid("V2 unary code is truncated"))
    }
}

struct Reader {
    index: Mmap,
    info: TableInfo,
    shards: u32,
    prefix_offset: usize,
    low_offset: usize,
    shard_first: Vec<u64>,
    shard_min: Vec<u64>,
    shard_files: Vec<File>,
}

impl Reader {
    fn open(data_base: PathBuf, index_path: &Path, preload: bool, lock: bool) -> Result<Self> {
        let index_file =
            File::open(index_path).map_err(|error| io_error("cannot open V2 index", error))?;
        let index_bytes = index_file
            .metadata()
            .map_err(|error| io_error("cannot stat V2 index", error))?
            .len();
        if index_bytes < TABLE_HEADER_BYTES as u64 || index_bytes > usize::MAX as u64 {
            return Err(invalid("invalid V2 index size"));
        }
        let index = unsafe { MmapOptions::new().map(&index_file) }
            .map_err(|error| io_error("cannot map V2 index", error))?;
        let index_locked = if lock {
            #[cfg(unix)]
            {
                index.lock().map_err(|error| {
                    io_error(
                        format!("cannot lock the complete {index_bytes}-byte V2 index in RAM"),
                        error,
                    )
                })?;
                true
            }
            #[cfg(not(unix))]
            {
                return Err(invalid(
                    "locking the V2 index in RAM is only supported on Unix",
                ));
            }
        } else {
            false
        };
        if preload && !index_locked {
            let mut touched = 0u8;
            for offset in (0..index.len()).step_by(4096) {
                touched ^= index[offset];
            }
            if let Some(last) = index.last() {
                touched ^= *last;
            }
            black_box(touched);
        }
        if bytes(&index, 0, 8)? != b"GIDX0002"
            || read_le_u32(&index, 8)? != TABLE_VERSION
            || read_le_u32(&index, 12)? != TABLE_HEADER_BYTES as u32
        {
            return Err(invalid("invalid GIDX0002 header"));
        }

        let records = read_le_u64(&index, 16)?;
        let parts = read_le_u32(&index, 24)?;
        let start_bits = read_le_u32(&index, 32)?;
        let rice_k = read_le_u32(&index, 36)?;
        let flags = read_le_u32(&index, 40)?;
        let blocks = read_le_u64(&index, 48)?;
        let records_per_part = read_le_u64(&index, 64)?;
        let min_endpoint = read_le_u64(&index, 72)?;
        let max_endpoint = read_le_u64(&index, 80)?;
        let fingerprint = read_le_u64(&index, 96)?;
        let shards = read_le_u32(&index, 132)?;
        let shard_dir_offset = read_le_u64(&index, 152)? as usize;
        let prefix_offset = read_le_u64(&index, 160)? as usize;
        let low_offset = read_le_u64(&index, 168)? as usize;
        let expected_index_bytes = (low_offset as u64)
            .checked_add(
                blocks
                    .checked_mul(4)
                    .ok_or_else(|| invalid("V2 index size overflow"))?,
            )
            .ok_or_else(|| invalid("V2 index size overflow"))?;
        if flags & REQUIRED_FLAGS != REQUIRED_FLAGS
            || read_le_u32(&index, 28)? != PAGE_BYTES as u32
            || read_le_u32(&index, 44)? != 4
            || read_le_u32(&index, 176)? != COUNT_BYTES as u32
            || read_le_u32(&index, 180)? != 24
            || records == 0
            || blocks == 0
            || parts == 0
            || shards == 0
            || shards > 4096
            || rice_k != 16
            || start_bits != bits_required(records - 1)
            || shard_dir_offset != TABLE_HEADER_BYTES
            || prefix_offset != TABLE_HEADER_BYTES + (shards as usize + 1) * 8
            || low_offset != prefix_offset + PREFIX_ENTRIES * 4
            || expected_index_bytes != index_bytes
        {
            return Err(invalid("inconsistent GIDX0002 header/layout"));
        }

        let mut previous_prefix_page = 0u32;
        for prefix in 0..PREFIX_ENTRIES {
            let value = read_le_u32(&index, prefix_offset + prefix * 4)?;
            if value < previous_prefix_page || value as u64 > blocks {
                return Err(invalid("invalid GIDX0002 prefix directory"));
            }
            previous_prefix_page = value;
        }
        if previous_prefix_page as u64 != blocks {
            return Err(invalid("invalid GIDX0002 prefix sentinel"));
        }

        let mut shard_first = Vec::with_capacity(shards as usize + 1);
        for shard in 0..=shards as usize {
            shard_first.push(read_le_u64(&index, shard_dir_offset + shard * 8)?);
        }
        if shard_first[0] != 0 || shard_first[shards as usize] != blocks {
            return Err(invalid("invalid V2 shard page directory"));
        }
        if shard_first
            .windows(2)
            .any(|pair| pair[0] > pair[1] || pair[1] > blocks)
        {
            return Err(invalid("non-monotonic V2 shard page directory"));
        }

        let mut shard_min = Vec::with_capacity(shards as usize);
        let mut shard_files = Vec::with_capacity(shards as usize);
        let mut seen_records = 0u64;
        let mut seen_pages = 0u64;
        let mut data_bytes = 0u64;
        for shard in 0..shards {
            let path = shard_path(&data_base, shard);
            let file = File::open(&path)
                .map_err(|error| io_error(format!("cannot open V2 shard {shard}"), error))?;
            let file_bytes = file
                .metadata()
                .map_err(|error| io_error(format!("cannot stat V2 shard {shard}"), error))?
                .len();
            let mut header = [0u8; TABLE_HEADER_BYTES];
            read_exact_at(&file, &mut header, 0)
                .map_err(|error| io_error(format!("read V2 shard header {shard}"), error))?;
            let shard_pages = shard_first[shard as usize + 1] - shard_first[shard as usize];
            let expected_bytes = TABLE_HEADER_BYTES as u64
                + shard_pages
                    .checked_mul(PAGE_BYTES as u64)
                    .ok_or_else(|| invalid("V2 shard size overflow"))?;
            if bytes(&header, 0, 8)? != b"GRTB0002"
                || read_le_u32(&header, 8)? != TABLE_VERSION
                || read_le_u32(&header, 40)? & REQUIRED_FLAGS != REQUIRED_FLAGS
                || read_le_u64(&header, 16)? != records
                || read_le_u32(&header, 32)? != start_bits
                || read_le_u32(&header, 36)? != rice_k
                || read_le_u64(&header, 48)? != shard_pages
                || read_le_u64(&header, 56)? != file_bytes
                || file_bytes != expected_bytes
                || read_le_u64(&header, 96)? != fingerprint
                || read_le_u32(&header, 128)? != shard
                || read_le_u32(&header, 132)? != shards
                || read_le_u64(&header, 136)? != seen_records
            {
                return Err(invalid(format!("inconsistent V2 shard header {shard}")));
            }
            shard_min.push(read_le_u64(&header, 72)?);
            seen_records = seen_records
                .checked_add(read_le_u64(&header, 144)?)
                .ok_or_else(|| invalid("V2 record total overflow"))?;
            seen_pages += shard_pages;
            data_bytes = data_bytes
                .checked_add(file_bytes)
                .ok_or_else(|| invalid("V2 data size overflow"))?;
            shard_files.push(file);
        }
        if seen_records != records || seen_pages != blocks {
            return Err(invalid("V2 shard collection totals mismatch"));
        }

        Ok(Self {
            index,
            info: TableInfo {
                records,
                blocks,
                data_bytes,
                index_bytes,
                index_locked,
                records_per_part,
                parts,
                start_bits,
                rice_k,
                min_endpoint,
                max_endpoint,
            },
            shards,
            prefix_offset,
            low_offset,
            shard_first,
            shard_min,
            shard_files,
        })
    }

    fn prepare_encoded(
        &self,
        query_data: Vec<u8>,
        control: &LookupControl,
    ) -> Result<PreparedLookup> {
        if query_data.is_empty() || !query_data.len().is_multiple_of(INTERNAL_QUERY_BYTES) {
            return Err(invalid(
                "query buffer must contain complete 24-byte records",
            ));
        }
        control.processed.store(0, Ordering::Release);
        let query_count = query_data.len() / INTERNAL_QUERY_BYTES;
        let mut work = Vec::with_capacity(query_count + 16);
        for tag in 0..query_count {
            if tag & 0x1fff == 0 {
                check_cancelled(control)?;
            }
            let endpoint = read_le_u64(&query_data, tag * INTERNAL_QUERY_BYTES + 16)?;
            let mut page = self.lower_page(endpoint);
            if page >= self.info.blocks {
                continue;
            }
            loop {
                work.push(Work {
                    endpoint,
                    tag,
                    page,
                });
                if page + 1 >= self.info.blocks || self.index_endpoint(page) != endpoint {
                    break;
                }
                page += 1;
            }
        }
        check_cancelled(control)?;
        work.sort_unstable_by_key(|item| (item.page, item.endpoint, item.tag));
        check_cancelled(control)?;

        let mut page_offsets = Vec::with_capacity(work.len().saturating_add(1));
        if !work.is_empty() {
            page_offsets.push(0);
            for index in 1..work.len() {
                if work[index - 1].page != work[index].page {
                    page_offsets.push(index as u32);
                }
            }
            page_offsets.push(work.len() as u32);
        }
        Ok(PreparedLookup {
            query_data,
            work,
            page_offsets,
            query_count,
        })
    }

    fn lookup_prepared(
        &self,
        prepared: &PreparedLookup,
        read_workers: usize,
        control: &LookupControl,
    ) -> Result<(usize, Vec<u8>)> {
        if read_workers == 0 {
            return Err(invalid("lookup read worker count must be positive"));
        }
        check_cancelled(control)?;
        control.processed.store(0, Ordering::Release);
        let page_count = prepared.page_count();
        if page_count == 0 {
            control
                .processed
                .store(prepared.query_count as u64, Ordering::Release);
            return Ok((0, Vec::new()));
        }

        let worker_count = min(read_workers, page_count);
        let next_page = AtomicUsize::new(0);
        let completed_pages = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let first_error = Mutex::new(None::<LocalLookupError>);
        let (sender, receiver) = mpsc::channel();

        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let sender = sender.clone();
                let next_page = &next_page;
                let completed_pages = &completed_pages;
                let failed = &failed;
                let first_error = &first_error;
                scope.spawn(move || {
                    let mut output = Vec::new();
                    while !failed.load(Ordering::Acquire) {
                        if control.is_cancelled() {
                            failed.store(true, Ordering::Release);
                            set_first_error(first_error, invalid("lookup cancelled"));
                            break;
                        }
                        let group = next_page.fetch_add(1, Ordering::Relaxed);
                        if group >= page_count {
                            break;
                        }
                        let start = prepared.page_offsets[group] as usize;
                        let stop = prepared.page_offsets[group + 1] as usize;
                        if let Err(error) = self.process_page(
                            &prepared.query_data,
                            &prepared.work[start..stop],
                            &mut output,
                        ) {
                            failed.store(true, Ordering::Release);
                            set_first_error(first_error, error);
                            break;
                        }
                        let completed = completed_pages.fetch_add(1, Ordering::Relaxed) + 1;
                        if completed & 0x3f == 0 || completed == page_count {
                            let processed = (completed as u64)
                                .saturating_mul(prepared.query_count as u64)
                                / page_count as u64;
                            control.processed.store(processed, Ordering::Release);
                        }
                    }
                    let _ = sender.send(output);
                });
            }
        });
        drop(sender);

        let mut output = Vec::new();
        for worker_output in receiver {
            output.extend_from_slice(&worker_output);
        }
        if let Some(error) = first_error
            .into_inner()
            .map_err(|_| invalid("lookup error lock is poisoned"))?
        {
            return Err(error);
        }
        check_cancelled(control)?;
        control
            .processed
            .store(prepared.query_count as u64, Ordering::Release);
        Ok((output.len() / INTERNAL_MATCH_BYTES, output))
    }

    fn process_page(&self, query_data: &[u8], work: &[Work], output: &mut Vec<u8>) -> Result<()> {
        let page = work
            .first()
            .ok_or_else(|| invalid("empty V2 page work group"))?
            .page;
        let mut page_data = [0u8; PAGE_BYTES];
        let decoded = self.decode_page(page, &mut page_data)?;
        let mut decoded_starts: Option<Vec<u64>> = None;
        let mut at = 0usize;
        while at < work.len() {
            let endpoint = work[at].endpoint;
            let mut same_stop = at + 1;
            while same_stop < work.len() && work[same_stop].endpoint == endpoint {
                same_stop += 1;
            }
            let mut row =
                decoded.endpoints[..decoded.count].partition_point(|value| *value < endpoint);
            if row < decoded.count && decoded.endpoints[row] == endpoint && decoded_starts.is_none()
            {
                decoded_starts = Some(self.load_starts(&page_data, &decoded)?);
            }
            while row < decoded.count && decoded.endpoints[row] == endpoint {
                let starts = decoded_starts
                    .as_ref()
                    .ok_or_else(|| invalid("missing decoded V2 starts"))?;
                for copy in &work[at..same_stop] {
                    let token_offset = copy.tag * INTERNAL_QUERY_BYTES;
                    output.extend_from_slice(&query_data[token_offset..token_offset + 16]);
                    output.extend_from_slice(&starts[row].to_le_bytes());
                }
                row += 1;
            }
            at = same_stop;
        }
        Ok(())
    }

    fn prefix_page(&self, prefix: u32) -> u32 {
        let offset = self.prefix_offset + prefix as usize * 4;
        u32::from_le_bytes(self.index[offset..offset + 4].try_into().unwrap())
    }

    fn endpoint_low(&self, page: u64) -> u32 {
        let offset = self.low_offset + page as usize * 4;
        u32::from_le_bytes(self.index[offset..offset + 4].try_into().unwrap())
    }

    fn index_endpoint(&self, page: u64) -> u64 {
        let mut left = 0u32;
        let mut right = 1u32 << 24;
        while left < right {
            let middle = left + (right - left).div_ceil(2);
            if self.prefix_page(middle) as u64 <= page {
                left = middle;
            } else {
                right = middle - 1;
            }
        }
        ((left as u64) << 32) | self.endpoint_low(page) as u64
    }

    fn lower_page(&self, endpoint: u64) -> u64 {
        if endpoint <= self.info.min_endpoint {
            return 0;
        }
        if endpoint > self.info.max_endpoint || endpoint >> 32 >= 1 << 24 {
            return self.info.blocks;
        }
        let high = (endpoint >> 32) as u32;
        let low = endpoint as u32;
        let mut left = self.prefix_page(high) as u64;
        let mut right = self.prefix_page(high + 1) as u64;
        while left < right {
            let middle = left + (right - left) / 2;
            if self.endpoint_low(middle) < low {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        left
    }

    fn page_shard(&self, page: u64) -> u32 {
        let mut left = 0u32;
        let mut right = self.shards;
        while left + 1 < right {
            let middle = left + (right - left) / 2;
            if self.shard_first[middle as usize] <= page {
                left = middle;
            } else {
                right = middle;
            }
        }
        left
    }

    fn decode_page(&self, page: u64, page_data: &mut [u8; PAGE_BYTES]) -> Result<DecodedPage> {
        let shard = self.page_shard(page);
        let local_page = page - self.shard_first[shard as usize];
        let offset = TABLE_HEADER_BYTES as u64 + local_page * PAGE_BYTES as u64;
        read_exact_at(&self.shard_files[shard as usize], page_data, offset)
            .map_err(|error| io_error("read V2 lookup page", error))?;
        let count = u16::from_le_bytes(page_data[..2].try_into().unwrap()) as usize;
        if count == 0 || count > 1024 {
            return Err(invalid("invalid V2 lookup page count"));
        }
        let mut endpoints = [0u64; 1024];
        let mut first_delta = 0usize;
        let mut endpoint = if local_page == 0 {
            let seed = self.shard_min[shard as usize];
            endpoints[0] = seed;
            first_delta = 1;
            seed
        } else {
            self.index_endpoint(page - 1)
        };
        let expected = self.index_endpoint(page);
        let mut bits = BitReader::new(&page_data[COUNT_BYTES..], 0);
        for slot in endpoints.iter_mut().take(count).skip(first_delta) {
            let quotient = bits.unary()?;
            let remainder = bits.read_bits(self.info.rice_k)?;
            if quotient > (u64::MAX >> self.info.rice_k) {
                return Err(invalid("V2 Rice quotient overflow"));
            }
            let delta = (quotient << self.info.rice_k) | remainder;
            endpoint = endpoint
                .checked_add(delta)
                .ok_or_else(|| invalid("V2 endpoint overflow"))?;
            *slot = endpoint;
        }
        if endpoint != expected {
            return Err(invalid("V2 page/index endpoint mismatch"));
        }
        Ok(DecodedPage {
            count,
            start_bit: bits.position,
            endpoints,
        })
    }

    fn load_starts(&self, page_data: &[u8; PAGE_BYTES], decoded: &DecodedPage) -> Result<Vec<u64>> {
        let mut bits = BitReader::new(&page_data[COUNT_BYTES..], decoded.start_bit);
        let mut starts = Vec::with_capacity(decoded.count);
        for _ in 0..decoded.count {
            let start = bits.read_bits(self.info.start_bits)?;
            if start >= self.info.records {
                return Err(invalid("V2 start outside collection"));
            }
            starts.push(start);
        }
        Ok(starts)
    }
}

fn set_first_error(target: &Mutex<Option<LocalLookupError>>, error: LocalLookupError) {
    if let Ok(mut slot) = target.lock()
        && slot.is_none()
    {
        *slot = Some(error);
    }
}

fn check_cancelled(control: &LookupControl) -> Result<()> {
    if control.is_cancelled() {
        Err(invalid("lookup cancelled"))
    } else {
        Ok(())
    }
}

fn shard_path(base: &Path, shard: u32) -> PathBuf {
    let raw = base.to_string_lossy();
    let stem = raw.strip_suffix(".grtb").unwrap_or(&raw);
    PathBuf::from(format!("{stem}.{shard:04}.grtb"))
}

fn bits_required(mut value: u64) -> u32 {
    let mut bits = 0;
    loop {
        bits += 1;
        value >>= 1;
        if value == 0 {
            return bits;
        }
    }
}

fn bytes(data: &[u8], offset: usize, count: usize) -> Result<&[u8]> {
    data.get(offset..offset.saturating_add(count))
        .ok_or_else(|| invalid("truncated V2 header/index"))
}

fn read_le_u32(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes(data, offset, 4)?.try_into().unwrap(),
    ))
}

fn read_le_u64(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes(data, offset, 8)?.try_into().unwrap(),
    ))
}

fn read_exact_at(file: &File, mut output: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !output.is_empty() {
        #[cfg(unix)]
        let read = file.read_at(output, offset)?;
        #[cfg(windows)]
        let read = file.seek_read(output, offset)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "unexpected EOF",
            ));
        }
        output = &mut output[read..];
        offset += read as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint_file(endpoints: &[u64]) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(ENDPOINT_MAGIC);
        output.extend_from_slice(&1u32.to_le_bytes());
        output.extend_from_slice(&8u32.to_le_bytes());
        output.extend_from_slice(&(endpoints.len() as u64).to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());
        for endpoint in endpoints {
            output.extend_from_slice(&endpoint.to_le_bytes());
        }
        output
    }

    #[test]
    fn parses_ordered_endpoint_file() {
        assert_eq!(
            parse_endpoint_file(&endpoint_file(&[1, u64::MAX])).unwrap(),
            vec![1, u64::MAX]
        );
    }

    #[test]
    fn rejects_bad_endpoint_size_and_flags() {
        let mut truncated = endpoint_file(&[1]);
        truncated.pop();
        assert!(parse_endpoint_file(&truncated).is_err());

        let mut flagged = endpoint_file(&[1]);
        flagged[24] = 1;
        assert!(parse_endpoint_file(&flagged).is_err());
    }

    #[test]
    fn candidate_file_preserves_duplicates() {
        let records = vec![
            CandidateRecord {
                ordinal: 0,
                start: 5,
            },
            CandidateRecord {
                ordinal: 0,
                start: 7,
            },
            CandidateRecord {
                ordinal: 1,
                start: 9,
            },
        ];
        let encoded = encode_candidate_file(2, &records).unwrap();
        assert_eq!(validate_candidate_file(&encoded, Some(2)).unwrap(), 3);
        assert_eq!(&encoded[..8], CANDIDATE_MAGIC);
        assert_eq!(read_le_u64(&encoded, 24).unwrap(), 3);
    }

    #[test]
    fn rejects_candidate_ordinal_outside_batch() {
        let error = encode_candidate_file(
            1,
            &[CandidateRecord {
                ordinal: 1,
                start: 0,
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("ordinal"));
    }

    #[test]
    fn cancellation_state_is_thread_safe() {
        let control = LookupControl::default();
        assert!(!control.is_cancelled());
        control.cancel();
        assert!(control.is_cancelled());
        assert!(check_cancelled(&control).is_err());
    }
}
