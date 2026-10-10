//! CurseForge resource channel.
//!
//! Implements the same surface as [`crate::resource::helpers::modrinth`] so the
//! frontend can offer CurseForge as a download source next to Modrinth. Requests
//! go through the MCIM mirror by default, which is what makes the channel usable
//! without a CurseForge API key.

pub mod misc;

use crate::error::USTBLResult;
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::mcim::{get_content_source_priority_list, ContentSource, MCIM_BASE};
use crate::resource::helpers::misc::{apply_other_resource_enhancements, version_pack_sort};
use crate::resource::helpers::mod_db::handle_search_query;
use crate::resource::models::{
  OtherResourceApiEndpoint, OtherResourceFileInfo, OtherResourceInfo, OtherResourceRequestType,
  OtherResourceSearchQuery, OtherResourceSearchRes, OtherResourceSource, OtherResourceVersionPack,
  OtherResourceVersionPackQuery, ResourceError,
};
use misc::{
  class_id_of_resource_type, fetch_curseforge_json, game_versions_of, loaders_of_game_versions,
  make_curseforge_request, mod_loader_type_of, resolve_category_id, sort_field_of,
  CurseForgeDataRes, CurseForgeFile, CurseForgeFingerprintRes, CurseForgeListRes, CurseForgeMod,
  CurseForgeTranslationRes, MAX_GAME_VERSION_REQUESTS, MINECRAFT_GAME_ID, PAGE_SIZE_MAX,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
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
    let client = app.state::<reqwest::Client>();
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

  for resource_info in &mut search_result.list {
    let _ = apply_other_resource_enhancements(app, resource_info).await;
  }

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

/// CurseForge identifies local files by a MurmurHash2 fingerprint of their
/// contents with whitespace bytes removed, so the same jar can be resolved on a
/// machine where the file was copied by another launcher.
///
/// See <https://docs.curseforge.com/rest-api/#get-fingerprints-matches>.
pub async fn fetch_remote_resource_by_local_curseforge(
  app: &AppHandle,
  file_path: &str,
) -> USTBLResult<OtherResourceFileInfo> {
  let content = fs::read(file_path).map_err(|_| ResourceError::FileOperationError)?;

  let fingerprint = murmur2::murmur2(
    &content
      .iter()
      .copied()
      .filter(|byte| !matches!(byte, 0x09 | 0x0a | 0x0d | 0x20))
      .collect::<Vec<u8>>(),
    1,
  );

  let priority = content_source_priority(app);
  let body = CurseForgeFingerprintsBody {
    fingerprints: vec![fingerprint],
  };

  let client = app.state::<reqwest::Client>();
  let mut last_error = None;

  for source in &priority {
    let url = misc::get_curseforge_api(*source, OtherResourceApiEndpoint::FromLocal, None)?;
    match make_curseforge_request::<CurseForgeFingerprintRes, CurseForgeFingerprintsBody>(
      &client,
      &url,
      *source,
      OtherResourceRequestType::Post(&body),
    )
    .await
    {
      Ok(response) => {
        let matched = response
          .data
          .exact_matches
          .into_iter()
          .find_map(|matched| matched.file)
          .ok_or(ResourceError::ParseError)?;

        return Ok(OtherResourceFileInfo::from(&matched));
      }
      Err(error) => {
        log::warn!(
          "CurseForge fingerprint lookup via {:?} failed: {:?}",
          source,
          error
        );
        last_error = Some(error);
      }
    }
  }

  // A fingerprint miss is not a transport failure: the file simply is not on
  // CurseForge, so report it as a parse miss for the caller to ignore.
  Err(last_error.unwrap_or_else(|| ResourceError::ParseError.into()))
}

/// Translate a CurseForge description through MCIM's translation service.
pub async fn translate_description_curseforge(
  app: &AppHandle,
  resource_id: &str,
) -> USTBLResult<Option<String>> {
  let client = app.state::<reqwest::Client>();
  let url = format!(
    "{}/translate/curseforge/{}",
    MCIM_BASE,
    urlencoding::encode(resource_id)
  );

  let result = client
    .get(&url)
    .send()
    .await
    .map_err(|_| ResourceError::NetworkError)?
    .json::<CurseForgeTranslationRes>()
    .await
    .map_err(|_| ResourceError::ParseError);

  // Only return Ok(None) when translation fails, to avoid blocking major
  // functionality.
  Ok(result.ok().map(|response| response.translated))
}
