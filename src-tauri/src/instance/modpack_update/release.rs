use super::diff::FileKey;
use super::remote::{RemoteModpack, RemoteRule, UPDATE_DIR};
use crate::file_error::{tag, AtPath};
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use crate::instance::modpack_publish::commands::{PreviousFile, PreviousRelease};
use crate::storage::save_json_async;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

pub const RELEASE_FILE_NAME: &str = "release.json";
pub const RELEASE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseFile {
  pub path: String,
  pub sha1: String,
  pub size: u64,
  #[serde(default)]
  pub disabled: bool,
  #[serde(default)]
  pub once: bool,
}

impl ReleaseFile {
  pub fn key(&self) -> FileKey {
    FileKey {
      sha1: self.sha1.clone(),
      size: self.size,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
  pub schema: u32,
  pub version_id: String,
  pub label: String,
  pub files: Vec<ReleaseFile>,
  #[serde(default)]
  pub rules: Vec<RemoteRule>,
  #[serde(default)]
  pub rules_manifest: bool,
}

impl Release {
  pub fn from_remote(remote: &RemoteModpack) -> Self {
    Self {
      schema: RELEASE_SCHEMA,
      version_id: remote.version_id.clone(),
      label: remote.label.clone(),
      files: remote
        .files
        .iter()
        .map(|file| ReleaseFile {
          path: file.path.clone(),
          sha1: file.key.sha1.clone(),
          size: file.key.size,
          disabled: file.disabled,
          once: file.once,
        })
        .collect(),
      rules: remote.rules.clone(),
      rules_manifest: remote.rules_manifest,
    }
  }

  pub fn from_baseline(baseline: &ModpackBaseline, label: impl Into<String>) -> Self {
    Self {
      schema: RELEASE_SCHEMA,
      version_id: baseline.origin.version_id.clone(),
      label: label.into(),
      files: baseline
        .files
        .iter()
        .filter(|file| file.is_managed())
        .filter_map(|file| {
          file.key().map(|key| ReleaseFile {
            path: file.path.clone(),
            sha1: key.sha1,
            size: key.size,
            disabled: file.disabled,
            once: false,
          })
        })
        .collect(),
      rules: Vec::new(),
      rules_manifest: false,
    }
  }

  /// Files by lowercase path, for lookups inside loops.
  pub fn index(&self) -> HashMap<String, &ReleaseFile> {
    self
      .files
      .iter()
      .map(|file| (file.path.to_lowercase(), file))
      .collect()
  }

  /// What the publish tree starts from, as the line's previous release.
  pub fn to_previous(&self) -> PreviousRelease {
    PreviousRelease {
      files: self
        .files
        .iter()
        .map(|file| PreviousFile {
          path: file.path.clone(),
          sha1: file.sha1.clone(),
          size: file.size,
          disabled: file.disabled,
          once: file.once,
        })
        .collect(),
      rules: self.rules.clone(),
      rules_manifest: self.rules_manifest,
      version_id: None,
      project_id: None,
    }
  }
}

/// Name of the record relative to the instance, as the update transaction
/// lists its metadata files.
pub fn meta_name() -> String {
  format!("{UPDATE_DIR}/{RELEASE_FILE_NAME}")
}

fn release_path(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join(RELEASE_FILE_NAME)
}

/// The saved record, or `None` when it is missing, unreadable or from another
/// schema. A damaged record is the same as no record: the caller falls back.
pub fn read(root: &Path) -> Option<Release> {
  let bytes = std::fs::read(release_path(root)).ok()?;
  serde_json::from_slice::<Release>(&bytes)
    .ok()
    .filter(|release| release.schema == RELEASE_SCHEMA)
}

pub async fn write(root: &Path, release: &Release) -> io::Result<()> {
  let path = release_path(root);
  save_json_async(release, &path).await.at_in(&path, root)
}

pub fn remove(root: &Path) -> io::Result<()> {
  let path = release_path(root);
  match std::fs::remove_file(&path) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => Err(tag(error, &path, Some(root))),
    _ => Ok(()),
  }
}

/// The release list the instance was last known to match, without the network.
/// The flag is set when the saved record is missing or belongs to another
/// version, so the list is rebuilt from the baseline (managed entries only, no
/// rules and no first-install flags).
pub fn release_for(root: &Path, baseline: &ModpackBaseline) -> (Release, bool) {
  let version_id = &baseline.origin.version_id;
  match read(root).filter(|release| release.version_id == *version_id) {
    Some(release) => (release, false),
    None => (Release::from_baseline(baseline, version_id.clone()), true),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::instance::modpack_update::diff::FileKey;
  use crate::instance::modpack_update::remote::{RemoteFile, RuleKind, RuleState};
  use std::sync::{Arc, OnceLock};

  fn origin() -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "12".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    }
  }

