//! Deciding how each file of an instance travels to the campus platform.

use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::modpack_update::diff::{FileKey, DISABLED_SUFFIX};
use crate::instance::modpack_update::remote::UPDATE_DIR;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Files above this size are stored once by hash and shared between versions;
/// smaller ones ride along in the version's bundle archive.
pub const BLOB_THRESHOLD: u64 = 4 * 1024 * 1024;
/// The server caps a bundle at 512 MiB; stay clear of it after compression.
pub const BUNDLE_BUDGET: u64 = 400 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
  pub path: String,
  pub key: FileKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum Placement {
  Blob,
  Bundle,
}

#[derive(Debug)]
pub struct PublishPlan {
  pub files: Vec<(LocalFile, Placement)>,
  /// Disabled mods and launcher metadata are never published.
  pub skipped: Vec<String>,
}

impl PublishPlan {
  pub fn manifest(&self) -> Value {
    let files: Vec<Value> = self
      .files
      .iter()
      .map(|(file, placement)| {
        json!({
          "path": file.path,
          "size": file.key.size,
          "sha1": file.key.sha1,
          "source": { "kind": match placement { Placement::Blob => "blob", Placement::Bundle => "bundle" } },
        })
      })
      .collect();
    json!({ "format": "ustbl.modpack/1", "files": files })
  }

  /// Distinct blobs to upload, keyed by hash.
  pub fn blobs(&self) -> BTreeMap<String, LocalFile> {
    self
      .files
      .iter()
      .filter(|(_, placement)| *placement == Placement::Blob)
      .map(|(file, _)| (file.key.sha1.clone(), file.clone()))
      .collect()
  }

  pub fn bundle_files(&self) -> Vec<&LocalFile> {
    self
      .files
      .iter()
      .filter(|(_, placement)| *placement == Placement::Bundle)
      .map(|(file, _)| file)
      .collect()
  }
}

fn is_publishable(path: &str, instance_name: &str) -> bool {
  let lowered = path.to_lowercase();
  !(lowered.ends_with(DISABLED_SUFFIX)
    || is_launcher_metadata_path(path, instance_name)
    || lowered == UPDATE_DIR
    || lowered.starts_with(&format!("{UPDATE_DIR}/")))
}

pub fn plan_files(files: Vec<LocalFile>, instance_name: &str) -> PublishPlan {
  let mut skipped = Vec::new();
  let mut kept: Vec<LocalFile> = Vec::new();
  for file in files {
    if is_publishable(&file.path, instance_name) {
      kept.push(file);
    } else {
      skipped.push(file.path);
    }
  }
  kept.sort_by(|a, b| a.path.cmp(&b.path));
  kept.dedup_by(|a, b| a.path == b.path);
  skipped.sort();

  let mut placed: Vec<(LocalFile, Placement)> = kept
    .into_iter()
    .map(|file| {
      let placement = if file.key.size > BLOB_THRESHOLD {
        Placement::Blob
      } else {
        Placement::Bundle
      };
      (file, placement)
    })
    .collect();

  let mut bundle_total: u64 = placed
    .iter()
    .filter(|(_, p)| *p == Placement::Bundle)
    .map(|(f, _)| f.key.size)
    .sum();
  while bundle_total > BUNDLE_BUDGET {
    let largest = placed
      .iter_mut()
      .filter(|(_, p)| *p == Placement::Bundle)
      .max_by_key(|(f, _)| f.key.size);
    let Some((file, placement)) = largest else {
      break;
    };
    bundle_total -= file.key.size;
    *placement = Placement::Blob;
  }
  PublishPlan {
    files: placed,
    skipped,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn file(path: &str, sha1: &str, size: u64) -> LocalFile {
    LocalFile {
      path: path.into(),
      key: FileKey {
        sha1: sha1.into(),
        size,
      },
    }
  }

  #[test]
  fn large_files_become_blobs_and_small_ones_are_bundled() {
    let plan = plan_files(
      vec![
        file("mods/big.jar", "b", BLOB_THRESHOLD + 1),
        file("config/a.toml", "a", 10),
        file("mods/edge.jar", "e", BLOB_THRESHOLD),
        file("empty.txt", "z", 0),
      ],
      "pack",
    );

    let by_path: BTreeMap<_, _> = plan
      .files
      .iter()
      .map(|(f, p)| (f.path.as_str(), *p))
      .collect();
    assert_eq!(by_path["mods/big.jar"], Placement::Blob);
    assert_eq!(by_path["config/a.toml"], Placement::Bundle);
    assert_eq!(by_path["mods/edge.jar"], Placement::Bundle);
    assert_eq!(by_path["empty.txt"], Placement::Bundle);
  }

  #[test]
  fn the_bundle_budget_pushes_the_largest_files_out() {
    let chunk = BLOB_THRESHOLD;
    let count = (BUNDLE_BUDGET / chunk) as usize + 3;
    let files: Vec<LocalFile> = (0..count)
      .map(|i| file(&format!("f{i:04}"), &format!("{i:040}"), chunk))
      .collect();

    let plan = plan_files(files, "pack");

    let bundled: u64 = plan.bundle_files().iter().map(|f| f.key.size).sum();
    assert!(bundled <= BUNDLE_BUDGET);
    assert_eq!(plan.blobs().len() + plan.bundle_files().len(), count);
    assert!(!plan.blobs().is_empty());
  }

  #[test]
  fn disabled_mods_and_launcher_files_are_skipped() {
    let plan = plan_files(
      vec![
        file("mods/a.jar", "a", 1),
        file("mods/b.jar.disabled", "b", 1),
        file("ustblcfg.json", "c", 1),
        file("ustbl-modpack-baseline.json", "d", 1),
        file("pack.json", "e", 1),
        file(".ustbl-update/backup/x", "f", 1),
      ],
      "pack",
    );

    assert_eq!(plan.files.len(), 1);
    assert_eq!(plan.skipped.len(), 5);
  }

  #[test]
  fn identical_content_is_uploaded_once_and_the_manifest_lists_every_path() {
    let big = BLOB_THRESHOLD + 1;
    let plan = plan_files(
      vec![
        file("mods/a.jar", "same", big),
        file("mods/copy.jar", "same", big),
      ],
      "pack",
    );

    assert_eq!(plan.blobs().len(), 1);
    let manifest = plan.manifest();
    assert_eq!(manifest["format"], "ustbl.modpack/1");
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["source"]["kind"], "blob");
    assert_eq!(files[0]["sha1"], "same");
  }
}
