//! Batched, cached description translation through MCIM.
//!
//! One request carries up to [`TRANSLATION_BATCH_SIZE`] ids, and results live in a
//! process-wide cache keyed by source and id, so paging, going back and searching
//! again do not hit the mirror twice. The cache remembers "no translation exists"
//! as well, but never a transport failure: a network error or a malformed reply
//! leaves the ids uncached so the next call retries them.
//!
//! Entry points, for any caller that needs translations (resource search, the
//! local mod list, ...):
//! - [`translate_resource_descriptions`]: cache first, one batched request per
//!   [`TRANSLATION_BATCH_SIZE`] missing ids. Returns `id -> translated text` for
//!   the ids that have a translation; the rest are absent from the map.
//! - [`cached_translations`]: cache only, never touches the network.
//!
//! Ids are Modrinth project ids for [`OtherResourceSource::Modrinth`] and
//! CurseForge mod ids (decimal strings) for [`OtherResourceSource::CurseForge`].

use crate::resource::helpers::mcim::{acquire_mcim_api_permit_for, MCIM_BASE};
use crate::resource::models::{OtherResourceInfo, OtherResourceSource};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

/// Ids per batched request. A search page is at most 50 entries on CurseForge.
pub const TRANSLATION_BATCH_SIZE: usize = 50;
/// Upper bound on cached entries; the oldest entry is evicted first.
pub const TRANSLATION_CACHE_CAPACITY: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TranslationSource {
  Modrinth,
  CurseForge,
}

impl TranslationSource {
  pub fn from_resource_source(source: &OtherResourceSource) -> Option<Self> {
    match source {
      OtherResourceSource::Modrinth => Some(Self::Modrinth),
      OtherResourceSource::CurseForge => Some(Self::CurseForge),
      _ => None,
    }
  }
}

type CacheKey = (TranslationSource, String);

/// Bounded translation cache. `None` records that the mirror has no translation.
pub struct TranslationCache {
  entries: HashMap<CacheKey, Option<String>>,
  order: VecDeque<CacheKey>,
  capacity: usize,
}

impl TranslationCache {
  pub fn new(capacity: usize) -> Self {
    Self {
      entries: HashMap::new(),
      order: VecDeque::new(),
      capacity: capacity.max(1),
    }
  }

  /// `Some(None)` is a cached "no translation"; `None` means unknown.
  pub fn get(&self, source: TranslationSource, id: &str) -> Option<&Option<String>> {
    self.entries.get(&(source, id.to_string()))
  }

  pub fn insert(&mut self, source: TranslationSource, id: &str, value: Option<String>) {
    let key = (source, id.to_string());
    if self.entries.insert(key.clone(), value).is_none() {
      self.order.push_back(key);
      while self.entries.len() > self.capacity {
        match self.order.pop_front() {
          Some(oldest) => {
            self.entries.remove(&oldest);
          }
          None => break,
        }
      }
    }
  }

  #[cfg(test)]
  pub fn len(&self) -> usize {
    self.entries.len()
  }
}

static TRANSLATION_CACHE: LazyLock<Mutex<TranslationCache>> =
  LazyLock::new(|| Mutex::new(TranslationCache::new(TRANSLATION_CACHE_CAPACITY)));

fn lock_cache(cache: &Mutex<TranslationCache>) -> std::sync::MutexGuard<'_, TranslationCache> {
  cache
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_ids(ids: &[String]) -> Vec<String> {
  let mut seen = HashSet::new();
  ids
    .iter()
    .filter(|id| !id.is_empty() && seen.insert(id.as_str()))
    .cloned()
    .collect()
}

#[derive(Deserialize)]
struct ModrinthTranslationItem {
  #[serde(default)]
  project_id: String,
  #[serde(default)]
  translated: String,
}

#[derive(Deserialize)]
struct CurseForgeTranslationItem {
  #[serde(default)]
  modid: u64,
  #[serde(default)]
  translated: String,
}

/// Parse a `POST /translate/modrinth` reply into `project_id -> translated`.
/// Entries with an empty translation are dropped.
pub fn parse_modrinth_translations(body: &str) -> Option<HashMap<String, String>> {
  let items: Vec<ModrinthTranslationItem> = serde_json::from_str(body).ok()?;
  Some(
    items
      .into_iter()
      .filter(|item| !item.project_id.is_empty() && !item.translated.is_empty())
      .map(|item| (item.project_id, item.translated))
      .collect(),
  )
}

/// Parse a `POST /translate/curseforge` reply into `mod id -> translated`.
pub fn parse_curseforge_translations(body: &str) -> Option<HashMap<String, String>> {
  let items: Vec<CurseForgeTranslationItem> = serde_json::from_str(body).ok()?;
  Some(
    items
      .into_iter()
      .filter(|item| item.modid != 0 && !item.translated.is_empty())
      .map(|item| (item.modid.to_string(), item.translated))
      .collect(),
  )
}

