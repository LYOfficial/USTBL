//! Persistent cache of CurseForge file metadata, keyed by file id.
//!
//! A CurseForge file is immutable once published, so its download URL, hash,
//! size and name never change. Caching them lets a modpack update skip the
//! lookup for every file it already saw, and the same file shared between
//! modpacks is only looked up once.
//!
//! The file lives in the launcher data directory. A missing, unreadable or
//! outdated file is an empty cache, and writes go through a temporary file so a
//! crash cannot leave a half-written cache behind.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::instance::helpers::modpack::curseforge::CurseForgeFileMeta;

pub const CACHE_FILE_NAME: &str = "curseforge_file_meta.json";
const CACHE_SCHEMA: u32 = 1;
/// Upper bound on stored entries (about 250 bytes each). When a save would
/// exceed it, the entries used least recently are dropped.
pub const MAX_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
  meta: CurseForgeFileMeta,
  /// Unix seconds of the last time the entry was stored or served.
  last_used: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
  schema: u32,
  entries: HashMap<u64, CacheEntry>,
}

#[derive(Serialize)]
struct CacheFileRef<'a> {
  schema: u32,
  entries: &'a HashMap<u64, CacheEntry>,
}

#[derive(Debug, Default)]
pub struct FileMetaCache {
  entries: HashMap<u64, CacheEntry>,
}

fn now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

pub fn cache_path(data_dir: &Path) -> PathBuf {
  data_dir.join(CACHE_FILE_NAME)
}

/// Whether an answer is complete enough to be reused forever. A file without a
/// hash, or a project whose class could not be read, may be answered better
/// later, so those are looked up again next time.
pub fn is_cacheable(meta: &CurseForgeFileMeta) -> bool {
  meta.sha1.is_some() && meta.class_id.is_some()
}

impl FileMetaCache {
  /// Read the cache at `path`. Anything unreadable counts as an empty cache.
  pub fn load(path: &Path) -> Self {
    let Ok(bytes) = fs::read(path) else {
      return Self::default();
    };
    match serde_json::from_slice::<CacheFile>(&bytes) {
      Ok(file) if file.schema == CACHE_SCHEMA => Self {
        entries: file.entries,
      },
      Ok(_) => Self::default(),
      Err(error) => {
        log::warn!("Ignoring unreadable CurseForge metadata cache: {error}");
        Self::default()
      }
    }
  }

  #[cfg(test)]
  fn len(&self) -> usize {
    self.entries.len()
  }

  /// The cached metadata of `file_id`, if it was stored for the same project.
  pub fn get(&self, file_id: u64, project_id: u64) -> Option<CurseForgeFileMeta> {
    self
      .entries
      .get(&file_id)
      .filter(|entry| entry.meta.project_id == project_id)
      .map(|entry| entry.meta.clone())
  }

  /// Mark an entry as used just now, so it outlives older ones when pruning.
  pub fn touch(&mut self, file_id: u64) {
    if let Some(entry) = self.entries.get_mut(&file_id) {
      entry.last_used = now_secs();
    }
  }

  pub fn insert(&mut self, file_id: u64, meta: CurseForgeFileMeta) {
    self.entries.insert(
      file_id,
      CacheEntry {
        meta,
        last_used: now_secs(),
      },
    );
  }

  fn prune(&mut self, max_entries: usize) {
    if self.entries.len() <= max_entries {
      return;
    }
    let mut by_age: Vec<(u64, u64)> = self
      .entries
      .iter()
      .map(|(id, entry)| (entry.last_used, *id))
      .collect();
    by_age.sort_unstable();
    let excess = self.entries.len() - max_entries;
    for (_, id) in by_age.into_iter().take(excess) {
      self.entries.remove(&id);
    }
  }

