//! The version of a modpack an instance is being updated to, independent of
//! which platform it came from.

use super::diff::FileKey;
use super::packs::PackSession;
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::baseline::{
  compute_content_hash, BaselineFile, BaselineLoader, ComponentRef,
};
use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::models::misc::InstanceError;
use crate::tasks::download::DownloadParam;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

pub fn safe_relative_path(path: &str) -> USTBLResult<PathBuf> {
  let path = PathBuf::from(path);
  if path.is_absolute() || path.components().any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_))) {
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  Ok(path)
}

/// Directory inside an instance that holds the staging area and the journal of
/// an update in progress. Nothing in it is ever managed by a modpack.
pub const UPDATE_DIR: &str = ".ustbl-update";

#[derive(Debug, Clone)]
pub enum FileSource {
  /// Fetched over the network; `dest` is replaced by the staging path.
  Download(Box<DownloadParam>),
  /// Taken out of an archive that was already downloaded.
  ArchiveEntry { archive: PathBuf, entry: String },
  /// Stored in the campus platform's packs, found by the file's sha1. The
  /// session is told which files are wanted before any is fetched.
  Pack(Arc<PackSession>),
}

#[derive(Debug, Clone)]
pub struct RemoteFile {
  /// Relative to the instance directory, always with `/`.
  pub path: String,
  pub key: FileKey,
  pub source: FileSource,
}

/// What it takes to rebuild a component profile on an instance that has none:
/// the declared chain and the files the pack ships itself.
#[derive(Debug, Clone)]
pub struct ComponentPack {
  /// `mmc-pack.json` as published.
  pub mmc_pack: serde_json::Value,
  pub instance_cfg: BTreeMap<String, String>,
  /// Named relative to the pack: `patches/<uid>.json`, `libraries/<file>`.
  pub files: Vec<RemoteFile>,
}

#[derive(Debug, Clone)]
pub struct RemoteModpack {
  pub version_id: String,
  /// Human readable version name for the interface.
  pub label: String,
  pub game_version: String,
  pub loader: BaselineLoader,
  /// Component chain of a component-profile pack in canonical order, `None`
  /// when the pack has none.
  pub components: Option<Vec<ComponentRef>>,
  /// Present exactly when `components` is.
  pub component_pack: Option<ComponentPack>,
  pub files: Vec<RemoteFile>,
}

impl RemoteModpack {
  /// Reject anything that could write outside the instance or over launcher
  /// owned files, and paths that collide on case-insensitive file systems.
  pub fn validate(&self, instance_name: &str) -> USTBLResult<()> {
    if let Some(pack) = &self.component_pack {
      for file in &pack.files {
        safe_relative(&file.path)?;
        if !file.path.starts_with("patches/") && !file.path.starts_with("libraries/") {
          return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
        }
      }
    }
    let mut seen = BTreeSet::new();
    for file in &self.files {
      safe_relative(&file.path)?;
      let lowered = file.path.to_lowercase();
      if lowered == UPDATE_DIR
        || lowered.starts_with(&format!("{UPDATE_DIR}/"))
        || is_launcher_metadata_path(&file.path, instance_name)
        || file
          .path
          .to_lowercase()
          .ends_with(super::diff::DISABLED_SUFFIX)
        || !seen.insert(lowered)
      {
        return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
      }
    }
    Ok(())
  }

  pub fn key_map(&self) -> BTreeMap<String, FileKey> {
    self
      .files
      .iter()
      .map(|file| (file.path.clone(), file.key.clone()))
      .collect()
  }

  pub fn baseline_files(&self) -> Vec<BaselineFile> {
    self
      .files
      .iter()
      .map(|file| BaselineFile {
        path: file.path.clone(),
        sha1: file.key.sha1.clone(),
        size: file.key.size,
      })
      .collect()
  }

  pub fn content_hash(&self) -> String {
    let components = self
      .components
      .as_ref()
      .map(|chain| serde_json::to_value(chain).expect("component refs serialize to JSON"));
    compute_content_hash(&self.loader, components.as_ref(), &self.baseline_files())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::ModLoaderType;

  fn pack(paths: &[&str]) -> RemoteModpack {
    RemoteModpack {
      version_id: "2".into(),
      label: "2.0".into(),
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16.0".into(),
      },
      components: None,
      component_pack: None,
      files: paths
        .iter()
        .map(|path| RemoteFile {
          path: path.to_string(),
          key: FileKey {
            sha1: "aa".into(),
            size: 1,
          },
          source: FileSource::Download(Box::new(DownloadParam {
            src: "https://example.com/a".parse().unwrap(),
            dest: PathBuf::new(),
            filename: None,
            sha1: None,
            custom_headers: None,
            transfer_options: Default::default(),
          })),
        })
        .collect(),
    }
  }

  #[test]
  fn ordinary_paths_are_accepted() {
    assert!(pack(&["mods/a.jar", "config/x/y.toml", "options.txt"])
      .validate("pack")
      .is_ok());
  }

  #[test]
  fn escaping_launcher_owned_and_colliding_paths_are_rejected() {
    for bad in [
      "../evil.jar",
      "/abs.jar",
      "mods\\a.jar",
      "C:/a.jar",
      "ustblcfg.json",
      "USTBL-modpack-baseline.json",
      "pack.json",
      "pack.jar",
      ".ustbl-update/journal.json",
      ".USTBL-UPDATE/x",
      "mods/a.jar.disabled",
    ] {
      assert!(pack(&[bad]).validate("pack").is_err(), "{bad} was accepted");
    }
    assert!(pack(&["mods/A.jar", "mods/a.jar"])
      .validate("pack")
      .is_err());
  }

  #[test]
  fn content_hash_matches_the_baseline_definition() {
    let remote = pack(&["b", "a"]);
    let mut reversed = remote.clone();
    reversed.files.reverse();
    assert_eq!(remote.content_hash(), reversed.content_hash());
    assert_ne!(remote.content_hash(), pack(&["a"]).content_hash());
  }
}
