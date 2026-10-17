//! The component profile of a MultiMC pack, as it travels to the campus platform.
//!
//! A pack that was imported from a MultiMC profile cannot be rebuilt from its
//! files alone. The platform therefore also stores what the importer kept in the
//! instance snapshot: `mmc-pack.json` verbatim, the patches the pack shipped, the
//! libraries it shipped and the launcher relevant part of `instance.cfg`.
//! Patches and libraries are content-addressed objects in the delta pack, like
//! every other file.

use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::hash_file;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::helpers::modpack::multimc::snapshot::ComponentSnapshot;
use crate::instance::modpack_update::diff::FileKey;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// `instance.cfg` keys that shape how the profile launches. Everything else
/// (java location, play time, commands to run around the game) is machine or
/// player state and is never published.
pub const PUBLISHED_CFG_KEYS: [&str; 10] = [
  "InstanceType",
  "name",
  "notes",
  "IconKey",
  "OverrideJavaArgs",
  "JvmArgs",
  "OverrideMemory",
  "MinMemAlloc",
  "MaxMemAlloc",
  "PermGen",
];

const MAX_CFG_VALUE: usize = 4096;

/// A blob of the profile and where its bytes are on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentBlob {
  pub key: FileKey,
  pub source: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ComponentPlan {
  pub mmc_pack: Value,
  pub instance_cfg: BTreeMap<String, String>,
  /// By component uid.
  pub patches: BTreeMap<String, ComponentBlob>,
  /// By path inside the pack, `libraries/<file>`.
  pub libraries: BTreeMap<String, ComponentBlob>,
}

