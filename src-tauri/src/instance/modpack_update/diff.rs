//! Three-way comparison between the modpack baseline, the files on disk and the
//! files of the version being installed.
//!
//! Files are compared by SHA-1 and size. Only paths the baseline or the new
//! version names are ever looked at, so anything the player added on their own
//! is never touched. A conflict is never resolved by the comparison itself: it
//! is listed, and the local file stays unless the caller explicitly chooses the
//! remote one.

use crate::instance::helpers::modpack::baseline::hash_file;
use super::merge::MergeConflict;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

/// Suffix the launcher gives to a mod the player switched off.
pub const DISABLED_SUFFIX: &str = ".disabled";

#[derive(Debug, PartialEq, Eq, Clone, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileKey {
  pub sha1: String,
  pub size: u64,
}

/// What is on disk for one managed path.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum LocalState {
  Absent,
  /// `<path>` exists; `<path>.disabled` does not.
  Present(FileKey),
  /// Only `<path>.disabled` exists.
  Disabled(FileKey),
  /// Both variants exist at once; keys are (enabled, disabled). The update
  /// plan refuses such paths when the new version or the baseline involves
  /// them: the player has to delete one variant by hand.
  Both(FileKey, FileKey),
}

impl LocalState {
  /// The content key of the file, from whichever variant holds it. `Both`
  /// reads as the enabled variant.
  pub fn key(&self) -> Option<&FileKey> {
    match self {
      LocalState::Absent => None,
      LocalState::Present(key) | LocalState::Disabled(key) => Some(key),
      LocalState::Both(enabled, _) => Some(enabled),
    }
  }

  /// Which variant the file sits at, when it exists at all.
  pub fn variant_disabled(&self) -> Option<bool> {
    match self {
      LocalState::Absent => None,
      LocalState::Present(_) | LocalState::Both(..) => Some(false),
      LocalState::Disabled(_) => Some(true),
    }
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
  Add,
  Update,
  Delete,
  /// The file only moves between its enabled and disabled variants.
  Disable,
  Enable,
  /// The modpack stops managing the file but leaves it on disk (it goes
  /// dormant, or its baseline entry is dropped while it stays).
  Retire,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
  pub path: String,
  pub kind: ChangeKind,
  /// Key of the file to write; `None` for deletions.
  pub remote: Option<FileKey>,
  /// Size of the file leaving the disk, for deletions and updates.
  pub local_size: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
  /// Edited here and changed by the new version.
  ModifiedLocallyChangedRemotely,
  /// Edited here, but the new version no longer ships it.
  ModifiedLocallyRemovedRemotely,
  /// Deleted here, but the new version changed it.
  DeletedLocallyChangedRemotely,
  /// A file the modpack never installed sits where the new version wants one.
  UnmanagedFileInTheWay,
  /// A file the modpack left alone (dormant) is managed again while the
  /// local content is neither the recorded nor the shipped one.
  Remanage,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
  pub path: String,
  pub kind: ConflictKind,
  pub baseline: Option<FileKey>,
  pub local: Option<FileKey>,
  pub remote: Option<FileKey>,
  /// The variant the path's file ends up at: a take-remote answer writes the
  /// remote file there; a kept-local file sitting at the other variant is
  /// renamed. `None` leaves the variant alone (a path the remote no longer
  /// ships has no disabled state to settle).
  #[serde(default)]
  pub remote_disabled: Option<bool>,
  /// The variant the local file sits at, when one exists. Together with
  /// `remote_disabled` it tells a keep-local answer whether the file moves.
  #[serde(default)]
  pub local_disabled: Option<bool>,
  pub hunks: Vec<MergeConflict>,
}

#[derive(Debug, PartialEq, Eq, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
  pub changes: Vec<FileChange>,
  pub conflicts: Vec<Conflict>,
  /// Text files whose local and remote edits were merged automatically.
  pub auto_merged: Vec<FileChange>,
  /// Paths whose local content already equals the new version.
  pub already_current: Vec<String>,
}

impl Diff {
  #[cfg(test)]
  pub fn is_empty(&self) -> bool {
    self.changes.is_empty() && self.conflicts.is_empty()
  }

