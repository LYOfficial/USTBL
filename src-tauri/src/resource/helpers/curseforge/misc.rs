//! CurseForge API request helpers.
//!
//! CurseForge requires an API key for every request and the official host
//! rejects key-less requests with `403`. The MCIM mirror serves the same routes
//! without a key, so the CurseForge channel works out of the box and the
//! official host is only contacted when a key is configured through
//! `USTBL_CURSEFORGE_API_KEY`. The key is never sent to the mirror.
//!
//! Reference: <https://docs.curseforge.com/rest-api/>

use crate::error::{USTBLError, USTBLResult};
use crate::resource::helpers::mcim::{curseforge_api_base, ContentSource};
use crate::resource::models::{
  OtherResourceApiEndpoint, OtherResourceDependency, OtherResourceFileInfo, OtherResourceInfo,
  OtherResourceRequestType, OtherResourceSource, ResourceError,
};
use lazy_static::lazy_static;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use url::Url;

/// Minecraft's game id on CurseForge.
pub const MINECRAFT_GAME_ID: u32 = 432;

/// Category class ids for the Minecraft resource kinds USTBL browses.
pub const CLASS_ID_MOD: u32 = 6;
pub const CLASS_ID_RESOURCE_PACK: u32 = 12;
pub const CLASS_ID_WORLD: u32 = 17;
pub const CLASS_ID_MODPACK: u32 = 4471;
pub const CLASS_ID_SHADER: u32 = 6552;
pub const CLASS_ID_DATAPACK: u32 = 6945;

/// CurseForge refuses `pageSize` above 50 and `index + pageSize` above 10,000.
pub const PAGE_SIZE_MAX: u32 = 50;

/// Upper bound on the per-game-version requests issued for one version pack
/// query. CurseForge filters a single version per request, so a full "all
/// versions" query has to be split up.
pub const MAX_GAME_VERSION_REQUESTS: usize = 16;

lazy_static! {
  /// CurseForge API key. Empty for normal builds, where the MCIM mirror is used
  /// instead. See <https://docs.curseforge.com/rest-api/#authentication>.
  pub static ref CURSEFORGE_API_KEY: String =
    env::var("USTBL_CURSEFORGE_API_KEY").unwrap_or_default();

  /// `classId` -> (category name -> category id). Filled lazily, because
  /// category ids are stable but not guessable from the localized names the UI
  /// uses as tags.
  static ref CATEGORY_CACHE: Mutex<HashMap<u32, HashMap<String, u32>>> =
    Mutex::new(HashMap::new());
}

/// Whether the official CurseForge API can be called at all.
pub fn has_official_api_key() -> bool {
  !CURSEFORGE_API_KEY.is_empty()
}

pub fn class_id_of_resource_type(resource_type: &str) -> Option<u32> {
  match resource_type {
    "mod" => Some(CLASS_ID_MOD),
    "resourcepack" => Some(CLASS_ID_RESOURCE_PACK),
    "world" => Some(CLASS_ID_WORLD),
    "modpack" => Some(CLASS_ID_MODPACK),
    "shader" => Some(CLASS_ID_SHADER),
    "datapack" => Some(CLASS_ID_DATAPACK),
    _ => None,
  }
}

pub fn resource_type_of_class_id(class_id: Option<u32>) -> String {
  match class_id {
    Some(CLASS_ID_MOD) => "mod",
    Some(CLASS_ID_RESOURCE_PACK) => "resourcepack",
    Some(CLASS_ID_WORLD) => "world",
    Some(CLASS_ID_MODPACK) => "modpack",
    Some(CLASS_ID_SHADER) => "shader",
    Some(CLASS_ID_DATAPACK) => "datapack",
    _ => "mod",
  }
  .to_string()
}

/// `modLoaderType` values accepted by the CurseForge API.
pub fn mod_loader_type_of(mod_loader: &str) -> Option<u32> {
  match mod_loader.to_lowercase().as_str() {
    "forge" | "legacyforge" => Some(1),
    "fabric" => Some(4),
    "quilt" => Some(5),
    "neoforge" => Some(6),
    _ => None,
  }
}

