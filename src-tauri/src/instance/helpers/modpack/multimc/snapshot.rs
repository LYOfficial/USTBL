//! What an imported component profile declared, kept next to the instance so it
//! can be exported again without loss.
//!
//! The launcher resolves a MultiMC pack into its own client json, which is not
//! enough to rebuild the pack: component flags, the shipped patch files, the
//! locally shipped libraries and `instance.cfg` would be gone. This snapshot
//! keeps them verbatim. It is never read when launching.

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
