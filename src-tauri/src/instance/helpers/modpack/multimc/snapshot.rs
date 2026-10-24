//! What an imported component profile declared, kept next to the instance so it
//! can be exported again without loss.
//!
//! The launcher resolves a MultiMC pack into its own client json, which is not
//! enough to rebuild the pack: component flags, the shipped patch files, the
//! locally shipped libraries and `instance.cfg` would be gone. This snapshot
//! keeps them verbatim. It is never read when launching.

use crate::error::USTBLResult;
use crate::instance::models::misc::InstanceError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Resolved patches of the profile, written by the importer for launching.
pub const COMPONENTS_FILE_NAME: &str = "ustbl-multimc-components.json";
/// The snapshot described in this module.
pub const SNAPSHOT_FILE_NAME: &str = "ustbl-multimc-profile.json";
/// Version 2 added the shipped patches and the local library index.
pub const SNAPSHOT_SCHEMA: u32 = 2;

/// A library file the pack ships itself (`MMC-hint: local`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLibrary {
  /// Name of the file inside the pack's `libraries/` directory.
  pub filename: String,
  /// Where the importer stored it, relative to the game's `libraries` directory.
  pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSnapshot {
  pub schema: u32,
  /// `mmc-pack.json` exactly as shipped.
  pub mmc_pack: Value,
  /// Every `instance.cfg` key of the pack.
  pub instance_cfg: BTreeMap<String, String>,
  /// Patch files the pack shipped, by component uid. Components absent here were
  /// resolved from the public metadata at import time and are exported as plain
  /// references again.
  #[serde(default)]
  pub local_patches: BTreeMap<String, Value>,
  #[serde(default)]
  pub local_libraries: Vec<LocalLibrary>,
}

impl ComponentSnapshot {
  /// `Ok(None)` when the file is absent or written by an older importer.
  pub async fn load(instance_dir: &Path) -> std::io::Result<Option<Self>> {
    let bytes = match tokio::fs::read(instance_dir.join(SNAPSHOT_FILE_NAME)).await {
      Ok(bytes) => bytes,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    let snapshot: Self = serde_json::from_slice(&bytes)?;
    Ok((snapshot.schema == SNAPSHOT_SCHEMA).then_some(snapshot))
  }
}

/// Whether the instance was imported from a component profile.
pub fn is_component_instance(instance_dir: &Path) -> bool {
  instance_dir.join(COMPONENTS_FILE_NAME).is_file()
}

/// The snapshot a component instance needs for export and publish, checked
/// before any heavier work starts. `Ok(None)` for a plain instance, which
/// needs no snapshot; `ModpackExportProfileMissing` for a component instance
/// whose snapshot is absent, corrupt or from an older importer.
pub async fn load_required_snapshot(instance_dir: &Path) -> USTBLResult<Option<ComponentSnapshot>> {
  if !is_component_instance(instance_dir) {
    return Ok(None);
  }
  let snapshot = ComponentSnapshot::load(instance_dir)
    .await
    .map_err(|_| InstanceError::ModpackExportProfileMissing)?
    .ok_or(InstanceError::ModpackExportProfileMissing)?;
  Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use std::path::PathBuf;

  struct Temp(PathBuf);
  impl Temp {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-snapshot-test-{}", uuid::Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }
  impl Drop for Temp {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[tokio::test]
  async fn a_plain_instance_needs_no_snapshot() {
    let temp = Temp::new();
    assert_eq!(load_required_snapshot(&temp.0).await.unwrap(), None);
  }

  #[tokio::test]
  async fn a_component_instance_with_a_current_snapshot_passes() {
    let temp = Temp::new();
    fs::write(temp.0.join(COMPONENTS_FILE_NAME), "[]").unwrap();
    fs::write(
      temp.0.join(SNAPSHOT_FILE_NAME),
      serde_json::to_vec(&serde_json::json!({
        "schema": SNAPSHOT_SCHEMA,
        "mmcPack": {},
        "instanceCfg": {},
      }))
      .unwrap(),
    )
    .unwrap();
    let snapshot = load_required_snapshot(&temp.0).await.unwrap().unwrap();
    assert_eq!(snapshot.schema, SNAPSHOT_SCHEMA);
  }

  #[tokio::test]
  async fn a_component_instance_without_a_usable_snapshot_is_refused() {
    let expected = InstanceError::ModpackExportProfileMissing.to_string();

    // No snapshot file at all.
    let temp = Temp::new();
    fs::write(temp.0.join(COMPONENTS_FILE_NAME), "[]").unwrap();
    let missing = load_required_snapshot(&temp.0).await.unwrap_err();
    assert_eq!(missing.0, expected);

    // A snapshot of an older schema.
    let temp = Temp::new();
    fs::write(temp.0.join(COMPONENTS_FILE_NAME), "[]").unwrap();
    fs::write(
      temp.0.join(SNAPSHOT_FILE_NAME),
      serde_json::to_vec(&serde_json::json!({
        "schema": SNAPSHOT_SCHEMA - 1,
        "mmcPack": {},
        "instanceCfg": {},
      }))
      .unwrap(),
    )
    .unwrap();
    let outdated = load_required_snapshot(&temp.0).await.unwrap_err();
    assert_eq!(outdated.0, expected);

    // A snapshot file that cannot be parsed at all.
    let temp = Temp::new();
    fs::write(temp.0.join(COMPONENTS_FILE_NAME), "[]").unwrap();
    fs::write(temp.0.join(SNAPSHOT_FILE_NAME), b"not json").unwrap();
    let corrupt = load_required_snapshot(&temp.0).await.unwrap_err();
    assert_eq!(corrupt.0, expected);
  }
}
