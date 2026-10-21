//! Baseline of the files a platform modpack installed into an instance.
//!
//! The baseline records the state the modpack put on disk: the comparison key of
//! every managed file (SHA-1 + size), the loader, and a content hash over all of
//! it. Later updates compare baseline, local files and the remote manifest.
//!
//! The file lives inside the instance directory, so its location is its owner:
//! it stores no instance name or path, and renaming or moving the instance does
//! not affect it.

use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
use crate::storage::{load_json_async, save_json_async};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const BASELINE_FILE_NAME: &str = "ustbl-modpack-baseline.json";
pub const BASELINE_SCHEMA: u32 = 1;

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineOrigin {
  pub platform: OriginPlatform,
  pub project_id: String,
  pub version_id: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineLoader {
  #[serde(rename = "type")]
  pub loader_type: ModLoaderType,
  pub version: String,
}

/// The loader version as the loader version lists spell it.
///
/// Instances detected from some version JSONs carry NeoForge versions as
/// `neoforge-26.1.2.87`, while the lists (and therefore installation) use
/// `26.1.2.87`. Publishing stores this form, so installs and update checks
/// compare like with like. Other loaders are returned unchanged.
pub fn canonical_loader_version(loader_type: ModLoaderType, version: &str) -> String {
  let version = version.trim();
  match loader_type {
    ModLoaderType::NeoForge => version
      .strip_prefix("neoforge-")
      .unwrap_or(version)
      .to_string(),
    _ => version.to_string(),
  }
}

/// A file managed by the modpack. `path` is relative to the instance directory
/// and always uses `/`.
#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineFile {
  pub path: String,
  pub sha1: String,
  pub size: u64,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackBaseline {
  pub schema: u32,
  pub origin: BaselineOrigin,
  pub loader: BaselineLoader,
  /// The component chain of a component-profile pack, as a list of
  /// [`ComponentRef`] sorted by uid. `None` for Modrinth/CurseForge packs.
  #[serde(default)]
  pub components: Option<Value>,
  pub files: Vec<BaselineFile>,
  pub content_hash: String,
}

/// One entry of a component chain. Only what decides whether two chains are the
/// same is kept; display names and flags are not part of the identity.
#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
pub struct ComponentRef {
  pub uid: String,
  #[serde(default)]
  pub version: String,
}

/// Canonical form of a component chain: sorted by uid, one entry per uid (the
/// last one wins), so equal chains compare equal whatever order they were listed in.
pub fn canonical_components(
  components: impl IntoIterator<Item = ComponentRef>,
) -> Vec<ComponentRef> {
  let by_uid: BTreeMap<String, ComponentRef> = components
    .into_iter()
    .map(|component| (component.uid.clone(), component))
    .collect();
  by_uid.into_values().collect()
}

/// The component chain declared by a `mmc-pack.json` document.
pub fn components_of_mmc_pack(pack: &Value) -> Vec<ComponentRef> {
  let entries = pack
    .get("components")
    .and_then(Value::as_array)
    .map(Vec::as_slice)
    .unwrap_or_default();
  canonical_components(entries.iter().filter_map(|entry| {
    let uid = entry.get("uid")?.as_str()?.to_string();
    let version = entry
      .get("version")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_string();
    Some(ComponentRef { uid, version })
  }))
}

/// Normalize a relative path to the form stored in the baseline.
pub fn normalize_relative_path(path: &str) -> String {
  path.replace('\\', "/").trim_start_matches('/').to_string()
}

/// SHA-1 and size of a file, streamed.
pub fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
  let mut file = File::open(path)?;
  let mut hasher = Sha1::new();
  let mut buffer = vec![0u8; 64 * 1024];
  let mut size = 0u64;
  loop {
    let read = file.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    size += read as u64;
  }
  Ok((hex::encode(hasher.finalize()), size))
}

/// Hash files that already exist under `root`, given their relative paths.
pub fn hash_existing_files(
  root: &Path,
  relative_paths: &[String],
) -> std::io::Result<Vec<BaselineFile>> {
  relative_paths
    .iter()
    .map(|relative| {
      let (sha1, size) = hash_file(&root.join(relative))?;
      Ok(BaselineFile {
        path: normalize_relative_path(relative),
        sha1,
        size,
      })
    })
    .collect()
}