fn is_plain_name(name: &str) -> bool {
  !name.is_empty()
    && name != "."
    && name != ".."
    && name.len() <= 200
    && name
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

fn blob_of(path: PathBuf) -> USTBLResult<ComponentBlob> {
  let (sha1, size) = hash_file(&path)?;
  Ok(ComponentBlob {
    key: FileKey { sha1, size },
    source: path,
  })
}

/// Collect the profile's blobs. Patches are written under `work_dir` because
/// the snapshot keeps them parsed; the libraries are read where the importer
/// stored them.
pub fn plan_components(
  snapshot: &ComponentSnapshot,
  libraries_dir: &Path,
  work_dir: &Path,
) -> USTBLResult<ComponentPlan> {
  fs::create_dir_all(work_dir)?;
  let mut patches = BTreeMap::new();
  for (uid, patch) in &snapshot.local_patches {
    if !is_plain_name(uid) {
      return Err(USTBLError(format!("Unsupported component id: {uid}")));
    }
    let path = work_dir.join(format!("{uid}.json"));
    fs::write(
      &path,
      serde_json::to_vec_pretty(patch).map_err(|e| USTBLError(e.to_string()))?,
    )?;
    patches.insert(uid.clone(), blob_of(path)?);
  }

  let mut libraries = BTreeMap::new();
  for library in &snapshot.local_libraries {
    let relative = safe_relative(&library.path)?;
    if !is_plain_name(&library.filename) {
      return Err(USTBLError(format!(
        "Unsupported library name: {}",
        library.filename
      )));
    }
    let full = libraries_dir.join(relative);
    if !full.is_file() {
      return Err(USTBLError(format!(
        "Local library {} is missing from the libraries directory",
        library.filename
      )));
    }
    libraries.insert(format!("libraries/{}", library.filename), blob_of(full)?);
  }

  let instance_cfg = snapshot
    .instance_cfg
    .iter()
    .filter(|(key, value)| {
      PUBLISHED_CFG_KEYS.contains(&key.as_str())
        && value.len() <= MAX_CFG_VALUE
        && !value.contains(['\r', '\n', '\0'])
    })
    .map(|(key, value)| (key.clone(), value.clone()))
    .collect();

  Ok(ComponentPlan {
    mmc_pack: snapshot.mmc_pack.clone(),
    instance_cfg,
    patches,
    libraries,
  })
}

impl ComponentPlan {
  /// The `components` section of the manifest.
  pub fn manifest(&self) -> Value {
    json!({
      "mmcPack": self.mmc_pack,
      "instanceCfg": self.instance_cfg,
      "patches": self.patches.iter().map(|(uid, blob)| json!({
        "uid": uid,
        "blob": blob.key.sha1,
        "size": blob.key.size,
      })).collect::<Vec<_>>(),
      "libraries": self.libraries.iter().map(|(path, blob)| json!({
        "path": path,
        "blob": blob.key.sha1,
        "size": blob.key.size,
      })).collect::<Vec<_>>(),
    })
  }

  /// Distinct objects to upload, keyed by hash, with where they are and the
  /// name they have inside the pack (which decides how they are compressed).
  pub fn blobs(&self) -> BTreeMap<String, (PathBuf, String)> {
    let patches = self
      .patches
      .iter()
      .map(|(uid, blob)| (blob, format!("patches/{uid}.json")));
    let libraries = self
      .libraries
      .iter()
      .map(|(path, blob)| (blob, path.clone()));
    patches
      .chain(libraries)
      .map(|(blob, name)| (blob.key.sha1.clone(), (blob.source.clone(), name)))
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::multimc::snapshot::{LocalLibrary, SNAPSHOT_SCHEMA};

  fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ustbl-components-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn snapshot() -> ComponentSnapshot {
    ComponentSnapshot {
      schema: SNAPSHOT_SCHEMA,
      mmc_pack: json!({
        "formatVersion": 1,
        "components": [{"uid": "net.minecraft", "version": "1.7.10", "important": true}],
      }),
      instance_cfg: [
        ("name", "GTNH"),
        ("OverrideJavaArgs", "true"),
        ("JvmArgs", "-Xmx6G"),
        ("JavaPath", "C:/java/bin/javaw.exe"),
        ("PreLaunchCommand", "calc"),
        ("notes", "line one\nline two"),
      ]
      .into_iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
      local_patches: [(
        "com.example.loader".to_string(),
        json!({"uid": "com.example.loader", "version": "1"}),
      )]
      .into(),
      local_libraries: vec![LocalLibrary {
        filename: "boot-1.jar".into(),
        path: "mods/boot-1.jar".into(),
      }],
    }
  }

  #[test]
  fn blobs_cover_patches_and_shipped_libraries_and_the_manifest_lists_them() {
    let (libraries, work) = (temp(), temp());
    fs::create_dir_all(libraries.join("mods")).unwrap();
    fs::write(libraries.join("mods/boot-1.jar"), "library bytes").unwrap();

    let plan = plan_components(&snapshot(), &libraries, &work).unwrap();

    let manifest = plan.manifest();
    assert_eq!(manifest["mmcPack"]["components"][0]["important"], true);
    assert_eq!(manifest["patches"][0]["uid"], "com.example.loader");
    assert_eq!(manifest["libraries"][0]["path"], "libraries/boot-1.jar");
    assert_eq!(manifest["libraries"][0]["size"], 13);
    let blobs = plan.blobs();
    assert_eq!(blobs.len(), 2);
    for item in ["patches", "libraries"] {
      let hash = manifest[item][0]["blob"].as_str().unwrap();
      assert!(blobs[hash].0.is_file());
    }
    let _ = fs::remove_dir_all(libraries);
    let _ = fs::remove_dir_all(work);
  }

  #[test]
  fn only_launch_relevant_cfg_keys_are_published() {
    let (libraries, work) = (temp(), temp());
    fs::create_dir_all(libraries.join("mods")).unwrap();
    fs::write(libraries.join("mods/boot-1.jar"), "x").unwrap();

    let cfg = plan_components(&snapshot(), &libraries, &work)
      .unwrap()
      .instance_cfg;

    assert_eq!(cfg["name"], "GTNH");
    assert_eq!(cfg["JvmArgs"], "-Xmx6G");
    assert!(!cfg.contains_key("JavaPath"));
    assert!(!cfg.contains_key("PreLaunchCommand"));
    // A value the platform would refuse is left out instead of failing the publish.
    assert!(!cfg.contains_key("notes"));
    let _ = fs::remove_dir_all(libraries);
    let _ = fs::remove_dir_all(work);
  }

  #[test]
  fn a_missing_library_or_an_unsafe_name_stops_the_publish() {
    let (libraries, work) = (temp(), temp());
    assert!(plan_components(&snapshot(), &libraries, &work).is_err());

    let mut odd = snapshot();
    odd.local_patches = [("../evil".to_string(), json!({}))].into();
    odd.local_libraries.clear();
    assert!(plan_components(&odd, &libraries, &work).is_err());
    let _ = fs::remove_dir_all(libraries);
    let _ = fs::remove_dir_all(work);
  }
}
