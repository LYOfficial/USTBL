//! Remote information (Chinese name and description) for the jars in an
//! instance's mods folder, fetched in batches.
//!
//! For `n` uncached mods the lookup takes three kinds of requests instead of
//! three per mod: `POST /version_files` (SHA-1 to project), `GET /projects?ids=`
//! (project to slug and type) and the batched MCIM translation. Requests go
//! through the same source order as every other Modrinth call.
//!
//! What gets remembered follows what was actually learned:
//! - a hash or project the first source lacks is asked again from the next
//!   source (MCIM only knows what it has cached); one that the last source
//!   answered without is a definite "not on Modrinth" and is cached like any
//!   other result;
//! - a failed request leaves the affected mods without an entry, so they are
//!   asked again next time. There is no per-mod fallback after a batch fails, to
//!   keep a bad connection from multiplying into hundreds of requests;
//! - a mod whose translation request failed is shown with what is known but not
//!   cached.

use crate::instance::constants::TRANSLATION_CACHE_EXPIRY_HOURS;
use crate::instance::helpers::mods::translation::{
  file_stamp, now_secs, sha1_of_mod, with_cache, LocalModTranslationEntry,
};
use crate::instance::models::misc::LocalModInfo;
use crate::resource::helpers::batch_lookup::LookupOutcome;
use crate::resource::helpers::misc::local_translated_name;
use crate::resource::helpers::modrinth::misc::{
  fetch_project_ids_by_hashes_modrinth, fetch_projects_modrinth, ModrinthProjectBrief,
  PROJECTS_BATCH_SIZE, VERSION_FILES_BATCH_SIZE,
};
use crate::resource::helpers::translation::{
  translate_resource_descriptions, translation_is_known,
};
use crate::resource::models::OtherResourceSource;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tauri::AppHandle;

/// Mods hashed at the same time. Hashing is disk-bound, so a handful is plenty.
const MAX_CONCURRENT_HASHES: usize = 4;

#[derive(Debug, Clone)]
pub struct RemoteModResolution {
  pub entry: LocalModTranslationEntry,
  /// Whether the entry reflects a complete answer and may be remembered.
  pub cacheable: bool,
}

#[derive(Debug, Default)]
pub struct TranslationLookup {
  pub found: HashMap<String, String>,
  /// Ids whose translation request failed (as opposed to having no translation).
  pub failed: HashSet<String>,
}

/// The network side of the lookup; a trait so the batching and caching rules can
/// be tested without a network.
#[allow(async_fn_in_trait)]
pub trait RemoteModLookup {
  /// `sha1 -> project id`. Hashes in neither field of the outcome could not be
  /// answered because every request failed.
  async fn project_ids_by_hash(&self, hashes: &[String]) -> LookupOutcome<String, String>;
  /// Projects by id, with the same outcome rules.
  async fn projects(&self, ids: &[String]) -> LookupOutcome<String, ModrinthProjectBrief>;
  async fn translations(&self, project_ids: &[String]) -> TranslationLookup;
  fn local_name(&self, project: &ModrinthProjectBrief) -> Option<String>;
}

fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
  let mut seen = HashSet::new();
  values
    .into_iter()
    .filter(|value| !value.is_empty() && seen.insert(value.clone()))
    .collect()
}

fn not_on_modrinth() -> RemoteModResolution {
  RemoteModResolution {
    entry: LocalModTranslationEntry::new(None, None),
    cacheable: true,
  }
}

