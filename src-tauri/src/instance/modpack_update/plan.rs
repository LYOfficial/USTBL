//! Comparing an instance with the version it is being updated to.

use super::diff::{collect_local_states, three_way, Diff, FileKey, LocalState};
use super::local_scan::{collect_states_cached, record_states, LocalScanCache};
use super::remote::RemoteModpack;
use super::merge::{merge_text, text_conflicts, TextMergeError};
use super::text_diff::{unified_diff, DiffHunk, CONTEXT_LINES};
use serde::Serialize;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use crate::instance::helpers::modpack::{base_store, text};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

/// The hash memory of the local-change scans, for the whole process. Counting
/// and listing read it; preview and apply only ever write to it.
static SCAN_CACHE: std::sync::LazyLock<Mutex<LocalScanCache>> =
  std::sync::LazyLock::new(|| Mutex::new(LocalScanCache::default()));

/// Lock the cache, replacing a poisoned one with an empty cache.
fn cache() -> std::sync::MutexGuard<'static, LocalScanCache> {
  SCAN_CACHE
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The shared scan cache. Poisoning is not an error for a cache: the poisoned
/// contents are dropped, which only costs the next scan a full re-hash.
pub(crate) fn scan_cache() -> &'static Mutex<LocalScanCache> {
  &SCAN_CACHE
}

pub struct Prepared {
  pub diff: Diff,
  /// Identifies this exact comparison. Applying with a different hash means
  /// the instance or the published version moved since the player looked.
  pub plan_hash: String,
  /// Bytes for automatically merged files, keyed by relative path.
  pub merged_contents: BTreeMap<String, Vec<u8>>,
  pub conflict_sources: BTreeMap<String, (Vec<u8>, Vec<u8>, Vec<u8>)>,
}

