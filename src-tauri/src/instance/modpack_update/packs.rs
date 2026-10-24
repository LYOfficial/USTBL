//! Reading files out of the packs the campus platform stores.
//!
//! A version's files live in immutable packs on the cloud drive, one per
//! publication. The platform only says where a pack is (`locate`); this module
//! reads each file an install or update needs out of those packs with HTTP
//! Range requests. There is no full pack: a fresh install reads every file the
//! same way an update does.

use super::sources::CAMPUS_API;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::models::misc::InstanceError;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;

pub const STORED: u16 = 0;
pub const DEFLATED: u16 = 8;

const CODE_NOT_LATEST: &str = "NOT_LATEST";
/// Transient failures (network, 5xx) are retried this many times in total.
const ATTEMPTS: usize = 3;
/// Re-asking where a pack is, after the signature ran out, happens this often
/// for one file before giving up.
const RELOCATIONS: usize = 2;

/// Where and how to fetch one pack. `headers` must be sent verbatim.
#[derive(Debug, Clone, Deserialize)]
pub struct PackLocation {
  pub id: i64,
  pub url: String,
  pub method: String,
  #[serde(default)]
  pub headers: HashMap<String, String>,
}

/// Where one file sits inside a pack.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct EntryLocation {
  pub pack: i64,
  pub data_offset: u64,
  pub compress_type: u16,
  pub compressed_size: u64,
  pub size: u64,
  pub crc32: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Located {
  pub packs: Vec<PackLocation>,
  /// File sha1 to its place in one of `packs`.
  #[serde(default)]
  pub entries: HashMap<String, EntryLocation>,
}

#[derive(Debug)]
pub enum TransportError {
  /// The signed request is no longer accepted.
  Expired,
  /// The platform refused with a machine readable code.
  Conflict(String),
  Failed(USTBLError),
}

impl From<USTBLError> for TransportError {
  fn from(error: USTBLError) -> Self {
    TransportError::Failed(error)
  }
}

impl From<TransportError> for USTBLError {
  fn from(error: TransportError) -> Self {
    match error {
      TransportError::Conflict(code) if code == CODE_NOT_LATEST => {
        InstanceError::ModpackUpdateStale.into()
      }
      TransportError::Conflict(_) => InstanceError::ModpackUpdatePackUnavailable.into(),
      TransportError::Expired => InstanceError::ModpackUpdatePackUnavailable.into(),
      TransportError::Failed(error) => error,
    }
  }
}

fn failed(error: impl std::error::Error) -> TransportError {
  TransportError::Failed(USTBLError(error.to_string()))
}

#[async_trait]
pub trait PackTransport: Send + Sync {
  async fn locate(&self, version_id: i64, sha1s: &[String]) -> Result<Located, TransportError>;
  /// Write `length` bytes of the pack, starting at `start`, to `dest`.
  async fn read_range(
    &self,
    pack: &PackLocation,
    start: u64,
    length: u64,
    dest: &Path,
  ) -> Result<(), TransportError>;
}

struct State {
  /// `None` until `prepare` has run.
  located: Option<Arc<Located>>,
  wanted: Vec<String>,
  /// Bumped whenever `located` is replaced, so concurrent fetches that hit an
  /// expired signature locate once between them.
  generation: u64,
}

/// The files of one campus version that live in packs.
pub struct PackSession {
  transport: Arc<dyn PackTransport>,
  version_id: i64,
  state: tokio::sync::Mutex<State>,
}

impl fmt::Debug for PackSession {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PackSession")
      .field("version_id", &self.version_id)
      .finish_non_exhaustive()
  }
}

impl PackSession {
  pub fn new(transport: Arc<dyn PackTransport>, version_id: i64) -> Arc<Self> {
    Arc::new(Self {
      transport,
      version_id,
      state: tokio::sync::Mutex::new(State {
        located: None,
        wanted: Vec::new(),
        generation: 0,
      }),
    })
  }

  /// Ask the platform which packs hold `sha1s`. A pack the platform cannot
  /// hand out (gone, or too many of them) is reported as unavailable.
  pub async fn prepare(&self, sha1s: &[String]) -> USTBLResult<()> {
    let mut wanted = sha1s.to_vec();
    wanted.sort();
    wanted.dedup();
    let mut state = self.state.lock().await;
    state.located = Some(self.plan(&wanted).await?);
    state.wanted = wanted;
    state.generation += 1;
    Ok(())
  }

  async fn plan(&self, wanted: &[String]) -> USTBLResult<Arc<Located>> {
    if wanted.is_empty() {
      return Ok(Arc::new(Located::default()));
    }
    let located = self.transport.locate(self.version_id, wanted).await?;
    Ok(Arc::new(located))
  }

