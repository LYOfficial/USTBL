//! Deciding how each file of an instance travels to the campus platform.

use super::components::ComponentPlan;
use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::modpack_update::diff::{FileKey, DISABLED_SUFFIX};
use crate::instance::modpack_update::remote::{RemoteRule, UPDATE_DIR};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FORMAT: &str = "ustbl.modpack/3";

/// One file the publisher ticked in the tree. `path` is always the enabled
/// name; a disabled mod is read from `<path>.disabled` and ships with
/// `disabled: true`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishFile {
  pub path: String,
  #[serde(default)]
  pub disabled: bool,
  #[serde(default)]
  pub once: bool,
}

/// Where the content of a file lives on disk.
pub fn disk_path(path: &str, disabled: bool) -> String {
  if disabled {
    format!("{path}{DISABLED_SUFFIX}")
  } else {
    path.to_string()
  }
}

/// Extensions of formats that are compressed already; deflating them again only
/// costs time.
const STORED_EXTENSIONS: [&str; 14] = [
  "jar", "zip", "png", "jpg", "jpeg", "gif", "ogg", "mp3", "gz", "xz", "zst", "7z", "rar", "webp",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
  /// The enabled name, as the manifest lists it.
  pub path: String,
  pub key: FileKey,
  pub disabled: bool,
  pub once: bool,
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
  /// The publisher's ignore/delete rules, exactly as set (never compressed).
  pub rules: Vec<RemoteRule>,
  /// Launcher metadata is never published.
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
          "disabled": file.disabled,
          "once": file.once,
        })
      })
      .collect();
    let mut manifest = json!({
      "format": MANIFEST_FORMAT,
      "files": files,
      "rules": self.rules,
    });
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
          path: root.join(disk_path(&file.path, file.disabled)),
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

/// A listed path is an enabled name, so one that ends in `.disabled` is not a
/// manifest path at all (a disabled mod is listed under its enabled name).
fn is_publishable(path: &str, instance_name: &str) -> bool {
  let lowered = path.to_lowercase();
  !(lowered.ends_with(DISABLED_SUFFIX)
    || is_launcher_metadata_path(path, instance_name)
    || lowered == UPDATE_DIR
    || lowered.starts_with(&format!("{UPDATE_DIR}/")))
}

/// Whether a ticked file may ship in the requested state: only a mod under
/// `mods/` can be published disabled.
pub fn disabled_state_allowed(path: &str, disabled: bool) -> bool {
  !disabled || path.to_lowercase().starts_with("mods/")
}

/// Filter before hashing: launcher metadata and the update folder never leave
/// the instance, and nothing is read for them.
pub(super) fn partition_publishable(
  files: Vec<PublishFile>,
  instance_name: &str,
) -> (Vec<PublishFile>, Vec<String>) {
  let (kept, skipped) = files
    .into_iter()
    .partition::<Vec<_>, _>(|file| is_publishable(&file.path, instance_name));
  (kept, skipped.into_iter().map(|file| file.path).collect())
}

