use crate::error::USTBLResult;
use crate::instance::constants::{
  TRANSLATION_CACHE_EXPIRY_HOURS, TRANSLATION_CACHE_FILE_NAME, TRANSLATION_CACHE_MAX_ENTRIES,
  TRANSLATION_CACHE_RETENTION_HOURS,
};
use crate::instance::models::misc::LocalModInfo;
use crate::resource::helpers::hashing::sha1_file_async;
use crate::resource::helpers::modrinth::{
  fetch_remote_resource_by_hash_modrinth, fetch_remote_resource_by_id_modrinth,
};
use crate::resource::helpers::translation::translation_is_known;
use crate::resource::models::OtherResourceSource;
use crate::storage::Storage;
use crate::APP_DATA_DIR;
use log::info;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

/// Bumped whenever the layout of the cache file changes. A file with any other
/// version (including the original layout keyed by file name, which has no
/// version at all) is ignored and rebuilt.
pub const CACHE_FORMAT_VERSION: u32 = 2;

/// Remote information about local mods, keyed by the SHA-1 of the jar so that a
/// replaced file never shows the data of its predecessor.
///
/// `files` remembers the SHA-1 computed for a path together with the size and
/// modification time it was computed at, so opening the mod list does not read
/// every jar again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModTranslationsCache {
  #[serde(default)]
  pub version: u32,
  #[serde(default)]
  pub translations: HashMap<String, LocalModTranslationEntry>,
  #[serde(default)]
  pub files: HashMap<String, FileStamp>,
}

impl Default for LocalModTranslationsCache {
  fn default() -> Self {
    Self {
      version: CACHE_FORMAT_VERSION,
      translations: HashMap::new(),
      files: HashMap::new(),
    }
  }
}

impl Storage for LocalModTranslationsCache {
  fn file_path() -> PathBuf {
    APP_DATA_DIR
      .get()
      .unwrap()
      .join(TRANSLATION_CACHE_FILE_NAME)
  }

  fn load() -> Result<Self, std::io::Error> {
    Ok(Self::from_json(
      &std::fs::read_to_string(Self::file_path())?,
    ))
  }

  /// Written to a temporary file first, so an interrupted write never leaves a
  /// truncated cache behind.
  fn save(&self) -> Result<(), std::io::Error> {
    let path = Self::file_path();
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_string_pretty(self)?)?;
    std::fs::rename(&temporary, &path)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModTranslationEntry {
  pub translated_name: Option<String>,
  pub translated_description: Option<String>,
  pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
  pub size: u64,
  pub modified_ms: u64,
  pub sha1: String,
  pub last_seen: u64,
}

fn now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

impl LocalModTranslationEntry {
  pub fn new(translated_name: Option<String>, translated_description: Option<String>) -> Self {
    Self {
      translated_name,
      translated_description,
      timestamp: now_secs(),
    }
  }

  pub fn is_expired_at(&self, now: u64, max_age_hours: u64) -> bool {
    now > self.timestamp + (max_age_hours * 60 * 60)
  }
}

impl LocalModTranslationsCache {
  /// Parse a cache file; anything unreadable or from another layout is an empty cache.
  pub fn from_json(json: &str) -> Self {
    match serde_json::from_str::<Self>(json) {
      Ok(cache) if cache.version == CACHE_FORMAT_VERSION => cache,
      _ => Self::default(),
    }
  }

  /// The SHA-1 recorded for `path`, if the file still has the recorded size and
  /// modification time.
  pub fn known_sha1(
    &mut self,
    path: &str,
    size: u64,
    modified_ms: u64,
    now: u64,
  ) -> Option<String> {
    let stamp = self.files.get_mut(path)?;
    if stamp.size != size || stamp.modified_ms != modified_ms {
      return None;
    }
    stamp.last_seen = now;
    Some(stamp.sha1.clone())
  }

  pub fn record_file(&mut self, path: &str, stamp: FileStamp) {
    self.files.insert(path.to_string(), stamp);
  }

  /// Entry for `sha1` regardless of age; callers decide whether it is stale.
  pub fn entry(&self, sha1: &str) -> Option<&LocalModTranslationEntry> {
    self.translations.get(sha1)
  }

  pub fn fresh_entry(&self, sha1: &str, now: u64) -> Option<&LocalModTranslationEntry> {
    self
      .entry(sha1)
      .filter(|entry| !entry.is_expired_at(now, TRANSLATION_CACHE_EXPIRY_HOURS))
  }

  pub fn insert_entry(&mut self, sha1: &str, entry: LocalModTranslationEntry) {
    self.translations.insert(sha1.to_string(), entry);
  }

