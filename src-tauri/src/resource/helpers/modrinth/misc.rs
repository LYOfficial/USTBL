use crate::error::{USTBLError, USTBLResult};
use crate::resource::helpers::mcim::{
  get_content_source_priority_list, modrinth_api_base, ContentSource, MCIM_BASE,
};
use crate::resource::helpers::misc::version_pack_sort;
use crate::resource::models::{
  OtherResourceApiEndpoint, OtherResourceDependency, OtherResourceFileInfo, OtherResourceInfo,
  OtherResourceRequestType, OtherResourceSearchRes, OtherResourceSource, OtherResourceVersionPack,
  ResourceError,
};
use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

pub async fn make_modrinth_request<T, P>(
  client: &reqwest::Client,
  url: &str,
  request_type: OtherResourceRequestType<'_, P>,
) -> USTBLResult<T>
where
  T: serde::de::DeserializeOwned,
  P: serde::Serialize,
{
  let request_builder = match request_type {
    OtherResourceRequestType::GetWithParams(params) => client.get(url).query(params),
    OtherResourceRequestType::Get => client.get(url),
    OtherResourceRequestType::Post(payload) => client.post(url).json(payload),
  };

  let response = request_builder
    .send()
    .await
    .map_err(|_| ResourceError::NetworkError)?;

  if !response.status().is_success() {
    return Err(ResourceError::NetworkError.into());
  }

  response
    .json::<T>()
    .await
    .map_err(|_| ResourceError::ParseError.into())
}

/// Effective content source order for the current configuration.
///
/// Falling back to the mirror keeps the launcher working when the configuration
/// cannot be read.
pub fn content_source_priority(app: &AppHandle) -> Vec<ContentSource> {
  match app
    .state::<std::sync::Mutex<crate::launcher_config::models::LauncherConfig>>()
    .lock()
  {
    Ok(config) => get_content_source_priority_list(&config),
    Err(_) => vec![ContentSource::Mcim, ContentSource::Official],
  }
}

/// Run a Modrinth API request against every configured source in order. MCIM
/// serves the same routes under `/modrinth/v2`, so a failing mirror falls back
/// to the official API and vice versa.
pub async fn make_modrinth_request_with_fallbacks<T, P>(
  app: &AppHandle,
  endpoint: OtherResourceApiEndpoint,
  param: Option<&str>,
  request_type: OtherResourceRequestType<'_, P>,
) -> USTBLResult<T>
where
  T: serde::de::DeserializeOwned,
  P: serde::Serialize,
{
  let client = app.state::<reqwest::Client>();
  let mut last_error: Option<USTBLError> = None;

  for source in content_source_priority(app) {
    let Some(url) = get_modrinth_api(source, endpoint, param)? else {
      continue;
    };

    // `request_type` cannot be cloned, so rebuild an equivalent borrow for the
    // attempt instead.
    let attempt = match &request_type {
      OtherResourceRequestType::GetWithParams(params) => {
        OtherResourceRequestType::GetWithParams(*params)
      }
      OtherResourceRequestType::Get => OtherResourceRequestType::Get,
      OtherResourceRequestType::Post(payload) => OtherResourceRequestType::Post(*payload),
    };

    match make_modrinth_request::<T, P>(&client, &url, attempt).await {
      Ok(value) => return Ok(value),
      Err(error) => {
        log::warn!("Modrinth request via {:?} failed: {:?}", source, error);
        last_error = Some(error);
      }
    }
  }

  Err(last_error.unwrap_or_else(|| ResourceError::NoDownloadApi.into()))
}

/// Modrinth API URL for the given endpoint, or `None` when the endpoint is
/// served by another host (translation goes through MCIM directly).
pub fn get_modrinth_api(
  source: ContentSource,
  endpoint: OtherResourceApiEndpoint,
  param: Option<&str>,
) -> USTBLResult<Option<String>> {
  let base_url = modrinth_api_base(source);

  let url = match endpoint {
    OtherResourceApiEndpoint::Search => format!("{}/search", base_url),
    OtherResourceApiEndpoint::VersionPack => {
      let project_id = param.ok_or(ResourceError::ParseError)?;
      format!("{}/project/{}/version", base_url, project_id)
    }
    OtherResourceApiEndpoint::FromLocal => {
      let hash = param.ok_or(ResourceError::ParseError)?;
      format!("{}/version_file/{}", base_url, hash)
    }
    OtherResourceApiEndpoint::ById => {
      let project_id = param.ok_or(ResourceError::ParseError)?;
      format!("{}/project/{}", base_url, project_id)
    }
    // Translation is an MCIM-only helper endpoint, not part of the official API.
    OtherResourceApiEndpoint::TranslateDesc => {
      let project_id = param.ok_or(ResourceError::ParseError)?;
      format!("{}/translate/modrinth/{}", MCIM_BASE, project_id)
    }
    OtherResourceApiEndpoint::Categories => return Ok(None),
  };

  Ok(Some(url))
}