/// Resolve `ids` from `cache`, asking `fetch` for the unknown ones in batches.
///
/// `fetch` returns the translations found for a batch, or `Err` when the request
/// failed. Ids missing from a successful reply are cached as "no translation";
/// ids of a failed batch are not cached and are left out of the result.
async fn translate_through_cache<F, Fut>(
  cache: &Mutex<TranslationCache>,
  source: TranslationSource,
  ids: &[String],
  fetch: F,
) -> HashMap<String, String>
where
  F: Fn(Vec<String>) -> Fut,
  Fut: Future<Output = Result<HashMap<String, String>, ()>>,
{
  let mut result = HashMap::new();
  let mut unknown = Vec::new();
  {
    let cache = lock_cache(cache);
    for id in unique_ids(ids) {
      match cache.get(source, &id) {
        Some(Some(text)) => {
          result.insert(id, text.clone());
        }
        Some(None) => {}
        None => unknown.push(id),
      }
    }
  }

  // Batches run one after another so a large list stays within the mirror's
  // concurrency budget.
  for chunk in unknown.chunks(TRANSLATION_BATCH_SIZE) {
    let Ok(mut found) = fetch(chunk.to_vec()).await else {
      continue;
    };
    let mut cache = lock_cache(cache);
    for id in chunk {
      let text = found.remove(id);
      cache.insert(source, id, text.clone());
      if let Some(text) = text {
        result.insert(id.clone(), text);
      }
    }
  }

  result
}

async fn fetch_translation_batch(
  client: &reqwest::Client,
  source: TranslationSource,
  ids: Vec<String>,
) -> Result<HashMap<String, String>, ()> {
  let (url, body) = match source {
    TranslationSource::Modrinth => (
      format!("{}/translate/modrinth", MCIM_BASE),
      serde_json::json!({ "project_ids": ids }),
    ),
    TranslationSource::CurseForge => {
      // Ids that are not numeric simply get no translation.
      let mod_ids: Vec<u64> = ids.iter().filter_map(|id| id.parse().ok()).collect();
      if mod_ids.is_empty() {
        return Ok(HashMap::new());
      }
      (
        format!("{}/translate/curseforge", MCIM_BASE),
        serde_json::json!({ "modids": mod_ids }),
      )
    }
  };

  let _permit = acquire_mcim_api_permit_for(&url).await;
  let started = std::time::Instant::now();
  let response = client
    .post(&url)
    .json(&body)
    .send()
    .await
    .map_err(|error| log::warn!("Translation request to {} failed: {}", url, error))?;
  if !response.status().is_success() {
    log::warn!(
      "Translation request to {} returned {}",
      url,
      response.status()
    );
    return Err(());
  }
  let text = response
    .text()
    .await
    .map_err(|error| log::warn!("Translation reply from {} unreadable: {}", url, error))?;

  let parsed = match source {
    TranslationSource::Modrinth => parse_modrinth_translations(&text),
    TranslationSource::CurseForge => parse_curseforge_translations(&text),
  };
  let found = parsed.ok_or_else(|| log::warn!("Translation reply from {} is malformed", url))?;
  log::debug!(
    "Translated {} of {} {:?} ids in {} ms",
    found.len(),
    ids.len(),
    source,
    started.elapsed().as_millis()
  );
  Ok(found)
}

/// Translations for `ids`, from the cache or one batched MCIM request per
/// [`TRANSLATION_BATCH_SIZE`] missing ids. Never fails: ids that could not be
/// translated (no translation, or the request failed) are absent from the result.
pub async fn translate_resource_descriptions(
  app: &AppHandle,
  source: &OtherResourceSource,
  ids: &[String],
) -> HashMap<String, String> {
  let Some(source) = TranslationSource::from_resource_source(source) else {
    return HashMap::new();
  };
  let client = app.state::<reqwest::Client>();
  translate_through_cache(&TRANSLATION_CACHE, source, ids, |batch| {
    fetch_translation_batch(&client, source, batch)
  })
  .await
}

/// Translations already in the cache; no network access.
pub fn cached_translations(
  source: &OtherResourceSource,
  ids: &[String],
) -> HashMap<String, String> {
  let Some(source) = TranslationSource::from_resource_source(source) else {
    return HashMap::new();
  };
  let cache = lock_cache(&TRANSLATION_CACHE);
  ids
    .iter()
    .filter_map(|id| match cache.get(source, id) {
      Some(Some(text)) => Some((id.clone(), text.clone())),
      _ => None,
    })
    .collect()
}