/// The pieces of one comparison, so the conflict setup can finish the work
/// without hashing the local files a second time.
pub struct Compared {
  pub diff: Diff,
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

/// Whether the new version needs a clean install instead of an in-place update.
///
/// A component chain decides the libraries, loader patches and launch arguments
/// of an instance, which files cannot express. A new version that declares a
/// chain different from the one the instance was installed with, or declares one
/// for an instance that has none, must be installed again. A new version
/// without a chain never forces a reinstall.
///
/// An instance that was created for the modpack and has not installed any
/// version yet (`fresh`) is the exception: it has nothing to lose, and the
/// engine restores the chain into it.
pub fn requires_reinstall(baseline: &ModpackBaseline, remote: &RemoteModpack, fresh: bool) -> bool {
  let Some(wanted) = &remote.components else {
    return false;
  };
  if fresh && remote.component_pack.is_some() {
    return false;
  }
  baseline.component_refs().as_ref() != Some(wanted)
}

/// How many files the modpack manages differ from what it installed. A file the
/// player switched off counts, since that is a deliberate local change.
pub async fn count_local_changes(root: &Path, baseline: &ModpackBaseline) -> USTBLResult<usize> {
  Ok(local_changes(root, baseline, false).await?.len())
}

/// Forget this instance's scan entries (deleted, or stopped following).
pub fn invalidate_scan_cache(root: &Path) {
  cache().invalidate(root);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalChangeKind {
  Modified,
  Deleted,
  Disabled,
}

/// Why a modified file comes without a line diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoDiffReason {
  /// Not a text file the launcher compares by line (or too large).
  NotText,
  /// The installed text was not kept, so there is nothing to compare with.
  BaseMissing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalChange {
  pub path: String,
  pub kind: LocalChangeKind,
  pub baseline: FileKey,
  pub local: Option<FileKey>,
  /// Line diff of the normalized texts, installed → local.
  pub hunks: Option<Vec<DiffHunk>>,
  pub no_diff: Option<NoDiffReason>,
}

fn diff_lines(text: &str) -> Vec<String> {
  text.lines().map(str::to_owned).collect()
}

/// Managed files that differ from what the modpack installed, by path. Text
/// files that only differ in normalized-away noise are not changes. With
/// `with_diff` each modified text file carries its line diff.
pub async fn local_changes(
  root: &Path,
  baseline: &ModpackBaseline,
  with_diff: bool,
) -> USTBLResult<Vec<LocalChange>> {
  let keys = baseline_keys(baseline);
  let root = root.to_path_buf();
  tokio::task::spawn_blocking(move || {
    // The local scan reads the hash cache; a file whose size already differs
    // from the baseline is a change without being hashed again.
    let local = collect_states_cached(scan_cache(), &root, &keys)?;
    let mut changes = Vec::new();
    for (path, key) in keys {
      let local_key = match local.get(&path) {
        Some(LocalState::Present(local_key)) if *local_key == key => continue,
        Some(LocalState::Present(local_key)) => local_key.clone(),
        Some(LocalState::Disabled(local_key)) => {
          changes.push(LocalChange {
            path,
            kind: LocalChangeKind::Disabled,
            baseline: key,
            local: Some(local_key.clone()),
            hunks: None,
            no_diff: None,
          });
          continue;
        }
        _ => {
          changes.push(LocalChange {
            path,
            kind: LocalChangeKind::Deleted,
            baseline: key,
            local: None,
            hunks: None,
            no_diff: None,
          });
          continue;
        }
      };
      let local_bytes = std::fs::read(root.join(&path))?;
      let local_text = text::normalize_text(&path, &local_bytes);
      let base_text = match &local_text {
        Some(_) => base_store::load(&root, &key.sha1)
          .ok()
          .flatten()
          .and_then(|base| text::normalize_text(&path, &base)),
        None => None,
      };
      let (hunks, no_diff) = match (base_text, local_text) {
        (Some(base), Some(local)) if base == local => continue,
        (Some(base), Some(local)) => (
          with_diff.then(|| {
            unified_diff(&diff_lines(&base), &diff_lines(&local), CONTEXT_LINES)
          }),
          None,
        ),
        (None, Some(_)) => (None, Some(NoDiffReason::BaseMissing)),
        _ => (None, Some(NoDiffReason::NotText)),
      };
      changes.push(LocalChange {
        path,
        kind: LocalChangeKind::Modified,
        baseline: key,
        local: Some(local_key),
        hunks,
        no_diff,
      });
    }
    Ok::<_, USTBLError>(changes)
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))?
}

/// Prepare an update and merge eligible text conflicts when remote bytes are
/// available. Remote bytes are supplied by the caller because their source may
/// be an archive, a campus pack, or a download.
#[cfg(test)]
pub async fn prepare_with_contents(
  root: &Path,
  baseline: &ModpackBaseline,
  remote: &RemoteModpack,
  remote_contents: &BTreeMap<String, Vec<u8>>,
) -> USTBLResult<Prepared> {
  let compared = compare(root, baseline, remote).await?;
  finish(compared, root, remote, remote_contents)
}

/// Hash the local files both versions care about once, returning the three
/// states and the raw diff. Merging needs the remote bytes, which the caller
/// fetches after this first look.
pub async fn compare(
  root: &Path,
  baseline: &ModpackBaseline,
  remote: &RemoteModpack,
) -> USTBLResult<Compared> {
  let baseline_map = baseline_keys(baseline);
  let remote_map = remote.key_map();
  let paths: BTreeSet<String> = baseline_map
    .keys()
    .chain(remote_map.keys())
    .cloned()
    .collect();
  let root = root.to_path_buf();
  let local_root = root.clone();
  let local = tokio::task::spawn_blocking(move || collect_local_states(&local_root, &paths))
    .await
    .map_err(|e| USTBLError(e.to_string()))??;
  record_states(scan_cache(), &root, &local);
  let mut local = local;
  for (path, baseline_key) in &baseline_map {
    let Some(super::diff::LocalState::Present(local_key)) = local.get(path) else {
      continue;
    };
    if local_key == baseline_key {
      continue;
    }
    let equivalent = base_store::load(&root, &baseline_key.sha1)
      .ok()
      .flatten()
      .and_then(|base| {
        let local_bytes = std::fs::read(root.join(path)).ok()?;
        Some(text::normalize_text(path, &base)? == text::normalize_text(path, &local_bytes)?)
      })
      .unwrap_or(false);
    if equivalent {
      local.insert(
        path.clone(),
        super::diff::LocalState::Present(baseline_key.clone()),
      );
    }
  }
  let diff = three_way(&baseline_map, &local, &remote_map);
  Ok(Compared { diff })
}

/// Finish a comparison after the remote bytes of conflicting text files were
/// fetched: merge what merges itself and give the rest its line conflicts.
pub fn finish(
  compared: Compared,
  root: &Path,
  remote: &RemoteModpack,
  remote_contents: &BTreeMap<String, Vec<u8>>,
) -> USTBLResult<Prepared> {
  let Compared { mut diff, .. } = compared;
  let mut merged_contents = BTreeMap::new();
  let mut conflict_sources = BTreeMap::new();
  let mut remaining = Vec::new();
  for conflict in diff.conflicts {
      let merged = match (
      conflict.kind,
      conflict.baseline.as_ref(),
      conflict.local.as_ref(),
      conflict.remote.as_ref(),
      remote_contents.get(&conflict.path),
      ) {
      (super::diff::ConflictKind::ModifiedLocallyChangedRemotely, Some(base), Some(_local_key), Some(remote_key), Some(remote_bytes)) => {
        let base_bytes = base_store::load(root, &base.sha1).ok().flatten();
        let local_bytes = std::fs::read(root.join(&conflict.path)).ok();
        base_bytes.and_then(|base_bytes| local_bytes.and_then(|local_bytes| {
          match merge_text(&conflict.path, &base_bytes, &local_bytes, remote_bytes) {
            Ok(bytes) => Some((remote_key.clone(), bytes)),
            Err(TextMergeError::Conflict(_)) | Err(TextMergeError::InvalidText) | Err(TextMergeError::InvalidMergedFormat) => None,
          }
        }))
      }
      _ => None,
    };
    if let Some((remote_key, bytes)) = merged {
      merged_contents.insert(conflict.path.clone(), bytes);
      diff.auto_merged.push(super::diff::FileChange {
        path: conflict.path,
        kind: super::diff::ChangeKind::Update,
        remote: Some(remote_key),
        local_size: conflict.local.map(|key| key.size),
      });
    } else {
      if let (
        super::diff::ConflictKind::ModifiedLocallyChangedRemotely,
        Some(base),
        Some(_),
        Some(_),
        Some(remote_bytes),
      ) = (
        conflict.kind,
        conflict.baseline.as_ref(),
        conflict.local.as_ref(),
        conflict.remote.as_ref(),
        remote_contents.get(&conflict.path),
      ) {
        if let (Some(base_bytes), Ok(local_bytes)) = (
          base_store::load(root, &base.sha1).ok().flatten(),
          std::fs::read(root.join(&conflict.path)),
        ) {
          if let Ok(hunks) = text_conflicts(&conflict.path, &base_bytes, &local_bytes, remote_bytes) {
            let mut conflict = conflict;
            conflict.hunks = hunks;
            conflict_sources.insert(conflict.path.clone(), (base_bytes, local_bytes, remote_bytes.clone()));
            remaining.push(conflict);
            continue;
          }
        }
      }
      remaining.push(conflict);
    }
  }
  diff.conflicts = remaining;
  Ok(Prepared {
    plan_hash: plan_hash(remote, &diff),
    diff,
    merged_contents,
    conflict_sources,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{
    components_of_mmc_pack, BaselineFile, BaselineLoader, ComponentRef,
  };
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
      components: None,
      component_pack: None,
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

    let prepared = prepare_with_contents(&root, &baseline, &remote, &BTreeMap::new()).await.unwrap();

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

    let first = prepare_with_contents(&root, &baseline, &remote_v2, &BTreeMap::new())
      .await
      .unwrap()
      .plan_hash;
    let again = prepare_with_contents(&root, &baseline, &remote_v2, &BTreeMap::new())
      .await
      .unwrap()
      .plan_hash;
    assert_eq!(first, again);

    fs::write(root.join("mods/a.jar"), "a1-edited").unwrap();
    let edited = prepare_with_contents(&root, &baseline, &remote_v2, &BTreeMap::new())
      .await
      .unwrap()
      .plan_hash;
    assert_ne!(first, edited);

    let mut other = remote(&[("mods/a.jar", "a2")]);
    other.version_id = "3".into();
    assert_ne!(
      first,
      prepare_with_contents(&root.clone(), &baseline, &other, &BTreeMap::new())
        .await
        .unwrap()
        .plan_hash
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_pack_identical_to_the_baseline_has_nothing_to_do() {
    let root = instance(&[("mods/a.jar", "a1")]);
    let prepared = prepare_with_contents(
      &root,
      &baseline(&[("mods/a.jar", "a1")]),
      &remote(&[("mods/a.jar", "a1")]),
      &BTreeMap::new(),
    )
    .await
    .unwrap();
    assert!(prepared.diff.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn cached_text_baseline_ignores_line_ending_only_local_change() {
    let root = instance(&[("config/a.toml", "a=1\r\n")]);
    let baseline = baseline(&[("config/a.toml", "a=1\n")]);
    let baseline_bytes = b"a=1\n";
    let sha1 = hex::encode(Sha1::digest(baseline_bytes));
    std::fs::create_dir_all(crate::instance::helpers::modpack::base_store::base_dir(&root))
      .unwrap();
    std::fs::write(
      crate::instance::helpers::modpack::base_store::base_path(&root, &sha1),
      baseline_bytes,
    )
    .unwrap();
    assert_eq!(baseline.files[0].sha1, sha1);
    let prepared = prepare_with_contents(
      &root,
      &baseline,
      &remote(&[("config/a.toml", "a=2\n")]),
      &BTreeMap::new(),
    )
    .await
    .unwrap();
    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes.len(), 1);
    let _ = fs::remove_dir_all(root);
  }

  /// A baseline built the way `create_instance` does for a component pack: the
  /// chain comes from the `mmc-pack.json` kept in the profile snapshot.
  fn component_baseline(snapshot_pack: &serde_json::Value) -> ModpackBaseline {
    baseline(&[("mods/a.jar", "a1")]).with_components(components_of_mmc_pack(snapshot_pack))
  }

  #[tokio::test]
  async fn recovered_base_enables_conflict_blocks() {
    let root = std::env::temp_dir().join(format!("ustbl-base-plan-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(root.join("config/a.properties"), "key=local\n").unwrap();
    let installed = baseline(&[("config/a.properties", "key=base\n")]);
    let remote = remote(&[("config/a.properties", "key=remote\n")]);
    let contents = BTreeMap::from([("config/a.properties".into(), b"key=remote\n".to_vec())]);
    let missing = prepare_with_contents(&root, &installed, &remote, &contents).await.unwrap();
    assert!(missing.diff.conflicts[0].hunks.is_empty());
    base_store::store(&root, &installed.files[0], b"key=base\n").unwrap();
    let recovered = prepare_with_contents(&root, &installed, &remote, &contents).await.unwrap();
    assert_eq!(recovered.diff.conflicts[0].hunks.len(), 1);
    assert_eq!(recovered.diff.conflicts[0].hunks[0].base, vec!["key=base"]);
    let _ = fs::remove_dir_all(root);
  }

  fn chain_of(pack: &serde_json::Value) -> Option<Vec<ComponentRef>> {
    Some(components_of_mmc_pack(pack))
  }

  fn gtnh_pack(forge: &str) -> serde_json::Value {
    serde_json::json!({
      "formatVersion": 1,
      "components": [
        {"uid": "net.minecraft", "version": "1.7.10", "important": true},
        {"uid": "net.minecraftforge", "version": forge, "cachedName": "Forge"},
      ]
    })
  }

  #[test]
  fn a_changed_component_chain_requires_a_reinstall() {
    let installed = component_baseline(&gtnh_pack("10.13.4.1614"));
    let mut remote = remote(&[("mods/a.jar", "a2")]);

    remote.components = chain_of(&gtnh_pack("10.13.4.1615"));
    assert!(requires_reinstall(&installed, &remote, false));

    let mut extra = gtnh_pack("10.13.4.1614");
    extra["components"]
      .as_array_mut()
      .unwrap()
      .push(serde_json::json!({"uid": "org.lwjgl3", "version": "3.3.1"}));
    remote.components = chain_of(&extra);
    assert!(requires_reinstall(&installed, &remote, false));
  }

  #[test]
  fn an_unchanged_chain_or_no_chain_updates_files_in_place() {
    let installed = component_baseline(&gtnh_pack("10.13.4.1614"));
    let mut remote = remote(&[("mods/a.jar", "a2")]);

    // Same chain, listed in another order and with different labels.
    remote.components = chain_of(&serde_json::json!({
      "components": [
        {"uid": "net.minecraftforge", "version": "10.13.4.1614"},
        {"uid": "net.minecraft", "version": "1.7.10", "cachedName": "Minecraft"},
      ]
    }));
    assert!(!requires_reinstall(&installed, &remote, false));

    remote.components = None;
    assert!(!requires_reinstall(&installed, &remote, false));
  }

  #[test]
  fn a_chain_for_an_instance_without_one_requires_a_reinstall() {
    let plain = baseline(&[("mods/a.jar", "a1")]);
    let mut remote = remote(&[("mods/a.jar", "a2")]);
    assert!(!requires_reinstall(&plain, &remote, false));
    remote.components = chain_of(&gtnh_pack("10.13.4.1614"));
    assert!(requires_reinstall(&plain, &remote, false));
  }

  #[tokio::test]
  async fn only_managed_files_that_differ_count_as_local_changes() {
    let root = std::env::temp_dir().join(format!("ustbl-plan-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/same.jar"), "same").unwrap();
    fs::write(root.join("mods/edited.jar"), "edited-by-player").unwrap();
    fs::write(root.join("mods/off.jar.disabled"), "off").unwrap();
    // Not listed in the baseline, so not the modpack's business.
    fs::write(root.join("mods/mine.jar"), "mine").unwrap();
    let installed = baseline(&[
      ("mods/same.jar", "same"),
      ("mods/edited.jar", "original"),
      ("mods/off.jar", "off"),
      ("mods/gone.jar", "gone"),
    ]);

    // edited, switched off, deleted
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 3);
    let clean = baseline(&[("mods/same.jar", "same")]);
    assert_eq!(count_local_changes(&root, &clean).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn local_change_count_ignores_cached_text_formatting_noise() {
    let root = instance(&[("config/a.toml", "a=1\r\n")]);
    let baseline_bytes = b"a=1\n";
    let baseline_sha1 = hex::encode(Sha1::digest(baseline_bytes));
    let baseline = baseline(&[("config/a.toml", "a=1\n")]);
    fs::create_dir_all(crate::instance::helpers::modpack::base_store::base_dir(&root)).unwrap();
    fs::write(
      crate::instance::helpers::modpack::base_store::base_path(&root, &baseline_sha1),
      baseline_bytes,
    )
    .unwrap();
    assert_eq!(count_local_changes(&root, &baseline).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn local_changes_list_kinds_and_text_diffs() {
    let root = instance(&[
      ("config/a.toml", "a=1\r\nb=3\r\n"),
      ("config/b.toml", "x=1\n"),
      ("mods/a.jar", "changed"),
    ]);
    let base_text = b"a=1\nb=2\n";
    let base_sha1 = hex::encode(Sha1::digest(base_text));
    fs::create_dir_all(crate::instance::helpers::modpack::base_store::base_dir(&root)).unwrap();
    fs::write(
      crate::instance::helpers::modpack::base_store::base_path(&root, &base_sha1),
      base_text,
    )
    .unwrap();
    let installed = baseline(&[
      ("config/a.toml", "a=1\nb=2\n"),
      // No kept copy of the installed text.
      ("config/b.toml", "x=0\n"),
      ("mods/a.jar", "original"),
      ("mods/gone.jar", "gone"),
    ]);

    let changes = local_changes(&root, &installed, true).await.unwrap();
    let by_path: BTreeMap<_, _> = changes.iter().map(|c| (c.path.as_str(), c)).collect();
    assert_eq!(by_path.len(), 4);

    let text = by_path["config/a.toml"];
    assert_eq!(text.kind, LocalChangeKind::Modified);
    let hunks = text.hunks.as_ref().unwrap();
    let rendered: Vec<String> = hunks[0]
      .lines
      .iter()
      .map(|line| format!("{:?}:{}", line.kind, line.text))
      .collect();
    assert_eq!(rendered, ["Context:a=1", "Delete:b=2", "Add:b=3"]);

    assert_eq!(by_path["config/b.toml"].no_diff, Some(NoDiffReason::BaseMissing));
    assert_eq!(by_path["mods/a.jar"].no_diff, Some(NoDiffReason::NotText));
    assert_eq!(by_path["mods/gone.jar"].kind, LocalChangeKind::Deleted);

    // Without diffs the same files are listed, only lighter.
    let light = local_changes(&root, &installed, false).await.unwrap();
    assert_eq!(light.len(), 4);
    assert!(light.iter().all(|change| change.hunks.is_none()));
    let _ = fs::remove_dir_all(root);
  }

  /// A base line list, replaced wholesale on disk and/or upstream. Each side
  /// is 3000 short lines, so the LCS middle is ~9M cells: past the budget the
  /// merge must degrade instead of building the matrix.
  fn big_text(tag: &str) -> String {
    (0..3000).map(|n| format!("{tag}{n}\n")).collect()
  }

  fn store_base(root: &Path, baseline: &ModpackBaseline, content: &str) {
    let file = &baseline.files[0];
    base_store::store(root, file, content.as_bytes()).unwrap();
  }

  #[tokio::test]
  async fn a_huge_one_sided_text_edit_still_updates() {
    let base = big_text("b");
    let local = big_text("local");
    let root = instance(&[("config/a.txt", &local)]);
    let installed = baseline(&[("config/a.txt", &base)]);
    store_base(&root, &installed, &base);
    // The new version did not touch the file; only the local side moved.
    let remote = remote(&[("config/a.txt", &base)]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    // The local edit simply stays: no conflict, nothing auto-taken.
    assert!(prepared.diff.conflicts.is_empty());
    assert!(prepared.diff.auto_merged.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn huge_two_sided_text_edits_degrade_to_one_whole_file_conflict() {
    let base = big_text("b");
    let local = big_text("local");
    let remote_text = big_text("remote");
    let root = instance(&[("config/a.txt", &local)]);
    let installed = baseline(&[("config/a.txt", &base)]);
    store_base(&root, &installed, &base);
    let remote = remote(&[("config/a.txt", &remote_text)]);
    let contents = BTreeMap::from([("config/a.txt".to_string(), remote_text.as_bytes().to_vec())]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &contents)
      .await
      .unwrap();
    // No InvalidText, no aborted update: exactly one conflict the user answers.
    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert_eq!(prepared.diff.conflicts[0].hunks.len(), 1);
    let hunk = &prepared.diff.conflicts[0].hunks[0];
    assert_eq!(hunk.local.len(), 3000);
    assert_eq!(hunk.remote.len(), 3000);
    let _ = fs::remove_dir_all(root);
  }

  /// §7.2: the local-change scan reads the hash cache; the update preview
  /// hashes the files itself, so a poisoned or stale entry can never leak
  /// into the plan the apply step trusts.
  #[tokio::test]
  async fn prepare_hashes_the_instance_itself_instead_of_trusting_the_cache() {
    let root = instance(&[("mods/a.jar", "a1")]);
    let installed = baseline(&[("mods/a.jar", "a1")]);
    let remote = remote(&[("mods/a.jar", "a2")]);

    // Prime the cache with a state that does not match the disk: the file is
    // "edited" according to the entry the scan now holds.
    record_states(
      scan_cache(),
      &root,
      &BTreeMap::from([(
        "mods/a.jar".to_string(),
        LocalState::Present(key("a1-edited")),
      )]),
    );
    // The scan trusts the entry while the stat holds...
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 1);
    // ...but the preview hashes the file itself: it matches the baseline, so
    // the update is a plain update without a conflict.
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes.len(), 1);
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Update);
    let _ = fs::remove_dir_all(root);
  }
}