  /// Write the cache to `path`, keeping at most `max_entries` entries.
  pub fn save(&mut self, path: &Path, max_entries: usize) -> io::Result<()> {
    self.prune(max_entries);
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(&CacheFileRef {
      schema: CACHE_SCHEMA,
      entries: &self.entries,
    })?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn meta(project_id: u64) -> CurseForgeFileMeta {
    CurseForgeFileMeta {
      project_id,
      class_id: Some(6),
      download_url: Some("https://edge.forgecdn.net/files/1/2/a.jar".into()),
      file_name: "a.jar".into(),
      file_length: Some(10),
      sha1: Some("abc".into()),
    }
  }

  fn temp_path() -> PathBuf {
    std::env::temp_dir()
      .join(format!("ustbl-cf-cache-{}", uuid::Uuid::new_v4()))
      .join(CACHE_FILE_NAME)
  }

  fn cleanup(path: &Path) {
    let _ = fs::remove_dir_all(path.parent().unwrap());
  }

  #[test]
  fn a_stored_entry_is_found_after_reload() {
    let path = temp_path();
    let mut cache = FileMetaCache::default();
    cache.insert(42, meta(7));
    cache.save(&path, MAX_ENTRIES).unwrap();

    let loaded = FileMetaCache::load(&path);
    assert_eq!(loaded.get(42, 7), Some(meta(7)));
    assert!(!path.with_extension("json.tmp").exists());
    cleanup(&path);
  }

  #[test]
  fn an_unknown_file_or_another_project_misses() {
    let mut cache = FileMetaCache::default();
    cache.insert(42, meta(7));
    assert_eq!(cache.get(43, 7), None);
    assert_eq!(cache.get(42, 8), None);
  }

  #[test]
  fn a_missing_file_is_an_empty_cache() {
    assert_eq!(FileMetaCache::load(&temp_path()).len(), 0);
  }

  #[test]
  fn a_corrupt_file_is_an_empty_cache_and_gets_replaced_on_save() {
    let path = temp_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"{ not json").unwrap();

    let mut cache = FileMetaCache::load(&path);
    assert_eq!(cache.len(), 0);
    cache.insert(1, meta(2));
    cache.save(&path, MAX_ENTRIES).unwrap();
    assert_eq!(FileMetaCache::load(&path).get(1, 2), Some(meta(2)));
    cleanup(&path);
  }

  #[test]
  fn a_truncated_file_is_an_empty_cache() {
    let path = temp_path();
    let mut cache = FileMetaCache::default();
    cache.insert(1, meta(2));
    cache.save(&path, MAX_ENTRIES).unwrap();
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

    assert_eq!(FileMetaCache::load(&path).len(), 0);
    cleanup(&path);
  }

  #[test]
  fn another_schema_is_an_empty_cache() {
    let path = temp_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, br#"{"schema":999,"entries":{}}"#).unwrap();
    assert_eq!(FileMetaCache::load(&path).len(), 0);
    cleanup(&path);
  }

  #[test]
  fn saving_keeps_the_most_recently_used_entries() {
    let path = temp_path();
    let mut cache = FileMetaCache::default();
    for id in 1..=5 {
      cache.insert(id, meta(id));
    }
    for (id, entry) in cache.entries.iter_mut() {
      entry.last_used = 1000 + id;
    }
    // Entry 1 is the oldest until it is used again.
    cache.entries.get_mut(&1).unwrap().last_used = 5000;
    cache.touch(1);

    cache.save(&path, 3).unwrap();

    let loaded = FileMetaCache::load(&path);
    assert_eq!(loaded.len(), 3);
    assert!(loaded.get(1, 1).is_some());
    assert!(loaded.get(4, 4).is_some());
    assert!(loaded.get(5, 5).is_some());
    assert!(loaded.get(2, 2).is_none());
    cleanup(&path);
  }

  #[test]
  fn incomplete_answers_are_not_cacheable() {
    assert!(is_cacheable(&meta(1)));
    assert!(!is_cacheable(&CurseForgeFileMeta {
      sha1: None,
      ..meta(1)
    }));
    assert!(!is_cacheable(&CurseForgeFileMeta {
      class_id: None,
      ..meta(1)
    }));
  }
}
