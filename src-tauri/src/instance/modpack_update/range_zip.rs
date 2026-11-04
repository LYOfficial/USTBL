//! Reading a zip archive through byte ranges.
//!
//! A modpack preview only needs the central directory and a handful of
//! entries of an archive that is a hundred megabytes large. [`RangeSource`]
//! says how bytes at an offset are obtained (an HTTP Range request, a local
//! file); [`read_listing`] finds the central directory from the tail of the
//! archive, and [`ZipListing::read_entry`] streams one entry out of it,
//! checking its size and CRC32.
//!
//! Anything unexpected (a server that ignores `Range`, a total length that
//! changes between answers, an archive with prepended data, an unsupported
//! compression method) is an error. Callers fall back to downloading the whole
//! archive, so nothing here tries to be lenient.

// Wired into the update preview by the commits that follow.
use super::diff::FileKey;
use async_trait::async_trait;
use flate2::{Decompress, FlushDecompress, Status};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::fmt;
use std::sync::Mutex;
use tauri_plugin_http::reqwest;
use url::Url;

/// How much of the end of the archive the first request asks for. The end of
/// central directory record is at most 22 bytes plus a 65535 byte comment; the
/// rest of this is room for the zip64 records and, for small archives, the
/// whole central directory.
pub const TAIL_READ: u64 = 128 * 1024;

/// Compressed bytes requested at a time while an entry is streamed.
const CHUNK: u64 = 4 * 1024 * 1024;

/// Sanity limits on what an archive may claim about its central directory.
const MAX_ENTRIES: u64 = 500_000;
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;

const EOCD_LEN: usize = 22;
const ZIP64_LOCATOR_LEN: usize = 20;
const ZIP64_EOCD_LEN: usize = 56;
const CENTRAL_LEN: usize = 46;
const LOCAL_LEN: usize = 30;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

const FLAG_ENCRYPTED: u16 = 1;
const FLAG_UTF8: u16 = 1 << 11;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeError(pub String);

impl fmt::Display for RangeError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl std::error::Error for RangeError {}

type RangeResult<T> = Result<T, RangeError>;

fn fail<T>(message: impl Into<String>) -> RangeResult<T> {
  Err(RangeError(message.into()))
}

/// The last bytes of an object and the object's whole length.
pub struct Tail {
  pub total: u64,
  /// The final `min(requested, total)` bytes.
  pub bytes: Vec<u8>,
}

/// Where the bytes of an archive come from.
#[async_trait]
pub trait RangeSource: Send + Sync {
  /// The last `len` bytes, or all of them for a shorter object.
  async fn tail(&self, len: u64) -> RangeResult<Tail>;
  /// Exactly `len` bytes at `offset`. Reading past the end is an error.
  async fn read(&self, offset: u64, len: u64) -> RangeResult<Vec<u8>>;
}

// ---- local file --------------------------------------------------------------

/// A file on disk, for an archive that was downloaded already.
pub struct LocalSource {
  path: std::path::PathBuf,
}

impl LocalSource {
  pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
    Self { path: path.into() }
  }

  fn read_blocking(path: &std::path::Path, offset: u64, len: u64) -> RangeResult<(u64, Vec<u8>)> {
    use std::io::{Read, Seek, SeekFrom};
    let io = |error: std::io::Error| RangeError(format!("cannot read {}: {error}", path.display()));
    let mut file = std::fs::File::open(path).map_err(io)?;
    let total = file.metadata().map_err(io)?.len();
    let end = offset
      .checked_add(len)
      .filter(|end| *end <= total)
      .ok_or_else(|| RangeError(format!("range {offset}+{len} is past the end ({total})")))?;
    file.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut bytes = vec![0u8; (end - offset) as usize];
    file.read_exact(&mut bytes).map_err(io)?;
    Ok((total, bytes))
  }
}

#[async_trait]
impl RangeSource for LocalSource {
  async fn tail(&self, len: u64) -> RangeResult<Tail> {
    let path = self.path.clone();
    tokio::task::spawn_blocking(move || {
      let total = std::fs::metadata(&path)
        .map_err(|error| RangeError(format!("cannot stat {}: {error}", path.display())))?
        .len();
      let take = len.min(total);
      let (_, bytes) = Self::read_blocking(&path, total - take, take)?;
      Ok(Tail { total, bytes })
    })
    .await
    .map_err(|error| RangeError(error.to_string()))?
  }

  async fn read(&self, offset: u64, len: u64) -> RangeResult<Vec<u8>> {
    let path = self.path.clone();
    tokio::task::spawn_blocking(move || Self::read_blocking(&path, offset, len).map(|(_, b)| b))
      .await
      .map_err(|error| RangeError(error.to_string()))?
  }
}

// ---- HTTP --------------------------------------------------------------------

/// `Content-Range: bytes <first>-<last>/<total>`.
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
  let rest = value.trim().strip_prefix("bytes ")?;
  let (range, total) = rest.split_once('/')?;
  let (first, last) = range.split_once('-')?;
  Some((
    first.trim().parse().ok()?,
    last.trim().parse().ok()?,
    total.trim().parse().ok()?,
  ))
}

/// An archive behind one or more equivalent URLs. The first URL that answers a
/// Range request properly is used from then on; the rest are only tried while
/// none has.
pub struct HttpSource {
  client: reqwest::Client,
  urls: Vec<Url>,
  state: Mutex<HttpState>,
}

struct HttpState {
  pinned: Option<Url>,
  total: Option<u64>,
}

impl HttpSource {
  /// `known_total` is the length of the archive when an earlier visit saw it;
  /// every answer must then agree.
  pub fn new(client: reqwest::Client, urls: Vec<Url>, known_total: Option<u64>) -> Self {
    Self {
      client,
      urls,
      state: Mutex::new(HttpState {
        pinned: None,
        total: known_total,
      }),
    }
  }

  /// The URL that served the ranges, once one did.
  pub fn pinned(&self) -> Option<Url> {
    self.state.lock().ok()?.pinned.clone()
  }

  fn candidates(&self) -> Vec<Url> {
    match self
      .state
      .lock()
      .ok()
      .and_then(|state| state.pinned.clone())
    {
      Some(url) => vec![url],
      None => self.urls.clone(),
    }
  }

  fn note_total(&self, url: &Url, total: u64) -> RangeResult<()> {
    let mut state = self
      .state
      .lock()
      .map_err(|_| RangeError("range state poisoned".into()))?;
    match state.total {
      Some(known) if known != total => {
        return fail(format!(
          "{url} reports {total} bytes in total, expected {known}"
        ))
      }
      _ => state.total = Some(total),
    }
    state.pinned = Some(url.clone());
    Ok(())
  }