// A unified struct for both search projects and get project by id responses
#[derive(Deserialize, Debug)]
pub struct ModrinthProject {
  #[serde(alias = "id")]
  pub project_id: String,
  pub project_type: String,
  pub slug: String,
  pub title: String,
  pub description: String,
  pub categories: Vec<String>,
  pub downloads: u64,
  pub icon_url: Option<String>,
  #[serde(alias = "updated")]
  pub date_modified: String,
  pub author: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ModrinthSearchRes {
  pub hits: Vec<ModrinthProject>,
  pub total_hits: u64,
  pub offset: u32,
  pub limit: u32,
}

structstruck::strike! {
#[strikethrough[derive(Deserialize, Debug)]]
  pub struct ModrinthFileInfo {
    pub url: String,
    pub filename: String,
    pub hashes: pub struct {
      pub sha1: String,
    },
  }
}

structstruck::strike! {
#[strikethrough[derive(Deserialize, Debug)]]
  pub struct ModrinthVersionPack {
    pub project_id: String,
    pub dependencies: Vec<pub struct {
      pub project_id: Option<String>,
      pub dependency_type: String,
    }>,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub name: String,
    pub date_published: String,
    pub downloads: u64,
    pub version_type: String,
    pub files: Vec<ModrinthFileInfo>,
  }
}

#[derive(Deserialize, Debug)]
pub struct ModrinthTranslationRes {
  pub translated: String,
}

fn normalize_modrinth_loader(loader: &str) -> Option<String> {
  if loader.is_empty() || loader == "minecraft" {
    None
  } else {
    match loader.to_lowercase().as_str() {
      "forge" => Some("Forge".to_string()),
      "fabric" => Some("Fabric".to_string()),
      "neoforge" => Some("NeoForge".to_string()),
      "vanilla" => Some("Vanilla".to_string()),
      "iris" => Some("Iris".to_string()),
      "canvas" => Some("Canvas".to_string()),
      "optifine" => Some("OptiFine".to_string()),
      _ => Some(loader.to_string()),
    }
  }
}

pub fn map_modrinth_file_to_version_pack(
  res: Vec<ModrinthVersionPack>,
) -> Vec<OtherResourceVersionPack> {
  let mut version_packs = std::collections::HashMap::new();

  for version in res {
    let game_versions = if version.game_versions.is_empty() {
      vec!["".to_string()]
    } else {
      version.game_versions.clone()
    };

    const ALLOWED_LOADERS: &[&str] = &[
      "forge",
      "fabric",
      "neoforge",
      "vanilla",
      "iris",
      "canvas",
      "optifine",
      "minecraft",
    ];

    let loaders = if version.loaders.is_empty() {
      vec!["".to_string()]
    } else {
      version
        .loaders
        .iter()
        .filter(|loader| ALLOWED_LOADERS.contains(&loader.as_str()))
        .cloned()
        .collect::<Vec<_>>()
    };

    for game_version in &game_versions {
      for loader in &loaders {
        let file_infos = version
          .files
          .iter()
          .map(|file| (&version, file, normalize_modrinth_loader(loader)).into())
          .collect::<Vec<_>>();

        version_packs
          .entry(game_version.clone())
          .or_insert_with(|| OtherResourceVersionPack {
            name: game_version.clone(),
            items: Vec::new(),
          })
          .items
          .extend(file_infos);
      }
    }
  }

  let mut list: Vec<OtherResourceVersionPack> = version_packs.into_values().collect();
  list.sort_by(version_pack_sort);

  list
}

impl From<ModrinthProject> for OtherResourceInfo {
  fn from(project: ModrinthProject) -> Self {
    Self {
      id: project.project_id,
      mcmod_id: 0,
      _type: project.project_type,
      name: project.title,
      slug: project.slug.to_string(),
      description: project.description,
      icon_src: project.icon_url.unwrap_or_default(),
      website_url: format!("https://modrinth.com/mod/{}", project.slug),
      tags: project.categories,
      last_updated: project.date_modified,
      downloads: project.downloads,
      source: OtherResourceSource::Modrinth,
      translated_name: None,
      translated_description: None,
      author: project.author,
    }
  }
}

impl From<(&ModrinthVersionPack, &ModrinthFileInfo, Option<String>)> for OtherResourceFileInfo {
  fn from(
    (version, file, loader): (&ModrinthVersionPack, &ModrinthFileInfo, Option<String>),
  ) -> Self {
    Self {
      resource_id: version.project_id.clone(),
      name: version.name.clone(),
      release_type: version.version_type.clone(),
      downloads: version.downloads,
      file_date: version.date_published.clone(),
      download_url: file.url.clone(),
      sha1: file.hashes.sha1.clone(),
      file_name: file.filename.clone(),
      dependencies: version
        .dependencies
        .iter()
        .map(|d| OtherResourceDependency {
          resource_id: d.project_id.clone().unwrap_or_default(),
          relation: d.dependency_type.clone(),
        })
        .collect(),
      loader,
    }
  }
}

impl From<ModrinthSearchRes> for OtherResourceSearchRes {
  fn from(res: ModrinthSearchRes) -> Self {
    let list = res.hits.into_iter().map(OtherResourceInfo::from).collect();

    Self {
      list,
      total: res.total_hits,
      page: res.offset / res.limit,
      page_size: res.limit,
    }
  }
}

pub async fn translate_description_modrinth(
  app: &AppHandle,
  resource_id: &str,
) -> USTBLResult<Option<String>> {
  let result = async {
    let url = get_modrinth_api(
      ContentSource::Mcim,
      OtherResourceApiEndpoint::TranslateDesc,
      Some(resource_id),
    )?
    .ok_or(ResourceError::NoDownloadApi)?;
    let client = app.state::<reqwest::Client>();

    let translation_res = client
      .get(&url)
      .send()
      .await?
      .json::<ModrinthTranslationRes>()
      .await?;

    Ok::<_, USTBLError>(translation_res.translated)
  }
  .await;

  // Only return Ok(None) when translation fails, to avoid blocking major functionality
  Ok(result.ok())
}
