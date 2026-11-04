//! Cached update plan for an instance. When the newest version, the baseline
//! and the local files' stat fingerprint all match, the plan can be shown
//! without hashing every managed file again.

use super::commands::UpdatePlan;
use super::file_diff::DiffSources;
use super::remote::UPDATE_DIR;
use sha1::{Digest, Sha1};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CACHE_FILE: &str = "plan-cache.json";
/// Schema 2 added `component_changes` to the plan; a schema 1 cache would show
/// a plan without them, so it is recomputed once. Schema 3 added `diff_sources`,
/// which a schema 2 cache cannot answer file diffs from.
const CACHE_SCHEMA: u32 = 3;

fn cache_path(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join(CACHE_FILE)
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCache {
  pub schema: u32,
  pub to_version_id: String,
  pub from_version_id: String,
  pub baseline_sha1: String,
  /// Paths the plan was computed over: every baseline and remote path.
  pub paths: Vec<String>,
  pub fingerprint: String,
  /// The new optional files the player picked when the plan was made; a plan
  /// is only reused for the same picks.
  #[serde(default)]
  pub optional_choice: Vec<String>,
  pub plan: UpdatePlan,
  /// Where the new text of the files that have a diff is read from.
  #[serde(default)]
  pub diff_sources: DiffSources,
}

impl PlanCache {
  pub fn new(
    from_version_id: String,
    baseline_sha1: String,
    paths: Vec<String>,
    fingerprint: String,
    optional_choice: Vec<String>,
    plan: UpdatePlan,
    diff_sources: DiffSources,
  ) -> Self {
    Self {
      schema: CACHE_SCHEMA,
      to_version_id: plan.to_version_id.clone(),
      from_version_id,
      baseline_sha1,
      paths,
      fingerprint,
      optional_choice,
      plan,
      diff_sources,
    }
  }
}

/// Fingerprint over the on-disk state of `paths` and their `.disabled`
/// variants: existence, size and modification time. Only stat calls; file
/// contents are never read, so a change that keeps size and mtime is missed.
pub fn fingerprint_paths(root: &Path, paths: &[String]) -> io::Result<String> {
  let mut entries = Vec::with_capacity(paths.len());
  for path in paths {
    for variant in [
      path.clone(),
      format!("{path}{}", super::diff::DISABLED_SUFFIX),
    ] {
      let file = root.join(&variant);
      let meta = match fs::metadata(&file) {
        Ok(meta) if meta.is_file() => Some(meta),
        Ok(_) => {
          return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{variant} is not a regular file"),
          ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
      };
      let (exists, size, mtime_ns) = match meta {
        Some(meta) => (
          true,
          meta.len(),
          meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        ),
        None => (false, 0, 0),
      };
      entries.push(format!("{variant}\0{exists}\0{size}\0{mtime_ns}"));
    }
  }
  entries.sort();
  let mut hasher = Sha1::new();
  for entry in &entries {
    hasher.update(entry.as_bytes());
    hasher.update(b"\n");
  }
  Ok(hex::encode(hasher.finalize()))
}

pub fn load_plan_cache(root: &Path) -> Option<PlanCache> {
  let bytes = fs::read(cache_path(root)).ok()?;
  let cache: PlanCache = serde_json::from_slice(&bytes).ok()?;
  if cache.schema != CACHE_SCHEMA {
    return None;
  }
  Some(cache)
}

pub fn write_plan_cache(root: &Path, cache: &PlanCache) -> io::Result<()> {
  let path = cache_path(root);
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let temp = path.with_extension("json.tmp");
  fs::write(&temp, serde_json::to_vec_pretty(cache)?)?;
  fs::rename(temp, path)
}

pub fn clear_plan_cache(root: &Path) {
  let _ = fs::remove_file(cache_path(root));
}

/// The paths a plan covers: every path either side lists, so a local edit
/// outside the comparison (say a player-deleted file reappearing) changes the
/// fingerprint.
pub fn plan_paths(
  baseline: &crate::instance::helpers::modpack::baseline::ModpackBaseline,
  remote: &super::remote::RemoteModpack,
) -> Vec<String> {
  let mut paths: Vec<String> = baseline
    .files
    .iter()
    .map(|file| file.path.clone())
    .chain(remote.files.iter().map(|file| file.path.clone()))
    .chain(
      remote
        .rules
        .iter()
        .filter_map(|rule| (rule.kind == super::remote::RuleKind::File).then(|| rule.path.clone())),
    )
    .collect();
  paths.sort();
  paths.dedup();
  paths
}

/// The sha1 over the baseline bytes as stored, identifying the install record
/// the plan was computed against.
pub fn baseline_sha1(
  baseline: &crate::instance::helpers::modpack::baseline::ModpackBaseline,
) -> String {
  hex::encode(Sha1::digest(
    serde_json::to_vec(baseline).unwrap_or_default(),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{
    BaselineFile, BaselineLoader, ModpackBaseline,
  };
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use std::time::SystemTime;

  fn origin() -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
      pack_format: None,
    }
  }

  fn baseline(files: &[(&str, u64)]) -> ModpackBaseline {
    ModpackBaseline::build(
      &origin(),
      BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16.0".into(),
      },
      files
        .iter()
        .map(|(path, size)| {
          BaselineFile::managed(
            path,
            crate::instance::modpack_update::diff::FileKey {
              sha1: "aa".into(),
              size: *size,
            },
            false,
          )
        })
        .collect(),
    )
  }

  fn plan() -> UpdatePlan {
    UpdatePlan {
      from_version_id: "1".into(),
      to_version_id: "2".into(),
      to_label: "2.0".into(),
      changes: Vec::new(),
      conflicts: Vec::new(),
      auto_merged: Vec::new(),
      loader_change: None,
      component_changes: Vec::new(),
      reinstall_required: false,
      restore_components: false,
      fast_forward: true,
      plan_hash: "hh".into(),
      new_optional: Vec::new(),
    }
  }

  #[test]
  fn an_old_plan_without_component_changes_still_deserializes() {
    let mut value = serde_json::to_value(plan()).unwrap();
    value.as_object_mut().unwrap().remove("componentChanges");
    let old: UpdatePlan = serde_json::from_value(value).unwrap();
    assert!(old.component_changes.is_empty());
  }

  #[test]
  fn a_cache_of_the_previous_schema_is_not_used() {
    let root = std::env::temp_dir().join(format!("ustbl-cache-test-{}", uuid::Uuid::new_v4()));
    let cache = PlanCache::new(
      "1".into(),
      "b".into(),
      Vec::new(),
      "f".into(),
      Vec::new(),
      plan(),
      DiffSources::default(),
    );
    let mut old = serde_json::to_value(&cache).unwrap();
    old["schema"] = 1.into();
    old["plan"]
      .as_object_mut()
      .unwrap()
      .remove("componentChanges");
    fs::create_dir_all(root.join(UPDATE_DIR)).unwrap();
    fs::write(cache_path(&root), serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(load_plan_cache(&root).is_none());
    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn fingerprint_changes_with_size_and_mtime() {
    let root = std::env::temp_dir().join(format!("ustbl-cache-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/a.jar"), b"aaaa").unwrap();
    let paths = vec!["mods/a.jar".to_string(), "mods/gone.jar".to_string()];

    let first = fingerprint_paths(&root, &paths).unwrap();
    // Same state fingerprints the same.
    assert_eq!(first, fingerprint_paths(&root, &paths).unwrap());

    // Size changes the fingerprint.
    fs::write(root.join("mods/a.jar"), b"aaaaa").unwrap();
    let resized = fingerprint_paths(&root, &paths).unwrap();
    assert_ne!(first, resized);

    // Same size, another modification time changes the fingerprint.
    let other = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    fs::File::options()
      .write(true)
      .open(root.join("mods/a.jar"))
      .unwrap()
      .set_modified(other)
      .unwrap();
    let touched = fingerprint_paths(&root, &paths).unwrap();
    assert_ne!(first, touched);
    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn fingerprint_sees_disabled_variants() {
    let root = std::env::temp_dir().join(format!("ustbl-cache-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("mods")).unwrap();
    let paths = vec!["mods/a.jar".to_string()];
    let absent = fingerprint_paths(&root, &paths).unwrap();
    fs::write(root.join("mods/a.jar.disabled"), b"off").unwrap();
    assert_ne!(absent, fingerprint_paths(&root, &paths).unwrap());
    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn cache_round_trips_and_clear_removes_it() {
    let root = std::env::temp_dir().join(format!("ustbl-cache-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/a.jar"), b"aaaa").unwrap();
    let base = baseline(&[("mods/a.jar", 4)]);
    let paths = vec!["mods/a.jar".to_string()];
    let fingerprint = fingerprint_paths(&root, &paths).unwrap();
    let cache = PlanCache::new(
      "1".into(),
      baseline_sha1(&base),
      paths.clone(),
      fingerprint.clone(),
      Vec::new(),
      plan(),
      DiffSources::default(),
    );

    assert!(load_plan_cache(&root).is_none());
    write_plan_cache(&root, &cache).unwrap();
    let loaded = load_plan_cache(&root).unwrap();
    assert_eq!(loaded.to_version_id, "2");
    assert_eq!(loaded.from_version_id, "1");
    assert_eq!(loaded.fingerprint, fingerprint);
    assert_eq!(loaded.paths, paths);
    assert_eq!(loaded.plan.plan_hash, "hh");
    assert!(loaded.plan.component_changes.is_empty());

    clear_plan_cache(&root);
    assert!(load_plan_cache(&root).is_none());
    // Corrupt bytes are not a cache.
    fs::create_dir_all(root.join(UPDATE_DIR)).unwrap();
    fs::write(cache_path(&root), b"not json").unwrap();
    assert!(load_plan_cache(&root).is_none());
    let _ = fs::remove_dir_all(&root);
  }
}