  /// Ask `url` for `range` and return the answer's body and total length.
  /// `expect` checks the start and end the server says it sent.
  async fn fetch(
    &self,
    url: &Url,
    range: String,
    expect: impl Fn(u64, u64, u64) -> bool,
  ) -> RangeResult<(u64, Vec<u8>)> {
    let response = self
      .client
      .get(url.clone())
      .header(reqwest::header::RANGE, range)
      .send()
      .await
      .map_err(|error| RangeError(format!("{url}: {error}")))?;
    if response.status().as_u16() != 206 {
      return fail(format!(
        "{url} answered {} to a Range request",
        response.status()
      ));
    }
    let (first, last, total) = response
      .headers()
      .get(reqwest::header::CONTENT_RANGE)
      .and_then(|value| value.to_str().ok())
      .and_then(parse_content_range)
      .ok_or_else(|| RangeError(format!("{url} sent no usable Content-Range")))?;
    if last < first || last >= total || !expect(first, last, total) {
      return fail(format!(
        "{url} answered with bytes {first}-{last}/{total}, not the range asked for"
      ));
    }
    let body = response
      .bytes()
      .await
      .map_err(|error| RangeError(format!("{url}: {error}")))?;
    if body.len() as u64 != last - first + 1 {
      return fail(format!(
        "{url} sent {} bytes for {first}-{last}",
        body.len()
      ));
    }
    self.note_total(url, total)?;
    Ok((total, body.to_vec()))
  }

  /// The last `len` bytes by an explicit range: some CDNs refuse a suffix
  /// range (`bytes=-N`) with 501. The total length comes from an earlier
  /// answer, or from a one-byte probe.
  async fn tail_from(&self, url: &Url, len: u64) -> RangeResult<Tail> {
    let known = self.state.lock().ok().and_then(|state| state.total);
    let total = match known {
      Some(total) => total,
      None => {
        self
          .fetch(url, "bytes=0-0".into(), |first, last, _| {
            first == 0 && last == 0
          })
          .await?
          .0
      }
    };
    if total == 0 {
      return fail(format!("{url} is empty"));
    }
    let start = total.saturating_sub(len);
    let end = total - 1;
    let (answered, bytes) = self
      .fetch(url, format!("bytes={start}-{end}"), |first, last, _| {
        first == start && last == end
      })
      .await?;
    if answered != total {
      return fail(format!(
        "{url} reports {answered} bytes in total, expected {total}"
      ));
    }
    Ok(Tail { total, bytes })
  }
}

#[async_trait]
impl RangeSource for HttpSource {
  async fn tail(&self, len: u64) -> RangeResult<Tail> {
    if len == 0 {
      return fail("empty tail");
    }
    let mut last_error = RangeError("no source".into());
    for url in self.candidates() {
      match self.tail_from(&url, len).await {
        Ok(tail) => return Ok(tail),
        Err(error) => {
          log::info!("range tail failed: {error}");
          last_error = error;
        }
      }
    }
    Err(last_error)
  }

  async fn read(&self, offset: u64, len: u64) -> RangeResult<Vec<u8>> {
    if len == 0 {
      return Ok(Vec::new());
    }
    let end = offset + len - 1;
    let mut last_error = RangeError("no source".into());
    for url in self.candidates() {
      match self
        .fetch(&url, format!("bytes={offset}-{end}"), |first, last, _| {
          first == offset && last == end
        })
        .await
      {
        Ok((_, bytes)) => return Ok(bytes),
        Err(error) => {
          log::info!("range read failed: {error}");
          last_error = error;
        }
      }
    }
    Err(last_error)
  }
}

// ---- the central directory -----------------------------------------------------

/// One entry of the central directory, as the archive states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipEntry {
  pub name: String,
  pub crc32: u32,
  pub size: u64,
  pub compressed_size: u64,
  /// Offset of the local file header.
  pub header_offset: u64,
  pub method: u16,
  pub flags: u16,
  pub made_by: u16,
  pub external_attributes: u32,
}

impl ZipEntry {
  pub fn is_dir(&self) -> bool {
    self.name.ends_with('/') || self.name.ends_with('\\')
  }

  pub fn is_encrypted(&self) -> bool {
    self.flags & FLAG_ENCRYPTED != 0
  }

  /// The Unix mode, when the archive was made on a Unix system.
  pub fn unix_mode(&self) -> Option<u32> {
    (self.made_by >> 8 == 3).then_some(self.external_attributes >> 16)
  }

  pub fn is_symlink(&self) -> bool {
    self
      .unix_mode()
      .is_some_and(|mode| mode & 0o170000 == 0o120000)
  }
}

/// Everything the central directory says about an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipListing {
  /// Length of the whole archive.
  pub total_len: u64,
  /// Where the central directory starts; entry data ends before it.
  pub directory_offset: u64,
  pub entries: Vec<ZipEntry>,
}

impl ZipListing {
  pub fn find(&self, name: &str) -> Option<&ZipEntry> {
    self.entries.iter().find(|entry| entry.name == name)
  }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
  u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
  u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
  let mut raw = [0u8; 8];
  raw.copy_from_slice(&bytes[at..at + 8]);
  u64::from_le_bytes(raw)
}

/// The end of central directory record: the last signature whose comment
/// length runs exactly to the end of the data.
fn find_eocd(tail: &[u8]) -> Option<usize> {
  if tail.len() < EOCD_LEN {
    return None;
  }
  let lowest = tail.len().saturating_sub(EOCD_LEN + u16::MAX as usize);
  (lowest..=tail.len() - EOCD_LEN).rev().find(|&at| {
    u32_at(tail, at) == EOCD_SIGNATURE
      && at + EOCD_LEN + u16_at(tail, at + 20) as usize == tail.len()
  })
}

struct Directory {
  offset: u64,
  size: u64,
  entries: u64,
  /// Absolute position of the record that follows the directory.
  end: u64,
}

