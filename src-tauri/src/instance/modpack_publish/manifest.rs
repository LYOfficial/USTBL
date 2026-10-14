//! Deciding how each file of an instance travels to the campus platform.

use super::components::ComponentPlan;
use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::modpack_update::diff::{FileKey, DISABLED_SUFFIX};
use crate::instance::modpack_update::remote::UPDATE_DIR;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FORMAT: &str = "ustbl.modpack/2";

/// Extensions of formats that are compressed already; deflating them again only
/// costs time.
const STORED_EXTENSIONS: [&str; 14] = [
  "jar", "zip", "png", "jpg", "jpeg", "gif", "ogg", "mp3", "gz", "xz", "zst", "7z", "rar", "webp",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
  pub path: String,
  pub key: FileKey,
}

/// One object of the delta pack: the content with this hash, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackObject {
  pub sha1: String,
  pub path: PathBuf,
  /// Stored as is rather than deflated.
  pub stored: bool,
}

#[derive(Debug)]
pub struct PublishPlan {
  pub files: Vec<LocalFile>,
  /// Disabled mods and launcher metadata are never published.
  pub skipped: Vec<String>,
  /// Set for an instance imported from a MultiMC profile.
  pub components: Option<ComponentPlan>,
}

fn is_stored(path: &str) -> bool {
  Path::new(path)
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| {
      STORED_EXTENSIONS
        .iter()
        .any(|stored| stored.eq_ignore_ascii_case(extension))
    })
}

impl PublishPlan {
  pub fn manifest(&self) -> Value {
    let files: Vec<Value> = self
      .files
      .iter()
      .map(|file| {
        json!({
          "path": file.path,
          "size": file.key.size,
          "sha1": file.key.sha1,
          "source": { "kind": "pack" },
        })
      })
      .collect();
    let mut manifest = json!({ "format": MANIFEST_FORMAT, "files": files });
    if let Some(components) = &self.components {
      manifest["components"] = components.manifest();
    }
    manifest
  }

  /// Every distinct object the version refers to, keyed by hash: the files of
  /// the instance and the profile's patches and libraries. Identical content
  /// under several paths is one object.
  pub fn objects(&self, root: &Path) -> BTreeMap<String, PackObject> {
    let mut objects = BTreeMap::new();
    for file in &self.files {
      objects
        .entry(file.key.sha1.clone())
        .or_insert_with(|| PackObject {
          sha1: file.key.sha1.clone(),
          path: root.join(&file.path),
          stored: is_stored(&file.path),
        });
    }
    if let Some(components) = &self.components {
      for (sha1, (path, name)) in components.blobs() {
        objects.entry(sha1.clone()).or_insert_with(|| PackObject {
          sha1,
          path,
          stored: is_stored(&name),
        });
      }
    }
    objects
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
  PublishPlan {
    files: kept,
    skipped,
    components: None,
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
  fn every_file_is_listed_as_a_pack_file_in_path_order() {
    let plan = plan_files(
      vec![
        file("options.txt", "b", 5),
        file("mods/a.jar", "a", 99_000_000),
        file("empty.txt", "z", 0),
      ],
      "pack",
    );
    let manifest = plan.manifest();
    assert_eq!(manifest["format"], "ustbl.modpack/2");
    let files = manifest["files"].as_array().unwrap();
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["empty.txt", "mods/a.jar", "options.txt"]);
    assert!(files.iter().all(|f| f["source"]["kind"] == "pack"));
  }

  #[test]
  fn identical_content_is_one_object_and_the_manifest_lists_every_path() {
    let plan = plan_files(
      vec![
        file("mods/a.jar", "same", 10),
        file("mods/copy.jar", "same", 10),
      ],
      "pack",
    );

    let objects = plan.objects(Path::new("/instance"));
    assert_eq!(objects.len(), 1);
    assert_eq!(objects["same"].path, Path::new("/instance/mods/a.jar"));
    assert_eq!(plan.manifest()["files"].as_array().unwrap().len(), 2);
  }

  #[test]
  fn compressed_formats_are_stored_and_the_rest_deflated() {
    let plan = plan_files(
      vec![
        file("mods/a.JAR", "1", 1),
        file("config/a.toml", "2", 1),
        file("resourcepacks/p.zip", "3", 1),
      ],
      "pack",
    );
    let objects = plan.objects(Path::new("/i"));
    assert!(objects["1"].stored);
    assert!(!objects["2"].stored);
    assert!(objects["3"].stored);
  }

  #[test]
  fn component_blobs_join_the_objects_and_the_manifest() {
    use crate::instance::modpack_publish::components::{ComponentBlob, ComponentPlan};
    let blob = |sha1: &str, source: &str| ComponentBlob {
      key: FileKey {
        sha1: sha1.into(),
        size: 3,
      },
      source: PathBuf::from(source),
    };
    let mut plan = plan_files(vec![file("mods/big.jar", "big", 10)], "pack");
    assert!(plan.manifest().get("components").is_none());
    plan.components = Some(ComponentPlan {
      mmc_pack: json!({"formatVersion": 1, "components": []}),
      instance_cfg: BTreeMap::new(),
      patches: [("a.b".to_string(), blob("p", "/work/a.b.json"))].into(),
      libraries: [("libraries/x.jar".to_string(), blob("l", "/libs/x.jar"))].into(),
    });

    let objects = plan.objects(Path::new("/instance"));

    assert_eq!(objects.len(), 3);
    assert_eq!(objects["big"].path, Path::new("/instance/mods/big.jar"));
    assert_eq!(objects["p"].path, Path::new("/work/a.b.json"));
    assert!(!objects["p"].stored);
    assert_eq!(objects["l"].path, Path::new("/libs/x.jar"));
    assert!(objects["l"].stored);
    assert_eq!(plan.manifest()["components"]["patches"][0]["blob"], "p");
  }
}
