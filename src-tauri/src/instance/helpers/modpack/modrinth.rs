use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use zip::ZipArchive;

use crate::error::USTBLResult;
use crate::instance::helpers::modpack::misc::{
  select_key_of_path, ModpackDownload, ModpackManifest, ModpackMetaInfo,
};
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

/// The project a Modrinth file belongs to, read from its download address
/// `https://cdn.modrinth.com/data/<projectId>/versions/<versionId>/<file>`.
fn project_id_of_url(url: &str) -> Option<String> {
  let url = url::Url::parse(url).ok()?;
  if !url.host_str()?.ends_with("modrinth.com") {
    return None;
  }
  let mut segments = url.path_segments()?;
  let (data, project, versions) = (segments.next()?, segments.next()?, segments.next()?);
  (data == "data"
    && versions == "versions"
    && !project.is_empty()
    && project.bytes().all(|byte| byte.is_ascii_alphanumeric()))
  .then(|| project.to_string())
}

impl ModrinthManifest {
  /// The key a choice about `file` is remembered under: its project when the
  /// address names one, else its path.
  fn select_key(file: &ModrinthFile) -> String {
    file
      .downloads
      .iter()
      .find_map(|url| project_id_of_url(url))
      .map(|project| format!("modrinth:{project}"))
      .unwrap_or_else(|| select_key_of_path(&file.path))
  }

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
          optional: file
            .env
            .as_ref()
            .is_some_and(|env| env.client == "optional"),
          select_key: Self::select_key(file),
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

  fn has_optional_files(&self) -> bool {
    self.files.iter().any(|file| {
      file
        .env
        .as_ref()
        .is_some_and(|env| env.client == "optional")
    })
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

  #[test]
  fn files_the_author_marked_optional_are_flagged() {
    let pack = manifest(serde_json::json!([
      file("mods/a.jar", Some(("optional", "optional"))),
      file("mods/b.jar", Some(("required", "optional"))),
      file("mods/c.jar", None),
    ]));
    assert!(pack.has_optional_files());
    let flags: Vec<_> = pack
      .download_params(Path::new("inst"))
      .unwrap()
      .into_iter()
      .map(|download| download.optional)
      .collect();
    assert_eq!(flags, [true, false, false]);

    let none = manifest(serde_json::json!([file(
      "mods/b.jar",
      Some(("required", "optional"))
    )]));
    assert!(!none.has_optional_files());
  }

  #[test]
  fn the_select_key_is_the_project_of_the_download_address() {
    let mut entry = file("mods/a-1.0.jar", Some(("optional", "optional")));
    entry["downloads"] = serde_json::json!([
      "https://cdn.modrinth.com/data/m5T5xmUy/versions/8qcmuwZa/BetterGrassify-1.8.8%2Bfabric.26.3.jar"
    ]);
    let mut renamed = entry.clone();
    renamed["path"] = "mods/a-2.0.jar".into();
    renamed["downloads"] = serde_json::json!([
      "https://cdn.modrinth.com/data/m5T5xmUy/versions/OTHERVER/BetterGrassify-2.0.jar"
    ]);
    let keys = |files: serde_json::Value| -> Vec<String> {
      manifest(files)
        .download_params(Path::new("inst"))
        .unwrap()
        .into_iter()
        .map(|download| download.select_key)
        .collect()
    };
    assert_eq!(keys(serde_json::json!([entry])), ["modrinth:m5T5xmUy"]);
    assert_eq!(keys(serde_json::json!([renamed])), ["modrinth:m5T5xmUy"]);
  }

  #[test]
  fn the_select_key_falls_back_to_the_path() {
    let mut other_host = file("mods/Keep.jar", None);
    other_host["downloads"] =
      serde_json::json!(["https://raw.githubusercontent.com/o/r/main/data/x/versions/y/a.jar"]);
    let mut odd_path = file("mods/odd.jar", None);
    odd_path["downloads"] =
      serde_json::json!(["https://cdn.modrinth.com/other/m5T5xmUy/versions/y/odd.jar"]);
    let mut second = file("mods/second.jar", None);
    second["downloads"] = serde_json::json!([
      "https://mirror.example/second.jar",
      "https://cdn.modrinth.com/data/AbCd1234/versions/y/second.jar"
    ]);
    let keys: Vec<_> = manifest(serde_json::json!([other_host, odd_path, second]))
      .download_params(Path::new("inst"))
      .unwrap()
      .into_iter()
      .map(|download| download.select_key)
      .collect();
    assert_eq!(
      keys,
      [
        "path:mods/keep.jar",
        "path:mods/odd.jar",
        "modrinth:AbCd1234"
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