/// Locate the central directory from the tail of the archive.
async fn find_directory(source: &dyn RangeSource, tail: &Tail) -> RangeResult<Directory> {
  let tail_start = tail.total - tail.bytes.len() as u64;
  let at = find_eocd(&tail.bytes)
    .ok_or_else(|| RangeError("no end of central directory record".into()))?;
  let eocd = &tail.bytes[at..];
  if u16_at(eocd, 4) != 0 || u16_at(eocd, 6) != 0 {
    return fail("multi-disk archives are not supported");
  }
  let (entries, size, offset) = (
    u16_at(eocd, 10) as u64,
    u32_at(eocd, 12) as u64,
    u32_at(eocd, 16) as u64,
  );
  let eocd_position = tail_start + at as u64;
  let wants_zip64 = u16_at(eocd, 10) == u16::MAX
    || u16_at(eocd, 8) == u16::MAX
    || u32_at(eocd, 12) == u32::MAX
    || u32_at(eocd, 16) == u32::MAX;
  if !wants_zip64 {
    return Ok(Directory {
      offset,
      size,
      entries,
      end: eocd_position,
    });
  }
  if at < ZIP64_LOCATOR_LEN {
    return fail("zip64 locator is missing");
  }
  let locator = &tail.bytes[at - ZIP64_LOCATOR_LEN..at];
  if u32_at(locator, 0) != ZIP64_LOCATOR_SIGNATURE || u32_at(locator, 4) != 0 {
    return fail("zip64 locator is missing");
  }
  let record_position = u64_at(locator, 8);
  if record_position.saturating_add(ZIP64_EOCD_LEN as u64) > eocd_position {
    return fail("zip64 end of central directory is out of range");
  }
  let record = if record_position >= tail_start {
    let from = (record_position - tail_start) as usize;
    tail.bytes[from..from + ZIP64_EOCD_LEN].to_vec()
  } else {
    source.read(record_position, ZIP64_EOCD_LEN as u64).await?
  };
  if record.len() < ZIP64_EOCD_LEN || u32_at(&record, 0) != ZIP64_EOCD_SIGNATURE {
    return fail("zip64 end of central directory is missing");
  }
  if u32_at(&record, 16) != 0
    || u32_at(&record, 20) != 0
    || u64_at(&record, 24) != u64_at(&record, 32)
  {
    return fail("multi-disk archives are not supported");
  }
  Ok(Directory {
    entries: u64_at(&record, 32),
    size: u64_at(&record, 40),
    offset: u64_at(&record, 48),
    end: record_position,
  })
}

/// The extra field `id`, if the entry has one.
fn extra_field(extra: &[u8], id: u16) -> Option<&[u8]> {
  let mut at = 0;
  while at + 4 <= extra.len() {
    let (field_id, len) = (u16_at(extra, at), u16_at(extra, at + 2) as usize);
    let body = extra.get(at + 4..at + 4 + len)?;
    if field_id == id {
      return Some(body);
    }
    at += 4 + len;
  }
  None
}

fn parse_directory(bytes: &[u8], count: u64) -> RangeResult<Vec<ZipEntry>> {
  let mut entries = Vec::with_capacity(count.min(MAX_ENTRIES) as usize);
  let mut seen = std::collections::HashSet::new();
  let mut at = 0usize;
  for _ in 0..count {
    let header = bytes
      .get(at..at + CENTRAL_LEN)
      .ok_or_else(|| RangeError("central directory is cut short".into()))?;
    if u32_at(header, 0) != CENTRAL_SIGNATURE {
      return fail("bad central directory entry");
    }
    let flags = u16_at(header, 8);
    let (name_len, extra_len, comment_len) = (
      u16_at(header, 28) as usize,
      u16_at(header, 30) as usize,
      u16_at(header, 32) as usize,
    );
    if u16_at(header, 34) != 0 {
      return fail("multi-disk archives are not supported");
    }
    let variable = bytes
      .get(at + CENTRAL_LEN..at + CENTRAL_LEN + name_len + extra_len + comment_len)
      .ok_or_else(|| RangeError("central directory is cut short".into()))?;
    let (name_bytes, rest) = variable.split_at(name_len);
    let extra = &rest[..extra_len];
    let name = std::str::from_utf8(name_bytes)
      .ok()
      // Without the UTF-8 flag the name is CP437 to readers that follow the
      // specification; only plain ASCII reads the same everywhere.
      .filter(|name| flags & FLAG_UTF8 != 0 || name.is_ascii())
      .ok_or_else(|| RangeError("entry name is not UTF-8".into()))?
      .to_string();
    let mut size = u32_at(header, 24) as u64;
    let mut compressed_size = u32_at(header, 20) as u64;
    let mut header_offset = u32_at(header, 42) as u64;
    if size == u32::MAX as u64
      || compressed_size == u32::MAX as u64
      || header_offset == u32::MAX as u64
    {
      let zip64 = extra_field(extra, 0x0001)
        .ok_or_else(|| RangeError(format!("{name}: zip64 values are missing")))?;
      let mut cursor = 0usize;
      let mut next = |wanted: bool, value: &mut u64| -> RangeResult<()> {
        if wanted {
          let raw = zip64
            .get(cursor..cursor + 8)
            .ok_or_else(|| RangeError(format!("{name}: zip64 extra field is cut short")))?;
          *value = u64_at(raw, 0);
          cursor += 8;
        }
        Ok(())
      };
      next(size == u32::MAX as u64, &mut size)?;
      next(compressed_size == u32::MAX as u64, &mut compressed_size)?;
      next(header_offset == u32::MAX as u64, &mut header_offset)?;
    }
    if !seen.insert(name.clone()) {
      return fail(format!("duplicate entry {name}"));
    }
    entries.push(ZipEntry {
      name,
      crc32: u32_at(header, 16),
      size,
      compressed_size,
      header_offset,
      method: u16_at(header, 10),
      flags,
      made_by: u16_at(header, 4),
      external_attributes: u32_at(header, 38),
    });
    at += CENTRAL_LEN + name_len + extra_len + comment_len;
  }
  Ok(entries)
}