/// Resolve `hashes` to names and descriptions. Hashes that could not be looked up
/// because a request failed are absent from the result.
pub async fn resolve_remote_mod_infos<L: RemoteModLookup>(
  lookup: &L,
  hashes: &[String],
) -> HashMap<String, RemoteModResolution> {
  let hashes = unique(hashes.iter().map(|hash| hash.to_lowercase()));
  let mut resolved = HashMap::new();

  // Batches run one after another so a large list stays within the mirror's
  // concurrency budget.
  let mut hash_projects: Vec<(String, String)> = Vec::new();
  for chunk in hashes.chunks(VERSION_FILES_BATCH_SIZE) {
    let outcome = lookup.project_ids_by_hash(chunk).await;
    for hash in chunk {
      if let Some(project_id) = outcome.found.get(hash) {
        hash_projects.push((hash.clone(), project_id.clone()));
      } else if outcome.not_found.contains(hash) {
        resolved.insert(hash.clone(), not_on_modrinth());
      }
    }
  }

  let project_ids = unique(hash_projects.iter().map(|(_, id)| id.clone()));
  let mut projects: HashMap<String, ModrinthProjectBrief> = HashMap::new();
  let mut failed_projects: HashSet<String> = HashSet::new();
  for chunk in project_ids.chunks(PROJECTS_BATCH_SIZE) {
    let outcome = lookup.projects(chunk).await;
    for id in chunk {
      if let Some(project) = outcome.found.get(id) {
        projects.insert(id.clone(), project.clone());
      } else if !outcome.not_found.contains(id) {
        failed_projects.insert(id.clone());
      }
    }
  }

  let translatable: Vec<String> = projects.keys().cloned().collect();
  let translations = if translatable.is_empty() {
    TranslationLookup::default()
  } else {
    lookup.translations(&translatable).await
  };

  for (hash, project_id) in hash_projects {
    if failed_projects.contains(&project_id) {
      continue;
    }
    let resolution = match projects.get(&project_id) {
      None => not_on_modrinth(),
      Some(project) => RemoteModResolution {
        entry: LocalModTranslationEntry::new(
          lookup.local_name(project),
          translations.found.get(&project_id).cloned(),
        ),
        cacheable: !translations.failed.contains(&project_id),
      },
    };
    resolved.insert(hash, resolution);
  }

  resolved
}

struct ModrinthLookup<'a> {
  app: &'a AppHandle,
}

impl RemoteModLookup for ModrinthLookup<'_> {
  async fn project_ids_by_hash(&self, hashes: &[String]) -> LookupOutcome<String, String> {
    fetch_project_ids_by_hashes_modrinth(self.app, hashes).await
  }

  async fn projects(&self, ids: &[String]) -> LookupOutcome<String, ModrinthProjectBrief> {
    fetch_projects_modrinth(self.app, ids).await
  }

  async fn translations(&self, project_ids: &[String]) -> TranslationLookup {
    let found =
      translate_resource_descriptions(self.app, &OtherResourceSource::Modrinth, project_ids).await;
    let failed = project_ids
      .iter()
      .filter(|id| !translation_is_known(&OtherResourceSource::Modrinth, id))
      .cloned()
      .collect();
    TranslationLookup { found, failed }
  }

  fn local_name(&self, project: &ModrinthProjectBrief) -> Option<String> {
    local_translated_name(
      self.app,
      &project.slug,
      &project.project_type,
      &OtherResourceSource::Modrinth,
    )
  }
}

/// SHA-1 of each path, in order; `None` for what cannot be read.
pub async fn hash_mods(app: &AppHandle, paths: &[PathBuf]) -> Vec<Option<String>> {
  futures::stream::iter(paths.to_vec())
    .map(|path| async move { sha1_of_mod(app, &path).await })
    .buffered(MAX_CONCURRENT_HASHES)
    .collect()
    .await
}

/// Entries for `hashes`: from the cache when fresh, otherwise looked up in
/// batches. Complete answers are written back to the cache (in memory; call
/// `persist_local_mod_cache` to save). Hashes whose lookup failed are absent.
pub async fn refresh_remote_infos(
  app: &AppHandle,
  hashes: &[String],
) -> HashMap<String, LocalModTranslationEntry> {
  let now = now_secs();
  let mut entries = HashMap::new();
  let mut unknown = Vec::new();
  with_cache(app, |cache| {
    for hash in unique(hashes.iter().map(|hash| hash.to_lowercase())) {
      match cache.fresh_entry(&hash, now) {
        Some(entry) => {
          entries.insert(hash, entry.clone());
        }
        None => unknown.push(hash),
      }
    }
  });
  if unknown.is_empty() {
    return entries;
  }

  let started = std::time::Instant::now();
  let resolved = resolve_remote_mod_infos(&ModrinthLookup { app }, &unknown).await;
  log::debug!(
    "Mod lookup: {} cached, {} asked, {} answered, {} ms",
    entries.len(),
    unknown.len(),
    resolved.len(),
    started.elapsed().as_millis()
  );

  with_cache(app, |cache| {
    for (hash, resolution) in &resolved {
      if resolution.cacheable {
        cache.insert_entry(hash, resolution.entry.clone());
      }
    }
  });
  entries.extend(
    resolved
      .into_iter()
      .map(|(hash, resolution)| (hash, resolution.entry)),
  );
  entries
}