/// Map the CurseForge `sortField` enum. Unknown values fall back to popularity.
pub fn sort_field_of(sort_by: &str) -> u32 {
  match sort_by {
    "Featured" => 1,
    "Popularity" => 2,
    "Latest update" => 3,
    "A-Z" => 4,
    "Total downloads" => 6,
    "Creation date" => 11,
    _ => 2,
  }
}

/// CurseForge `releaseType`: 1 release, 2 beta, 3 alpha.
pub fn release_type_of(release_type: u64) -> String {
  match release_type {
    2 => "beta",
    3 => "alpha",
    _ => "release",
  }
  .to_string()
}

/// CurseForge dependency `relationType` mapped onto the relation names the
/// frontend already knows from Modrinth.
pub fn relation_of(relation_type: u64) -> String {
  match relation_type {
    2 => "optional",
    4 => "tool",
    5 => "incompatible",
    6 => "include",
    1 => "embedded",
    _ => "required",
  }
  .to_string()
}

/// Whether a `gameVersions` entry looks like a Minecraft version rather than an
/// environment tag (`Client`, `Server`) or a loader name (`Forge`, `Fabric`).
pub fn is_game_version_like(value: &str) -> bool {
  if !value.chars().next().is_some_and(|c| c.is_ascii_digit()) {
    return false;
  }
  value
    .chars()
    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Minecraft versions a file targets; used to group files into version packs
/// the way the Modrinth channel does.
pub fn game_versions_of(file: &CurseForgeFile) -> Vec<String> {
  file
    .game_versions
    .iter()
    .filter(|version| is_game_version_like(version))
    .cloned()
    .collect()
}

/// Mod loaders of a file, most specific first.
///
/// `gameVersions` mixes Minecraft versions with loader and environment tags, and
/// a single file may target several loaders (a real Fabric 1.20.1 file reports
/// `["Fabric", "Client", "1.20.1", "Quilt"]`). Order is fixed rather than taken
/// from the response so a file that lists both `Forge` and `NeoForge` is never
/// mislabelled by array order.
pub fn loaders_of_game_versions(game_versions: &[String]) -> Vec<String> {
  const LOADERS: [(&str, &str); 5] = [
    ("neoforge", "NeoForge"),
    ("forge", "Forge"),
    ("fabric", "Fabric"),
    ("quilt", "Quilt"),
    ("liteloader", "LiteLoader"),
  ];

  LOADERS
    .iter()
    .filter(|(needle, _)| game_versions.iter().any(|v| v.eq_ignore_ascii_case(needle)))
    .map(|(_, name)| name.to_string())
    .collect()
}

fn loader_of_game_versions(game_versions: &[String]) -> Option<String> {
  loaders_of_game_versions(game_versions).into_iter().next()
}

#[derive(Deserialize, Debug)]
pub struct CurseForgeCategoriesRes {
  pub data: Vec<CurseForgeCategory>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeCategory {
  pub id: u32,
  pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct CurseForgeListRes<T> {
  pub data: Vec<T>,
  #[serde(default)]
  pub pagination: Option<CurseForgePagination>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgePagination {
  #[serde(default)]
  pub page_size: u32,
  #[serde(default)]
  pub total_count: u64,
}

#[derive(Deserialize, Debug)]
pub struct CurseForgeDataRes<T> {
  pub data: T,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeMod {
  pub id: u64,
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub slug: String,
  #[serde(default)]
  pub summary: String,
  #[serde(default)]
  pub download_count: u64,
  #[serde(default)]
  pub class_id: Option<u32>,
  #[serde(default)]
  pub date_modified: String,
  #[serde(default)]
  pub links: Option<CurseForgeLinks>,
  #[serde(default)]
  pub logo: Option<CurseForgeLogo>,
  #[serde(default)]
  pub authors: Vec<CurseForgeAuthor>,
  #[serde(default)]
  pub categories: Vec<CurseForgeCategory>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeLinks {
  #[serde(default)]
  pub website_url: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeLogo {
  #[serde(default)]
  pub thumbnail_url: Option<String>,
  #[serde(default)]
  pub url: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct CurseForgeAuthor {
  #[serde(default)]
  pub name: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFile {
  pub id: u64,
  #[serde(default)]
  pub mod_id: u64,
  #[serde(default)]
  pub display_name: String,
  #[serde(default)]
  pub file_name: String,
  #[serde(default)]
  pub release_type: u64,
  #[serde(default)]
  pub file_date: String,
  #[serde(default)]
  pub download_count: u64,
  #[serde(default)]
  pub download_url: Option<String>,
  #[serde(default)]
  pub game_versions: Vec<String>,
  #[serde(default)]
  pub hashes: Vec<CurseForgeHash>,
  #[serde(default)]
  pub dependencies: Vec<CurseForgeDependency>,
}

#[derive(Deserialize, Debug)]
pub struct CurseForgeHash {
  pub value: String,
  pub algo: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeDependency {
  #[serde(default)]
  pub mod_id: u64,
  #[serde(default)]
  pub relation_type: u64,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFingerprintRes {
  pub data: CurseForgeFingerprintMatches,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFingerprintMatches {
  #[serde(default)]
  pub exact_matches: Vec<CurseForgeFingerprintMatch>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFingerprintMatch {
  #[serde(default)]
  pub file: Option<CurseForgeFile>,
}

#[derive(Deserialize, Debug)]
pub struct CurseForgeTranslationRes {
  #[serde(default)]
  pub translated: String,
}

/// Attach CurseForge credentials, but only for the official host: the API key is
/// a user secret and must not be handed to a third-party mirror.
pub fn authenticated(
  request: reqwest::RequestBuilder,
  source: ContentSource,
) -> reqwest::RequestBuilder {
  let request = request.header("accept", "application/json");
  match source {
    ContentSource::Official if has_official_api_key() => {
      request.header("x-api-key", CURSEFORGE_API_KEY.as_str())
    }
    _ => request,
  }
}

pub fn get_curseforge_api(
  source: ContentSource,
  endpoint: OtherResourceApiEndpoint,
  param: Option<&str>,
) -> USTBLResult<String> {
  let api_base = curseforge_api_base(source);

  let url = match endpoint {
    OtherResourceApiEndpoint::Search => format!("{}/mods/search", api_base),
    OtherResourceApiEndpoint::VersionPack => {
      let resource_id = param.ok_or(ResourceError::ParseError)?;
      format!("{}/mods/{}/files", api_base, resource_id)
    }
    OtherResourceApiEndpoint::FromLocal => format!("{}/fingerprints", api_base),
    OtherResourceApiEndpoint::ById => {
      let resource_id = param.ok_or(ResourceError::ParseError)?;
      format!("{}/mods/{}", api_base, resource_id)
    }
    OtherResourceApiEndpoint::Categories => format!("{}/categories", api_base),
    // Modrinth-style endpoints have no CurseForge counterpart.
    OtherResourceApiEndpoint::TranslateDesc => Err(ResourceError::NoDownloadApi)?,
  };

  Ok(url)
}

/// The CurseForge CDN URL for a file, used when the API omits `downloadUrl`
/// (which happens for files whose distribution is restricted).
pub fn forge_cdn_url(file_id: u64, file_name: &str) -> USTBLResult<Url> {
  Url::parse(&format!(
    "https://edge.forgecdn.net/files/{}/{}/{}",
    file_id / 1000,
    file_id % 1000,
    urlencoding::encode(file_name)
  ))
  .map_err(|_| ResourceError::ParseError.into())
}

/// Shared request helper, because `/mods/search`, `/mods/{id}/files` and the
/// records around them all use the same response envelope.
pub async fn make_curseforge_request<T, P>(
  client: &reqwest::Client,
  url: &str,
  source: ContentSource,
  request_type: OtherResourceRequestType<'_, P>,
) -> USTBLResult<T>
where
  T: serde::de::DeserializeOwned,
  P: serde::Serialize,
{
  let request = match request_type {
    OtherResourceRequestType::GetWithParams(params) => {
      authenticated(client.get(url), source).query(params)
    }
    OtherResourceRequestType::Get => authenticated(client.get(url), source),
    OtherResourceRequestType::Post(payload) => {
      authenticated(client.post(url), source).json(payload)
    }
  };

  let response = request
    .send()
    .await
    .map_err(|_| ResourceError::NetworkError)?;

  if !response.status().is_success() {
    log::warn!(
      "CurseForge request to {} failed with status {}",
      url,
      response.status()
    );
    return Err(ResourceError::NetworkError.into());
  }

  response
    .json::<T>()
    .await
    .map_err(|_| ResourceError::ParseError.into())
}

/// Resolve a category *name* (the tag vocabulary the UI uses matches CurseForge
/// category names) to its numeric id for a class. Returns `None` when the
/// category no longer exists upstream, in which case the search simply runs
/// without a category filter instead of failing.
pub async fn resolve_category_id(
  client: &reqwest::Client,
  source: ContentSource,
  class_id: u32,
  category_name: &str,
) -> Option<u32> {
  if let Ok(cache) = CATEGORY_CACHE.lock() {
    if let Some(categories) = cache.get(&class_id) {
      return categories.get(category_name).copied();
    }
  }

  let url = get_curseforge_api(source, OtherResourceApiEndpoint::Categories, None).ok()?;
  let params = HashMap::from([
    ("gameId".to_string(), MINECRAFT_GAME_ID.to_string()),
    ("classId".to_string(), class_id.to_string()),
  ]);

  let response = make_curseforge_request::<CurseForgeCategoriesRes, HashMap<String, String>>(
    client,
    &url,
    source,
    OtherResourceRequestType::GetWithParams(&params),
  )
  .await
  .ok()?;

  let map: HashMap<String, u32> = response
    .data
    .into_iter()
    .map(|category| (category.name, category.id))
    .collect();
  let resolved = map.get(category_name).copied();

  if let Ok(mut cache) = CATEGORY_CACHE.lock() {
    cache.insert(class_id, map);
  }

  resolved
}

/// Query CurseForge paging through any source in `priority`, falling back to
/// the next source on failure. The official host is skipped when no API key is
/// configured, because it would only answer `403`.
pub async fn fetch_curseforge_json<T>(
  app: &AppHandle,
  priority: &[ContentSource],
  endpoint: OtherResourceApiEndpoint,
  param: Option<&str>,
  params: &HashMap<String, String>,
) -> USTBLResult<T>
where
  T: serde::de::DeserializeOwned,
{
  let client = app.state::<reqwest::Client>();
  let mut last_error: Option<USTBLError> = None;

  for source in priority {
    if *source == ContentSource::Official && !has_official_api_key() {
      continue;
    }

    let url = get_curseforge_api(*source, endpoint, param)?;
    match make_curseforge_request::<T, HashMap<String, String>>(
      &client,
      &url,
      *source,
      OtherResourceRequestType::GetWithParams(params),
    )
    .await
    {
      Ok(value) => return Ok(value),
      Err(error) => {
        log::warn!("CurseForge request via {:?} failed: {:?}", source, error);
        last_error = Some(error);
      }
    }
  }

  Err(last_error.unwrap_or_else(|| ResourceError::NoDownloadApi.into()))
}

impl From<&CurseForgeFile> for OtherResourceFileInfo {
  fn from(file: &CurseForgeFile) -> Self {
    let download_url = file
      .download_url
      .clone()
      .filter(|url| !url.is_empty())
      .or_else(|| {
        forge_cdn_url(file.id, &file.file_name)
          .ok()
          .map(|url| url.to_string())
      })
      .unwrap_or_default();

    Self {
      resource_id: file.mod_id.to_string(),
      name: if file.display_name.is_empty() {
        file.file_name.clone()
      } else {
        file.display_name.clone()
      },
      release_type: release_type_of(file.release_type),
      downloads: file.download_count,
      file_date: file.file_date.clone(),
      download_url,
      sha1: file
        .hashes
        .iter()
        .find(|hash| hash.algo == 1)
        .map(|hash| hash.value.clone())
        .unwrap_or_default(),
      file_name: file.file_name.clone(),
      dependencies: file
        .dependencies
        .iter()
        .filter(|dependency| dependency.mod_id != 0)
        .map(|dependency| OtherResourceDependency {
          resource_id: dependency.mod_id.to_string(),
          relation: relation_of(dependency.relation_type),
        })
        .collect(),
      loader: loader_of_game_versions(&file.game_versions),
    }
  }
}

impl From<CurseForgeMod> for OtherResourceInfo {
  fn from(project: CurseForgeMod) -> Self {
    let icon_src = project
      .logo
      .as_ref()
      .and_then(|logo| logo.thumbnail_url.clone().or_else(|| logo.url.clone()))
      .unwrap_or_default();
    let website_url = project
      .links
      .as_ref()
      .and_then(|links| links.website_url.clone())
      .filter(|url| !url.is_empty())
      .unwrap_or_else(|| {
        format!(
          "https://www.curseforge.com/minecraft/{}",
          if project.slug.is_empty() {
            format!("mc-mods/{}", project.id)
          } else {
            project.slug.clone()
          }
        )
      });

    Self {
      id: project.id.to_string(),
      mcmod_id: 0,
      _type: resource_type_of_class_id(project.class_id),
      name: project.name,
      slug: project.slug,
      description: project.summary,
      icon_src,
      website_url,
      tags: project
        .categories
        .into_iter()
        .map(|category| category.name)
        .collect(),
      last_updated: project.date_modified,
      downloads: project.download_count,
      source: OtherResourceSource::CurseForge,
      translated_name: None,
      translated_description: None,
      author: project.authors.into_iter().next().map(|author| author.name),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{
    authenticated, class_id_of_resource_type, game_versions_of, is_game_version_like,
    loader_of_game_versions, loaders_of_game_versions, mod_loader_type_of, relation_of,
    release_type_of, resource_type_of_class_id, sort_field_of, CurseForgeFile, CLASS_ID_DATAPACK,
    CLASS_ID_MOD, CLASS_ID_MODPACK, CLASS_ID_RESOURCE_PACK, CLASS_ID_SHADER, CLASS_ID_WORLD,
  };

  #[test]
  fn resource_types_map_to_curseforge_classes() {
    assert_eq!(class_id_of_resource_type("mod"), Some(CLASS_ID_MOD));
    assert_eq!(
      class_id_of_resource_type("resourcepack"),
      Some(CLASS_ID_RESOURCE_PACK)
    );
    assert_eq!(class_id_of_resource_type("world"), Some(CLASS_ID_WORLD));
    assert_eq!(class_id_of_resource_type("modpack"), Some(CLASS_ID_MODPACK));
    assert_eq!(class_id_of_resource_type("shader"), Some(CLASS_ID_SHADER));
    assert_eq!(
      class_id_of_resource_type("datapack"),
      Some(CLASS_ID_DATAPACK)
    );
    assert_eq!(class_id_of_resource_type("unknown"), None);

    assert_eq!(resource_type_of_class_id(Some(CLASS_ID_SHADER)), "shader");
    assert_eq!(resource_type_of_class_id(None), "mod");
  }

  #[test]
  fn mod_loaders_map_to_curseforge_mod_loader_types() {
    assert_eq!(mod_loader_type_of("Forge"), Some(1));
    assert_eq!(mod_loader_type_of("forge"), Some(1));
    assert_eq!(mod_loader_type_of("LegacyForge"), Some(1));
    assert_eq!(mod_loader_type_of("Fabric"), Some(4));
    assert_eq!(mod_loader_type_of("NeoForge"), Some(6));
    assert_eq!(mod_loader_type_of("All"), None);
  }

  #[test]
  fn sort_fields_match_the_frontend_sort_list() {
    assert_eq!(sort_field_of("Popularity"), 2);
    assert_eq!(sort_field_of("Latest update"), 3);
    assert_eq!(sort_field_of("A-Z"), 4);
    assert_eq!(sort_field_of("Total downloads"), 6);
    assert_eq!(sort_field_of("Creation date"), 11);
    assert_eq!(sort_field_of("unknown"), 2);
  }

  #[test]
  fn release_types_and_relations_use_frontend_vocabulary() {
    assert_eq!(release_type_of(1), "release");
    assert_eq!(release_type_of(2), "beta");
    assert_eq!(release_type_of(3), "alpha");

    assert_eq!(relation_of(1), "embedded");
    assert_eq!(relation_of(2), "optional");
    assert_eq!(relation_of(3), "required");
    assert_eq!(relation_of(4), "tool");
    assert_eq!(relation_of(5), "incompatible");
  }

  #[test]
  fn only_minecraft_versions_are_treated_as_game_versions() {
    assert!(is_game_version_like("1.20.1"));
    assert!(is_game_version_like("1.7.10"));
    assert!(is_game_version_like("1.20.1-pre1"));
    assert!(is_game_version_like("23w14a"));
    assert!(!is_game_version_like("Client"));
    assert!(!is_game_version_like("Server"));
    assert!(!is_game_version_like("Forge"));
    assert!(!is_game_version_like("NeoForge"));
    assert!(!is_game_version_like(""));
  }

  #[test]
  fn files_are_grouped_by_minecraft_version_and_loader() {
    let file = CurseForgeFile {
      id: 9058038,
      mod_id: 238222,
      display_name: "jei-1.20.1-forge.jar".to_string(),
      file_name: "jei-1.20.1-forge.jar".to_string(),
      release_type: 2,
      file_date: "2024-01-01T00:00:00Z".to_string(),
      download_count: 10,
      download_url: None,
      game_versions: vec![
        "Client".to_string(),
        "1.20.1".to_string(),
        "Forge".to_string(),
        "Server".to_string(),
      ],
      hashes: vec![],
      dependencies: vec![],
    };

    assert_eq!(game_versions_of(&file), vec!["1.20.1".to_string()]);
    assert_eq!(
      loader_of_game_versions(&file.game_versions),
      Some("Forge".to_string())
    );
  }

  #[test]
  fn every_supported_loader_is_reported_independently_of_array_order() {
    // Shape observed on the live CurseForge API for a Fabric 1.20.1 file.
    let fabric = vec![
      "Fabric".to_string(),
      "Client".to_string(),
      "1.20.1".to_string(),
      "Quilt".to_string(),
    ];
    assert_eq!(
      loaders_of_game_versions(&fabric),
      vec!["Fabric".to_string(), "Quilt".to_string()]
    );
    assert_eq!(
      fabric
        .iter()
        .filter(|version| is_game_version_like(version))
        .cloned()
        .collect::<Vec<_>>(),
      vec!["1.20.1".to_string()]
    );

    // NeoForge wins over Forge no matter how the response orders them.
    let forwards = vec!["Forge".to_string(), "NeoForge".to_string()];
    let backwards = vec!["NeoForge".to_string(), "Forge".to_string()];
    assert_eq!(
      loader_of_game_versions(&forwards),
      Some("NeoForge".to_string())
    );
    assert_eq!(
      loader_of_game_versions(&backwards),
      Some("NeoForge".to_string())
    );

    // Environment-only entries never look like a loader or a version.
    let none = vec!["Client".to_string(), "Server".to_string()];
    assert!(loaders_of_game_versions(&none).is_empty());
  }

  #[test]
  fn api_keys_are_only_sent_to_the_official_host() {
    // Without a configured key neither host receives an x-api-key header.
    let client = reqwest::Client::new();
    let mirror = authenticated(
      client.get("https://mod.mcimirror.top/curseforge/v1/mods/1"),
      super::ContentSource::Mcim,
    )
    .build()
    .unwrap();
    assert!(mirror.headers().get("x-api-key").is_none());

    let official = authenticated(
      client.get("https://api.curseforge.com/v1/mods/1"),
      super::ContentSource::Official,
    )
    .build()
    .unwrap();
    if super::has_official_api_key() {
      assert!(official.headers().get("x-api-key").is_some());
    } else {
      assert!(official.headers().get("x-api-key").is_none());
    }
  }
}