/// Read the central directory of the archive behind `source`: one request for
/// the tail, and one more when the directory does not fit in it.
pub async fn read_listing(source: &dyn RangeSource) -> RangeResult<ZipListing> {
  let tail = source.tail(TAIL_READ).await?;
  if tail.bytes.len() as u64 > tail.total {
    return fail("tail is longer than the archive");
  }
  let directory = find_directory(source, &tail).await?;
  if directory.entries > MAX_ENTRIES || directory.size > MAX_DIRECTORY_BYTES {
    return fail("central directory is too large");
  }
  if directory.offset.checked_add(directory.size) != Some(directory.end) {
    // Prepended data or a gap: offsets in the entries would need adjusting.
    return fail("central directory does not end where the archive's records begin");
  }
  let tail_start = tail.total - tail.bytes.len() as u64;
  let bytes = if directory.offset >= tail_start {
    let from = (directory.offset - tail_start) as usize;
    tail.bytes[from..from + directory.size as usize].to_vec()
  } else {
    let bytes = source.read(directory.offset, directory.size).await?;
    if bytes.len() as u64 != directory.size {
      return fail("central directory read came back short");
    }
    bytes
  };
  let entries = parse_directory(&bytes, directory.entries)?;
  for entry in &entries {
    let data_end = entry
      .header_offset
      .checked_add((LOCAL_LEN + entry.name.len()) as u64)
      .and_then(|end| end.checked_add(entry.compressed_size));
    if data_end.is_none_or(|end| end > directory.offset) {
      return fail(format!("{} points outside the archive", entry.name));
    }
  }
  Ok(ZipListing {
    total_len: tail.total,
    directory_offset: directory.offset,
    entries,
  })
}

// ---- one entry -----------------------------------------------------------------

impl ZipListing {
  /// Stream the content of `entry` into `sink`, decompressed. The entry's size
  /// and CRC32 are verified once the last byte went through; until then `sink`
  /// may have seen bytes of an entry that turns out to be damaged.
  pub async fn read_entry(
    &self,
    source: &dyn RangeSource,
    entry: &ZipEntry,
    mut sink: impl FnMut(&[u8]) + Send,
  ) -> RangeResult<()> {
    if entry.is_encrypted() {
      return fail(format!("{} is encrypted", entry.name));
    }
    let name = &entry.name;
    let header = source
      .read(entry.header_offset, (LOCAL_LEN + entry.name.len()) as u64)
      .await?;
    if header.len() < LOCAL_LEN + entry.name.len() || u32_at(&header, 0) != LOCAL_SIGNATURE {
      return fail(format!("{name}: bad local file header"));
    }
    if u16_at(&header, 26) as usize != entry.name.len()
      || &header[LOCAL_LEN..LOCAL_LEN + entry.name.len()] != entry.name.as_bytes()
    {
      return fail(format!("{name}: local header names another entry"));
    }
    let data_start =
      entry.header_offset + (LOCAL_LEN + entry.name.len()) as u64 + u16_at(&header, 28) as u64;
    if data_start
      .checked_add(entry.compressed_size)
      .is_none_or(|end| end > self.directory_offset)
    {
      return fail(format!("{name}: data runs past the central directory"));
    }

    let mut crc = crc32fast::Hasher::new();
    let mut produced = 0u64;
    let mut emit = |bytes: &[u8]| -> RangeResult<()> {
      produced += bytes.len() as u64;
      if produced > entry.size {
        return fail(format!("{name}: more data than its declared size"));
      }
      crc.update(bytes);
      sink(bytes);
      Ok(())
    };
    match entry.method {
      METHOD_STORED => {
        if entry.size != entry.compressed_size {
          return fail(format!("{name}: stored entry with two sizes"));
        }
        let mut fetched = 0u64;
        while fetched < entry.compressed_size {
          let want = (entry.compressed_size - fetched).min(CHUNK);
          let bytes = source.read(data_start + fetched, want).await?;
          if bytes.len() as u64 != want {
            return fail(format!("{name}: data read came back short"));
          }
          emit(&bytes)?;
          fetched += want;
        }
      }
      METHOD_DEFLATE => {
        let mut inflater = Decompress::new(false);
        let mut out = vec![0u8; 64 * 1024];
        let mut fetched = 0u64;
        let mut finished = false;
        while !finished {
          let want = (entry.compressed_size - fetched).min(CHUNK);
          let input = if want > 0 {
            source.read(data_start + fetched, want).await?
          } else {
            Vec::new()
          };
          if input.len() as u64 != want {
            return fail(format!("{name}: data read came back short"));
          }
          fetched += want;
          let mut used_total = 0usize;
          loop {
            let (before_in, before_out) = (inflater.total_in(), inflater.total_out());
            let status = inflater
              .decompress(&input[used_total..], &mut out, FlushDecompress::None)
              .map_err(|error| RangeError(format!("{name}: {error}")))?;
            let used = (inflater.total_in() - before_in) as usize;
            let made = (inflater.total_out() - before_out) as usize;
            used_total += used;
            emit(&out[..made])?;
            if status == Status::StreamEnd {
              finished = true;
              break;
            }
            // Keep going while there is input left or the output buffer was
            // filled to the brim (more may be pending).
            if (used == 0 && made == 0) || (used_total >= input.len() && made < out.len()) {
              break;
            }
          }
          if !finished && fetched >= entry.compressed_size && used_total >= input.len() {
            return fail(format!("{name}: compressed data ends early"));
          }
        }
      }
      other => {
        return fail(format!(
          "{name}: compression method {other} is not supported"
        ))
      }
    }
    if produced != entry.size {
      return fail(format!("{name}: {produced} bytes, expected {}", entry.size));
    }
    if crc.finalize() != entry.crc32 {
      return fail(format!("{name}: CRC32 does not match"));
    }
    Ok(())
  }

  /// The decompressed content of an entry of at most `max` bytes.
  pub async fn read_entry_bytes(
    &self,
    source: &dyn RangeSource,
    entry: &ZipEntry,
    max: u64,
  ) -> RangeResult<Vec<u8>> {
    if entry.size > max {
      return fail(format!("{} is larger than {max} bytes", entry.name));
    }
    let mut content = Vec::with_capacity(entry.size as usize);
    self
      .read_entry(source, entry, |bytes| content.extend_from_slice(bytes))
      .await?;
    Ok(content)
  }

  /// SHA-1 and size of an entry's content, streamed.
  pub async fn hash_entry(
    &self,
    source: &dyn RangeSource,
    entry: &ZipEntry,
  ) -> RangeResult<FileKey> {
    let mut hasher = Sha1::new();
    self
      .read_entry(source, entry, |bytes| hasher.update(bytes))
      .await?;
    Ok(FileKey {
      sha1: hex::encode(hasher.finalize()),
      size: entry.size,
    })
  }
}

#[cfg(test)]
pub(crate) mod testing {
  //! A zip writer that lets tests choose the bytes the archive format leaves
  //! open: comments, zip64 records, local extra fields, Unix modes.

  use super::*;
  use flate2::write::DeflateEncoder;
  use flate2::Compression;
  use std::io::Write;
  use std::sync::atomic::{AtomicUsize, Ordering};