pub fn apply_entry(mod_info: &mut LocalModInfo, entry: &LocalModTranslationEntry) {
  mod_info.translated_name = entry.translated_name.clone();
  mod_info.translated_description = entry.translated_description.clone();
}

/// Fill in what the cache already knows, without reading any jar or touching the
/// network. A jar is known when its path, size and modification time match a
/// recorded hash that has an entry; entries past their expiry are still shown
/// while they are refreshed.
///
/// Returns the paths that need a lookup: unhashed jars and jars whose entry is
/// missing or expired.
pub fn apply_cached_infos(app: &AppHandle, mods: &mut [LocalModInfo]) -> Vec<PathBuf> {
  let now = now_secs();
  let stamps: Vec<Option<(u64, u64)>> = mods
    .iter()
    .map(|mod_info| file_stamp(&mod_info.file_path))
    .collect();

  with_cache(app, |cache| {
    let mut pending = Vec::new();
    for (mod_info, stamp) in mods.iter_mut().zip(stamps) {
      let key = mod_info.file_path.to_string_lossy().to_string();
      let sha1 =
        stamp.and_then(|(size, modified_ms)| cache.known_sha1(&key, size, modified_ms, now));
      let entry = sha1.as_deref().and_then(|sha1| cache.entry(sha1));
      match entry {
        Some(entry) => {
          apply_entry(mod_info, entry);
          if entry.is_expired_at(now, TRANSLATION_CACHE_EXPIRY_HOURS) {
            pending.push(mod_info.file_path.clone());
          }
        }
        None => pending.push(mod_info.file_path.clone()),
      }
    }
    pending
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  #[derive(Default)]
  struct FakeLookup {
    projects_of_hash: HashMap<String, String>,
    projects: HashMap<String, ModrinthProjectBrief>,
    translations: HashMap<String, String>,
    fail_hashes: bool,
    fail_projects: HashSet<String>,
    fail_translation: bool,
    calls: Mutex<Vec<(&'static str, usize)>>,
  }

  fn project(id: &str, slug: &str) -> ModrinthProjectBrief {
    ModrinthProjectBrief {
      id: id.into(),
      slug: slug.into(),
      project_type: "mod".into(),
    }
  }

  impl RemoteModLookup for FakeLookup {
    async fn project_ids_by_hash(&self, hashes: &[String]) -> LookupOutcome<String, String> {
      self.calls.lock().unwrap().push(("versions", hashes.len()));
      let mut outcome = LookupOutcome::default();
      if self.fail_hashes {
        return outcome;
      }
      for hash in hashes {
        match self.projects_of_hash.get(hash) {
          Some(project_id) => {
            outcome.found.insert(hash.clone(), project_id.clone());
          }
          None => {
            outcome.not_found.insert(hash.clone());
          }
        }
      }
      outcome
    }

    async fn projects(&self, ids: &[String]) -> LookupOutcome<String, ModrinthProjectBrief> {
      self.calls.lock().unwrap().push(("projects", ids.len()));
      let mut outcome = LookupOutcome::default();
      if ids.iter().any(|id| self.fail_projects.contains(id)) {
        return outcome;
      }
      for id in ids {
        match self.projects.get(id) {
          Some(project) => {
            outcome.found.insert(id.clone(), project.clone());
          }
          None => {
            outcome.not_found.insert(id.clone());
          }
        }
      }
      outcome
    }

    async fn translations(&self, ids: &[String]) -> TranslationLookup {
      self.calls.lock().unwrap().push(("translations", ids.len()));
      if self.fail_translation {
        return TranslationLookup {
          found: HashMap::new(),
          failed: ids.iter().cloned().collect(),
        };
      }
      TranslationLookup {
        found: ids
          .iter()
          .filter_map(|id| Some((id.clone(), self.translations.get(id)?.clone())))
          .collect(),
        failed: HashSet::new(),
      }
    }

    fn local_name(&self, project: &ModrinthProjectBrief) -> Option<String> {
      (project.slug == "sodium").then(|| "钠".to_string())
    }
  }

  fn hashes(list: &[&str]) -> Vec<String> {
    list.iter().map(|hash| hash.to_string()).collect()
  }

  fn lookup() -> FakeLookup {
    FakeLookup {
      projects_of_hash: HashMap::from([
        ("h1".into(), "P1".into()),
        ("h2".into(), "P2".into()),
        // Two files of the same project.
        ("h3".into(), "P1".into()),
      ]),
      projects: HashMap::from([
        ("P1".into(), project("P1", "sodium")),
        ("P2".into(), project("P2", "lithium")),
      ]),
      translations: HashMap::from([("P1".into(), "渲染优化".into())]),
      ..Default::default()
    }
  }

  #[tokio::test]
  async fn many_mods_take_one_request_per_kind() {
    let fake = lookup();
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h1", "h2", "h3", "h1"])).await;

    assert_eq!(
      *fake.calls.lock().unwrap(),
      vec![("versions", 3), ("projects", 2), ("translations", 2)]
    );
    assert_eq!(result.len(), 3);
    let first = &result["h1"];
    assert!(first.cacheable);
    assert_eq!(first.entry.translated_name.as_deref(), Some("钠"));
    assert_eq!(
      first.entry.translated_description.as_deref(),
      Some("渲染优化")
    );
    assert_eq!(
      result["h3"].entry.translated_name,
      first.entry.translated_name
    );
    // Known project without any translation: a complete answer.
    assert!(result["h2"].cacheable);
    assert_eq!(result["h2"].entry.translated_description, None);
  }

  #[tokio::test]
  async fn hashes_missing_from_a_successful_reply_are_cached_as_not_found() {
    let fake = lookup();
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h1", "unknown"])).await;
    let missing = &result["unknown"];
    assert!(missing.cacheable);
    assert_eq!(missing.entry.translated_name, None);
    assert_eq!(missing.entry.translated_description, None);

    // Nothing else is requested for hashes that are not on Modrinth.
    let only_unknown = lookup();
    resolve_remote_mod_infos(&only_unknown, &hashes(&["unknown"])).await;
    assert_eq!(*only_unknown.calls.lock().unwrap(), vec![("versions", 1)]);
  }

  #[tokio::test]
  async fn a_project_missing_from_a_successful_reply_is_not_found() {
    let mut fake = lookup();
    fake.projects.remove("P2");
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h2"])).await;
    assert!(result["h2"].cacheable);
    assert_eq!(result["h2"].entry.translated_name, None);
  }

  #[tokio::test]
  async fn a_failed_version_lookup_yields_nothing_and_does_not_fan_out() {
    let mut fake = lookup();
    fake.fail_hashes = true;
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h1", "h2", "unknown"])).await;
    assert!(result.is_empty());
    assert_eq!(*fake.calls.lock().unwrap(), vec![("versions", 3)]);
  }

  #[tokio::test]
  async fn a_failed_project_lookup_only_affects_its_mods() {
    let mut fake = lookup();
    fake.fail_projects.insert("P2".into());
    // P1 and P2 are looked up in one request, so both fail together.
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h1", "h2", "unknown"])).await;
    assert!(!result.contains_key("h1") && !result.contains_key("h2"));
    assert!(result["unknown"].cacheable);
  }

  #[tokio::test]
  async fn failed_translations_are_shown_but_not_cached() {
    let mut fake = lookup();
    fake.fail_translation = true;
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h1"])).await;
    let resolution = &result["h1"];
    assert!(!resolution.cacheable);
    assert_eq!(resolution.entry.translated_name.as_deref(), Some("钠"));
    assert_eq!(resolution.entry.translated_description, None);
  }

  #[tokio::test]
  async fn large_lists_are_split_into_batches() {
    let fake = FakeLookup::default();
    let many: Vec<String> = (0..VERSION_FILES_BATCH_SIZE * 2 + 1)
      .map(|n| format!("hash{n}"))
      .collect();
    resolve_remote_mod_infos(&fake, &many).await;
    assert_eq!(
      *fake.calls.lock().unwrap(),
      vec![
        ("versions", VERSION_FILES_BATCH_SIZE),
        ("versions", VERSION_FILES_BATCH_SIZE),
        ("versions", 1)
      ]
    );
  }
}