  /// Append another path's result.
  #[cfg(test)]
  pub fn extend(&mut self, other: Diff) {
    self.changes.extend(other.changes);
    self.conflicts.extend(other.conflicts);
    self.auto_merged.extend(other.auto_merged);
    self.already_current.extend(other.already_current);
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictChoice {
  #[default]
  KeepLocal,
  TakeRemote,
}

/// The content side of one managed path the new version also ships: the
/// three-way comparison of 《整合包配置合并与块级发布》§6.1. The disabled
/// state is decided elsewhere; here only keys are compared. `local_key` is
/// the key of whichever variant is on disk. Only the tests use it directly;
/// the decision function of `decide.rs` encodes the same rows.
#[cfg(test)]
pub fn ship_managed(path: &str, b: &FileKey, r: &FileKey, local_key: Option<&FileKey>) -> Diff {
  let mut diff = Diff::default();
  let conflict = |kind| Conflict {
    path: path.to_string(),
    kind,
    baseline: Some(b.clone()),
    local: local_key.cloned(),
    remote: Some(r.clone()),
    remote_disabled: None,
    local_disabled: local_key.is_some().then_some(false),
    hunks: Vec::new(),
  };
  let write = |kind, remote: &FileKey| FileChange {
    path: path.to_string(),
    kind,
    remote: Some(remote.clone()),
    local_size: local_key.map(|key| key.size),
  };
  if b == r {
    // The new version did not touch it, so whatever the player did stays.
    return diff;
  }
  match local_key {
    None => diff
      .conflicts
      .push(conflict(ConflictKind::DeletedLocallyChangedRemotely)),
    Some(key) if key == b => diff.changes.push(write(ChangeKind::Update, r)),
    Some(key) if key == r => diff.already_current.push(path.to_string()),
    Some(_) => diff
      .conflicts
      .push(conflict(ConflictKind::ModifiedLocallyChangedRemotely)),
  }
  diff
}

/// Compare the three states. `local` must hold an entry for every path in
/// `baseline` and `remote`; a missing entry is read as absent. Only the
/// tests use it; the update engine decides path by path with `decide.rs`.
#[cfg(test)]
pub fn three_way(
  baseline: &BTreeMap<String, FileKey>,
  local: &BTreeMap<String, LocalState>,
  remote: &BTreeMap<String, FileKey>,
) -> Diff {
  let paths: BTreeSet<&String> = baseline.keys().chain(remote.keys()).collect();
  let mut diff = Diff::default();

  for path in paths {
    let b = baseline.get(path);
    let r = remote.get(path);
    let l = local.get(path).unwrap_or(&LocalState::Absent);
    let local_key = l.key().cloned();
    let conflict = |kind| Conflict {
      path: path.clone(),
      kind,
      baseline: b.cloned(),
      local: local_key.clone(),
      remote: r.cloned(),
      remote_disabled: None,
      local_disabled: local_key.is_some().then_some(false),
      hunks: Vec::new(),
    };
    let write = |kind, remote: &FileKey| FileChange {
      path: path.clone(),
      kind,
      remote: Some(remote.clone()),
      local_size: local_key.as_ref().map(|key| key.size),
    };

    match (b, r) {
      (None, None) => {}
      (None, Some(r)) => match &local_key {
        None => diff.changes.push(write(ChangeKind::Add, r)),
        Some(key) if key == r => diff.already_current.push(path.clone()),
        Some(_) => diff
          .conflicts
          .push(conflict(ConflictKind::UnmanagedFileInTheWay)),
      },
      (Some(b), None) => match &local_key {
        None => {}
        Some(key) if key == b => diff.changes.push(FileChange {
          path: path.clone(),
          kind: ChangeKind::Delete,
          remote: None,
          local_size: Some(key.size),
        }),
        Some(_) => diff
          .conflicts
          .push(conflict(ConflictKind::ModifiedLocallyRemovedRemotely)),
      },
      (Some(b), Some(r)) => {
        diff.extend(ship_managed(path, b, r, local_key.as_ref()));
      }
    }
  }
  diff
}

/// One concrete step on disk after conflicts were answered.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Operation {
  pub path: String,
  /// Content to write, or `None` to remove the file.
  pub write: Option<FileKey>,
  /// The variant the file ends up at when something is written.
  pub final_disabled: bool,
  /// The file only moves between its variants; `write` is `None`.
  pub rename: bool,
  /// Directories to remove afterwards if empty, deepest last; each entry is
  /// walked up to and including itself. Only delete rules set this.
  pub prune_dirs: Vec<String>,
}

/// Read the on-disk state of the given relative paths, both filename variants
/// included.
pub fn collect_local_states(
  root: &Path,
  paths: &BTreeSet<String>,
) -> io::Result<BTreeMap<String, LocalState>> {
  let mut states = BTreeMap::new();
  for path in paths {
    states.insert(path.clone(), scan_path(root, path)?);
  }
  Ok(states)
}

/// The on-disk state of one relative path.
pub fn scan_path(root: &Path, path: &str) -> io::Result<LocalState> {
  let file = root.join(path);
  let enabled = if file.is_file() {
    let (sha1, size) = hash_file(&file)?;
    Some(FileKey { sha1, size })
  } else if file.exists() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("{path} is not a regular file"),
    ));
  } else {
    None
  };
  let disabled_file = root.join(format!("{path}{DISABLED_SUFFIX}"));
  let disabled = if disabled_file.is_file() {
    let (sha1, size) = hash_file(&disabled_file)?;
    Some(FileKey { sha1, size })
  } else if disabled_file.exists() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("{path}{DISABLED_SUFFIX} is not a regular file"),
    ));
  } else {
    None
  };
  Ok(match (enabled, disabled) {
    (None, None) => LocalState::Absent,
    (Some(key), None) => LocalState::Present(key),
    (None, Some(key)) => LocalState::Disabled(key),
    (Some(enabled), Some(disabled)) => LocalState::Both(enabled, disabled),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(tag: &str) -> FileKey {
    FileKey {
      sha1: tag.into(),
      size: tag.len() as u64,
    }
  }

  fn map(entries: &[(&str, &str)]) -> BTreeMap<String, FileKey> {
    entries
      .iter()
      .map(|(path, tag)| (path.to_string(), key(tag)))
      .collect()
  }

  fn local(entries: &[(&str, LocalState)]) -> BTreeMap<String, LocalState> {
    entries
      .iter()
      .map(|(path, state)| (path.to_string(), state.clone()))
      .collect()
  }

  fn present(tag: &str) -> LocalState {
    LocalState::Present(key(tag))
  }

  fn kinds(diff: &Diff) -> Vec<(String, ChangeKind)> {
    diff
      .changes
      .iter()
      .map(|change| (change.path.clone(), change.kind))
      .collect()
  }

  fn conflict_kinds(diff: &Diff) -> Vec<(String, ConflictKind)> {
    diff
      .conflicts
      .iter()
      .map(|conflict| (conflict.path.clone(), conflict.kind))
      .collect()
  }

  #[test]
  fn untouched_install_updates_adds_and_removes_cleanly() {
    let baseline = map(&[("mods/a", "a1"), ("mods/old", "old"), ("config/c", "c1")]);
    let locals = local(&[
      ("mods/a", present("a1")),
      ("mods/old", present("old")),
      ("config/c", present("c1")),
      ("mods/new", LocalState::Absent),
    ]);
    let remote = map(&[("mods/a", "a2"), ("config/c", "c1"), ("mods/new", "new")]);

    let diff = three_way(&baseline, &locals, &remote);

    assert_eq!(
      kinds(&diff),
      vec![
        ("mods/a".to_string(), ChangeKind::Update),
        ("mods/new".to_string(), ChangeKind::Add),
        ("mods/old".to_string(), ChangeKind::Delete),
      ]
    );
    assert!(diff.conflicts.is_empty());
  }

  #[test]
  fn local_edits_survive_when_the_new_version_did_not_change_the_file() {
    let baseline = map(&[("config/c", "c1")]);
    let locals = local(&[("config/c", present("mine"))]);
    let diff = three_way(&baseline, &locals, &map(&[("config/c", "c1")]));
    assert!(diff.is_empty());
  }

  #[test]
  fn a_file_the_player_deleted_stays_deleted_unless_the_pack_changes_it() {
    let baseline = map(&[("mods/a", "a1"), ("mods/b", "b1")]);
    let locals = local(&[
      ("mods/a", LocalState::Absent),
      ("mods/b", LocalState::Absent),
    ]);
    let diff = three_way(
      &baseline,
      &locals,
      &map(&[("mods/a", "a1"), ("mods/b", "b2")]),
    );
    assert!(diff.changes.is_empty());
    assert_eq!(
      conflict_kinds(&diff),
      vec![(
        "mods/b".to_string(),
        ConflictKind::DeletedLocallyChangedRemotely
      )]
    );
  }

  #[test]
  fn edits_on_both_sides_conflict_unless_they_agree() {
    let baseline = map(&[("a", "a1"), ("b", "b1")]);
    let locals = local(&[("a", present("mine")), ("b", present("b2"))]);
    let diff = three_way(&baseline, &locals, &map(&[("a", "a2"), ("b", "b2")]));
    assert_eq!(
      conflict_kinds(&diff),
      vec![(
        "a".to_string(),
        ConflictKind::ModifiedLocallyChangedRemotely
      )]
    );
    assert_eq!(diff.already_current, vec!["b".to_string()]);
    assert!(diff.changes.is_empty());
  }

  #[test]
  fn removed_upstream_but_edited_locally_is_a_conflict_not_a_deletion() {
    let baseline = map(&[("a", "a1")]);
    let diff = three_way(
      &baseline,
      &local(&[("a", present("mine"))]),
      &BTreeMap::new(),
    );
    assert!(diff.changes.is_empty());
    assert_eq!(
      conflict_kinds(&diff),
      vec![(
        "a".to_string(),
        ConflictKind::ModifiedLocallyRemovedRemotely
      )]
    );
  }

  #[test]
  fn removing_a_file_that_is_already_gone_is_a_no_op() {
    let diff = three_way(
      &map(&[("a", "a1")]),
      &local(&[("a", LocalState::Absent)]),
      &BTreeMap::new(),
    );
    assert!(diff.is_empty());
  }

  #[test]
  fn a_foreign_file_in_the_way_of_a_new_one_is_a_conflict_unless_identical() {
    let locals = local(&[("a", present("mine")), ("b", present("b1"))]);
    let diff = three_way(&BTreeMap::new(), &locals, &map(&[("a", "a1"), ("b", "b1")]));
    assert_eq!(
      conflict_kinds(&diff),
      vec![("a".to_string(), ConflictKind::UnmanagedFileInTheWay)]
    );
    assert_eq!(diff.already_current, vec!["b".to_string()]);
  }

  #[test]
  fn a_disabled_variant_compares_by_content_like_the_enabled_one() {
    // `three_way` only sees keys: a disabled file compares exactly like the
    // enabled one with the same content. The disabled state itself is the
    // decision layer's business, not the comparison's.
    let disabled = |tag: &str| LocalState::Disabled(key(tag));
    let diff = three_way(
      &map(&[("a", "a1")]),
      &local(&[("a", disabled("a1"))]),
      &map(&[("a", "a2")]),
    );
    assert_eq!(kinds(&diff), vec![("a".to_string(), ChangeKind::Update)]);
    assert!(diff.conflicts.is_empty());
    let diff = three_way(
      &map(&[("a", "a1")]),
      &local(&[("a", disabled("mine"))]),
      &map(&[("a", "a2")]),
    );
    assert_eq!(
      conflict_kinds(&diff),
      vec![(
        "a".to_string(),
        ConflictKind::ModifiedLocallyChangedRemotely
      )]
    );
    let diff = three_way(
      &map(&[("a", "a1")]),
      &local(&[("a", disabled("a1"))]),
      &BTreeMap::new(),
    );
    assert_eq!(kinds(&diff), vec![("a".to_string(), ChangeKind::Delete)]);
  }

  #[test]
  fn unlisted_paths_are_never_part_of_the_comparison() {
    let locals = local(&[("mods/mine", present("mine"))]);
    let diff = three_way(&map(&[("a", "a1")]), &locals, &map(&[("a", "a1")]));
    assert!(diff.is_empty());
  }

  #[test]
  fn both_variants_read_as_the_enabled_one_for_the_comparison() {
    let states = local(&[("a", LocalState::Both(key("e"), key("d")))]);
    let diff = three_way(&map(&[("a", "e")]), &states, &map(&[("a", "e2")]));
    assert_eq!(kinds(&diff), vec![("a".to_string(), ChangeKind::Update)]);
  }

  #[test]
  fn local_states_distinguish_present_disabled_both_and_absent() {
    let root = std::env::temp_dir().join(format!("ustbl-diff-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("mods/on.jar"), b"abc").unwrap();
    std::fs::write(root.join("mods/off.jar.disabled"), b"abc").unwrap();
    std::fs::write(root.join("mods/dup.jar"), b"abc").unwrap();
    std::fs::write(root.join("mods/dup.jar.disabled"), b"abcd").unwrap();
    std::fs::create_dir_all(root.join("mods/dir")).unwrap();

    let abc = FileKey {
      sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
      size: 3,
    };
    let abcd = FileKey {
      sha1: "81fe8bfe87576c3ecb22426f8e57847382917acf".into(),
      size: 4,
    };
    let paths: BTreeSet<String> = [
      "mods/on.jar",
      "mods/off.jar",
      "mods/dup.jar",
      "mods/none.jar",
    ]
    .iter()
    .map(|p| p.to_string())
    .collect();
    let states = collect_local_states(&root, &paths).unwrap();
    assert_eq!(states["mods/on.jar"], LocalState::Present(abc.clone()));
    assert_eq!(states["mods/off.jar"], LocalState::Disabled(abc.clone()));
    assert_eq!(
      states["mods/dup.jar"],
      LocalState::Both(abc.clone(), abcd.clone())
    );
    assert_eq!(states["mods/none.jar"], LocalState::Absent);

    let in_the_way: BTreeSet<String> = ["mods/dir".to_string()].into();
    assert!(collect_local_states(&root, &in_the_way).is_err());
    let _ = std::fs::remove_dir_all(root);
  }
}