  pub struct TestEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub deflate: bool,
    /// Store the sizes and the offset in a zip64 extra field.
    pub zip64: bool,
    pub local_extra: usize,
    pub mode: u32,
  }

  impl TestEntry {
    pub fn new(name: &str, data: &[u8]) -> Self {
      Self {
        name: name.to_string(),
        data: data.to_vec(),
        deflate: true,
        zip64: false,
        local_extra: 0,
        mode: 0o100644,
      }
    }

    pub fn stored(mut self) -> Self {
      self.deflate = false;
      self
    }

    pub fn zip64(mut self) -> Self {
      self.zip64 = true;
      self
    }

    pub fn local_extra(mut self, len: usize) -> Self {
      self.local_extra = len;
      self
    }

    pub fn mode(mut self, mode: u32) -> Self {
      self.mode = mode;
      self
    }
  }

  pub fn build_zip(entries: &[TestEntry], comment: &[u8], zip64_records: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    for entry in entries {
      let compressed = if entry.deflate {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&entry.data).unwrap();
        encoder.finish().unwrap()
      } else {
        entry.data.clone()
      };
      let crc = crc32fast::hash(&entry.data);
      let method = if entry.deflate {
        METHOD_DEFLATE
      } else {
        METHOD_STORED
      };
      let offset = out.len() as u64;
      out.extend(LOCAL_SIGNATURE.to_le_bytes());
      out.extend(20u16.to_le_bytes());
      out.extend(FLAG_UTF8.to_le_bytes());
      out.extend(method.to_le_bytes());
      out.extend(0u16.to_le_bytes());
      out.extend(0x21u16.to_le_bytes());
      out.extend(crc.to_le_bytes());
      out.extend((compressed.len() as u32).to_le_bytes());
      out.extend((entry.data.len() as u32).to_le_bytes());
      out.extend((entry.name.len() as u16).to_le_bytes());
      out.extend((entry.local_extra as u16).to_le_bytes());
      out.extend(entry.name.as_bytes());
      out.extend(vec![0u8; entry.local_extra]);
      out.extend(&compressed);

      let mut extra = Vec::new();
      let (size_field, compressed_field, offset_field) = if entry.zip64 {
        extra.extend(1u16.to_le_bytes());
        extra.extend(24u16.to_le_bytes());
        extra.extend((entry.data.len() as u64).to_le_bytes());
        extra.extend((compressed.len() as u64).to_le_bytes());
        extra.extend(offset.to_le_bytes());
        (u32::MAX, u32::MAX, u32::MAX)
      } else {
        (
          entry.data.len() as u32,
          compressed.len() as u32,
          offset as u32,
        )
      };
      directory.extend(CENTRAL_SIGNATURE.to_le_bytes());
      directory.extend(((3u16 << 8) | 30).to_le_bytes());
      directory.extend(20u16.to_le_bytes());
      directory.extend(FLAG_UTF8.to_le_bytes());
      directory.extend(method.to_le_bytes());
      directory.extend(0u16.to_le_bytes());
      directory.extend(0x21u16.to_le_bytes());
      directory.extend(crc.to_le_bytes());
      directory.extend(compressed_field.to_le_bytes());
      directory.extend(size_field.to_le_bytes());
      directory.extend((entry.name.len() as u16).to_le_bytes());
      directory.extend((extra.len() as u16).to_le_bytes());
      directory.extend(0u16.to_le_bytes());
      directory.extend(0u16.to_le_bytes());
      directory.extend(0u16.to_le_bytes());
      directory.extend((entry.mode << 16).to_le_bytes());
      directory.extend(offset_field.to_le_bytes());
      directory.extend(entry.name.as_bytes());
      directory.extend(&extra);
    }
    let directory_offset = out.len() as u64;
    out.extend(&directory);
    let count = entries.len();
    if zip64_records {
      let record_position = out.len() as u64;
      out.extend(ZIP64_EOCD_SIGNATURE.to_le_bytes());
      out.extend(44u64.to_le_bytes());
      out.extend(45u16.to_le_bytes());
      out.extend(45u16.to_le_bytes());
      out.extend(0u32.to_le_bytes());
      out.extend(0u32.to_le_bytes());
      out.extend((count as u64).to_le_bytes());
      out.extend((count as u64).to_le_bytes());
      out.extend((directory.len() as u64).to_le_bytes());
      out.extend(directory_offset.to_le_bytes());
      out.extend(ZIP64_LOCATOR_SIGNATURE.to_le_bytes());
      out.extend(0u32.to_le_bytes());
      out.extend(record_position.to_le_bytes());
      out.extend(1u32.to_le_bytes());
    }
    out.extend(EOCD_SIGNATURE.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    let (count16, size32, offset32) = if zip64_records {
      (u16::MAX, u32::MAX, u32::MAX)
    } else {
      (
        count as u16,
        directory.len() as u32,
        directory_offset as u32,
      )
    };
    out.extend(count16.to_le_bytes());
    out.extend(count16.to_le_bytes());
    out.extend(size32.to_le_bytes());
    out.extend(offset32.to_le_bytes());
    out.extend((comment.len() as u16).to_le_bytes());
    out.extend(comment);
    out
  }

  /// An archive held in memory that counts the requests made to it.
  pub struct BytesSource {
    pub data: Vec<u8>,
    pub requests: AtomicUsize,
  }

  impl BytesSource {
    pub fn new(data: Vec<u8>) -> Self {
      Self {
        data,
        requests: AtomicUsize::new(0),
      }
    }

    pub fn requests(&self) -> usize {
      self.requests.load(Ordering::SeqCst)
    }
  }

  #[async_trait]
  impl RangeSource for BytesSource {
    async fn tail(&self, len: u64) -> RangeResult<Tail> {
      self.requests.fetch_add(1, Ordering::SeqCst);
      let total = self.data.len() as u64;
      let take = len.min(total) as usize;
      Ok(Tail {
        total,
        bytes: self.data[self.data.len() - take..].to_vec(),
      })
    }

    async fn read(&self, offset: u64, len: u64) -> RangeResult<Vec<u8>> {
      self.requests.fetch_add(1, Ordering::SeqCst);
      let end = offset
        .checked_add(len)
        .filter(|end| *end <= self.data.len() as u64)
        .ok_or_else(|| RangeError("past the end".into()))?;
      Ok(self.data[offset as usize..end as usize].to_vec())
    }
  }
}

#[cfg(test)]
mod tests {
  use super::testing::*;
  use super::*;
  use std::io::Write;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::sync::Arc;

