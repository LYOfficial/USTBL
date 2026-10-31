use crate::error::USTBLResult;
use crate::instance::constants::{
  TRANSLATION_CACHE_EXPIRY_HOURS, TRANSLATION_CACHE_FILE_NAME, TRANSLATION_CACHE_MAX_ENTRIES,
  TRANSLATION_CACHE_RETENTION_HOURS,
};
use crate::resource::helpers::hashing::sha1_file_async;
use crate::storage::Storage;
use crate::APP_DATA_DIR;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

/// Bumped whenever the layout of the cache file changes. A file with any other
/// version (including the original layout keyed by file name, which has no
/// version at all) is ignored and rebuilt, except [`PREVIOUS_FORMAT_VERSION`],
/// which is upgraded.
pub const CACHE_FORMAT_VERSION: u32 = 3;
const PREVIOUS_FORMAT_VERSION: u32 = 2;

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

/// What the lookup of a jar has established so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EntryOrigin {
  /// Written by version 2 of the cache, which only knew Modrinth and did not say
  /// whether an empty entry meant "not on Modrinth". Replaced while loading.
  #[default]
  Legacy,
  /// The mod is on Modrinth.
  Modrinth,
  /// Modrinth answered that it does not know the jar; CurseForge has not been
  /// asked (or could not be reached), so the entry is not final.
  NotOnModrinth,
  /// Not on Modrinth, and CurseForge identified the jar.
  CurseForge,
  /// Neither Modrinth nor CurseForge knows the jar.
  Nowhere,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModTranslationEntry {
  pub translated_name: Option<String>,
  pub translated_description: Option<String>,
  pub timestamp: u64,
  #[serde(default)]
  pub origin: EntryOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
  pub size: u64,
  pub modified_ms: u64,
  pub sha1: String,
  pub last_seen: u64,
}

pub fn now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

impl LocalModTranslationEntry {
  /// An entry for a mod found on Modrinth.
  pub fn new(translated_name: Option<String>, translated_description: Option<String>) -> Self {
    Self::with_origin(
      EntryOrigin::Modrinth,
      translated_name,
      translated_description,
    )
  }

  pub fn with_origin(
    origin: EntryOrigin,
    translated_name: Option<String>,
    translated_description: Option<String>,
  ) -> Self {
    Self {
      translated_name,
      translated_description,
      timestamp: now_secs(),
      origin,
    }
  }

