//! Line diffs of single files of an update plan, made when the player opens
//! them. The plan itself carries no contents; it only remembers, next to the
//! plan cache, where the new text of each candidate file can be read from.

use super::commands::UpdatePlan;
use super::diff::{ChangeKind, Diff, FileChange};
use super::merge::merge_text;
use super::plan_cache::{self, PlanCache};
use super::remote::{RemoteModpack, StoredFileSource, StoredRemoteFile};
use super::text_diff::{lines_of, unified_diff_hiding, DiffHunk, CONTEXT_LINES};
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::text;
use crate::instance::models::misc::{InstanceError, OriginPlatform};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The archive a preview read by byte ranges: enough to open it again.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeArchive {
  pub platform: OriginPlatform,
  pub version_id: String,
  pub url: String,
  pub sha1: String,
}

/// Where the new content of the files a diff can be asked for is read from.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSources {
  pub files: Vec<StoredRemoteFile>,
  /// Set when the files are `Listed` entries of this archive.
  #[serde(default)]
  pub range: Option<RangeArchive>,
}

impl DiffSources {
  pub fn find(&self, path: &str) -> Option<&StoredRemoteFile> {
    self.files.iter().find(|file| file.path == path)
  }
}

/// Whether a change is one a text diff can be asked for: an update that does
/// not restore ignored edits, of a text file within the size limit.
fn diffable(change: &FileChange) -> bool {
  change.kind == ChangeKind::Update
    && !change.restored
    && text::is_text_path(&change.path, &[])
    && change
      .remote
      .as_ref()
      .is_some_and(|key| key.size <= text::MAX_TEXT_SIZE as u64)
    && change.local_size.unwrap_or(0) <= text::MAX_TEXT_SIZE as u64
}

/// The sources of every diffable update and automatic merge of a plan.
pub fn collect_sources(
  diff: &Diff,
  remote: &RemoteModpack,
  range: Option<RangeArchive>,
) -> DiffSources {
  let by_path: BTreeMap<&str, _> = remote
    .files
    .iter()
    .map(|file| (file.path.as_str(), file))
    .collect();
  let files: Vec<StoredRemoteFile> = diff
    .changes
    .iter()
    .chain(&diff.auto_merged)
    .filter(|change| diffable(change))
    .filter_map(|change| by_path.get(change.path.as_str()))
    .map(|file| StoredRemoteFile {
      path: file.path.clone(),
      key: file.key.clone(),
      source: StoredFileSource::from(&file.source),
      disabled: file.disabled,
      once: file.once,
    })
    .collect();
  let listed = files
    .iter()
    .any(|file| matches!(file.source, StoredFileSource::Listed { .. }));
  DiffSources {
    files,
    range: range.filter(|_| listed),
  }
}

/// What the diff of a path is between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
  /// The local file and the new version's file.
  Update,
  /// The local file and the result of merging the new version into it.
  Merged,
}

pub fn side_of(plan: &UpdatePlan, path: &str) -> Option<Side> {
  let listed = |changes: &[FileChange]| {
    changes
      .iter()
      .any(|change| change.path == path && change.kind == ChangeKind::Update && !change.restored)
  };
  if listed(&plan.changes) {
    Some(Side::Update)
  } else if listed(&plan.auto_merged) {
    Some(Side::Merged)
  } else {
    None
  }
}

/// The cached plan the dialog is showing, if it still is the current one:
/// same hash, and the local files still look as they did when it was made.
pub fn load_current(root: &Path, plan_hash: &str) -> USTBLResult<PlanCache> {
  let stale = || InstanceError::ModpackUpdateStale;
  let cache = plan_cache::load_plan_cache(root).ok_or_else(stale)?;
  if cache.plan.plan_hash != plan_hash {
    return Err(stale().into());
  }
  match plan_cache::fingerprint_paths(root, &cache.paths) {
    Ok(fingerprint) if fingerprint == cache.fingerprint => Ok(cache),
    _ => Err(stale().into()),
  }
}

/// Why a file has no diff to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileDiffUnavailable {
  /// Not UTF-8 text within the size limit.
  NotText,
  /// The plan has nothing to compare for this path, or a side cannot be read.
  Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackFileDiff {
  /// Line diff of the normalized texts; empty when only line endings, a BOM
  /// or a properties timestamp differ.
  pub hunks: Option<Vec<DiffHunk>>,
  pub unavailable: Option<FileDiffUnavailable>,
}

impl ModpackFileDiff {
  pub fn unavailable(reason: FileDiffUnavailable) -> Self {
    Self {
      hunks: None,
      unavailable: Some(reason),
    }
  }
}

/// The diff from `old` to `new`, both read as the update reads text.
pub fn diff_bytes(path: &str, old: &[u8], new: &[u8]) -> ModpackFileDiff {
  match (
    text::normalize_text(path, old),
    text::normalize_text(path, new),
  ) {
    (Some(old), Some(new)) => ModpackFileDiff {
      hunks: Some(unified_diff_hiding(
        &lines_of(&old),
        &lines_of(&new),
        CONTEXT_LINES,
        &|_| false,
      )),
      unavailable: None,
    },
    _ => ModpackFileDiff::unavailable(FileDiffUnavailable::NotText),
  }
}