  fn sample() -> Vec<TestEntry> {
    vec![
      TestEntry::new("manifest.json", b"{\"name\":\"pack\"}"),
      TestEntry::new(
        "overrides/config/a.toml",
        b"key = 1\n".repeat(500).as_slice(),
      ),
      TestEntry::new("overrides/mods/b.jar", &[7u8; 3000]).stored(),
      TestEntry::new("overrides/empty.txt", b""),
      TestEntry::new("overrides/", b"").stored(),
    ]
  }

  async fn read_all(listing: &ZipListing, source: &dyn RangeSource, name: &str) -> Vec<u8> {
    let entry = listing.find(name).unwrap();
    listing
      .read_entry_bytes(source, entry, u64::MAX)
      .await
      .unwrap()
  }

  #[tokio::test]
  async fn the_listing_matches_what_the_zip_crate_reads() {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let deflated = zip::write::SimpleFileOptions::default();
    let stored = deflated.compression_method(zip::CompressionMethod::Stored);
    writer.start_file("manifest.json", deflated).unwrap();
    writer.write_all(b"{\"name\":\"pack\"}").unwrap();
    writer
      .start_file("overrides/config/a.toml", deflated)
      .unwrap();
    writer.write_all(&b"key = 1\n".repeat(500)).unwrap();
    writer.start_file("overrides/mods/b.jar", stored).unwrap();
    writer.write_all(&[7u8; 3000]).unwrap();
    writer.add_directory("overrides/empty/", stored).unwrap();
    let bytes = writer.finish().unwrap().into_inner();

    let source = BytesSource::new(bytes.clone());
    let listing = read_listing(&source).await.unwrap();
    let mut reference = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(listing.entries.len(), reference.len());
    for (index, entry) in listing.entries.iter().enumerate() {
      let known = reference.by_index(index).unwrap();
      assert_eq!(entry.name, known.name());
      assert_eq!(entry.crc32, known.crc32());
      assert_eq!(entry.size, known.size());
      assert_eq!(entry.compressed_size, known.compressed_size());
      assert_eq!(entry.is_dir(), known.is_dir());
      assert_eq!(entry.unix_mode(), known.unix_mode());
    }
    assert_eq!(
      read_all(&listing, &source, "manifest.json").await,
      b"{\"name\":\"pack\"}"
    );
    assert_eq!(
      read_all(&listing, &source, "overrides/config/a.toml").await,
      b"key = 1\n".repeat(500)
    );
    assert_eq!(
      read_all(&listing, &source, "overrides/mods/b.jar").await,
      vec![7u8; 3000]
    );
  }

  #[tokio::test]
  async fn a_small_archive_is_listed_with_one_request() {
    let source = BytesSource::new(build_zip(&sample(), b"", false));
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(source.requests(), 1);
    assert_eq!(listing.entries.len(), 5);
    assert!(listing.entries[4].is_dir());
  }

  #[tokio::test]
  async fn a_comment_does_not_hide_the_end_record() {
    for comment in [b"hello".to_vec(), vec![b'x'; 60_000], {
      // A signature inside the comment whose length field does not reach the
      // end of the data is not an end record.
      let mut fake = EOCD_SIGNATURE.to_le_bytes().to_vec();
      fake.extend(vec![0u8; 16]);
      fake.extend(5u16.to_le_bytes());
      fake.extend(b"..");
      fake
    }] {
      let source = BytesSource::new(build_zip(&sample(), &comment, false));
      let listing = read_listing(&source).await.unwrap();
      assert_eq!(
        listing.entries.len(),
        5,
        "comment of {} bytes",
        comment.len()
      );
      assert_eq!(
        read_all(&listing, &source, "manifest.json").await,
        b"{\"name\":\"pack\"}"
      );
    }
  }

  #[tokio::test]
  async fn zip64_end_records_and_entry_fields_are_followed() {
    let entries = vec![
      TestEntry::new("manifest.json", b"{}").zip64(),
      TestEntry::new("overrides/a.txt", b"alpha")
        .stored()
        .zip64()
        .local_extra(11),
      TestEntry::new("overrides/b.txt", b"beta"),
    ];
    for records in [false, true] {
      let source = BytesSource::new(build_zip(&entries, b"c", records));
      let listing = read_listing(&source).await.unwrap();
      assert_eq!(listing.entries.len(), 3, "zip64 records: {records}");
      assert_eq!(
        read_all(&listing, &source, "overrides/a.txt").await,
        b"alpha"
      );
      assert_eq!(
        read_all(&listing, &source, "overrides/b.txt").await,
        b"beta"
      );
      assert_eq!(listing.find("overrides/a.txt").unwrap().size, 5);
    }
  }

  #[tokio::test]
  async fn a_directory_beyond_the_tail_costs_a_second_request() {
    let entries: Vec<TestEntry> = (0..3000)
      .map(|index| {
        TestEntry::new(
          &format!("overrides/config/some/long/path/{index:05}.toml"),
          b"x",
        )
      })
      .collect();
    let bytes = build_zip(&entries, b"", true);
    let source = BytesSource::new(bytes);
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(listing.entries.len(), 3000);
    assert!(listing.total_len - listing.directory_offset > TAIL_READ);
    // The tail, then the directory; the zip64 record sits in the tail.
    assert_eq!(source.requests(), 2);
  }

  #[tokio::test]
  async fn entries_are_read_with_their_local_extra_field() {
    let entries = vec![
      TestEntry::new("overrides/a.txt", &b"a".repeat(10_000)).local_extra(40),
      TestEntry::new("overrides/b.txt", b"stored")
        .stored()
        .local_extra(3),
    ];
    let source = BytesSource::new(build_zip(&entries, b"", false));
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(
      read_all(&listing, &source, "overrides/a.txt").await,
      b"a".repeat(10_000)
    );
    assert_eq!(
      read_all(&listing, &source, "overrides/b.txt").await,
      b"stored"
    );
    let key = listing
      .hash_entry(&source, listing.find("overrides/b.txt").unwrap())
      .await
      .unwrap();
    assert_eq!(key.sha1, hex::encode(Sha1::digest(b"stored")));
    assert_eq!(key.size, 6);
  }

