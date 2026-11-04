use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use zip::ZipArchive;

use crate::error::USTBLResult;
use crate::instance::helpers::modpack::misc::{ModpackDownload, ModpackManifest, ModpackMetaInfo};
use crate::instance::models::misc::{InstanceError, ModLoader, ModLoaderType};
use crate::resource::models::OtherResourceSource;
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};

structstruck::strike! {
#[strikethrough[derive(Deserialize, Serialize, Debug, Clone)]]
#[strikethrough[serde(rename_all = "camelCase")]]
pub struct ModrinthFile {
  pub path: String,
  pub hashes: struct ModrinthFileHashes {
    pub sha1: String,
    pub sha512: String,
  },
  pub env: Option<pub struct ModrinthFileEnv {
    pub client: String,
    pub server: String,
  }>,
  pub downloads: Vec<String>,
  pub file_size: u64,
}
}

#[derive(Default, Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModrinthManifest {
  #[serde(default)]
  pub format_version: u64,
  #[serde(default)]
  pub game: String,
  pub version_id: String,
  pub name: String,
  pub summary: Option<String>,
  pub files: Vec<ModrinthFile>,
  pub dependencies: HashMap<String, String>,
}

impl ModrinthManifest {
  fn download_params(&self, instance_path: &Path) -> USTBLResult<Vec<ModpackDownload>> {
    self
      .files
      .iter()
      // This launcher is a client: files the format marks as unsupported on
      // the client are never installed.
      .filter(|file| {
        file
          .env
          .as_ref()
          .is_none_or(|env| env.client != "unsupported")
      })
      .map(|file| {
        let (download_url, others) = file
          .downloads
          .split_first()
          .ok_or(InstanceError::InvalidSourcePath)?;
        // The first address is the main source; the others are tried when it
        // fails. An unusable extra address is not worth failing the pack for.
        let fallbacks = others
          .iter()
          .filter_map(|other| url::Url::parse(other).ok())
          .collect();
        Ok(ModpackDownload {
          param: DownloadParam {
            src: url::Url::parse(download_url).map_err(|_| InstanceError::InvalidSourcePath)?,
            sha1: Some(file.hashes.sha1.clone()),
            dest: instance_path.join(&file.path),
            filename: None,
            custom_headers: None,
            transfer_options: DownloadTransferOptions::with_fallbacks(fallbacks),
          },
          size: Some(file.file_size),
        })
      })
      .collect::<USTBLResult<Vec<_>>>()
  }
}