/// The diff of a merged file: local text to the three-way merge result.
pub fn merged_diff(path: &str, base: &[u8], local: &[u8], remote: &[u8]) -> ModpackFileDiff {
  match merge_text(path, base, local, remote) {
    Ok(merged) => diff_bytes(path, local, &merged),
    Err(_) => ModpackFileDiff::unavailable(FileDiffUnavailable::Unavailable),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::modpack_update::diff::FileKey;
  use crate::instance::modpack_update::plan_cache::PlanCache;

  fn key(size: u64) -> FileKey {
    FileKey {
      sha1: "aa".into(),
      size,
    }
  }

  fn change(path: &str, kind: ChangeKind, size: u64, restored: bool) -> FileChange {
    FileChange {
      path: path.into(),
      kind,
      remote: Some(key(size)),
      local_size: Some(size),
      restored,
    }
  }

  fn plan(hash: &str, changes: Vec<FileChange>, auto_merged: Vec<FileChange>) -> UpdatePlan {
    UpdatePlan {
      from_version_id: "1".into(),
      to_version_id: "2".into(),
      to_label: "2".into(),
      changes,
      conflicts: Vec::new(),
      auto_merged,
      loader_change: None,
      component_changes: Vec::new(),
      reinstall_required: false,
      restore_components: false,
      fast_forward: false,
      plan_hash: hash.into(),
      new_optional: Vec::new(),
    }
  }

  fn root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-file-diff-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("config")).unwrap();
    root
  }

  fn cache_for(root: &Path, plan: UpdatePlan) -> PlanCache {
    let paths = vec!["config/a.cfg".to_string()];
    let fingerprint = plan_cache::fingerprint_paths(root, &paths).unwrap();
    PlanCache::new(
      "1".into(),
      "base".into(),
      paths,
      fingerprint,
      Vec::new(),
      plan,
      DiffSources::default(),
    )
  }

  #[test]
  fn only_text_updates_within_the_limit_are_diffable() {
    assert!(diffable(&change(
      "config/a.lang",
      ChangeKind::Update,
      10,
      false
    )));
    assert!(!diffable(&change(
      "config/a.lang",
      ChangeKind::Add,
      10,
      false
    )));
    assert!(!diffable(&change(
      "config/a.lang",
      ChangeKind::Update,
      10,
      true
    )));
    assert!(!diffable(&change(
      "mods/a.jar",
      ChangeKind::Update,
      10,
      false
    )));
    let large = text::MAX_TEXT_SIZE as u64 + 1;
    assert!(!diffable(&change(
      "config/a.cfg",
      ChangeKind::Update,
      large,
      false
    )));
  }

  #[test]
  fn a_path_is_an_update_a_merge_or_nothing() {
    let plan = plan(
      "h",
      vec![
        change("config/u.cfg", ChangeKind::Update, 1, false),
        change("config/r.cfg", ChangeKind::Update, 1, true),
      ],
      vec![change("config/m.cfg", ChangeKind::Update, 1, false)],
    );
    assert_eq!(side_of(&plan, "config/u.cfg"), Some(Side::Update));
    assert_eq!(side_of(&plan, "config/m.cfg"), Some(Side::Merged));
    assert_eq!(side_of(&plan, "config/r.cfg"), None);
    assert_eq!(side_of(&plan, "config/other.cfg"), None);
  }

  #[test]
  fn a_plan_that_is_not_the_cached_one_is_stale() {
    let root = root();
    std::fs::write(root.join("config/a.cfg"), "a=1\n").unwrap();
    let missing = load_current(&root, "h").unwrap_err();
    assert!(missing.0.contains("STALE"), "{missing:?}");

    let cache = cache_for(&root, plan("h", Vec::new(), Vec::new()));
    plan_cache::write_plan_cache(&root, &cache).unwrap();
    assert!(load_current(&root, "h").is_ok());
    let other = load_current(&root, "other").unwrap_err();
    assert!(other.0.contains("STALE"), "{other:?}");

    // A local file that changed since the plan was made.
    std::fs::write(root.join("config/a.cfg"), "a=1\nb=2\n").unwrap();
    let moved = load_current(&root, "h").unwrap_err();
    assert!(moved.0.contains("STALE"), "{moved:?}");
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn a_text_pair_gives_hunks_and_anything_else_says_why_not() {
    let diff = diff_bytes("assets/m/lang/en_US.lang", b"a=1\nb=2\n", b"a=1\nb=3\n");
    let hunks = diff.hunks.unwrap();
    assert_eq!(hunks.len(), 1);
    assert!(hunks[0].lines.iter().any(|line| line.text == "b=3"));
    // Line endings alone are not a difference.
    let same = diff_bytes("config/a.cfg", b"a=1\r\n", b"a=1\n");
    assert_eq!(same.hunks, Some(Vec::new()));
    assert_eq!(
      diff_bytes("mods/a.jar", b"PK", b"PK2").unavailable,
      Some(FileDiffUnavailable::NotText)
    );
    assert_eq!(
      diff_bytes("config/a.cfg", b"a=\xE4", b"a=1").unavailable,
      Some(FileDiffUnavailable::NotText)
    );
  }

  #[test]
  fn a_merge_is_shown_from_the_local_text_to_the_result() {
    let base = b"a=1\nb=2\nc=3\nd=4\ne=5\n";
    let local = b"a=L\nb=2\nc=3\nd=4\ne=5\n";
    let remote = b"a=1\nb=2\nc=3\nd=4\ne=R\n";
    let diff = merged_diff("config/a.cfg", base, local, remote);
    let lines: Vec<_> = diff
      .hunks
      .unwrap()
      .into_iter()
      .flat_map(|hunk| hunk.lines)
      .filter(|line| line.kind != crate::instance::modpack_update::text_diff::DiffLineKind::Context)
      .map(|line| line.text)
      .collect();
    assert_eq!(lines, vec!["e=5", "e=R"]);
    // Both sides edit the same line: nothing to show.
    let conflict = merged_diff("config/a.cfg", base, local, b"a=R\nb=2\nc=3\nd=4\ne=5\n");
    assert_eq!(conflict.unavailable, Some(FileDiffUnavailable::Unavailable));
  }
}