  #[tokio::test]
  async fn damaged_entries_are_refused() {
    let bytes = build_zip(&sample(), b"", false);
    let source = BytesSource::new(bytes.clone());
    let listing = read_listing(&source).await.unwrap();

    // A flipped byte in stored data fails the CRC32.
    let entry = listing.find("overrides/mods/b.jar").unwrap().clone();
    let mut damaged = bytes.clone();
    damaged[entry.header_offset as usize + LOCAL_LEN + entry.name.len() + 10] ^= 0xff;
    let damaged = BytesSource::new(damaged);
    let error = listing
      .read_entry_bytes(&damaged, &entry, u64::MAX)
      .await
      .unwrap_err();
    assert!(error.0.contains("CRC32"), "{error}");

    // A size that disagrees with the content.
    let mut lying = entry.clone();
    lying.size += 1;
    lying.compressed_size += 1;
    assert!(listing
      .read_entry_bytes(&source, &lying, u64::MAX)
      .await
      .is_err());
    let mut short = listing.find("overrides/config/a.toml").unwrap().clone();
    short.size -= 1;
    assert!(listing
      .read_entry_bytes(&source, &short, u64::MAX)
      .await
      .is_err());

    // Over the caller's limit, encrypted, an unknown method.
    assert!(listing.read_entry_bytes(&source, &entry, 10).await.is_err());
    let mut encrypted = entry.clone();
    encrypted.flags |= FLAG_ENCRYPTED;
    assert!(listing
      .read_entry_bytes(&source, &encrypted, u64::MAX)
      .await
      .is_err());
    let mut other = entry.clone();
    other.method = 12;
    assert!(listing
      .read_entry_bytes(&source, &other, u64::MAX)
      .await
      .is_err());

    // A local header that names another entry.
    let mut elsewhere = entry.clone();
    elsewhere.header_offset = listing.find("manifest.json").unwrap().header_offset;
    assert!(listing
      .read_entry_bytes(&source, &elsewhere, u64::MAX)
      .await
      .is_err());
  }

  #[tokio::test]
  async fn an_archive_that_does_not_parse_is_refused() {
    let good = build_zip(&sample(), b"", false);
    for bad in [
      Vec::new(),
      b"not a zip at all".to_vec(),
      good[..good.len() - 10].to_vec(),
      // Prepended data moves every offset.
      [b"prefix".to_vec(), good.clone()].concat(),
    ] {
      let source = BytesSource::new(bad);
      assert!(read_listing(&source).await.is_err());
    }
    // Two entries under one name.
    let twice = vec![TestEntry::new("a.txt", b"1"), TestEntry::new("a.txt", b"2")];
    assert!(
      read_listing(&BytesSource::new(build_zip(&twice, b"", false)))
        .await
        .is_err()
    );
    // A name that is not UTF-8 and has no flag for it.
    let mut raw = build_zip(&[TestEntry::new("abc", b"1")], b"", false);
    let at = raw
      .windows(4)
      .rposition(|w| w == CENTRAL_SIGNATURE.to_le_bytes())
      .unwrap();
    raw[at + 8] = 0;
    raw[at + 9] = 0;
    raw[at + CENTRAL_LEN] = 0xe9;
    assert!(read_listing(&BytesSource::new(raw)).await.is_err());
  }

  #[tokio::test]
  async fn symlink_and_mode_come_from_unix_archives() {
    let entries = vec![
      TestEntry::new("overrides/link", b"target").mode(0o120777),
      TestEntry::new("overrides/file", b"x"),
    ];
    let source = BytesSource::new(build_zip(&entries, b"", false));
    let listing = read_listing(&source).await.unwrap();
    assert!(listing.find("overrides/link").unwrap().is_symlink());
    assert!(!listing.find("overrides/file").unwrap().is_symlink());
  }

  #[tokio::test]
  async fn a_local_file_gives_the_same_listing() {
    let bytes = build_zip(&sample(), b"tail", true);
    let path = std::env::temp_dir().join(format!("ustbl-range-{}.zip", uuid::Uuid::new_v4()));
    std::fs::write(&path, &bytes).unwrap();
    let local = LocalSource::new(&path);
    let listing = read_listing(&local).await.unwrap();
    assert_eq!(
      listing,
      read_listing(&BytesSource::new(bytes)).await.unwrap()
    );
    assert_eq!(
      read_all(&listing, &local, "overrides/mods/b.jar").await,
      vec![7u8; 3000]
    );
    assert!(local.read(listing.total_len, 1).await.is_err());
    let _ = std::fs::remove_file(path);
  }

  // ---- HTTP ------------------------------------------------------------------

  #[derive(Clone, Copy, PartialEq)]
  enum Behaviour {
    Honest,
    /// Answers 200 with the whole body.
    IgnoreRange,
    /// Reports a total that is one more on every answer after the first.
    DriftingTotal,
    /// Answers 404.
    Missing,
    /// Refuses suffix ranges with 501, as some CDNs do; explicit ranges work.
    NoSuffix,
  }

  struct TestServer {
    url: Url,
    requests: Arc<AtomicUsize>,
  }

