use crate::error::USTBLResult;
use crate::instance::helpers::modpack::curseforge::CurseForgeManifest;
use crate::instance::helpers::modpack::modrinth::ModrinthManifest;
use crate::instance::helpers::modpack::multimc::MultiMcManifest;
use crate::instance::models::misc::{InstanceError, ModLoader, ModLoaderType};
use crate::resource::commands::fetch_mod_loader_version_list;
use crate::resource::models::OtherResourceSource;
use crate::tasks::download::DownloadParam;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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
  ) -> USTBLResult<Vec<ModpackDownload>>;
  /// Archive directories holding files to copy into the instance, in layering
  /// order: when several contain the same file, the later directory wins.
  fn get_overrides_paths(&self) -> Vec<String>;
}

/// A file the modpack manifest asks the launcher to download.
pub struct ModpackDownload {
  pub param: DownloadParam,
  /// Size in bytes when the manifest (or the platform API) reports it.
  pub size: Option<u64>,
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

pub(crate) fn is_launcher_metadata_path(relative: &str, instance_name: &str) -> bool {
  relative.eq_ignore_ascii_case("ustblcfg.json")
    || relative.eq_ignore_ascii_case("ustbl-multimc-components.json")
    || relative.eq_ignore_ascii_case(super::multimc::snapshot::SNAPSHOT_FILE_NAME)
    || relative.eq_ignore_ascii_case(super::baseline::BASELINE_FILE_NAME)
    || relative.eq_ignore_ascii_case(&format!("{instance_name}.json"))
    || relative.eq_ignore_ascii_case(&format!("{instance_name}.jar"))
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
) -> USTBLResult<Vec<ModpackDownload>> {
  for parser in get_parsers() {
    if let Ok(manifest) = parser(file) {
      return manifest.get_download_params(app, instance_path).await;
    }
  }

  Err(InstanceError::ModpackManifestParseError.into())
}

/// Extract the modpack's override directories into the instance and return the
/// relative paths (using `/`) of the files that exist afterwards.
///
/// Directories are layered in the order the format defines, so a later layer
/// replaces files of an earlier one. `server-overrides/` of the Modrinth format
/// is deliberately never extracted: it only applies to servers.
pub fn extract_overrides(file: &File, instance_path: &Path) -> USTBLResult<Vec<String>> {
  let overrides_paths = get_parsers()
    .into_iter()
    .find_map(|parser| parser(file).ok())
    .map(|manifest| manifest.get_overrides_paths())
    .ok_or(InstanceError::ModpackManifestParseError)?;
  let instance_name = instance_path
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .to_string();
  let mut archive = ZipArchive::new(file)?;
  let mut extracted = BTreeSet::new();

  for overrides_path in overrides_paths {
    let prefix = format!("{}/", overrides_path.trim_end_matches('/'));
    for i in 0..archive.len() {
      let mut entry = archive.by_index(i)?;
      let relative = match entry.name().strip_prefix(&prefix) {
        Some(relative) if !relative.is_empty() && !entry.is_dir() => relative.to_string(),
        _ => continue,
      };
      let relative_path = super::multimc::profile::safe_relative(&relative)?.to_path_buf();
      if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
        return Err(crate::error::USTBLError(
          "Modpack symlinks are not supported".into(),
        ));
      }
      // An override must not replace launcher-owned metadata at the instance root.
      if is_launcher_metadata_path(&relative, &instance_name) {
        continue;
      }
      if !entry.is_file() {
        continue;
      }

      let outpath = instance_path.join(relative_path);
      if let Some(parent) = outpath.parent() {
        if !parent.exists() {
          fs::create_dir_all(parent)?;
        }
      }
      let mut outfile = File::create(&outpath)?;
      std::io::copy(&mut entry, &mut outfile)?;
      extracted.insert(relative);
    }
  }
  Ok(extracted.into_iter().collect())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;
  use zip::write::SimpleFileOptions;

  const MRPACK_INDEX: &str =
    r#"{"versionId":"1","name":"test","files":[],"dependencies":{"minecraft":"1.20.1"}}"#;

  /// Write an mrpack with the given entries and extract it into a fresh instance.
  fn extract(entries: &[(&str, &str)]) -> (std::path::PathBuf, std::path::PathBuf, Vec<String>) {
    let root = std::env::temp_dir().join(format!("ustbl-modpack-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let archive_path = root.join("pack.mrpack");
    let instance_path = root.join("instance");
    let mut archive = zip::ZipWriter::new(File::create(&archive_path).unwrap());
    for (path, content) in std::iter::once(&("modrinth.index.json", MRPACK_INDEX)).chain(entries) {
      archive
        .start_file(*path, SimpleFileOptions::default())
        .unwrap();
      archive.write_all(content.as_bytes()).unwrap();
    }
    archive.finish().unwrap();

    let file = File::open(&archive_path).unwrap();
    let extracted = extract_overrides(&file, &instance_path).unwrap();
    (root, instance_path, extracted)
  }

  #[test]
  fn launcher_metadata_override_is_skipped() {
    let (root, instance_path, extracted) = extract(&[
      ("overrides/ustblcfg.json", "old config"),
      ("overrides/ustbl-modpack-baseline.json", "old baseline"),
      ("overrides/ustbl-multimc-profile.json", "old profile"),
      ("overrides/config/example.cfg", "kept"),
    ]);

    assert!(!instance_path.join("ustblcfg.json").exists());
    assert!(!instance_path.join("ustbl-modpack-baseline.json").exists());
    assert!(!instance_path.join("ustbl-multimc-profile.json").exists());
    assert_eq!(
      fs::read_to_string(instance_path.join("config/example.cfg")).unwrap(),
      "kept"
    );
    assert_eq!(extracted, vec!["config/example.cfg".to_string()]);
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn client_overrides_layer_over_overrides_and_server_overrides_are_ignored() {
    let (root, instance_path, extracted) = extract(&[
      ("overrides/config/a.cfg", "base"),
      ("overrides/config/b.cfg", "only in base"),
      ("client-overrides/config/a.cfg", "client"),
      ("client-overrides/mods/c.jar", "client only"),
      ("server-overrides/config/s.cfg", "server"),
    ]);

    let read = |path: &str| fs::read_to_string(instance_path.join(path)).unwrap();
    assert_eq!(read("config/a.cfg"), "client");
    assert_eq!(read("config/b.cfg"), "only in base");
    assert_eq!(read("mods/c.jar"), "client only");
    assert!(!instance_path.join("config/s.cfg").exists());
    assert_eq!(
      extracted,
      vec!["config/a.cfg", "config/b.cfg", "mods/c.jar"]
    );
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn pack_with_only_client_overrides_still_extracts() {
    let (root, instance_path, extracted) =
      extract(&[("client-overrides/options.txt", "lang:en_us")]);

    assert_eq!(
      fs::read_to_string(instance_path.join("options.txt")).unwrap(),
      "lang:en_us"
    );
    assert_eq!(extracted, vec!["options.txt".to_string()]);
    let _ = fs::remove_dir_all(root);
  }
}