  /// Put the file with this sha1 at `dest`. Callers verify the result.
  pub async fn fetch(&self, sha1: &str, dest: &Path) -> USTBLResult<()> {
    let mut relocations = 0;
    loop {
      let (located, generation) = {
        let state = self.state.lock().await;
        match &state.located {
          None => return Err(InstanceError::ModpackUpdateRemoteInvalid.into()),
          Some(located) => (located.clone(), state.generation),
        }
      };
      let entry = located
        .entries
        .get(sha1)
        .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
      let pack = located
        .packs
        .iter()
        .find(|pack| pack.id == entry.pack)
        .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
      let raw = raw_path(dest);
      match self
        .transport
        .read_range(pack, entry.data_offset, entry.compressed_size, &raw)
        .await
      {
        Ok(()) => {
          let (entry, dest) = (*entry, dest.to_path_buf());
          return tokio::task::spawn_blocking(move || {
            let result = decode_entry(&raw, &entry, &dest);
            let _ = fs::remove_file(&raw);
            result
          })
          .await
          .map_err(|e| USTBLError(e.to_string()))?;
        }
        Err(TransportError::Expired) if relocations < RELOCATIONS => {
          relocations += 1;
          self.relocate(generation).await?;
        }
        Err(error) => {
          let _ = fs::remove_file(&raw);
          return Err(error.into());
        }
      }
    }
  }

  /// Ask where the packs are again, unless another fetch already did.
  async fn relocate(&self, seen: u64) -> USTBLResult<()> {
    let mut state = self.state.lock().await;
    if state.generation != seen {
      return Ok(());
    }
    let wanted = state.wanted.clone();
    state.located = Some(self.plan(&wanted).await?);
    state.generation += 1;
    Ok(())
  }
}

fn raw_path(dest: &Path) -> PathBuf {
  let mut name = dest.file_name().unwrap_or_default().to_os_string();
  name.push(".pack");
  dest.with_file_name(name)
}

/// Write the entry's bytes, still compressed as stored in the pack, out as the
/// original file, checking size and CRC.
fn decode_entry(raw: &Path, entry: &EntryLocation, dest: &Path) -> USTBLResult<()> {
  if fs::metadata(raw)?.len() != entry.compressed_size {
    return Err(InstanceError::ModpackUpdateVerifyFailed.into());
  }
  if let Some(parent) = dest.parent() {
    fs::create_dir_all(parent)?;
  }
  let input = File::open(raw)?;
  let mut reader: Box<dyn Read> = match entry.compress_type {
    STORED => Box::new(input),
    DEFLATED => Box::new(flate2::read::DeflateDecoder::new(input)),
    _ => return Err(InstanceError::ModpackUpdateRemoteInvalid.into()),
  };
  let mut out = File::create(dest)?;
  let mut crc = flate2::Crc::new();
  let mut size = 0u64;
  let mut buffer = vec![0u8; 64 * 1024];
  let outcome = loop {
    let read = match reader.read(&mut buffer) {
      Ok(0) => break Ok(()),
      Ok(read) => read,
      Err(error) => break Err(USTBLError::from(error)),
    };
    size += read as u64;
    // A corrupt stream must not fill the disk.
    if size > entry.size {
      break Err(InstanceError::ModpackUpdateVerifyFailed.into());
    }
    crc.update(&buffer[..read]);
    if let Err(error) = out.write_all(&buffer[..read]) {
      break Err(USTBLError::from(error));
    }
  };
  let verified = outcome.and_then(|()| {
    if size == entry.size && crc.sum() == entry.crc32 {
      Ok(())
    } else {
      Err(InstanceError::ModpackUpdateVerifyFailed.into())
    }
  });
  if verified.is_err() {
    drop(out);
    let _ = fs::remove_file(dest);
  }
  verified
}

// ---- the real transport -------------------------------------------------------

pub struct AppTransport {
  app: AppHandle,
}

impl AppTransport {
  pub fn new(app: AppHandle) -> Arc<Self> {
    Arc::new(Self { app })
  }
}