  /// Drop entries nobody has refreshed for a long time and keep both maps below
  /// [`TRANSLATION_CACHE_MAX_ENTRIES`], oldest first.
  pub fn prune(&mut self, now: u64) {
    let cutoff = now.saturating_sub(TRANSLATION_CACHE_RETENTION_HOURS * 60 * 60);
    self
      .translations
      .retain(|_, entry| entry.timestamp >= cutoff);
    self.files.retain(|_, stamp| stamp.last_seen >= cutoff);

    if self.translations.len() > TRANSLATION_CACHE_MAX_ENTRIES {
      let mut by_age: Vec<(u64, String)> = self
        .translations
        .iter()
        .map(|(key, entry)| (entry.timestamp, key.clone()))
        .collect();
      by_age.sort();
      let excess = self.translations.len() - TRANSLATION_CACHE_MAX_ENTRIES;
      for (_, key) in by_age.into_iter().take(excess) {
        self.translations.remove(&key);
      }
    }
    if self.files.len() > TRANSLATION_CACHE_MAX_ENTRIES {
      let mut by_age: Vec<(u64, String)> = self
        .files
        .iter()
        .map(|(key, stamp)| (stamp.last_seen, key.clone()))
        .collect();
      by_age.sort();
      let excess = self.files.len() - TRANSLATION_CACHE_MAX_ENTRIES;
      for (_, key) in by_age.into_iter().take(excess) {
        self.files.remove(&key);
      }
    }
  }
}

fn with_cache<R>(app: &AppHandle, f: impl FnOnce(&mut LocalModTranslationsCache) -> R) -> R {
  let state = app.state::<Mutex<LocalModTranslationsCache>>();
  let mut guard = state
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner());
  f(&mut guard)
}

/// Size and modification time (milliseconds since the epoch) of a file.
pub fn file_stamp(path: &Path) -> Option<(u64, u64)> {
  let metadata = std::fs::metadata(path).ok()?;
  let modified = metadata
    .modified()
    .ok()?
    .duration_since(UNIX_EPOCH)
    .ok()?
    .as_millis() as u64;
  Some((metadata.len(), modified))
}

/// SHA-1 of a mod jar, read from the cache when the file is unchanged since it
/// was last hashed.
pub async fn sha1_of_mod(app: &AppHandle, path: &Path) -> Option<String> {
  let key = path.to_string_lossy().to_string();
  let (size, modified_ms) = file_stamp(path)?;
  let now = now_secs();
  if let Some(sha1) = with_cache(app, |cache| cache.known_sha1(&key, size, modified_ms, now)) {
    return Some(sha1);
  }

  let sha1 = sha1_file_async(path.to_path_buf()).await.ok()?;
  with_cache(app, |cache| {
    cache.record_file(
      &key,
      FileStamp {
        size,
        modified_ms,
        sha1: sha1.clone(),
        last_seen: now,
      },
    )
  });
  Some(sha1)
}

/// Prune and write the cache to disk.
pub fn persist_local_mod_cache(app: &AppHandle) -> USTBLResult<()> {
  with_cache(app, |cache| {
    cache.prune(now_secs());
    cache.save()
  })?;
  Ok(())
}