/// Content hash over the loader, the optional component chain and every managed
/// file, independent of the order the files were listed in.
pub fn compute_content_hash(
  loader: &BaselineLoader,
  components: Option<&Value>,
  files: &[BaselineFile],
) -> String {
  let mut hasher = Sha1::new();
  hasher.update(
    format!(
      "loader\0{}\0{}\n",
      loader.loader_type.to_string().to_lowercase(),
      loader.version
    )
    .as_bytes(),
  );
  if let Some(components) = components {
    hasher.update(format!("components\0{components}\n").as_bytes());
  }
  let mut sorted: Vec<&BaselineFile> = files.iter().collect();
  sorted.sort_by(|a, b| a.path.cmp(&b.path));
  for file in sorted {
    hasher.update(format!("file\0{}\0{}\0{}\n", file.path, file.sha1, file.size).as_bytes());
  }
  hex::encode(hasher.finalize())
}

impl ModpackBaseline {
  /// Build a baseline. When a path is listed more than once the last entry wins,
  /// and the result is sorted by path.
  pub fn build(origin: &InstanceOrigin, loader: BaselineLoader, files: Vec<BaselineFile>) -> Self {
    let mut by_path = BTreeMap::new();
    for mut file in files {
      file.path = normalize_relative_path(&file.path);
      file.sha1 = file.sha1.to_lowercase();
      by_path.insert(file.path.clone(), file);
    }
    let files: Vec<BaselineFile> = by_path.into_values().collect();
    let content_hash = compute_content_hash(&loader, None, &files);
    Self {
      schema: BASELINE_SCHEMA,
      origin: BaselineOrigin {
        platform: origin.platform,
        project_id: origin.project_id.clone(),
        version_id: origin.version_id.clone(),
      },
      loader,
      components: None,
      files,
      content_hash,
    }
  }

  /// Record the component chain of a component-profile pack.
  pub fn with_components(mut self, components: Vec<ComponentRef>) -> Self {
    let value = serde_json::to_value(canonical_components(components))
      .expect("component refs serialize to JSON");
    self.content_hash = compute_content_hash(&self.loader, Some(&value), &self.files);
    self.components = Some(value);
    self
  }

  /// The recorded component chain, `None` when the pack has none.
  pub fn component_refs(&self) -> Option<Vec<ComponentRef>> {
    let value = self.components.clone()?;
    serde_json::from_value::<Vec<ComponentRef>>(value)
      .ok()
      .map(canonical_components)
  }

  /// Whether the baseline and the instance config describe the same identity.
  // Consumed by the update engine; only tests use it for now.
  #[cfg_attr(not(test), allow(dead_code))]
  /// A mismatch means one of the two files was replaced or edited by hand.
  pub fn is_consistent_with(&self, origin: &InstanceOrigin) -> bool {
    self.schema == BASELINE_SCHEMA
      && self.origin.platform == origin.platform
      && self.origin.project_id == origin.project_id
      && self.origin.version_id == origin.version_id
  }

  pub async fn save(&self, instance_dir: &Path) -> std::io::Result<()> {
    save_json_async(self, &instance_dir.join(BASELINE_FILE_NAME)).await
  }

