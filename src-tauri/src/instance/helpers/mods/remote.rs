//! Remote information (Chinese name and description) for the jars in an
//! instance's mods folder, fetched in batches.
//!
//! For `n` uncached mods the lookup takes three kinds of requests instead of
//! three per mod: `POST /version_files` (SHA-1 to project), `GET /projects?ids=`
//! (project to slug and type) and the batched MCIM translation. Requests go
//! through the same source order as every other Modrinth call. The first two are
//! split into small batches that run in parallel (see `MAX_CONCURRENT_BATCHES`),
//! because the mirror is slow per connection and large replies take long.
//!
//! What gets remembered follows what was actually learned:
//! - a hash or project the first source lacks is asked again from the next
//!   source (MCIM only knows what it has cached); one that the last source
//!   answered without is a definite "not on Modrinth" and is cached like any
//!   other result;
//! - a failed request leaves the mods of that batch without an entry, so they are
//!   asked again next time; other batches are unaffected. There is no per-mod
//!   fallback after a batch fails, to keep a bad connection from multiplying into
//!   hundreds of requests;
//! - a mod whose translation request failed is shown with what is known but not
//!   cached.
//!
//! A jar Modrinth does not know is then looked up on CurseForge, again in
//! batches: `POST /fingerprints` (MurmurHash2 to mod id), `POST /mods` (mod id to
//! slug and class) and the batched MCIM translation. Only those jars are
//! fingerprinted. If the fingerprint lookup fails, nothing is concluded about
//! them: the cache keeps "not on Modrinth, CurseForge not asked yet" and the next
//! refresh asks CurseForge again without repeating the Modrinth requests.

use crate::instance::constants::TRANSLATION_CACHE_EXPIRY_HOURS;
use crate::instance::helpers::mods::translation::{
  file_stamp, now_secs, sha1_of_mod, with_cache, EntryOrigin, LocalModTranslationEntry,
};
use crate::instance::models::misc::LocalModInfo;
use crate::resource::helpers::batch_lookup::LookupOutcome;
use crate::resource::helpers::curseforge::fetch_mods_curseforge;
use crate::resource::helpers::curseforge::fetch_remote_resources_by_fingerprints_curseforge;
use crate::resource::helpers::curseforge::misc::{resource_type_of_class_id, CurseForgeMod};
use crate::resource::helpers::hashing::curseforge_fingerprint_file_async;
use crate::resource::helpers::mcim::MCIM_MAX_CONCURRENT_API_REQUESTS;
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

/// Lookup batches in flight at the same time. Follows
/// `MCIM_MAX_CONCURRENT_API_REQUESTS`, the launcher's own budget for mirror API
/// requests: a batch beyond it would only queue for a permit. Official sources
/// take no permit, but are held to the same bound so a large mod list does not
/// open dozens of connections.
const MAX_CONCURRENT_BATCHES: usize = MCIM_MAX_CONCURRENT_API_REQUESTS;

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

/// Modrinth does not know the jar. Final only once CurseForge has been asked,
/// but definite as far as Modrinth goes, so it is cached and CurseForge is
/// queried from there.
fn not_on_modrinth() -> RemoteModResolution {
  RemoteModResolution {
    entry: LocalModTranslationEntry::with_origin(EntryOrigin::NotOnModrinth, None, None),
    cacheable: true,
  }
}

fn batches_of(values: &[String], size: usize) -> Vec<Vec<String>> {
  values.chunks(size).map(<[String]>::to_vec).collect()
}

