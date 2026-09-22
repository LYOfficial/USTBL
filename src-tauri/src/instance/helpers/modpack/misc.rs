use crate::error::USTBLResult;
use crate::instance::helpers::modpack::curseforge::CurseForgeManifest;
use crate::instance::helpers::modpack::modrinth::ModrinthManifest;
use crate::instance::helpers::modpack::multimc::MultiMcManifest;
use crate::instance::models::misc::{InstanceError, ModLoader, ModLoaderType};
use crate::resource::commands::fetch_mod_loader_version_list;
use crate::resource::models::OtherResourceSource;
use crate::tasks::PTaskParam;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::path::Path;
use tauri::AppHandle;
use zip::ZipArchive;

#[async_trait]
pub trait ModpackManifest {
  fn from_archive(file: &File) -> USTBLResult<Self>
  where
    Self: Sized;
  fn get_client_version(&self) -> USTBLResult<String>;
  fn get_mod_loader_type_version(&self) -> USTBLResult<(ModLoaderType, String)>;
  async fn get_meta_info(&self, app: &AppHandle) -> USTBLResult<ModpackMetaInfo>;
  async fn get_download_params(
    &self,
    app: &AppHandle,
    instance_path: &Path,
  ) -> USTBLResult<Vec<PTaskParam>>;
  fn get_overrides_path(&self) -> String;
}

type ManifestBox = Box<dyn ModpackManifest + Send + Sync>;
type Parser = Box<dyn Fn(&File) -> USTBLResult<ManifestBox> + Send + Sync>;

fn get_parsers() -> Vec<Parser> {
  vec![
    Box::new(|f| {
      CurseForgeManifest::from_archive(f).map(|m| {
        let b: ManifestBox = Box::new(m);
        b
      })
    }),
    Box::new(|f| {
      ModrinthManifest::from_archive(f).map(|m| {
        let b: ManifestBox = Box::new(m);
        b
      })
    }),
    Box::new(|f| {
      MultiMcManifest::from_archive(f).map(|m| {
        let b: ManifestBox = Box::new(m);
        b
      })
    }),
  ]
}

impl ModLoader {
  pub async fn with_branch(&self, app: &AppHandle, mc_version: String) -> USTBLResult<Self> {
    let version_list =
      fetch_mod_loader_version_list(app.clone(), mc_version, self.loader_type).await?;
    if let Some(version) = version_list.iter().find(|v| v.version == self.version) {
      return Ok(Self {
        branch: version.branch.clone(),
        ..self.clone()
      });
    }
    Err(InstanceError::ModLoaderVersionParseError.into())
  }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModpackMetaInfo {
  pub name: String,
  pub version: Option<String>,
  pub description: Option<String>,
  pub author: Option<String>,
  pub modpack_source: OtherResourceSource,
  pub client_version: String,
  pub mod_loader: Option<ModLoader>,
}

impl ModpackMetaInfo {
  pub async fn from_archive(app: &AppHandle, file: &File) -> USTBLResult<Self> {
    for parser in get_parsers() {
      if let Ok(manifest) = parser(file) {
        return manifest.get_meta_info(app).await;
      }
    }

    Err(InstanceError::ModpackManifestParseError.into())
  }
}

pub async fn get_download_params(
  app: &AppHandle,
  file: &File,
  instance_path: &Path,
) -> USTBLResult<Vec<PTaskParam>> {
  for parser in get_parsers() {
    if let Ok(manifest) = parser(file) {
      return manifest.get_download_params(app, instance_path).await;
    }
  }

  Err(InstanceError::ModpackManifestParseError.into())
}

pub fn extract_overrides(file: &File, instance_path: &Path) -> USTBLResult<()> {
  let get_overrides_path = |file| {
    for parser in get_parsers() {
      if let Ok(manifest) = parser(file) {
        return Some(manifest.get_overrides_path());
      }
    }
    None
  };
  let overrides_path = get_overrides_path(file).ok_or(InstanceError::ModpackManifestParseError)?;
  let mut archive = ZipArchive::new(file)?;
  for i in 0..archive.len() {
    let mut file = archive.by_index(i)?;
    let prefix = format!("{}/", overrides_path.trim_end_matches('/'));
    let outpath = if let Some(relative) = file.name().strip_prefix(&prefix) {
      if relative.is_empty() || file.is_dir() {
        continue;
      }
      let relative_path = super::multimc::profile::safe_relative(relative)?;
      if file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
        return Err(crate::error::USTBLError(
          "Modpack symlinks are not supported".into(),
        ));
      }
      // An override must not replace launcher-owned metadata at the instance root.
      let instance_name = instance_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
      if relative.eq_ignore_ascii_case("ustblcfg.json")
        || relative.eq_ignore_ascii_case("ustbl-multimc-components.json")
        || relative.eq_ignore_ascii_case(&format!("{instance_name}.json"))
        || relative.eq_ignore_ascii_case(&format!("{instance_name}.jar"))
      {
        return Err(crate::error::USTBLError(format!(
          "Modpack override conflicts with instance metadata: {relative}"
        )));
      }
      instance_path.join(relative_path)
    } else {
      continue;
    };

    if file.is_file() {
      // Create parent directories if they don't exist
      if let Some(p) = outpath.parent() {
        if !p.exists() {
          fs::create_dir_all(p)?;
        }
      }

      // Extract file
      let mut outfile = File::create(&outpath)?;
      std::io::copy(&mut file, &mut outfile)?;
    }
  }
  Ok(())
}