  async fn serve(data: Vec<u8>, behaviour: Behaviour) -> TestServer {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
      "http://{}/pack.zip",
      listener.local_addr().unwrap()
    ))
    .unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    tokio::spawn(async move {
      loop {
        let Ok((mut stream, _)) = listener.accept().await else {
          return;
        };
        let (data, counter) = (data.clone(), counter.clone());
        tokio::spawn(async move {
          let mut head = Vec::new();
          let mut buffer = [0u8; 1024];
          while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap_or(0);
            if read == 0 {
              return;
            }
            head.extend_from_slice(&buffer[..read]);
          }
          let seen = counter.fetch_add(1, Ordering::SeqCst);
          let head = String::from_utf8_lossy(&head).to_lowercase();
          let range = head
            .lines()
            .find_map(|line| line.strip_prefix("range: bytes="))
            .map(str::to_string);
          let total = data.len() as u64;
          let (status, extra, body): (&str, String, &[u8]) = match (behaviour, range) {
            (Behaviour::Missing, _) => ("404 Not Found", String::new(), b""),
            (Behaviour::IgnoreRange, _) | (_, None) => ("200 OK", String::new(), &data),
            (Behaviour::NoSuffix, Some(range)) if range.starts_with('-') => (
              "501 Not Implemented",
              String::new(),
              b"Unsupported client range",
            ),
            (_, Some(range))
              if range.split_once('-').is_some_and(|(first, _)| {
                first
                  .trim()
                  .parse::<u64>()
                  .is_ok_and(|first| first >= total)
              }) =>
            {
              ("416 Range Not Satisfiable", String::new(), b"")
            }
            (_, Some(range)) => {
              let (first, last) = match range.split_once('-').unwrap() {
                ("", suffix) => {
                  let suffix: u64 = suffix.trim().parse().unwrap();
                  (total - suffix.min(total), total - 1)
                }
                (first, last) => (
                  first.trim().parse().unwrap(),
                  last.trim().parse::<u64>().unwrap().min(total - 1),
                ),
              };
              let reported = if behaviour == Behaviour::DriftingTotal && seen > 1 {
                total + 1
              } else {
                total
              };
              (
                "206 Partial Content",
                format!("Content-Range: bytes {first}-{last}/{reported}\r\n"),
                &data[first as usize..=last as usize],
              )
            }
          };
          let response = format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
          );
          let _ = stream.write_all(response.as_bytes()).await;
          let _ = stream.write_all(body).await;
        });
      }
    });
    TestServer { url, requests }
  }

  fn client() -> reqwest::Client {
    reqwest::Client::builder()
      .no_gzip()
      .no_proxy()
      .build()
      .unwrap()
  }

  #[tokio::test]
  async fn an_http_server_that_honours_ranges_serves_the_listing_and_entries() {
    let bytes = build_zip(&sample(), b"", false);
    let server = serve(bytes.clone(), Behaviour::Honest).await;
    let source = HttpSource::new(client(), vec![server.url.clone()], None);
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(listing.total_len, bytes.len() as u64);
    assert_eq!(source.pinned(), Some(server.url.clone()));
    assert_eq!(
      read_all(&listing, &source, "overrides/mods/b.jar").await,
      vec![7u8; 3000]
    );
    assert_eq!(
      read_all(&listing, &source, "overrides/config/a.toml").await,
      b"key = 1\n".repeat(500)
    );
    // A probe for the length and the tail, then a header and a body request per entry.
    assert_eq!(server.requests.load(Ordering::SeqCst), 2 + 2 * 2);
  }

  #[tokio::test]
  async fn a_server_that_refuses_suffix_ranges_serves_the_listing_and_entries() {
    let bytes = build_zip(&sample(), b"", false);
    let server = serve(bytes.clone(), Behaviour::NoSuffix).await;
    let source = HttpSource::new(client(), vec![server.url.clone()], None);
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(listing.total_len, bytes.len() as u64);
    assert_eq!(source.pinned(), Some(server.url.clone()));
    assert_eq!(
      read_all(&listing, &source, "overrides/mods/b.jar").await,
      vec![7u8; 3000]
    );
    assert_eq!(
      read_all(&listing, &source, "overrides/config/a.toml").await,
      b"key = 1\n".repeat(500)
    );
    // With the length known, the tail is one explicit range.
    let known = HttpSource::new(client(), vec![server.url.clone()], Some(bytes.len() as u64));
    let before = server.requests.load(Ordering::SeqCst);
    assert_eq!(read_listing(&known).await.unwrap(), listing);
    assert_eq!(server.requests.load(Ordering::SeqCst), before + 1);
  }

  #[tokio::test]
  async fn a_tail_shorter_than_the_archive_is_an_explicit_range_too() {
    let mut entries = vec![TestEntry::new("overrides/big.bin", &vec![9u8; 300 * 1024]).stored()];
    entries.extend(sample());
    let bytes = build_zip(&entries, b"", false);
    assert!(bytes.len() as u64 > TAIL_READ);
    let server = serve(bytes.clone(), Behaviour::NoSuffix).await;
    let source = HttpSource::new(client(), vec![server.url.clone()], None);
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(listing.total_len, bytes.len() as u64);
    assert_eq!(
      read_all(&listing, &source, "overrides/big.bin").await,
      vec![9u8; 300 * 1024]
    );
  }

  #[tokio::test]
  async fn a_server_that_ignores_range_is_refused() {
    let server = serve(build_zip(&sample(), b"", false), Behaviour::IgnoreRange).await;
    let source = HttpSource::new(client(), vec![server.url.clone()], None);
    let error = read_listing(&source).await.unwrap_err();
    assert!(error.0.contains("200"), "{error}");
    assert_eq!(source.pinned(), None);
  }

  #[tokio::test]
  async fn a_total_that_changes_or_disagrees_with_the_known_size_is_refused() {
    let bytes = build_zip(&sample(), b"", false);
    let server = serve(bytes.clone(), Behaviour::DriftingTotal).await;
    let source = HttpSource::new(client(), vec![server.url.clone()], None);
    let listing = read_listing(&source).await.unwrap();
    let entry = listing.find("overrides/mods/b.jar").unwrap();
    let error = listing
      .read_entry_bytes(&source, entry, u64::MAX)
      .await
      .unwrap_err();
    assert!(error.0.contains("in total"), "{error}");

    let honest = serve(bytes.clone(), Behaviour::Honest).await;
    let source = HttpSource::new(
      client(),
      vec![honest.url.clone()],
      Some(bytes.len() as u64 + 5),
    );
    assert!(read_listing(&source).await.is_err());
  }

  #[tokio::test]
  async fn the_next_url_is_tried_when_the_first_cannot_serve_ranges() {
    let bytes = build_zip(&sample(), b"", false);
    let missing = serve(bytes.clone(), Behaviour::Missing).await;
    let ignoring = serve(bytes.clone(), Behaviour::IgnoreRange).await;
    let honest = serve(bytes, Behaviour::Honest).await;
    let source = HttpSource::new(
      client(),
      vec![
        missing.url.clone(),
        ignoring.url.clone(),
        honest.url.clone(),
      ],
      None,
    );
    let listing = read_listing(&source).await.unwrap();
    assert_eq!(source.pinned(), Some(honest.url.clone()));
    assert_eq!(listing.entries.len(), 5);
    // Once pinned, the others are left alone.
    let before = (
      missing.requests.load(Ordering::SeqCst),
      ignoring.requests.load(Ordering::SeqCst),
    );
    read_all(&listing, &source, "manifest.json").await;
    assert_eq!(
      before,
      (
        missing.requests.load(Ordering::SeqCst),
        ignoring.requests.load(Ordering::SeqCst)
      )
    );
  }

  #[test]
  fn content_range_headers_are_parsed_strictly() {
    assert_eq!(parse_content_range("bytes 0-99/1000"), Some((0, 99, 1000)));
    assert_eq!(parse_content_range("bytes 5-5/6"), Some((5, 5, 6)));
    for bad in [
      "bytes 0-99/*",
      "items 0-1/2",
      "bytes */10",
      "bytes 0-1",
      "bytes a-b/c",
    ] {
      assert_eq!(parse_content_range(bad), None, "{bad}");
    }
  }
}
