use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use zip::ZipArchive;

use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::misc::{ModpackManifest, ModpackMetaInfo};
use crate::instance::models::misc::{InstanceError, ModLoader, ModLoaderType};
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::curseforge::misc::has_official_api_key;
use crate::resource::helpers::mcim::{
  curseforge_api_base, get_content_source_priority_list, ContentSource,
};
use crate::resource::models::OtherResourceSource;
use crate::tasks::download::DownloadParam;
use crate::tasks::PTaskParam;
use std::sync::Mutex;

lazy_static! {
  static ref CURSEFORGE_API_KEY: String = env::var("USTBL_CURSEFORGE_API_KEY").unwrap_or_default();
}

/// Source order for CurseForge metadata lookups, following the resource download
/// strategy.
///
/// Without an API key the official host only answers `403`, so the MCIM mirror is
/// the source that actually keeps CurseForge modpacks installable.
fn curseforge_content_sources(app: &AppHandle) -> Vec<ContentSource> {
  app
    .state::<Mutex<LauncherConfig>>()
    .lock()
    .map(|config| get_content_source_priority_list(&config))
    .unwrap_or_else(|_| vec![ContentSource::Mcim, ContentSource::Official])
}

/// Fetch a CurseForge API document, trying the configured sources in order.
async fn fetch_curseforge<T>(
  client: &reqwest::Client,
  priority: &[ContentSource],
  endpoint: &str,
) -> USTBLResult<T>
where
  T: serde::de::DeserializeOwned,
{
  let mut last_error: Option<USTBLError> = None;

  for source in priority {
    // The key is only meaningful on the official host, and is never sent to the
    // mirror.
    if *source == ContentSource::Official && !has_official_api_key() {
      continue;
    }

    let url = format!("{}{}", curseforge_api_base(*source), endpoint);
    let request = client.get(&url).header("accept", "application/json");
    let request = match source {
      ContentSource::Official => request.header("x-api-key", CURSEFORGE_API_KEY.as_str()),
      ContentSource::Mcim => request,
    };

    match request.send().await {
      Ok(response) if response.status().is_success() => match response.json::<T>().await {
        Ok(value) => return Ok(value),
        Err(error) => {
          eprintln!("{:?}", error);
          last_error = Some(USTBLError(format!("{:?}", error)));
        }
      },
      Ok(response) => {
        log::warn!(
          "CurseForge request to {} failed with status {}",
          url,
          response.status()
        );
        last_error = Some(InstanceError::NetworkError.into());
      }
      Err(error) => {
        log::warn!("CurseForge request to {} failed: {:?}", url, error);
        last_error = Some(InstanceError::NetworkError.into());
      }
    }
  }

  Err(last_error.unwrap_or_else(|| InstanceError::NetworkError.into()))
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeModLoader {
  pub id: String,
  pub primary: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFiles {
  #[serde(rename = "projectID")]
  pub project_id: u64,
  #[serde(rename = "fileID")]
  pub file_id: u64,
  pub required: bool,
}

structstruck::strike! {
#[strikethrough[derive(Deserialize, Serialize, Debug, Clone)]]
#[strikethrough[serde(rename_all = "camelCase")]]
  pub struct CurseForgeManifest {
    pub name: String,
    pub version: Option<String>,
    pub author: String,
    pub overrides: String,
    pub minecraft: struct {
      pub version: String,
      pub mod_loaders: Vec<CurseForgeModLoader>,
    },
    pub files: Vec<CurseForgeFiles>,
  }
}

structstruck::strike! {
#[strikethrough[derive(Deserialize, Serialize, Debug, Clone)]]
#[strikethrough[serde(rename_all = "camelCase")]]
  pub struct CurseForgeFileManifest {
    pub data: struct {
      pub download_url: Option<String>,
      pub file_name: String,
      pub hashes: Option<Vec<pub struct {
        pub value: String,
        pub algo: u64,
      }>>,
    }
  }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeProjectRes {
  pub data: CurseForgeProject,
}

structstruck::strike! {
#[strikethrough[derive(Deserialize, Serialize, Debug, Clone)]]
#[strikethrough[serde(rename_all = "camelCase")]]
  pub struct CurseForgeProject {
    pub id: i32,
    pub class_id: Option<i32>,
  }
}

#[async_trait]
impl ModpackManifest for CurseForgeManifest {
  fn from_archive(file: &File) -> USTBLResult<Self> {
    let mut archive = ZipArchive::new(file)?;
    let mut manifest_file = archive.by_name("manifest.json")?;
    let mut manifest_content = String::new();
    manifest_file.read_to_string(&mut manifest_content)?;
    let manifest: Self = serde_json::from_str(&manifest_content).inspect_err(|e| {
      eprintln!("{:?}", e);
    })?;

    Ok(manifest)
  }

  async fn get_meta_info(&self, app: &AppHandle) -> USTBLResult<ModpackMetaInfo> {
    let client_version = self.get_client_version()?;
    let mod_loader = if let Ok((loader_type, version)) = self.get_mod_loader_type_version() {
      Some(
        ModLoader {
          loader_type,
          version,
          ..Default::default()
        }
        .with_branch(app, client_version.clone())
        .await?,
      )
    } else {
      None
    };
    Ok(ModpackMetaInfo {
      name: self.name.clone(),
      version: self.version.clone(),
      description: None,
      author: Some(self.author.clone()),
      modpack_source: OtherResourceSource::CurseForge,
      client_version,
      mod_loader,
    })
  }

  fn get_client_version(&self) -> USTBLResult<String> {
    Ok(self.minecraft.version.clone())
  }

  fn get_mod_loader_type_version(&self) -> USTBLResult<(ModLoaderType, String)> {
    let loader = self
      .minecraft
      .mod_loaders
      .iter()
      .find(|l| l.primary)
      .ok_or(InstanceError::ModLoaderVersionParseError)?;

    let Some((loader_type, version)) = loader.id.split_once('-') else {
      return Err(InstanceError::ModLoaderVersionParseError.into());
    };
    Ok((
      ModLoaderType::from_str(loader_type)
        .ok()
        .ok_or(InstanceError::ModLoaderVersionParseError)?,
      version.to_string(),
    ))
  }

  async fn get_download_params(
    &self,
    app: &AppHandle,
    instance_path: &Path,
  ) -> USTBLResult<Vec<PTaskParam>> {
    let client = app.state::<reqwest::Client>();
    let instance_path = instance_path.to_path_buf();
    // Resolved once so each file lookup does not have to re-read the config.
    let priority = curseforge_content_sources(app);

    let tasks = self.files.iter().map(|file| {
      let client = client.clone();
      let instance_path = instance_path.clone();
      let priority = priority.clone();
      let file_id = file.file_id;
      let project_id = file.project_id;

      async move {
        let class_id = {
          let project: CurseForgeProjectRes =
            fetch_curseforge(&client, &priority, &format!("/mods/{project_id}")).await?;
          project.data.class_id
        };

        let file_manifest: CurseForgeFileManifest = fetch_curseforge(
          &client,
          &priority,
          &format!("/mods/{project_id}/files/{file_id}"),
        )
        .await?;

        let download_url = file_manifest.data.download_url.clone().unwrap_or_else(|| {
          format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            urlencoding::encode(&file_manifest.data.file_name)
          )
        });

        let sha1 = file_manifest
          .data
          .hashes
          .as_ref()
          .and_then(|hs| hs.iter().find(|h| h.algo == 1))
          .map(|h| h.value.clone());

        let task_param = PTaskParam::Download(DownloadParam {
          src: url::Url::parse(&download_url).map_err(|_| InstanceError::InvalidSourcePath)?,
          sha1,
          dest: instance_path
            .join(match class_id {
              Some(12) | Some(6945) => "resourcepacks",
              Some(6552) => "shaderpacks",
              _ => "mods",
            })
            .join(&file_manifest.data.file_name),
          filename: Some(file_manifest.data.file_name.clone()),
          custom_headers: None,
          transfer_options: Default::default(),
        });

        Ok::<PTaskParam, USTBLError>(task_param)
      }
    });

    let results = futures::future::join_all(tasks).await;

    let mut task_params = Vec::new();
    for result in results {
      task_params.push(result?);
    }
    Ok(task_params)
  }

  fn get_overrides_path(&self) -> String {
    self.overrides.clone()
  }
}