/// Resolve `hashes` to names and descriptions. Hashes that could not be looked up
/// because a request failed are absent from the result.
pub async fn resolve_remote_mod_infos<L: RemoteModLookup>(
  lookup: &L,
  hashes: &[String],
) -> HashMap<String, RemoteModResolution> {
  let hashes = unique(hashes.iter().map(|hash| hash.to_lowercase()));
  let mut resolved = HashMap::new();

  // Each batch has its own source fallback and its own failure; a failed batch
  // leaves only its own hashes unanswered. `buffered` keeps at most
  // MAX_CONCURRENT_BATCHES in flight and yields in batch order, not completion
  // order, so the merge below does not depend on which batch finished first.
  // Batches are owned: a closure over borrowed slices makes the future fail
  // `tokio::spawn`'s Send check ("FnOnce is not general enough").
  let mut hash_projects: Vec<(String, String)> = Vec::new();
  let batches: Vec<_> = futures::stream::iter(batches_of(&hashes, VERSION_FILES_BATCH_SIZE))
    .map(|chunk| async move {
      let outcome = lookup.project_ids_by_hash(&chunk).await;
      (chunk, outcome)
    })
    .buffered(MAX_CONCURRENT_BATCHES)
    .collect()
    .await;
  for (chunk, outcome) in batches {
    for hash in &chunk {
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
  let batches: Vec<_> = futures::stream::iter(batches_of(&project_ids, PROJECTS_BATCH_SIZE))
    .map(|chunk| async move {
      let outcome = lookup.projects(&chunk).await;
      (chunk, outcome)
    })
    .buffered(MAX_CONCURRENT_BATCHES)
    .collect()
    .await;
  for (chunk, outcome) in batches {
    for id in &chunk {
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

/// The network side of the CurseForge lookup; a trait for the same reason as
/// [`RemoteModLookup`].
#[allow(async_fn_in_trait)]
pub trait CurseForgeModLookup {
  /// `fingerprint -> mod id` for the fingerprints CurseForge knows. `None` when
  /// the lookup failed, in which case nothing is known about any of them.
  async fn mod_ids_by_fingerprint(&self, fingerprints: &[u32]) -> Option<HashMap<u32, u64>>;
  /// Mods by id, with the same outcome rules as [`RemoteModLookup::projects`].
  async fn mods(&self, ids: &[u64]) -> LookupOutcome<u64, CurseForgeMod>;
  async fn translations(&self, mod_ids: &[String]) -> TranslationLookup;
  fn local_name(&self, project: &CurseForgeMod) -> Option<String>;
}

fn nowhere() -> RemoteModResolution {
  RemoteModResolution {
    entry: LocalModTranslationEntry::with_origin(EntryOrigin::Nowhere, None, None),
    cacheable: true,
  }
}

/// Resolve jars that Modrinth does not know. `fingerprints` maps the SHA-1 of
/// each jar to its CurseForge fingerprint. Jars whose lookup failed are absent
/// from the result.
pub async fn resolve_curseforge_mod_infos<L: CurseForgeModLookup>(
  lookup: &L,
  fingerprints: &HashMap<String, u32>,
) -> HashMap<String, RemoteModResolution> {
  let mut asked: Vec<u32> = fingerprints.values().copied().collect();
  asked.sort_unstable();
  asked.dedup();
  if asked.is_empty() {
    return HashMap::new();
  }
  // A failed fingerprint lookup settles nothing, so there is no follow-up request.
  let Some(mod_id_of) = lookup.mod_ids_by_fingerprint(&asked).await else {
    return HashMap::new();
  };

  let mut mod_ids: Vec<u64> = mod_id_of.values().copied().collect();
  mod_ids.sort_unstable();
  mod_ids.dedup();
  let (mods, translations) = if mod_ids.is_empty() {
    (LookupOutcome::default(), TranslationLookup::default())
  } else {
    let mods = lookup.mods(&mod_ids).await;
    let translatable: Vec<String> = mods.found.keys().map(|id| id.to_string()).collect();
    let translations = if translatable.is_empty() {
      TranslationLookup::default()
    } else {
      lookup.translations(&translatable).await
    };
    (mods, translations)
  };

  let mut resolved = HashMap::new();
  for (hash, fingerprint) in fingerprints {
    let Some(mod_id) = mod_id_of.get(fingerprint) else {
      resolved.insert(hash.clone(), nowhere());
      continue;
    };
    match mods.found.get(mod_id) {
      Some(project) => {
        let key = mod_id.to_string();
        resolved.insert(
          hash.clone(),
          RemoteModResolution {
            entry: LocalModTranslationEntry::with_origin(
              EntryOrigin::CurseForge,
              lookup.local_name(project),
              translations.found.get(&key).cloned(),
            ),
            cacheable: !translations.failed.contains(&key),
          },
        );
      }
      None if mods.not_found.contains(mod_id) => {
        resolved.insert(hash.clone(), nowhere());
      }
      None => {}
    }
  }
  resolved
}

struct CurseForgeLookup<'a> {
  app: &'a AppHandle,
}

impl CurseForgeModLookup for CurseForgeLookup<'_> {
  async fn mod_ids_by_fingerprint(&self, fingerprints: &[u32]) -> Option<HashMap<u32, u64>> {
    let matches = fetch_remote_resources_by_fingerprints_curseforge(self.app, fingerprints)
      .await
      .ok()?;
    Some(
      matches
        .into_iter()
        .filter_map(|(fingerprint, file)| {
          let mod_id = file.resource_id.parse::<u64>().ok().filter(|id| *id != 0)?;
          Some((fingerprint, mod_id))
        })
        .collect(),
    )
  }

  async fn mods(&self, ids: &[u64]) -> LookupOutcome<u64, CurseForgeMod> {
    fetch_mods_curseforge(self.app, ids).await
  }

  async fn translations(&self, mod_ids: &[String]) -> TranslationLookup {
    let found =
      translate_resource_descriptions(self.app, &OtherResourceSource::CurseForge, mod_ids).await;
    let failed = mod_ids
      .iter()
      .filter(|id| !translation_is_known(&OtherResourceSource::CurseForge, id))
      .cloned()
      .collect();
    TranslationLookup { found, failed }
  }

  fn local_name(&self, project: &CurseForgeMod) -> Option<String> {
    local_translated_name(
      self.app,
      &project.slug,
      &resource_type_of_class_id(project.class_id),
      &OtherResourceSource::CurseForge,
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
    for (hash, resolution) in resolved {
      let entry = if resolution.cacheable {
        cache.insert_modrinth_outcome(&hash, resolution.entry)
      } else {
        resolution.entry
      };
      entries.insert(hash, entry);
    }
  });
  entries
}

/// Ask CurseForge about jars that Modrinth does not know. `targets` pairs the
/// SHA-1 of each jar with a path to read it from; only these files are read.
/// Returns the entries that were settled and writes the cacheable ones to the
/// cache (in memory). Jars whose lookup failed, or that cannot be read, are
/// absent and keep their "not on Modrinth" entry.
pub async fn refresh_curseforge_infos(
  app: &AppHandle,
  targets: &[(String, PathBuf)],
) -> HashMap<String, LocalModTranslationEntry> {
  if targets.is_empty() {
    return HashMap::new();
  }
  let started = std::time::Instant::now();
  let fingerprints: HashMap<String, u32> = futures::stream::iter(targets.to_vec())
    .map(|(hash, path)| async move {
      let fingerprint = curseforge_fingerprint_file_async(path).await.ok()?;
      Some((hash, fingerprint))
    })
    .buffered(MAX_CONCURRENT_HASHES)
    .filter_map(|result| async move { result })
    .collect()
    .await;

  let resolved = resolve_curseforge_mod_infos(&CurseForgeLookup { app }, &fingerprints).await;
  log::debug!(
    "CurseForge mod lookup: {} asked, {} fingerprinted, {} answered, {} ms",
    targets.len(),
    fingerprints.len(),
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
  resolved
    .into_iter()
    .map(|(hash, resolution)| (hash, resolution.entry))
    .collect()
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
          if entry.needs_curseforge() || entry.is_expired_at(now, TRANSLATION_CACHE_EXPIRY_HOURS) {
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
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::sync::Mutex;

  #[derive(Default)]
  struct FakeLookup {
    projects_of_hash: HashMap<String, String>,
    projects: HashMap<String, ModrinthProjectBrief>,
    translations: HashMap<String, String>,
    fail_hashes: bool,
    /// A version batch containing any of these hashes fails.
    fail_hash_batches_with: HashSet<String>,
    fail_projects: HashSet<String>,
    fail_translation: bool,
    calls: Mutex<Vec<(&'static str, usize)>>,
    versions_in_flight: InFlight,
    projects_in_flight: InFlight,
  }

  /// Counts the requests of one kind that are in flight at the same time.
  #[derive(Default)]
  struct InFlight {
    now: AtomicUsize,
    peak: AtomicUsize,
  }

  impl InFlight {
    /// Stay in flight for a few polls, so batches that are allowed to overlap do.
    /// The earlier a request started, the longer it stays, so overlapping batches
    /// finish in reverse order.
    async fn occupy_a_slot(&self) {
      let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
      self.peak.fetch_max(now, Ordering::SeqCst);
      for _ in 0..(10usize.saturating_sub(2 * now)) {
        tokio::task::yield_now().await;
      }
      self.now.fetch_sub(1, Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
      self.peak.load(Ordering::SeqCst)
    }
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
      self.versions_in_flight.occupy_a_slot().await;
      let mut outcome = LookupOutcome::default();
      if self.fail_hashes
        || hashes
          .iter()
          .any(|hash| self.fail_hash_batches_with.contains(hash))
      {
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
      self.projects_in_flight.occupy_a_slot().await;
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
  async fn origins_tell_modrinth_hits_from_jars_modrinth_lacks() {
    let fake = lookup();
    let result = resolve_remote_mod_infos(&fake, &hashes(&["h1", "unknown"])).await;
    assert_eq!(result["h1"].entry.origin, EntryOrigin::Modrinth);
    assert_eq!(result["unknown"].entry.origin, EntryOrigin::NotOnModrinth);
    assert!(result["unknown"].entry.needs_curseforge());
    assert!(!result["h1"].entry.needs_curseforge());
  }

  #[derive(Default)]
  struct FakeCurseForge {
    mod_of_fingerprint: HashMap<u32, u64>,
    mods: HashMap<u64, CurseForgeMod>,
    mods_unanswered: bool,
    translations: HashMap<String, String>,
    fail_fingerprints: bool,
    fail_translation: bool,
    calls: Mutex<Vec<(&'static str, usize)>>,
  }

  fn cf_mod(id: u64, slug: &str) -> CurseForgeMod {
    serde_json::from_str(&format!(r#"{{"id":{id},"slug":"{slug}","classId":6}}"#)).unwrap()
  }

  impl CurseForgeModLookup for FakeCurseForge {
    async fn mod_ids_by_fingerprint(&self, fingerprints: &[u32]) -> Option<HashMap<u32, u64>> {
      self
        .calls
        .lock()
        .unwrap()
        .push(("fingerprints", fingerprints.len()));
      if self.fail_fingerprints {
        return None;
      }
      Some(
        fingerprints
          .iter()
          .filter_map(|fp| Some((*fp, *self.mod_of_fingerprint.get(fp)?)))
          .collect(),
      )
    }

    async fn mods(&self, ids: &[u64]) -> LookupOutcome<u64, CurseForgeMod> {
      self.calls.lock().unwrap().push(("mods", ids.len()));
      let mut outcome = LookupOutcome::default();
      if self.mods_unanswered {
        return outcome;
      }
      for id in ids {
        match self.mods.get(id) {
          Some(project) => {
            outcome.found.insert(*id, cf_mod(project.id, &project.slug));
          }
          None => {
            outcome.not_found.insert(*id);
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

    fn local_name(&self, project: &CurseForgeMod) -> Option<String> {
      (project.slug == "jei").then(|| "物品管理器".to_string())
    }
  }

  fn curseforge() -> FakeCurseForge {
    FakeCurseForge {
      mod_of_fingerprint: HashMap::from([(11, 238222), (22, 306612), (33, 238222)]),
      mods: HashMap::from([
        (238222, cf_mod(238222, "jei")),
        (306612, cf_mod(306612, "x")),
      ]),
      translations: HashMap::from([("238222".into(), "显示配方".into())]),
      ..Default::default()
    }
  }

  fn fingerprints(list: &[(&str, u32)]) -> HashMap<String, u32> {
    list
      .iter()
      .map(|(hash, fingerprint)| (hash.to_string(), *fingerprint))
      .collect()
  }

  #[tokio::test]
  async fn a_curseforge_hit_gets_its_name_and_description_in_one_request_per_kind() {
    let fake = curseforge();
    let result =
      resolve_curseforge_mod_infos(&fake, &fingerprints(&[("h1", 11), ("h2", 22), ("h3", 33)]))
        .await;

    // Two files of the same mod share one project and one translation.
    let mut calls = fake.calls.lock().unwrap().clone();
    calls.sort();
    assert_eq!(
      calls,
      vec![("fingerprints", 3), ("mods", 2), ("translations", 2)]
    );
    let hit = &result["h1"];
    assert!(hit.cacheable);
    assert_eq!(hit.entry.origin, EntryOrigin::CurseForge);
    assert_eq!(hit.entry.translated_name.as_deref(), Some("物品管理器"));
    assert_eq!(
      hit.entry.translated_description.as_deref(),
      Some("显示配方")
    );
    assert_eq!(
      result["h3"].entry.translated_name,
      hit.entry.translated_name
    );
    // A CurseForge mod without any translation is still a complete answer.
    assert!(result["h2"].cacheable);
    assert_eq!(result["h2"].entry.origin, EntryOrigin::CurseForge);
    assert_eq!(result["h2"].entry.translated_name, None);
  }

  #[tokio::test]
  async fn unknown_to_both_platforms_is_cached_as_nowhere() {
    let fake = curseforge();
    let result = resolve_curseforge_mod_infos(&fake, &fingerprints(&[("gone", 99)])).await;
    assert!(result["gone"].cacheable);
    assert_eq!(result["gone"].entry.origin, EntryOrigin::Nowhere);
    assert!(!result["gone"].entry.needs_curseforge());
    // No project or translation requests for fingerprints nobody knows.
    assert_eq!(*fake.calls.lock().unwrap(), vec![("fingerprints", 1)]);
  }

  #[tokio::test]
  async fn a_matched_mod_missing_from_the_project_reply_is_nowhere() {
    let mut fake = curseforge();
    fake.mods.remove(&306612);
    let result = resolve_curseforge_mod_infos(&fake, &fingerprints(&[("h2", 22)])).await;
    assert_eq!(result["h2"].entry.origin, EntryOrigin::Nowhere);
    assert!(result["h2"].cacheable);
  }

  #[tokio::test]
  async fn a_failed_fingerprint_lookup_concludes_nothing_and_stops() {
    let mut fake = curseforge();
    fake.fail_fingerprints = true;
    let result =
      resolve_curseforge_mod_infos(&fake, &fingerprints(&[("h1", 11), ("gone", 99)])).await;
    assert!(result.is_empty());
    assert_eq!(*fake.calls.lock().unwrap(), vec![("fingerprints", 2)]);
  }

  #[tokio::test]
  async fn a_failed_project_lookup_only_affects_the_matched_mods() {
    let mut fake = curseforge();
    fake.mods_unanswered = true;
    let result =
      resolve_curseforge_mod_infos(&fake, &fingerprints(&[("h1", 11), ("gone", 99)])).await;
    assert!(!result.contains_key("h1"));
    assert_eq!(result["gone"].entry.origin, EntryOrigin::Nowhere);
  }

  #[tokio::test]
  async fn failed_translations_are_shown_but_not_cached_for_curseforge_too() {
    let mut fake = curseforge();
    fake.fail_translation = true;
    let result = resolve_curseforge_mod_infos(&fake, &fingerprints(&[("h1", 11)])).await;
    let resolution = &result["h1"];
    assert!(!resolution.cacheable);
    assert_eq!(
      resolution.entry.translated_name.as_deref(),
      Some("物品管理器")
    );
    assert_eq!(resolution.entry.translated_description, None);
  }

  #[tokio::test]
  async fn nothing_to_ask_sends_no_request() {
    let fake = curseforge();
    assert!(resolve_curseforge_mod_infos(&fake, &HashMap::new())
      .await
      .is_empty());
    assert!(fake.calls.lock().unwrap().is_empty());
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

  /// `count` hashes `hash0..`, each belonging to its own project `project0..`.
  fn one_project_per_hash(count: usize) -> (FakeLookup, Vec<String>) {
    let mut fake = FakeLookup::default();
    for n in 0..count {
      fake
        .projects_of_hash
        .insert(format!("hash{n}"), format!("project{n}"));
      fake.projects.insert(
        format!("project{n}"),
        project(&format!("project{n}"), "sodium"),
      );
    }
    let all = (0..count).map(|n| format!("hash{n}")).collect();
    (fake, all)
  }

  #[tokio::test]
  async fn large_project_lists_are_split_into_batches() {
    let count = PROJECTS_BATCH_SIZE * 2 + 1;
    let (fake, all) = one_project_per_hash(count);
    let result = resolve_remote_mod_infos(&fake, &all).await;

    assert_eq!(result.len(), count);
    let calls = fake.calls.lock().unwrap();
    let mut project_batches: Vec<usize> = calls
      .iter()
      .filter(|(kind, _)| *kind == "projects")
      .map(|(_, size)| *size)
      .collect();
    project_batches.sort_unstable();
    assert_eq!(
      project_batches,
      vec![1, PROJECTS_BATCH_SIZE, PROJECTS_BATCH_SIZE]
    );
  }

  #[tokio::test]
  async fn version_batches_run_in_parallel_within_the_limit() {
    let fake = FakeLookup::default();
    let many: Vec<String> = (0..VERSION_FILES_BATCH_SIZE * 12)
      .map(|n| format!("hash{n}"))
      .collect();
    resolve_remote_mod_infos(&fake, &many).await;

    assert_eq!(fake.calls.lock().unwrap().len(), 12);
    assert!(fake.versions_in_flight.peak() > 1);
    assert!(fake.versions_in_flight.peak() <= MAX_CONCURRENT_BATCHES);
  }

  #[tokio::test]
  async fn project_batches_run_in_parallel_within_the_limit() {
    let (fake, all) = one_project_per_hash(PROJECTS_BATCH_SIZE * 12);
    // The version batches are counted apart from the project batches, so only
    // the overlap of the project requests is asserted here.
    let result = resolve_remote_mod_infos(&fake, &all).await;

    assert_eq!(result.len(), all.len());
    let project_calls = fake
      .calls
      .lock()
      .unwrap()
      .iter()
      .filter(|(kind, _)| *kind == "projects")
      .count();
    assert_eq!(project_calls, 12);
    assert!(fake.projects_in_flight.peak() > 1);
    assert!(fake.projects_in_flight.peak() <= MAX_CONCURRENT_BATCHES);
  }

  #[tokio::test]
  async fn a_failed_version_batch_only_affects_its_own_hashes() {
    let count = VERSION_FILES_BATCH_SIZE * 2 + 1;
    let mut fake = FakeLookup::default();
    let all: Vec<String> = (0..count).map(|n| format!("hash{n}")).collect();
    // hash130 is in the second batch.
    fake.fail_hash_batches_with.insert("hash130".into());
    let result = resolve_remote_mod_infos(&fake, &all).await;

    for (n, hash) in all.iter().enumerate() {
      let in_failed_batch = (VERSION_FILES_BATCH_SIZE..VERSION_FILES_BATCH_SIZE * 2).contains(&n);
      assert_eq!(result.contains_key(hash), !in_failed_batch, "{hash}");
    }
    // The failed batch is not retried per mod or in smaller pieces.
    assert_eq!(fake.calls.lock().unwrap().len(), 3);
  }

  #[tokio::test]
  async fn results_do_not_depend_on_which_batch_finishes_first() {
    let count = PROJECTS_BATCH_SIZE * 6;
    let (mut fake, all) = one_project_per_hash(count);
    // Give every other project a distinct slug so a mix-up between batches shows.
    for n in (0..count).step_by(2) {
      fake.projects.insert(
        format!("project{n}"),
        project(&format!("project{n}"), "lithium"),
      );
    }
    let result = resolve_remote_mod_infos(&fake, &all).await;

    assert_eq!(result.len(), count);
    for n in 0..count {
      let expected = (n % 2 == 1).then(|| "钠".to_string());
      assert_eq!(
        result[&format!("hash{n}")].entry.translated_name,
        expected,
        "hash{n}"
      );
    }
  }
}