fn transient(status: reqwest::StatusCode) -> bool {
  status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

fn invalid() -> TransportError {
  TransportError::Failed(InstanceError::ModpackUpdateRemoteInvalid.into())
}

fn signed_request(
  client: &reqwest::Client,
  pack: &PackLocation,
) -> Result<reqwest::RequestBuilder, TransportError> {
  if !pack.url.starts_with("https://") || !pack.method.eq_ignore_ascii_case("GET") {
    return Err(invalid());
  }
  let mut request = client.get(&pack.url);
  for (name, value) in &pack.headers {
    request = request.header(name, value);
  }
  Ok(request)
}

#[async_trait]
impl PackTransport for AppTransport {
  async fn locate(&self, version_id: i64, sha1s: &[String]) -> Result<Located, TransportError> {
    let client = self.app.state::<reqwest::Client>();
    let url = format!("{CAMPUS_API}/versions/{version_id}/locate");
    // Servers from before v3 reject a request without `mode`; they answer the
    // "update" shape, which is the only one used. Newer ones ignore the field.
    let body = serde_json::json!({ "mode": "update", "sha1s": sha1s });
    let mut attempt = 0;
    loop {
      attempt += 1;
      let response = match client.post(&url).json(&body).send().await {
        Ok(response) => response,
        Err(_) if attempt < ATTEMPTS => {
          tokio::time::sleep(Duration::from_secs(2)).await;
          continue;
        }
        Err(error) => return Err(failed(error)),
      };
      let status = response.status();
      if status.is_success() {
        return response.json().await.map_err(failed);
      }
      if status == reqwest::StatusCode::CONFLICT {
        let detail: serde_json::Value = response.json().await.unwrap_or_default();
        let code = detail["detail"]["code"].as_str().unwrap_or_default();
        return Err(TransportError::Conflict(code.to_string()));
      }
      if transient(status) && attempt < ATTEMPTS {
        tokio::time::sleep(Duration::from_secs(2)).await;
        continue;
      }
      return Err(failed(response.error_for_status().unwrap_err()));
    }
  }

  async fn read_range(
    &self,
    pack: &PackLocation,
    start: u64,
    length: u64,
    dest: &Path,
  ) -> Result<(), TransportError> {
    if length == 0 {
      return tokio::fs::write(dest, b"").await.map_err(failed);
    }
    if let Some(parent) = dest.parent() {
      tokio::fs::create_dir_all(parent).await.map_err(failed)?;
    }
    let client = self.app.state::<reqwest::Client>();
    let range = format!("bytes={start}-{}", start + length - 1);
    let mut attempt = 0;
    loop {
      attempt += 1;
      let outcome = async {
        let response = signed_request(&client, pack)?
          .header(reqwest::header::RANGE, &range)
          .send()
          .await
          .map_err(failed)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
          return Err(TransportError::Expired);
        }
        // Anything but 206 means the range was not honoured; do not take the
        // whole pack for one file.
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
          return Err(TransportError::Failed(USTBLError(format!(
            "unexpected status {status} reading a pack"
          ))));
        }
        let mut response = response;
        let mut file = tokio::fs::File::create(dest).await.map_err(failed)?;
        let mut written = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
          written += chunk.len() as u64;
          if written > length {
            break;
          }
          file.write_all(&chunk).await.map_err(failed)?;
        }
        file.flush().await.map_err(failed)?;
        if written != length {
          return Err(TransportError::Failed(USTBLError(
            "pack range ended early".into(),
          )));
        }
        Ok(())
      }
      .await;
      match outcome {
        Err(TransportError::Failed(_)) if attempt < ATTEMPTS => {
          tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
        }
        other => return other,
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder =
      flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
  }

  fn crc(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
  }

  fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ustbl-packs-test-{}-{name}", uuid::Uuid::new_v4()))
  }

  fn location(id: i64) -> PackLocation {
    PackLocation {
      id,
      url: "https://cloud.example/p".into(),
      method: "GET".into(),
      headers: HashMap::new(),
    }
  }

  #[test]
  fn stored_and_deflated_entries_are_restored_and_checked() {
    let data = b"hello pack ".repeat(50);
    for (method, raw) in [(STORED, data.clone()), (DEFLATED, deflate(&data))] {
      let (source, dest) = (temp("raw"), temp("out"));
      fs::write(&source, &raw).unwrap();
      let entry = EntryLocation {
        pack: 1,
        data_offset: 0,
        compress_type: method,
        compressed_size: raw.len() as u64,
        size: data.len() as u64,
        crc32: crc(&data),
      };
      decode_entry(&source, &entry, &dest).unwrap();
      assert_eq!(fs::read(&dest).unwrap(), data);

      // A wrong checksum or size leaves nothing behind.
      let _ = fs::remove_file(&dest);
      let bad = EntryLocation { crc32: 1, ..entry };
      assert!(decode_entry(&source, &bad, &dest).is_err());
      assert!(!dest.exists());
      let bad = EntryLocation { size: 3, ..entry };
      assert!(decode_entry(&source, &bad, &dest).is_err());
      assert!(!dest.exists());
      let bad = EntryLocation {
        compressed_size: raw.len() as u64 + 1,
        ..entry
      };
      assert!(decode_entry(&source, &bad, &dest).is_err());
      let _ = fs::remove_file(source);
    }
  }

  #[test]
  fn unknown_compression_is_rejected() {
    let (source, dest) = (temp("raw"), temp("out"));
    fs::write(&source, b"x").unwrap();
    let entry = EntryLocation {
      pack: 1,
      data_offset: 0,
      compress_type: 14,
      compressed_size: 1,
      size: 1,
      crc32: crc(b"x"),
    };
    assert!(decode_entry(&source, &entry, &dest).is_err());
    let _ = fs::remove_file(source);
  }

  /// A platform holding packs of files, with a signature that can be made to
  /// run out.
  struct Fake {
    packs: HashMap<i64, Vec<u8>>,
    entries: HashMap<String, EntryLocation>,
    calls: Mutex<Vec<String>>,
    /// Number of range reads that answer "expired" before one succeeds.
    expire: Mutex<usize>,
    /// Code the platform refuses `locate` with.
    refuse: Option<&'static str>,
  }

  impl Fake {
    /// One pack (id 7) holding all `files`.
    fn new(files: &[(&str, &[u8])]) -> Self {
      Self::chain(&[(7, files)])
    }

    /// One pack per `(id, files)`, as a first pack followed by delta packs.
    fn chain(packs: &[(i64, &[(&str, &[u8])])]) -> Self {
      let mut bytes = HashMap::new();
      let mut entries = HashMap::new();
      for (id, files) in packs {
        let mut pack = Vec::new();
        for (name, data) in *files {
          let compressed = deflate(data);
          entries.insert(
            name.to_string(),
            EntryLocation {
              pack: *id,
              data_offset: pack.len() as u64,
              compress_type: DEFLATED,
              compressed_size: compressed.len() as u64,
              size: data.len() as u64,
              crc32: crc(data),
            },
          );
          pack.extend(compressed);
        }
        bytes.insert(*id, pack);
      }
      Self {
        packs: bytes,
        entries,
        calls: Mutex::default(),
        expire: Mutex::new(0),
        refuse: None,
      }
    }

    fn calls(&self) -> Vec<String> {
      self.calls.lock().unwrap().clone()
    }
  }

  #[async_trait]
  impl PackTransport for Fake {
    async fn locate(&self, _version_id: i64, sha1s: &[String]) -> Result<Located, TransportError> {
      self.calls.lock().unwrap().push("locate".into());
      if let Some(code) = self.refuse {
        return Err(TransportError::Conflict(code.into()));
      }
      let entries: HashMap<String, EntryLocation> = sha1s
        .iter()
        .map(|sha1| (sha1.clone(), self.entries[sha1]))
        .collect();
      let mut ids: Vec<i64> = entries.values().map(|entry| entry.pack).collect();
      ids.sort();
      ids.dedup();
      Ok(Located {
        packs: ids.into_iter().map(location).collect(),
        entries,
      })
    }

    async fn read_range(
      &self,
      pack: &PackLocation,
      start: u64,
      length: u64,
      dest: &Path,
    ) -> Result<(), TransportError> {
      self
        .calls
        .lock()
        .unwrap()
        .push(format!("range:{}:{start}+{length}", pack.id));
      {
        let mut expire = self.expire.lock().unwrap();
        if *expire > 0 {
          *expire -= 1;
          return Err(TransportError::Expired);
        }
      }
      let (start, length) = (start as usize, length as usize);
      fs::create_dir_all(dest.parent().unwrap()).unwrap();
      fs::write(dest, &self.packs[&pack.id][start..start + length]).unwrap();
      Ok(())
    }
  }

  fn session(fake: Fake) -> (Arc<PackSession>, Arc<Fake>, PathBuf) {
    let fake = Arc::new(fake);
    let work = temp("work");
    (PackSession::new(fake.clone(), 3), fake, work)
  }

  #[tokio::test]
  async fn entries_are_read_with_ranges_and_the_signature_is_renewed_once() {
    let mut fake = Fake::new(&[("aa", b"first file ".repeat(20).leak()), ("bb", b"second")]);
    *fake.expire.get_mut().unwrap() = 1;
    let (session, fake, work) = session(fake);
    session
      .prepare(&["bb".into(), "aa".into(), "bb".into()])
      .await
      .unwrap();

    let dest = work.join("staging/mods/b.jar");
    session.fetch("bb", &dest).await.unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"second");
    assert!(!raw_path(&dest).exists());

    let calls = fake.calls();
    // One locate up front, one more after the expired read, and the retry.
    assert_eq!(calls.iter().filter(|c| c.starts_with("locate")).count(), 2);
    assert_eq!(calls.iter().filter(|c| c.starts_with("range")).count(), 2);
    let _ = fs::remove_dir_all(work);
  }

  #[tokio::test]
  async fn a_signature_that_keeps_expiring_gives_up() {
    let mut fake = Fake::new(&[("aa", b"data")]);
    *fake.expire.get_mut().unwrap() = 10;
    let (session, _fake, work) = session(fake);
    session.prepare(&["aa".into()]).await.unwrap();
    let error = session.fetch("aa", &work.join("a")).await.unwrap_err();
    assert_eq!(
      error,
      USTBLError::from(InstanceError::ModpackUpdatePackUnavailable)
    );
    let _ = fs::remove_dir_all(work);
  }

  #[tokio::test]
  async fn a_fresh_install_reads_every_file_from_the_pack_that_holds_it() {
    // The first pack and two delta packs; nothing else exists to fall back on.
    let fake = Fake::chain(&[
      (1, &[("aa", b"alpha alpha alpha"), ("bb", b"beta")]),
      (4, &[("cc", b"gamma gamma")]),
      (9, &[("dd", b"delta")]),
    ]);
    let (session, fake, work) = session(fake);
    let all: Vec<String> = ["dd", "bb", "aa", "cc"].map(String::from).into();
    session.prepare(&all).await.unwrap();

    let expected: [(&str, &[u8]); 4] = [
      ("aa", b"alpha alpha alpha"),
      ("bb", b"beta"),
      ("cc", b"gamma gamma"),
      ("dd", b"delta"),
    ];
    for (sha1, data) in expected {
      let dest = work.join(format!("out/{sha1}"));
      session.fetch(sha1, &dest).await.unwrap();
      assert_eq!(fs::read(&dest).unwrap(), data);
    }

    let calls = fake.calls();
    assert_eq!(calls.iter().filter(|c| *c == "locate").count(), 1);
    let ranges: Vec<&String> = calls.iter().filter(|c| c.starts_with("range")).collect();
    assert_eq!(ranges.len(), 4);
    for pack in [1, 4, 9] {
      let prefix = format!("range:{pack}:");
      let expected = if pack == 1 { 2 } else { 1 };
      assert_eq!(
        ranges.iter().filter(|c| c.starts_with(&prefix)).count(),
        expected
      );
    }
    let _ = fs::remove_dir_all(work);
  }

  #[tokio::test]
  async fn unavailable_packs_are_reported_without_another_attempt() {
    for code in ["MODPACK_UPDATE_PACK_UNAVAILABLE", "TOO_MANY_PACKS"] {
      let mut fake = Fake::new(&[("aa", b"alpha")]);
      fake.refuse = Some(code);
      let (session, fake, _work) = session(fake);
      let error = session.prepare(&["aa".into()]).await.unwrap_err();
      assert_eq!(
        error,
        USTBLError::from(InstanceError::ModpackUpdatePackUnavailable)
      );
      // Exactly one locate: nothing else is asked for in its place.
      assert_eq!(fake.calls(), vec!["locate".to_string()]);
      // And the session stays unusable rather than half prepared.
      assert!(session.fetch("aa", Path::new("unused")).await.is_err());
    }
  }

  #[tokio::test]
  async fn a_version_that_is_no_longer_the_latest_is_stale() {
    let mut fake = Fake::new(&[("aa", b"alpha")]);
    fake.refuse = Some("NOT_LATEST");
    let (session, _fake, _work) = session(fake);
    let error = session.prepare(&["aa".into()]).await.unwrap_err();
    assert_eq!(error, USTBLError::from(InstanceError::ModpackUpdateStale));
  }

  #[tokio::test]
  async fn nothing_is_located_when_no_file_is_wanted() {
    let (session, fake, _work) = session(Fake::new(&[("aa", b"x")]));
    session.prepare(&[]).await.unwrap();
    assert!(fake.calls().is_empty());
  }

  #[tokio::test]
  async fn fetching_before_preparing_or_an_unknown_file_is_invalid() {
    let (session, _fake, work) = session(Fake::new(&[("aa", b"x")]));
    assert!(session.fetch("aa", &work.join("a")).await.is_err());
    session.prepare(&["aa".into()]).await.unwrap();
    assert!(session.fetch("zz", &work.join("z")).await.is_err());
    let _ = fs::remove_dir_all(work);
  }
}