  /// Whether CurseForge still has to be asked about this jar, however recent
  /// the entry is.
  pub fn needs_curseforge(&self) -> bool {
    self.origin == EntryOrigin::NotOnModrinth
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
      Ok(cache) if cache.version == PREVIOUS_FORMAT_VERSION => cache.upgraded_from_previous(),
      _ => Self::default(),
    }
  }

  /// Version 2 held Modrinth results only. An entry with a name or description
  /// came from a Modrinth project and is kept. An empty one is either "not on
  /// Modrinth" or a Modrinth project with nothing to show, which the file does
  /// not tell apart, so it is dropped and looked up again (the recorded file
  /// hashes stay, so no jar is read again).
  fn upgraded_from_previous(mut self) -> Self {
    self.version = CACHE_FORMAT_VERSION;
    self.translations.retain(|_, entry| {
      if entry.origin != EntryOrigin::Legacy {
        return true;
      }
      if entry.translated_name.is_none() && entry.translated_description.is_none() {
        return false;
      }
      entry.origin = EntryOrigin::Modrinth;
      true
    });
    self
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

  /// Store the outcome of a Modrinth lookup. "Not on Modrinth" says nothing
  /// about CurseForge, so when the entry it replaces came from CurseForge (or
  /// already carries such a result) the name and description are kept until
  /// CurseForge has answered again; a failed CurseForge request then leaves the
  /// old result in place instead of an empty one. Returns the stored entry.
  pub fn insert_modrinth_outcome(
    &mut self,
    sha1: &str,
    mut entry: LocalModTranslationEntry,
  ) -> LocalModTranslationEntry {
    if entry.origin == EntryOrigin::NotOnModrinth {
      if let Some(previous) = self.translations.get(sha1).filter(|previous| {
        matches!(
          previous.origin,
          EntryOrigin::CurseForge | EntryOrigin::NotOnModrinth
        )
      }) {
        entry.translated_name = previous.translated_name.clone();
        entry.translated_description = previous.translated_description.clone();
      }
    }
    self.translations.insert(sha1.to_string(), entry.clone());
    entry
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

pub fn with_cache<R>(app: &AppHandle, f: impl FnOnce(&mut LocalModTranslationsCache) -> R) -> R {
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

#[cfg(test)]
mod tests {
  use super::*;

  fn entry_at(timestamp: u64) -> LocalModTranslationEntry {
    LocalModTranslationEntry {
      translated_name: Some("名".into()),
      translated_description: None,
      timestamp,
      origin: EntryOrigin::Modrinth,
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
  fn version_2_cache_keeps_named_entries_and_drops_empty_ones() {
    let old = r#"{"version":2,
      "translations":{
        "named":{"translated_name":"钠","translated_description":null,"timestamp":5},
        "described":{"translated_name":null,"translated_description":"描述","timestamp":5},
        "empty":{"translated_name":null,"translated_description":null,"timestamp":5}},
      "files":{"/m/a.jar":{"size":1,"modified_ms":2,"sha1":"empty","last_seen":5}}}"#;
    let cache = LocalModTranslationsCache::from_json(old);

    assert_eq!(cache.version, CACHE_FORMAT_VERSION);
    assert_eq!(cache.entry("named").unwrap().origin, EntryOrigin::Modrinth);
    assert_eq!(
      cache.entry("described").unwrap().origin,
      EntryOrigin::Modrinth
    );
    // An empty v2 entry may have been "not on Modrinth", so it must not block CurseForge.
    assert!(cache.entry("empty").is_none());
    // The hash of the jar is still known, so it is not read again.
    assert_eq!(cache.files["/m/a.jar"].sha1, "empty");
  }

  fn curseforge_entry(timestamp: u64) -> LocalModTranslationEntry {
    LocalModTranslationEntry {
      translated_name: Some("物品管理器".into()),
      translated_description: Some("显示配方".into()),
      timestamp,
      origin: EntryOrigin::CurseForge,
    }
  }

  fn not_on_modrinth() -> LocalModTranslationEntry {
    LocalModTranslationEntry::with_origin(EntryOrigin::NotOnModrinth, None, None)
  }

  #[test]
  fn expired_curseforge_entry_keeps_its_names_when_modrinth_still_lacks_the_jar() {
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("s", curseforge_entry(1));

    // Modrinth is asked again after expiry and still does not know the jar;
    // CurseForge then fails, so nothing else is written.
    let stored = cache.insert_modrinth_outcome("s", not_on_modrinth());

    for entry in [&stored, cache.entry("s").unwrap()] {
      assert_eq!(entry.origin, EntryOrigin::NotOnModrinth);
      assert!(entry.needs_curseforge());
      assert_eq!(entry.translated_name.as_deref(), Some("物品管理器"));
      assert_eq!(entry.translated_description.as_deref(), Some("显示配方"));
    }
    // A fresh timestamp, so the next refresh goes straight to CurseForge.
    assert!(cache.entry("s").unwrap().timestamp > 1);
  }

  #[test]
  fn names_survive_repeated_modrinth_misses_until_curseforge_answers() {
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("s", curseforge_entry(1));
    cache.insert_modrinth_outcome("s", not_on_modrinth());
    cache.insert_modrinth_outcome("s", not_on_modrinth());
    assert_eq!(
      cache.entry("s").unwrap().translated_name.as_deref(),
      Some("物品管理器")
    );
  }

  #[test]
  fn a_curseforge_conclusion_replaces_the_kept_names() {
    let mut cache = LocalModTranslationsCache::default();
    cache.insert_entry("s", curseforge_entry(1));
    cache.insert_modrinth_outcome("s", not_on_modrinth());

    let updated =
      LocalModTranslationEntry::with_origin(EntryOrigin::CurseForge, Some("新名".into()), None);
    cache.insert_entry("s", updated);
    let entry = cache.entry("s").unwrap();
    assert_eq!(entry.translated_name.as_deref(), Some("新名"));
    assert_eq!(entry.translated_description, None);

    cache.insert_entry(
      "s",
      LocalModTranslationEntry::with_origin(EntryOrigin::Nowhere, None, None),
    );
    let entry = cache.entry("s").unwrap();
    assert_eq!(entry.origin, EntryOrigin::Nowhere);
    assert!(entry.translated_name.is_none() && entry.translated_description.is_none());
  }

  #[test]
  fn only_curseforge_results_are_carried_over_a_modrinth_miss() {
    let mut cache = LocalModTranslationsCache::default();
    // A name from a Modrinth project is not carried over, nor is anything for a new hash.
    cache.insert_entry("moved", entry_at(1));
    cache.insert_modrinth_outcome("moved", not_on_modrinth());
    cache.insert_modrinth_outcome("new", not_on_modrinth());
    assert!(cache.entry("moved").unwrap().translated_name.is_none());
    assert!(cache.entry("new").unwrap().translated_name.is_none());

    // Modrinth hits are stored as they are.
    cache.insert_entry("c", curseforge_entry(1));
    let hit = LocalModTranslationEntry::new(Some("钠".into()), None);
    cache.insert_modrinth_outcome("c", hit);
    assert_eq!(cache.entry("c").unwrap().origin, EntryOrigin::Modrinth);
    assert_eq!(
      cache.entry("c").unwrap().translated_name.as_deref(),
      Some("钠")
    );
  }

  #[test]
  fn origins_round_trip_and_only_not_on_modrinth_needs_curseforge() {
    let mut cache = LocalModTranslationsCache::default();
    for (key, origin) in [
      ("a", EntryOrigin::Modrinth),
      ("b", EntryOrigin::NotOnModrinth),
      ("c", EntryOrigin::CurseForge),
      ("d", EntryOrigin::Nowhere),
    ] {
      cache.insert_entry(
        key,
        LocalModTranslationEntry::with_origin(origin, None, None),
      );
    }
    let loaded = LocalModTranslationsCache::from_json(&serde_json::to_string(&cache).unwrap());
    let needs: Vec<&str> = ["a", "b", "c", "d"]
      .into_iter()
      .filter(|key| loaded.entry(key).unwrap().needs_curseforge())
      .collect();
    assert_eq!(needs, vec!["b"]);
    assert_eq!(loaded.entry("c").unwrap().origin, EntryOrigin::CurseForge);
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