/// Fill `translated_description` from the cache for entries that lack one.
pub fn apply_cached_translations(list: &mut [OtherResourceInfo]) {
  for info in list.iter_mut() {
    if info.translated_description.is_some() {
      continue;
    }
    let cached = cached_translations(&info.source, std::slice::from_ref(&info.id));
    if let Some(text) = cached.get(&info.id) {
      info.translated_description = Some(text.clone());
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| id.to_string()).collect()
  }

  fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect()
  }

  #[test]
  fn parses_modrinth_batch_reply() {
    let body = r#"[{"project_id":"AANobbMI","translated":"渲染引擎","original":"x","translated_at":"t"},
      {"project_id":"empty","translated":""}]"#;
    assert_eq!(
      parse_modrinth_translations(body).unwrap(),
      map(&[("AANobbMI", "渲染引擎")])
    );
    assert_eq!(parse_modrinth_translations("[]").unwrap().len(), 0);
    assert!(parse_modrinth_translations("not json").is_none());
    assert!(parse_modrinth_translations(r#"{"detail":"error"}"#).is_none());
  }

  #[test]
  fn parses_curseforge_batch_reply() {
    let body =
      r#"[{"modid":238222,"translated":"查看物品和配方","original":"x","translated_at":"t"}]"#;
    assert_eq!(
      parse_curseforge_translations(body).unwrap(),
      map(&[("238222", "查看物品和配方")])
    );
    assert!(parse_curseforge_translations("oops").is_none());
  }

  #[test]
  fn cache_evicts_oldest_beyond_capacity() {
    let mut cache = TranslationCache::new(2);
    cache.insert(TranslationSource::Modrinth, "a", Some("A".into()));
    cache.insert(TranslationSource::Modrinth, "b", None);
    cache.insert(TranslationSource::Modrinth, "c", Some("C".into()));
    assert_eq!(cache.len(), 2);
    assert!(cache.get(TranslationSource::Modrinth, "a").is_none());
    assert_eq!(cache.get(TranslationSource::Modrinth, "b"), Some(&None));
    // Updating an existing entry neither grows nor reorders the cache.
    cache.insert(TranslationSource::Modrinth, "b", Some("B".into()));
    assert_eq!(cache.len(), 2);
    // The same id from the other source is a different entry.
    assert!(cache.get(TranslationSource::CurseForge, "c").is_none());
  }

  #[tokio::test]
  async fn cached_and_known_missing_ids_are_not_requested_again() {
    let cache = Mutex::new(TranslationCache::new(100));
    let calls = AtomicUsize::new(0);
    let fetch = |batch: Vec<String>| {
      calls.fetch_add(1, Ordering::SeqCst);
      async move {
        // Only "a" has a translation; every other id is silently omitted.
        Ok(
          batch
            .iter()
            .filter(|id| id.as_str() == "a")
            .map(|id| (id.clone(), "甲".to_string()))
            .collect(),
        )
      }
    };

    let first = translate_through_cache(
      &cache,
      TranslationSource::Modrinth,
      &ids(&["a", "b", "a"]),
      &fetch,
    )
    .await;
    assert_eq!(first, map(&[("a", "甲")]));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // "b" is remembered as having no translation, so nothing is requested.
    let second = translate_through_cache(
      &cache,
      TranslationSource::Modrinth,
      &ids(&["b", "a"]),
      &fetch,
    )
    .await;
    assert_eq!(second, map(&[("a", "甲")]));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Another source has its own entries.
    translate_through_cache(&cache, TranslationSource::CurseForge, &ids(&["a"]), &fetch).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn failed_batches_are_not_cached() {
    let cache = Mutex::new(TranslationCache::new(100));
    let fail = |_: Vec<String>| async { Err(()) };
    let result =
      translate_through_cache(&cache, TranslationSource::Modrinth, &ids(&["a"]), &fail).await;
    assert!(result.is_empty());
    assert_eq!(lock_cache(&cache).len(), 0);

    let ok = |batch: Vec<String>| async move {
      Ok(batch.into_iter().map(|id| (id, "译".to_string())).collect())
    };
    let retried =
      translate_through_cache(&cache, TranslationSource::Modrinth, &ids(&["a"]), &ok).await;
    assert_eq!(retried, map(&[("a", "译")]));
  }

  #[tokio::test]
  async fn requests_are_split_into_batches() {
    let cache = Mutex::new(TranslationCache::new(1000));
    let sizes = Mutex::new(Vec::new());
    let fetch = |batch: Vec<String>| {
      sizes.lock().unwrap().push(batch.len());
      async { Ok(HashMap::new()) }
    };
    let many: Vec<String> = (0..TRANSLATION_BATCH_SIZE * 2 + 3)
      .map(|n| n.to_string())
      .collect();
    translate_through_cache(&cache, TranslationSource::CurseForge, &many, &fetch).await;
    assert_eq!(
      *sizes.lock().unwrap(),
      vec![TRANSLATION_BATCH_SIZE, TRANSLATION_BATCH_SIZE, 3]
    );
  }
}