#[async_trait]
impl ModpackManifest for ModrinthManifest {
  fn from_archive(file: &File) -> USTBLResult<Self> {
    let mut archive = ZipArchive::new(file)?;
    let mut manifest_file = archive.by_name("modrinth.index.json")?;
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
      version: Some(self.version_id.clone()),
      description: self.summary.clone(),
      author: None,
      modpack_source: OtherResourceSource::Modrinth,
      client_version,
      mod_loader,
    })
  }

  fn get_client_version(&self) -> USTBLResult<String> {
    Ok(
      self
        .dependencies
        .get("minecraft")
        .ok_or(InstanceError::ModpackManifestParseError)?
        .to_string(),
    )
  }

  fn get_mod_loader_type_version(&self) -> USTBLResult<(ModLoaderType, String)> {
    for (key, val) in &self.dependencies {
      match key.as_str() {
        "minecraft" => continue,
        "forge" => return Ok((ModLoaderType::Forge, val.to_string())),
        "fabric-loader" => return Ok((ModLoaderType::Fabric, val.to_string())),
        "neoforge" => return Ok((ModLoaderType::NeoForge, val.to_string())),
        _ => return Err(InstanceError::UnsupportedModLoader.into()),
      }
    }
    Err(InstanceError::ModpackManifestParseError.into())
  }

  async fn get_download_params(
    &self,
    _app: &AppHandle,
    instance_path: &Path,
  ) -> USTBLResult<Vec<ModpackDownload>> {
    self.download_params(instance_path)
  }

  fn get_overrides_paths(&self) -> Vec<String> {
    // Format order: `overrides/` applies everywhere, `client-overrides/` is laid on
    // top of it. `server-overrides/` never applies to a client.
    vec!["overrides".to_string(), "client-overrides".to_string()]
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn manifest(files: serde_json::Value) -> ModrinthManifest {
    serde_json::from_value(serde_json::json!({
      "formatVersion": 1,
      "game": "minecraft",
      "versionId": "1",
      "name": "pack",
      "files": files,
      "dependencies": { "minecraft": "1.20.1" },
    }))
    .unwrap()
  }

  fn file(path: &str, env: Option<(&str, &str)>) -> serde_json::Value {
    let mut file = serde_json::json!({
      "path": path,
      "hashes": { "sha1": "a", "sha512": "b" },
      "downloads": [format!("https://cdn.example/{path}")],
      "fileSize": 3,
    });
    if let Some((client, server)) = env {
      file["env"] = serde_json::json!({ "client": client, "server": server });
    }
    file
  }

  #[test]
  fn files_unsupported_on_the_client_are_skipped() {
    let pack = manifest(serde_json::json!([
      file("mods/server-only.jar", Some(("unsupported", "required"))),
      file("mods/both.jar", Some(("required", "required"))),
      file("mods/optional.jar", Some(("optional", "unsupported"))),
      file("mods/no-env.jar", None),
    ]));
    let root = Path::new("inst");
    let dests: Vec<_> = pack
      .download_params(root)
      .unwrap()
      .into_iter()
      .map(|download| download.param.dest)
      .collect();
    assert_eq!(
      dests,
      vec![
        root.join("mods/both.jar"),
        root.join("mods/optional.jar"),
        root.join("mods/no-env.jar"),
      ]
    );
  }

  fn only_download(downloads: serde_json::Value) -> USTBLResult<ModpackDownload> {
    let mut entry = file("mods/a.jar", None);
    entry["downloads"] = downloads;
    let mut params = manifest(serde_json::json!([entry])).download_params(Path::new("inst"))?;
    Ok(params.remove(0))
  }

  #[test]
  fn extra_addresses_become_fallback_sources() {
    let download = only_download(serde_json::json!([
      "https://cdn.example/a.jar",
      "https://mirror.example/a.jar",
      "https://other.example/a.jar",
    ]))
    .unwrap();
    assert_eq!(download.param.src.as_str(), "https://cdn.example/a.jar");
    let fallbacks: Vec<_> = download
      .param
      .transfer_options
      .fallback_sources
      .iter()
      .map(|url| url.as_str())
      .collect();
    assert_eq!(
      fallbacks,
      [
        "https://mirror.example/a.jar",
        "https://other.example/a.jar"
      ]
    );
  }

  #[test]
  fn a_single_address_keeps_the_default_transfer_options() {
    let download = only_download(serde_json::json!(["https://cdn.example/a.jar"])).unwrap();
    assert_eq!(download.param.transfer_options, Default::default());
  }

  #[test]
  fn unusable_extra_addresses_are_skipped() {
    let download = only_download(serde_json::json!([
      "https://cdn.example/a.jar",
      "not a url",
      "https://mirror.example/a.jar",
    ]))
    .unwrap();
    assert_eq!(
      download.param.transfer_options.fallback_sources,
      vec![url::Url::parse("https://mirror.example/a.jar").unwrap()]
    );
  }

  #[test]
  fn a_missing_or_unusable_first_address_still_fails() {
    assert!(only_download(serde_json::json!([])).is_err());
    assert!(only_download(serde_json::json!([
      "not a url",
      "https://mirror.example/a.jar"
    ]))
    .is_err());
  }

  #[test]
  fn a_skipped_file_does_not_need_a_usable_download() {
    let mut unsupported = file("mods/server-only.jar", Some(("unsupported", "required")));
    unsupported["downloads"] = serde_json::json!([]);
    let pack = manifest(serde_json::json!([unsupported]));
    assert!(pack.download_params(Path::new("inst")).unwrap().is_empty());
  }
}
