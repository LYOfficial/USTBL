//! Comparing an instance with the version it is being updated to.

use super::diff::{collect_local_states, three_way, Diff, FileKey};
use super::remote::RemoteModpack;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct Prepared {
  pub diff: Diff,
  /// Identifies this exact comparison. Applying with a different hash means
  /// the instance or the published version moved since the player looked.
  pub plan_hash: String,
}

fn baseline_keys(baseline: &ModpackBaseline) -> BTreeMap<String, FileKey> {
  baseline
    .files
    .iter()
    .map(|file| {
      (
        file.path.clone(),
        FileKey {
          sha1: file.sha1.clone(),
          size: file.size,
        },
      )
    })
    .collect()
}

pub fn plan_hash(remote: &RemoteModpack, diff: &Diff) -> String {
  let mut hasher = Sha1::new();
  hasher.update(remote.version_id.as_bytes());
  hasher.update(b"\0");
  hasher.update(remote.content_hash().as_bytes());
  hasher.update(b"\0");
  hasher.update(serde_json::to_vec(diff).unwrap_or_default());
  hex::encode(hasher.finalize())
}

/// Hash the files both versions care about and compare the three states.
pub async fn prepare(
  root: &Path,
  baseline: &ModpackBaseline,
  remote: &RemoteModpack,
) -> USTBLResult<Prepared> {
  let baseline_map = baseline_keys(baseline);
  let remote_map = remote.key_map();
  let paths: BTreeSet<String> = baseline_map
    .keys()
    .chain(remote_map.keys())
    .cloned()
    .collect();
  let root = root.to_path_buf();
  let local = tokio::task::spawn_blocking(move || collect_local_states(&root, &paths))
    .await
    .map_err(|e| USTBLError(e.to_string()))??;
  let diff = three_way(&baseline_map, &local, &remote_map);
  Ok(Prepared {
    plan_hash: plan_hash(remote, &diff),
    diff,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::instance::modpack_update::diff::{ChangeKind, ConflictKind};
  use crate::instance::modpack_update::remote::{FileSource, RemoteFile};
  use sha1::Digest;
  use std::{fs, path::PathBuf};

  fn key(content: &str) -> FileKey {
    FileKey {
      sha1: hex::encode(Sha1::digest(content.as_bytes())),
      size: content.len() as u64,
    }
  }

  fn loader() -> BaselineLoader {
    BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: "0.16.0".into(),
    }
  }

  fn baseline(files: &[(&str, &str)]) -> ModpackBaseline {
    let origin = InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    };
    ModpackBaseline::build(
      &origin,
      loader(),
      files
        .iter()
        .map(|(path, content)| BaselineFile {
          path: path.to_string(),
          sha1: key(content).sha1,
          size: key(content).size,
        })
        .collect(),
    )
  }

  fn remote(files: &[(&str, &str)]) -> RemoteModpack {
    RemoteModpack {
      version_id: "2".into(),
      label: "2".into(),
      game_version: "1.21.1".into(),
      loader: loader(),
      files: files
        .iter()
        .map(|(path, content)| RemoteFile {
          path: path.to_string(),
          key: key(content),
          source: FileSource::ArchiveEntry {
            archive: PathBuf::new(),
            entry: path.to_string(),
          },
        })
        .collect(),
    }
  }

  fn instance(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-plan-test-{}", uuid::Uuid::new_v4()));
    for (path, content) in files {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    root
  }

  #[tokio::test]
  async fn untouched_files_update_and_edited_ones_conflict() {
    let root = instance(&[("mods/a.jar", "a1-edited"), ("mods/b.jar", "b1")]);
    let baseline = baseline(&[("mods/a.jar", "a1"), ("mods/b.jar", "b1")]);
    let remote = remote(&[
      ("mods/a.jar", "a2"),
      ("mods/b.jar", "b2"),
      ("mods/c.jar", "c2"),
    ]);

    let prepared = prepare(&root, &baseline, &remote).await.unwrap();

    let changes: Vec<_> = prepared
      .diff
      .changes
      .iter()
      .map(|c| (c.path.as_str(), c.kind))
      .collect();
    assert_eq!(
      changes,
      [
        ("mods/b.jar", ChangeKind::Update),
        ("mods/c.jar", ChangeKind::Add)
      ]
    );
    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert_eq!(
      prepared.diff.conflicts[0].kind,
      ConflictKind::ModifiedLocallyChangedRemotely
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn the_plan_hash_follows_the_instance_and_the_remote() {
    let root = instance(&[("mods/a.jar", "a1")]);
    let baseline = baseline(&[("mods/a.jar", "a1")]);
    let remote_v2 = remote(&[("mods/a.jar", "a2")]);

    let first = prepare(&root, &baseline, &remote_v2)
      .await
      .unwrap()
      .plan_hash;
    let again = prepare(&root, &baseline, &remote_v2)
      .await
      .unwrap()
      .plan_hash;
    assert_eq!(first, again);

    fs::write(root.join("mods/a.jar"), "a1-edited").unwrap();
    let edited = prepare(&root, &baseline, &remote_v2)
      .await
      .unwrap()
      .plan_hash;
    assert_ne!(first, edited);

    let mut other = remote(&[("mods/a.jar", "a2")]);
    other.version_id = "3".into();
    assert_ne!(
      first,
      prepare(&root.clone(), &baseline, &other)
        .await
        .unwrap()
        .plan_hash
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_pack_identical_to_the_baseline_has_nothing_to_do() {
    let root = instance(&[("mods/a.jar", "a1")]);
    let prepared = prepare(
      &root,
      &baseline(&[("mods/a.jar", "a1")]),
      &remote(&[("mods/a.jar", "a1")]),
    )
    .await
    .unwrap();
    assert!(prepared.diff.is_empty());
    let _ = fs::remove_dir_all(root);
  }
}