  #[test]
  fn baseline_fallback_keeps_only_managed_files_and_no_publish_flags() {
    let baseline = ModpackBaseline::build(
      &origin(),
      BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16".into(),
      },
      vec![
        BaselineFile::managed(
          "mods/a.jar",
          FileKey {
            sha1: "a".into(),
            size: 1,
          },
          true,
        ),
        BaselineFile::dormant(
          "config/once.toml",
          Some(FileKey {
            sha1: "b".into(),
            size: 1,
          }),
          false,
        ),
      ],
    );
    let release = Release::from_baseline(&baseline, "old");
    assert_eq!(release.version_id, "12");
    assert_eq!(release.label, "old");
    assert_eq!(release.files.len(), 1);
    assert!(release.files[0].disabled);
    assert!(!release.files[0].once);
    assert!(release.rules.is_empty());
    assert!(!release.rules_manifest);
  }

  #[test]
  fn remote_release_round_trips_attributes_and_rules_without_urls() {
    let remote = RemoteModpack {
      disabled_authoritative: true,
      version_id: "12".into(),
      label: "v12".into(),
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16".into(),
      },
      components: None,
      component_pack: None,
      files: vec![RemoteFile {
        path: "mods/a.jar".into(),
        key: FileKey {
          sha1: "abc".into(),
          size: 3,
        },
        source: super::super::remote::FileSource::Download(Box::new(
          crate::tasks::download::DownloadParam {
            src: "https://example.invalid/a.jar".parse().unwrap(),
            dest: std::path::PathBuf::new(),
            filename: None,
            sha1: None,
            custom_headers: None,
            transfer_options: Default::default(),
          },
        )),
        disabled: true,
        once: true,
      }],
      rules: vec![RemoteRule {
        path: "config/cache".into(),
        kind: RuleKind::Dir,
        state: RuleState::Ignore,
      }],
      rules_manifest: true,
      path_index: Arc::new(OnceLock::new()),
    };
    let release = Release::from_remote(&remote);
    let json = serde_json::to_string(&release).unwrap();
    assert!(!json.contains("example.invalid"));
    assert_eq!(release.index()["mods/a.jar"].once, true);
    assert_eq!(release.rules[0].state, RuleState::Ignore);
  }

  #[tokio::test]
  async fn read_write_remove_accept_only_current_schema() {
    let root = std::env::temp_dir().join(format!("ustbl-release-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let release = Release {
      schema: RELEASE_SCHEMA,
      version_id: "1".into(),
      label: "one".into(),
      files: vec![],
      rules: vec![],
      rules_manifest: false,
    };
    write(&root, &release).await.unwrap();
    assert!(root.join(UPDATE_DIR).join(RELEASE_FILE_NAME).is_file());
    assert!(!root.join(RELEASE_FILE_NAME).exists());
    assert_eq!(read(&root), Some(release));
    std::fs::write(
      release_path(&root),
      br#"{"schema":99,"versionId":"1","label":"one","files":[],"rules":[],"rulesManifest":false}"#,
    )
    .unwrap();
    assert_eq!(read(&root), None);
    std::fs::write(release_path(&root), b"{ not json").unwrap();
    assert_eq!(read(&root), None);
    remove(&root).unwrap();
    assert!(!release_path(&root).exists());
    let _ = std::fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_release_of_another_version_is_treated_as_missing() {
    let root = std::env::temp_dir().join(format!("ustbl-release-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let baseline = ModpackBaseline::build(
      &origin(),
      BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16".into(),
      },
      vec![BaselineFile::managed(
        "mods/a.jar",
        FileKey {
          sha1: "a".into(),
          size: 1,
        },
        false,
      )],
    );
    let (fallback, flag) = release_for(&root, &baseline);
    assert!(flag);
    assert_eq!(fallback.version_id, "12");
    let mut stale = fallback.clone();
    stale.version_id = "11".into();
    stale.files.clear();
    write(&root, &stale).await.unwrap();
    let (used, flag) = release_for(&root, &baseline);
    assert!(flag);
    assert_eq!(used.files.len(), 1);
    let mut current = stale;
    current.version_id = "12".into();
    write(&root, &current).await.unwrap();
    let (used, flag) = release_for(&root, &baseline);
    assert!(!flag);
    assert!(used.files.is_empty());
    let _ = std::fs::remove_dir_all(root);
  }
}
