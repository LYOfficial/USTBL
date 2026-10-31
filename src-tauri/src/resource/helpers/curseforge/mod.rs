//! CurseForge resource channel.
//!
//! Implements the same surface as [`crate::resource::helpers::modrinth`] so the
//! frontend can offer CurseForge as a download source next to Modrinth. Requests
//! go through the MCIM mirror by default, which is what makes the channel usable
//! without a CurseForge API key.

pub mod misc;

use crate::error::USTBLResult;
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::batch_lookup::{lookup_across_sources, LookupOutcome};
use crate::resource::helpers::hashing::curseforge_fingerprint_file_async;
use crate::resource::helpers::mcim::{
  curseforge_api_base, get_content_source_priority_list, ContentSource,
};
use crate::resource::helpers::misc::{
  apply_local_resource_enhancements, apply_other_resource_enhancements, version_pack_sort,
};
use crate::resource::helpers::mod_db::handle_search_query;
use crate::resource::helpers::translation::{
  apply_cached_translations, translate_resource_descriptions,
};
use crate::resource::models::{
  OtherResourceApiEndpoint, OtherResourceFileInfo, OtherResourceInfo, OtherResourceRequestType,
  OtherResourceSearchQuery, OtherResourceSearchRes, OtherResourceSource, OtherResourceVersionPack,
  OtherResourceVersionPackQuery, ResourceError,
};
use crate::utils::web::ApiClient;
use misc::{
  class_id_of_resource_type, fetch_curseforge_json, game_versions_of, has_official_api_key,
  loaders_of_game_versions, make_curseforge_request, mod_loader_type_of, resolve_category_id,
  sort_field_of, CurseForgeDataRes, CurseForgeFile, CurseForgeFingerprintMatch,
  CurseForgeFingerprintRes, CurseForgeListRes, CurseForgeMod, MAX_GAME_VERSION_REQUESTS,
  MINECRAFT_GAME_ID, PAGE_SIZE_MAX,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

const ALL_FILTER: &str = "All";

/// Pages fetched for an unfiltered ("All versions") version pack query.
const ALL_VERSION_PAGES: u32 = 2;

/// Upper bound on parallel CurseForge requests. MCIM asks clients to keep
/// concurrency reasonable, and a major version expands into many requests.
const MAX_CONCURRENT_REQUESTS: usize = 4;

#[derive(Serialize)]
struct CurseForgeFingerprintsBody {
  fingerprints: Vec<u32>,
}

fn content_source_priority(app: &AppHandle) -> Vec<ContentSource> {
  match app.state::<Mutex<LauncherConfig>>().lock() {
    Ok(config) => get_content_source_priority_list(&config),
    // The mirror is the only source guaranteed to work without an API key, so
    // prefer it when the configuration cannot be read.
    Err(_) => vec![ContentSource::Mcim, ContentSource::Official],
  }
}

pub async fn fetch_resource_list_by_name_curseforge(
  app: &AppHandle,
  query: &OtherResourceSearchQuery,
) -> USTBLResult<OtherResourceSearchRes> {
  let priority = content_source_priority(app);

  let OtherResourceSearchQuery {
    resource_type,
    search_query,
    game_version,
    selected_tag,
    sort_by,
    page,
    page_size,
  } = query;

  let class_id = class_id_of_resource_type(resource_type).ok_or(ResourceError::ParseError)?;

  let page_size = (*page_size).clamp(1, PAGE_SIZE_MAX);
  let mut params = HashMap::from([
    ("gameId".to_string(), MINECRAFT_GAME_ID.to_string()),
    ("classId".to_string(), class_id.to_string()),
    ("index".to_string(), (page * page_size).to_string()),
    ("pageSize".to_string(), page_size.to_string()),
  ]);

  let handled_search_query =
    handle_search_query(app, search_query, &OtherResourceSource::CurseForge)
      .await
      .unwrap_or(search_query.clone()); // Handle Chinese query
  if !handled_search_query.is_empty() {
    params.insert("searchFilter".to_string(), handled_search_query);
  }

  if !game_version.is_empty() && game_version != ALL_FILTER {
    params.insert("gameVersion".to_string(), game_version.clone());
  }

  let sort_field = sort_field_of(sort_by);
  params.insert("sortField".to_string(), sort_field.to_string());
  params.insert(
    "sortOrder".to_string(),
    // Only the name sort reads better ascending.
    (if sort_field == 4 { "asc" } else { "desc" }).to_string(),
  );

  if !selected_tag.is_empty() && selected_tag != ALL_FILTER {
    let client = app.state::<ApiClient>();
    // Category ids are not guessable, and legacy categories in the UI's tag list
    // may no longer exist; in that case the search runs unfiltered.
    for source in &priority {
      if let Some(category_id) = resolve_category_id(&client, *source, class_id, selected_tag).await
      {
        params.insert("categoryId".to_string(), category_id.to_string());
        break;
      }
    }
  }

  let results = fetch_curseforge_json::<CurseForgeListRes<CurseForgeMod>>(
    app,
    &priority,
    OtherResourceApiEndpoint::Search,
    None,
    &params,
  )
  .await?;

  let (total, page_size) = results
    .pagination
    .as_ref()
    .map(|pagination| (pagination.total_count, pagination.page_size))
    .unwrap_or((results.data.len() as u64, page_size));

  let mut search_result = OtherResourceSearchRes {
    list: results
      .data
      .into_iter()
      .map(OtherResourceInfo::from)
      .collect(),
    total,
    page: *page,
    page_size,
  };

  // Descriptions are translated in a second step (`translate_resource_descriptions`),
  // so the list is returned as soon as the search itself is done.
  for resource_info in &mut search_result.list {
    apply_local_resource_enhancements(app, resource_info);
  }
  apply_cached_translations(&mut search_result.list);

  Ok(search_result)
}

struct FilePageRequest {
  game_version: Option<String>,
  index: u32,
}

async fn fetch_file_page(
  app: &AppHandle,
  priority: &[ContentSource],
  resource_id: &str,
  mod_loader: &str,
  request: &FilePageRequest,
) -> USTBLResult<Vec<CurseForgeFile>> {
  let mut params = HashMap::from([
    ("index".to_string(), request.index.to_string()),
    ("pageSize".to_string(), PAGE_SIZE_MAX.to_string()),
  ]);
  if let Some(game_version) = &request.game_version {
    params.insert("gameVersion".to_string(), game_version.clone());
  }
  if mod_loader != ALL_FILTER {
    if let Some(loader_type) = mod_loader_type_of(mod_loader) {
      params.insert("modLoaderType".to_string(), loader_type.to_string());
    }
  }

  let results = fetch_curseforge_json::<CurseForgeListRes<CurseForgeFile>>(
    app,
    priority,
    OtherResourceApiEndpoint::VersionPack,
    Some(resource_id),
    &params,
  )
  .await?;

  Ok(results.data)
}

pub async fn fetch_resource_version_packs_curseforge(
  app: &AppHandle,
  query: &OtherResourceVersionPackQuery,
) -> USTBLResult<Vec<OtherResourceVersionPack>> {
  let priority = content_source_priority(app);

  let OtherResourceVersionPackQuery {
    resource_id,
    mod_loader,
    game_versions,
  } = query;

  let requested_versions: Vec<String> = game_versions
    .iter()
    .filter(|version| !version.is_empty() && version.as_str() != ALL_FILTER)
    .take(MAX_GAME_VERSION_REQUESTS)
    .cloned()
    .collect();

  // CurseForge filters one game version per request, so an unfiltered query is
  // paginated while a filtered one fans out per version.
  let requests: Vec<FilePageRequest> = if requested_versions.is_empty() {
    (0..ALL_VERSION_PAGES)
      .map(|page| FilePageRequest {
        game_version: None,
        index: page * PAGE_SIZE_MAX,
      })
      .collect()
  } else {
    requested_versions
      .iter()
      .map(|game_version| FilePageRequest {
        game_version: Some(game_version.clone()),
        index: 0,
      })
      .collect()
  };

  let mut files: Vec<CurseForgeFile> = Vec::new();
  let mut seen_ids: HashSet<u64> = HashSet::new();
  let mut last_error = None;
  // A major version can expand into a dozen exact versions, so requests are
  // issued in small batches to stay polite to the mirror.
  for batch in requests.chunks(MAX_CONCURRENT_REQUESTS) {
    let results = futures::future::join_all(
      batch
        .iter()
        .map(|request| fetch_file_page(app, &priority, resource_id, mod_loader, request)),
    )
    .await;

    for result in results {
      match result {
        Ok(page_files) => {
          for file in page_files {
            if seen_ids.insert(file.id) {
              files.push(file);
            }
          }
        }
        Err(error) => last_error = Some(error),
      }
    }
  }

  if files.is_empty() {
    if let Some(error) = last_error {
      return Err(error);
    }
    return Ok(Vec::new());
  }

  let mut packs: HashMap<String, Vec<OtherResourceFileInfo>> = HashMap::new();
  for file in &files {
    let file_info = OtherResourceFileInfo::from(file);
    let versions = game_versions_of(file);

    if versions.is_empty() {
      // Files without a parseable Minecraft version still belong to the single
      // requested version when the caller asked for exactly one.
      if let Some(single) = requested_versions.first() {
        packs.entry(single.clone()).or_default().push(file_info);
      }
      continue;
    }

    // Files that support several loaders are listed once per loader, matching
    // the Modrinth channel: the frontend picks the entry whose loader matches
    // the instance.
    let loaders = loaders_of_game_versions(&file.game_versions);
    for version in versions {
      let entry = packs.entry(version).or_default();
      if loaders.is_empty() {
        entry.push(file_info.clone());
      } else {
        for loader in &loaders {
          entry.push(OtherResourceFileInfo {
            loader: Some(loader.clone()),
            ..file_info.clone()
          });
        }
      }
    }
  }

  let mut list: Vec<OtherResourceVersionPack> = packs
    .into_iter()
    .map(|(name, mut items)| {
      items.sort_by(|a, b| b.file_date.cmp(&a.file_date));
      OtherResourceVersionPack { name, items }
    })
    .collect();
  list.sort_by(version_pack_sort);

  Ok(list)
}

pub async fn fetch_remote_resource_by_id_curseforge(
  app: &AppHandle,
  resource_id: &str,
) -> USTBLResult<OtherResourceInfo> {
  let priority = content_source_priority(app);

  let results = fetch_curseforge_json::<CurseForgeDataRes<CurseForgeMod>>(
    app,
    &priority,
    OtherResourceApiEndpoint::ById,
    Some(resource_id),
    &HashMap::new(),
  )
  .await?;

  let mut resource_info: OtherResourceInfo = results.data.into();
  let _ = apply_other_resource_enhancements(app, &mut resource_info).await;

  Ok(resource_info)
}

/// Fingerprints per `POST /fingerprints` request.
const FINGERPRINTS_BATCH_SIZE: usize = 500;

/// Match `/fingerprints` results back to the fingerprints that were asked for.
/// Fingerprints without an exact match are absent from the result.
fn index_exact_matches(
  matches: Vec<CurseForgeFingerprintMatch>,
  requested: &[u32],
) -> HashMap<u32, CurseForgeFile> {
  matches
    .into_iter()
    .filter_map(|matched| matched.file)
    .filter_map(|file| {
      let fingerprint = match file.file_fingerprint {
        // A reply without the field can only be attributed when one fingerprint was sent.
        0 if requested.len() == 1 => requested[0],
        0 => return None,
        fingerprint => fingerprint,
      };
      requested
        .contains(&fingerprint)
        .then_some((fingerprint, file))
    })
    .collect()
}

/// Resolve many fingerprints with one `POST /fingerprints` per
/// [`FINGERPRINTS_BATCH_SIZE`]. A fingerprint the first source lacks is asked
/// again, in one request, from the next source; there is no per-file fallback. A
/// fingerprint missing from the result is unknown to the last source that
/// answered. If a fingerprint could not be settled because every request that
/// would have done so failed, the whole lookup fails instead.
///
/// Misses are not retried against the same source after a pause: without an API
/// key MCIM is the only source, and a local jar that is simply not on CurseForge
/// is the common case, so a retry would only add latency to every lookup.
pub async fn fetch_remote_resources_by_fingerprints_curseforge(
  app: &AppHandle,
  fingerprints: &[u32],
) -> USTBLResult<HashMap<u32, OtherResourceFileInfo>> {
  let sources: Vec<ContentSource> = content_source_priority(app)
    .into_iter()
    .filter(|source| *source != ContentSource::Official || has_official_api_key())
    .collect();
  if sources.is_empty() {
    return Err(ResourceError::NoDownloadApi.into());
  }
  let client = app.state::<ApiClient>();
  let mut found = HashMap::new();

  for chunk in fingerprints.chunks(FINGERPRINTS_BATCH_SIZE) {
    let outcome = lookup_across_sources(&sources, chunk, |source, asked: Vec<u32>| {
      let client = &client;
      async move {
        let url =
          misc::get_curseforge_api(source, OtherResourceApiEndpoint::FromLocal, None).ok()?;
        let body = CurseForgeFingerprintsBody {
          fingerprints: asked.clone(),
        };
        match make_curseforge_request::<CurseForgeFingerprintRes, CurseForgeFingerprintsBody>(
          client,
          &url,
          source,
          OtherResourceRequestType::Post(&body),
        )
        .await
        {
          Ok(response) => Some(index_exact_matches(response.data.exact_matches, &asked)),
          Err(error) => {
            log::warn!(
              "CurseForge fingerprint lookup via {:?} failed: {:?}",
              source,
              error
            );
            None
          }
        }
      }
    })
    .await;

    if outcome.found.len() + outcome.not_found.len() < chunk.len() {
      return Err(ResourceError::NetworkError.into());
    }
    for (fingerprint, file) in outcome.found {
      found.insert(fingerprint, OtherResourceFileInfo::from(&file));
    }
  }

  Ok(found)
}

/// Mods per `POST /mods` request.
pub const MODS_BATCH_SIZE: usize = 200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeModsBody {
  mod_ids: Vec<u64>,
  filter_pc_only: bool,
}

/// `mod id -> mod` from a `POST /mods` reply. Unknown ids are absent from the reply.
fn mods_by_id(reply: CurseForgeDataRes<Vec<CurseForgeMod>>) -> HashMap<u64, CurseForgeMod> {
  reply
    .data
    .into_iter()
    .map(|project| (project.id, project))
    .collect()
}

/// Projects for many mod ids, one `POST /mods` per [`MODS_BATCH_SIZE`]. Like
/// the fingerprint lookup, an id the first source lacks is asked again from the
/// next source, and it is reported as not found only when the last source
/// answered without it. Ids no source could answer for are in neither field of
/// the outcome.
pub async fn fetch_mods_curseforge(
  app: &AppHandle,
  mod_ids: &[u64],
) -> LookupOutcome<u64, CurseForgeMod> {
  let sources: Vec<ContentSource> = content_source_priority(app)
    .into_iter()
    .filter(|source| *source != ContentSource::Official || has_official_api_key())
    .collect();
  let client = app.state::<ApiClient>();
  let mut outcome = LookupOutcome::default();

  for chunk in mod_ids.chunks(MODS_BATCH_SIZE) {
    outcome.extend(
      lookup_across_sources(&sources, chunk, |source, asked: Vec<u64>| {
        let client = &client;
        async move {
          let url = format!("{}/mods", curseforge_api_base(source));
          let body = CurseForgeModsBody {
            mod_ids: asked,
            filter_pc_only: false,
          };
          match make_curseforge_request::<CurseForgeDataRes<Vec<CurseForgeMod>>, CurseForgeModsBody>(
            client,
            &url,
            source,
            OtherResourceRequestType::Post(&body),
          )
          .await
          {
            Ok(reply) => Some(mods_by_id(reply)),
            Err(error) => {
              log::warn!("CurseForge {} via {:?} failed: {:?}", url, source, error);
              None
            }
          }
        }
      })
      .await,
    );
  }
  outcome
}

/// CurseForge identifies local files by a MurmurHash2 fingerprint of their
/// contents with whitespace bytes removed, so the same jar can be resolved on a
/// machine where the file was copied by another launcher.
///
/// See <https://docs.curseforge.com/rest-api/#get-fingerprints-matches>.
pub async fn fetch_remote_resource_by_local_curseforge(
  app: &AppHandle,
  file_path: &str,
) -> USTBLResult<OtherResourceFileInfo> {
  let fingerprint = curseforge_fingerprint_file_async(PathBuf::from(file_path))
    .await
    .map_err(|_| ResourceError::FileOperationError)?;

  // A fingerprint miss is not a transport failure: the file simply is not on
  // CurseForge, so report it as a parse miss for the caller to ignore.
  fetch_remote_resources_by_fingerprints_curseforge(app, &[fingerprint])
    .await?
    .remove(&fingerprint)
    .ok_or_else(|| ResourceError::ParseError.into())
}
/// Translate a CurseForge description through MCIM's translation service.
pub async fn translate_description_curseforge(
  app: &AppHandle,
  resource_id: &str,
) -> USTBLResult<Option<String>> {
  // Goes through the shared batch translator so the result is cached; failures
  // yield Ok(None) to avoid blocking major functionality.
  let mut translations = translate_resource_descriptions(
    app,
    &OtherResourceSource::CurseForge,
    &[resource_id.to_string()],
  )
  .await;
  Ok(translations.remove(resource_id))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mods_reply_is_indexed_by_id_and_keeps_slug_and_class() {
    // Shape of a live `POST /mods` reply; ids CurseForge does not know are left out.
    let reply: CurseForgeDataRes<Vec<CurseForgeMod>> = serde_json::from_str(
      r#"{"data":[{"id":238222,"slug":"jei","name":"JEI","classId":6},
        {"id":306612,"slug":"fabric-api","classId":6}]}"#,
    )
    .unwrap();
    let indexed = mods_by_id(reply);
    assert_eq!(indexed.len(), 2);
    assert_eq!(indexed[&238222].slug, "jei");
    assert_eq!(indexed[&306612].class_id, Some(6));
    assert!(!indexed.contains_key(&99999999));
  }

  #[test]
  fn mods_request_body_uses_the_documented_keys() {
    let body = CurseForgeModsBody {
      mod_ids: vec![1, 2],
      filter_pc_only: false,
    };
    assert_eq!(
      serde_json::to_string(&body).unwrap(),
      r#"{"modIds":[1,2],"filterPcOnly":false}"#
    );
  }

  fn matches_from(json: &str) -> (Vec<CurseForgeFingerprintMatch>, Vec<u32>) {
    let reply: CurseForgeFingerprintRes = serde_json::from_str(json).unwrap();
    let requested = vec![1481194740, 12345];
    (reply.data.exact_matches, requested)
  }

  #[test]
  fn exact_matches_are_attributed_by_file_fingerprint() {
    // Shape of a live `POST /fingerprints` reply: one match, one unmatched fingerprint.
    let (matches, requested) = matches_from(
      r#"{"data":{"isCacheBuilt":true,
        "exactMatches":[{"id":238222,"file":{"id":9382240,"modId":238222,"fileName":"jei.jar",
          "fileFingerprint":1481194740,"hashes":[{"value":"aa","algo":1}]},"latestFiles":[]}],
        "exactFingerprints":[1481194740],"unmatchedFingerprints":[12345]}}"#,
    );
    let indexed = index_exact_matches(matches, &requested);
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[&1481194740].id, 9382240);
    assert!(!indexed.contains_key(&12345));
  }

  #[test]
  fn matches_for_fingerprints_nobody_asked_for_are_dropped() {
    let (matches, _) = matches_from(
      r#"{"data":{"exactMatches":[{"file":{"id":1,"fileFingerprint":777}},{"file":null}]}}"#,
    );
    assert!(index_exact_matches(matches, &[1481194740]).is_empty());
  }

  #[test]
  fn replies_without_a_file_fingerprint_only_work_for_a_single_request() {
    let (matches, _) = matches_from(r#"{"data":{"exactMatches":[{"file":{"id":5}}]}}"#);
    let single = index_exact_matches(matches, &[42]);
    assert_eq!(single[&42].id, 5);

    let (matches, _) = matches_from(r#"{"data":{"exactMatches":[{"file":{"id":5}}]}}"#);
    assert!(index_exact_matches(matches, &[42, 43]).is_empty());
  }
}