pub async fn add_local_mod_translations(
  app: &AppHandle,
  mod_info: &mut LocalModInfo,
) -> USTBLResult<()> {
  let file_name = mod_info.file_name.clone();
  let Some(sha1) = sha1_of_mod(app, &mod_info.file_path).await else {
    return Ok(());
  };

  let cached = with_cache(app, |cache| cache.fresh_entry(&sha1, now_secs()).cloned());
  if let Some(entry) = cached {
    info!("Using cached translation for mod: {}", file_name);
    mod_info.translated_name = entry.translated_name;
    mod_info.translated_description = entry.translated_description;
    return Ok(());
  }

  // Query Modrinth for translated metadata. A failure is not cached, so the next
  // listing tries again.
  let app_clone = app.clone();
  let hash = sha1.clone();
  let lookup = tokio::spawn(async move {
    let file_info = fetch_remote_resource_by_hash_modrinth(&app_clone, &hash).await?;
    fetch_remote_resource_by_id_modrinth(&app_clone, &file_info.resource_id).await
  })
  .await;

  if let Ok(Ok(resource_info)) = lookup {
    info!("Fetched translation for mod: {}", file_name);
    mod_info.translated_name = resource_info.translated_name.clone();
    mod_info.translated_description = resource_info.translated_description.clone();

    // The description translator swallows request failures; if it left no trace in
    // its own cache the result may be incomplete and must not be remembered.
    if translation_is_known(&OtherResourceSource::Modrinth, &resource_info.id) {
      with_cache(app, |cache| {
        cache.insert_entry(
          &sha1,
          LocalModTranslationEntry::new(
            resource_info.translated_name,
            resource_info.translated_description,
          ),
        )
      });
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn entry_at(timestamp: u64) -> LocalModTranslationEntry {
    LocalModTranslationEntry {
      translated_name: Some("名".into()),
      translated_description: None,
      timestamp,
    }
  }

  fn stamp(sha1: &str, last_seen: u64) -> FileStamp {
    FileStamp {
      size: 10,
      modified_ms: 20,
      sha1: sha1.into(),
      last_seen,
    }
  }

  #[test]
  fn old_file_name_keyed_cache_is_treated_as_empty() {
    let old =
      r#"{"sodium.jar":{"translated_name":"钠","translated_description":null,"timestamp":1}}"#;
    let cache = LocalModTranslationsCache::from_json(old);
    assert_eq!(cache.version, CACHE_FORMAT_VERSION);
    assert!(cache.translations.is_empty() && cache.files.is_empty());

    // A jar whose name collides with a field of the new layout must not break loading.
    let tricky = r#"{"files":{"translated_name":null,"timestamp":1}}"#;
    assert!(LocalModTranslationsCache::from_json(tricky)
      .translations
      .is_empty());
    assert!(LocalModTranslationsCache::from_json("garbage")
      .files
      .is_empty());
    assert!(LocalModTranslationsCache::from_json("").files.is_empty());
  }

  #[test]
  fn cache_round_trips_and_is_keyed_by_sha1() {
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("aa11", entry_at(100));
    cache.record_file("/mods/a.jar", stamp("aa11", 100));
    let json = serde_json::to_string(&cache).unwrap();
    let loaded = LocalModTranslationsCache::from_json(&json);
    assert_eq!(
      loaded.entry("aa11").unwrap().translated_name.as_deref(),
      Some("名")
    );
    // The file name is no longer a key.
    assert!(loaded.entry("a.jar").is_none());
  }

  #[test]
  fn replaced_jar_with_same_name_does_not_inherit_the_old_entry() {
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("old-sha1", entry_at(100));
    cache.record_file("/mods/a.jar", stamp("old-sha1", 100));

    // Same path, different size or modification time: the recorded hash is not trusted.
    assert_eq!(
      cache.known_sha1("/mods/a.jar", 10, 20, 150),
      Some("old-sha1".into())
    );
    assert_eq!(cache.known_sha1("/mods/a.jar", 11, 20, 150), None);
    assert_eq!(cache.known_sha1("/mods/a.jar", 10, 21, 150), None);
    assert_eq!(cache.known_sha1("/mods/b.jar", 10, 20, 150), None);
    // A new hash finds no entry, so the mod is looked up again.
    assert!(cache.entry("new-sha1").is_none());
  }

  #[test]
  fn entries_expire_after_the_configured_hours() {
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("s", entry_at(1_000));
    let limit = 1_000 + TRANSLATION_CACHE_EXPIRY_HOURS * 3600;
    assert!(cache.fresh_entry("s", limit).is_some());
    assert!(cache.fresh_entry("s", limit + 1).is_none());
    // Stale entries stay readable for callers that want to show them while refreshing.
    assert!(cache.entry("s").is_some());
  }

  #[test]
  fn prune_drops_old_entries_and_caps_both_maps() {
    let now = TRANSLATION_CACHE_RETENTION_HOURS * 3600 + 1_000_000;
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("ancient", entry_at(10));
    cache.record_file("/gone.jar", stamp("ancient", 10));
    for index in 0..TRANSLATION_CACHE_MAX_ENTRIES + 3 {
      cache.insert_entry(&format!("s{index}"), entry_at(now - 1000 + index as u64));
      cache.record_file(
        &format!("/m{index}.jar"),
        stamp("x", now - 1000 + index as u64),
      );
    }
    cache.prune(now);

    assert!(cache.entry("ancient").is_none());
    assert!(!cache.files.contains_key("/gone.jar"));
    assert_eq!(cache.translations.len(), TRANSLATION_CACHE_MAX_ENTRIES);
    assert_eq!(cache.files.len(), TRANSLATION_CACHE_MAX_ENTRIES);
    // The oldest survivors are the ones evicted.
    assert!(cache.entry("s0").is_none());
    assert!(cache
      .entry(&format!("s{}", TRANSLATION_CACHE_MAX_ENTRIES + 2))
      .is_some());
  }
}