  #[cfg_attr(not(test), allow(dead_code))]
  pub async fn load(instance_dir: &Path) -> std::io::Result<Self> {
    load_json_async::<Self>(&instance_dir.join(BASELINE_FILE_NAME)).await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;

  fn origin() -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Modrinth,
      project_id: "UCpApD3P".into(),
      version_id: "hudo6QuU".into(),
      game_version: "1.21.11".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    }
  }

  fn loader(version: &str) -> BaselineLoader {
    BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: version.into(),
    }
  }

  fn file(path: &str, sha1: &str, size: u64) -> BaselineFile {
    BaselineFile {
      path: path.into(),
      sha1: sha1.into(),
      size,
    }
  }

  #[test]
  fn content_hash_ignores_listing_order_but_not_content() {
    let a = vec![file("mods/a.jar", "aa", 1), file("config/b.toml", "bb", 2)];
    let mut reversed = a.clone();
    reversed.reverse();
    let base = compute_content_hash(&loader("0.16.0"), None, &a);
    assert_eq!(
      base,
      compute_content_hash(&loader("0.16.0"), None, &reversed)
    );
    assert_ne!(base, compute_content_hash(&loader("0.16.1"), None, &a));
    let mut changed = a.clone();
    changed[0].sha1 = "cc".into();
    assert_ne!(
      base,
      compute_content_hash(&loader("0.16.0"), None, &changed)
    );
    let mut resized = a.clone();
    resized[0].size = 9;
    assert_ne!(
      base,
      compute_content_hash(&loader("0.16.0"), None, &resized)
    );
  }

  #[test]
  fn canonical_loader_version_drops_only_the_neoforge_prefix() {
    assert_eq!(
      canonical_loader_version(ModLoaderType::NeoForge, "neoforge-26.1.2.87"),
      "26.1.2.87"
    );
    assert_eq!(
      canonical_loader_version(ModLoaderType::NeoForge, "21.10.0-beta"),
      "21.10.0-beta"
    );
    assert_eq!(
      canonical_loader_version(ModLoaderType::Fabric, "neoforge-0.16.0"),
      "neoforge-0.16.0"
    );
  }

  #[test]
  fn build_normalizes_sorts_and_lets_the_last_entry_win() {
    let baseline = ModpackBaseline::build(
      &origin(),
      loader("0.16.0"),
      vec![
        file("mods\\a.jar", "AA", 1),
        file("config/x.toml", "bb", 2),
        file("mods/a.jar", "cc", 3),
      ],
    );
    assert_eq!(
      baseline.files,
      vec![file("config/x.toml", "bb", 2), file("mods/a.jar", "cc", 3)]
    );
    assert_eq!(
      baseline.content_hash,
      compute_content_hash(&baseline.loader, None, &baseline.files)
    );
  }

  #[test]
  fn component_chain_comes_from_the_mmc_pack_and_ignores_order_and_labels() {
    let pack = serde_json::json!({
      "formatVersion": 1,
      "components": [
        {"uid": "net.minecraftforge", "version": "10.13.4.1614", "cachedName": "Forge"},
        {"uid": "net.minecraft", "version": "1.7.10", "important": true},
        {"cachedName": "no uid"},
        {"uid": "org.lwjgl"}
      ]
    });
    let refs = components_of_mmc_pack(&pack);
    let expected = |uid: &str, version: &str| ComponentRef {
      uid: uid.into(),
      version: version.into(),
    };
    assert_eq!(
      refs,
      vec![
        expected("net.minecraft", "1.7.10"),
        expected("net.minecraftforge", "10.13.4.1614"),
        expected("org.lwjgl", ""),
      ]
    );
    let mut reversed = refs.clone();
    reversed.reverse();
    assert_eq!(canonical_components(reversed), refs);
    assert!(components_of_mmc_pack(&serde_json::json!({})).is_empty());
  }

  #[test]
  fn recorded_components_survive_a_round_trip_and_change_the_content_hash() {
    let plain = ModpackBaseline::build(&origin(), loader("0.16.0"), vec![file("a", "aa", 1)]);
    assert!(plain.components.is_none() && plain.component_refs().is_none());
    let chain = vec![ComponentRef {
      uid: "net.minecraft".into(),
      version: "1.7.10".into(),
    }];
    let with = plain.clone().with_components(chain.clone());
    assert_eq!(with.component_refs(), Some(chain));
    assert_ne!(with.content_hash, plain.content_hash);
    assert_eq!(
      with.content_hash,
      compute_content_hash(&with.loader, with.components.as_ref(), &with.files)
    );
    let parsed: ModpackBaseline =
      serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    assert_eq!(parsed, with);
  }

  #[test]
  fn consistency_check_compares_the_identity_triple() {
    let baseline = ModpackBaseline::build(&origin(), loader("0.16.0"), vec![]);
    assert!(baseline.is_consistent_with(&origin()));
    let mut other = origin();
    other.version_id = "other".into();
    assert!(!baseline.is_consistent_with(&other));
    other = origin();
    other.platform = OriginPlatform::CurseForge;
    assert!(!baseline.is_consistent_with(&other));
  }

  #[test]
  fn serialized_baseline_has_no_instance_identity() {
    let baseline = ModpackBaseline::build(&origin(), loader("0.16.0"), vec![file("a", "aa", 1)]);
    let json = serde_json::to_string(&baseline).unwrap();
    for forbidden in ["\"name\"", "\"id\"", "\"versionPath\"", "\"root\""] {
      assert!(!json.contains(forbidden), "{forbidden} leaked into {json}");
    }
    let parsed: ModpackBaseline = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, baseline);
  }

  #[test]
  fn hashes_existing_files_with_sha1_and_size() {
    let root = std::env::temp_dir().join(format!("ustbl-baseline-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/a.toml"), b"abc").unwrap();
    let files = hash_existing_files(&root, &["config\\a.toml".to_string()]).unwrap();
    assert_eq!(
      files,
      vec![file(
        "config/a.toml",
        "a9993e364706816aba3e25717850c26c9cd0d89d",
        3
      )]
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn save_then_load_round_trips_inside_the_instance_directory() {
    let root = std::env::temp_dir().join(format!("ustbl-baseline-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let baseline = ModpackBaseline::build(&origin(), loader("0.16.0"), vec![file("a", "aa", 1)]);
    baseline.save(&root).await.unwrap();
    assert!(root.join(BASELINE_FILE_NAME).exists());
    assert_eq!(ModpackBaseline::load(&root).await.unwrap(), baseline);
    let _ = fs::remove_dir_all(root);
  }
}