pub fn plan_files(
  files: Vec<LocalFile>,
  rules: Vec<RemoteRule>,
  instance_name: &str,
) -> PublishPlan {
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
    rules,
    skipped,
    components: None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::modpack_update::remote::{RuleKind, RuleState};

  fn file(path: &str, sha1: &str, size: u64) -> LocalFile {
    LocalFile {
      path: path.into(),
      key: FileKey {
        sha1: sha1.into(),
        size,
      },
      disabled: false,
      once: false,
    }
  }

  fn picked(path: &str) -> PublishFile {
    PublishFile {
      path: path.into(),
      disabled: false,
      once: false,
    }
  }

  fn plan(files: Vec<LocalFile>) -> PublishPlan {
    plan_files(files, Vec::new(), "pack")
  }

  #[test]
  fn launcher_files_are_filtered_before_hashing_but_disabled_mods_are_kept() {
    let (kept, skipped) = partition_publishable(
      vec![
        picked("mods/a.jar"),
        PublishFile {
          path: "mods/b.jar".into(),
          disabled: true,
          once: false,
        },
        picked("ustblcfg.json"),
        picked("ustbl-modpack-baseline.json"),
        picked("pack.json"),
        picked(".ustbl-update/backup/x"),
        picked("mods/c.jar.disabled"),
      ],
      "pack",
    );

    let kept: Vec<_> = kept.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(kept, ["mods/a.jar", "mods/b.jar"]);
    assert_eq!(skipped.len(), 5);
  }

  #[test]
  fn a_disabled_mod_ships_under_its_enabled_name_and_is_read_from_the_disabled_file() {
    let mut disabled = file("mods/a.jar", "a", 1);
    disabled.disabled = true;
    let plan = plan(vec![disabled, file("config/a.toml", "x", 1)]);

    let manifest = plan.manifest();
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files[0]["path"], "config/a.toml");
    assert_eq!(files[0]["disabled"], false);
    assert_eq!(files[1]["path"], "mods/a.jar");
    assert_eq!(files[1]["disabled"], true);
    let objects = plan.objects(Path::new("/instance"));
    assert_eq!(
      objects["a"].path,
      Path::new("/instance/mods/a.jar.disabled")
    );
    assert_eq!(objects["x"].path, Path::new("/instance/config/a.toml"));
  }

  #[test]
  fn only_mods_can_ship_disabled() {
    assert!(disabled_state_allowed("mods/a.jar", true));
    assert!(disabled_state_allowed("Mods/a.jar", true));
    assert!(!disabled_state_allowed("config/a.toml", true));
    assert!(disabled_state_allowed("config/a.toml", false));
  }

  #[test]
  fn once_and_rules_go_into_the_manifest_as_set() {
    let mut once = file("config/x.toml", "x", 1);
    once.once = true;
    let rules = vec![
      RemoteRule {
        path: "config/yes_steve_model/cache".into(),
        kind: RuleKind::Dir,
        state: RuleState::Ignore,
      },
      RemoteRule {
        path: "resourcepacks/old.zip".into(),
        kind: RuleKind::File,
        state: RuleState::Delete,
      },
    ];
    let manifest = plan_files(vec![once], rules, "pack").manifest();

    assert_eq!(manifest["files"][0]["once"], true);
    assert_eq!(
      manifest["rules"],
      json!([
        { "path": "config/yes_steve_model/cache", "kind": "dir", "state": "ignore" },
        { "path": "resourcepacks/old.zip", "kind": "file", "state": "delete" },
      ])
    );
  }

  #[test]
  fn a_version_may_carry_rules_and_no_files() {
    let rules = vec![RemoteRule {
      path: "mods/old.jar".into(),
      kind: RuleKind::File,
      state: RuleState::Delete,
    }];
    let manifest = plan_files(Vec::new(), rules, "pack").manifest();
    assert_eq!(manifest["files"], json!([]));
    assert_eq!(manifest["rules"].as_array().unwrap().len(), 1);
  }

  #[test]
  fn every_file_is_listed_as_a_pack_file_in_path_order() {
    let manifest = plan(vec![
      file("options.txt", "b", 5),
      file("mods/a.jar", "a", 99_000_000),
      file("empty.txt", "z", 0),
    ])
    .manifest();
    assert_eq!(manifest["format"], "ustbl.modpack/3");
    let files = manifest["files"].as_array().unwrap();
    assert!(files
      .iter()
      .all(|f| f["disabled"] == false && f["once"] == false));
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["empty.txt", "mods/a.jar", "options.txt"]);
    assert!(files.iter().all(|f| f["source"]["kind"] == "pack"));
    assert_eq!(manifest["rules"], json!([]));
  }

  #[test]
  fn identical_content_is_one_object_and_the_manifest_lists_every_path() {
    let plan = plan(vec![
      file("mods/a.jar", "same", 10),
      file("mods/copy.jar", "same", 10),
    ]);

    let objects = plan.objects(Path::new("/instance"));
    assert_eq!(objects.len(), 1);
    assert_eq!(objects["same"].path, Path::new("/instance/mods/a.jar"));
    assert_eq!(plan.manifest()["files"].as_array().unwrap().len(), 2);
  }

  #[test]
  fn compressed_formats_are_stored_and_the_rest_deflated() {
    let plan = plan(vec![
      file("mods/a.JAR", "1", 1),
      file("config/a.toml", "2", 1),
      file("resourcepacks/p.zip", "3", 1),
    ]);
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
    let mut plan = plan(vec![file("mods/big.jar", "big", 10)]);
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
