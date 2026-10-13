//! Three-way comparison between the modpack baseline, the files on disk and the
//! files of the version being installed.
//!
//! Files are compared by SHA-1 and size. Only paths the baseline or the new
//! version names are ever looked at, so anything the player added on their own
//! is never touched. A conflict is never resolved by the comparison itself: it
//! is listed, and the local file stays unless the caller explicitly chooses the
//! remote one.

use crate::instance::helpers::modpack::baseline::hash_file;
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
  Present(FileKey),
  /// Only `<path>.disabled` exists. A disabled file is a deliberate local
  /// choice, so it always counts as a local change, whatever it contains.
  Disabled(FileKey),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
  Add,
  Update,
  Delete,
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
  /// Switched off here (`.disabled`) while the new version touches it.
  DisabledLocally,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
  pub path: String,
  pub kind: ConflictKind,
  pub baseline: Option<FileKey>,
  pub local: Option<FileKey>,
  pub remote: Option<FileKey>,
}

#[derive(Debug, PartialEq, Eq, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
  pub changes: Vec<FileChange>,
  pub conflicts: Vec<Conflict>,
  /// Paths whose local content already equals the new version.
  pub already_current: Vec<String>,
}

impl Diff {
  pub fn is_empty(&self) -> bool {
    self.changes.is_empty() && self.conflicts.is_empty()
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictChoice {
  #[default]
  KeepLocal,
  TakeRemote,
}

/// Compare the three states. `local` must hold an entry for every path in
/// `baseline` and `remote`; a missing entry is read as absent.
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
    let local_key = match l {
      LocalState::Present(key) | LocalState::Disabled(key) => Some(key.clone()),
      LocalState::Absent => None,
    };
    let conflict = |kind| Conflict {
      path: path.clone(),
      kind,
      baseline: b.cloned(),
      local: local_key.clone(),
      remote: r.cloned(),
    };
    let write = |kind, remote: &FileKey| FileChange {
      path: path.clone(),
      kind,
      remote: Some(remote.clone()),
      local_size: local_key.as_ref().map(|key| key.size),
    };

    match (b, r) {
      (None, None) => {}
      (None, Some(r)) => match l {
        LocalState::Absent => diff.changes.push(write(ChangeKind::Add, r)),
        LocalState::Present(key) if key == r => diff.already_current.push(path.clone()),
        LocalState::Present(_) => diff
          .conflicts
          .push(conflict(ConflictKind::UnmanagedFileInTheWay)),
        LocalState::Disabled(_) => diff.conflicts.push(conflict(ConflictKind::DisabledLocally)),
      },
      (Some(b), None) => match l {
        LocalState::Absent => {}
        LocalState::Present(key) if key == b => diff.changes.push(FileChange {
          path: path.clone(),
          kind: ChangeKind::Delete,
          remote: None,
          local_size: Some(key.size),
        }),
        LocalState::Present(_) => diff
          .conflicts
          .push(conflict(ConflictKind::ModifiedLocallyRemovedRemotely)),
        LocalState::Disabled(_) => diff.conflicts.push(conflict(ConflictKind::DisabledLocally)),
      },
      (Some(b), Some(r)) if b == r => {
        // The new version did not touch it, so whatever the player did stays.
      }
      (Some(b), Some(r)) => match l {
        LocalState::Absent => diff
          .conflicts
          .push(conflict(ConflictKind::DeletedLocallyChangedRemotely)),
        LocalState::Present(key) if key == b => diff.changes.push(write(ChangeKind::Update, r)),
        LocalState::Present(key) if key == r => diff.already_current.push(path.clone()),
        LocalState::Present(_) => diff
          .conflicts
          .push(conflict(ConflictKind::ModifiedLocallyChangedRemotely)),
        LocalState::Disabled(_) => diff.conflicts.push(conflict(ConflictKind::DisabledLocally)),
      },
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
  /// Also move `<path>.disabled` out of the way.
  pub clear_disabled: bool,
}

/// Turn a diff and the player's answers into the steps to run. Unanswered
/// conflicts keep the local file.
pub fn resolve(diff: &Diff, choices: &BTreeMap<String, ConflictChoice>) -> Vec<Operation> {
  let mut operations: Vec<Operation> = diff
    .changes
    .iter()
    .map(|change| Operation {
      path: change.path.clone(),
      write: change.remote.clone(),
      clear_disabled: false,
    })
    .collect();
  for conflict in &diff.conflicts {
    if choices.get(&conflict.path).copied().unwrap_or_default() != ConflictChoice::TakeRemote {
      continue;
    }
    operations.push(Operation {
      path: conflict.path.clone(),
      write: conflict.remote.clone(),
      clear_disabled: conflict.kind == ConflictKind::DisabledLocally,
    });
  }
  operations.sort_by(|a, b| a.path.cmp(&b.path));
  operations
}

/// Read the on-disk state of the given relative paths.
pub fn collect_local_states(
  root: &Path,
  paths: &BTreeSet<String>,
) -> io::Result<BTreeMap<String, LocalState>> {
  let mut states = BTreeMap::new();
  for path in paths {
    let file = root.join(path);
    let state = if file.is_file() {
      let (sha1, size) = hash_file(&file)?;
      LocalState::Present(FileKey { sha1, size })
    } else if file.exists() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{path} is not a regular file"),
      ));
    } else {
      let disabled = root.join(format!("{path}{DISABLED_SUFFIX}"));
      if disabled.is_file() {
        let (sha1, size) = hash_file(&disabled)?;
        LocalState::Disabled(FileKey { sha1, size })
      } else {
        LocalState::Absent
      }
    };
    states.insert(path.clone(), state);
  }
  Ok(states)
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
  fn disabled_files_count_as_local_changes_in_every_situation() {
    let disabled = |tag: &str| LocalState::Disabled(key(tag));
    let cases: [(&[(&str, &str)], &[(&str, &str)]); 3] = [
      (&[("a", "a1")], &[("a", "a2")]),
      (&[("a", "a1")], &[]),
      (&[], &[("a", "a1")]),
    ];
    for (baseline, remote) in cases {
      for tag in ["a1", "a2", "other"] {
        let diff = three_way(
          &map(baseline),
          &local(&[("a", disabled(tag))]),
          &map(remote),
        );
        assert!(diff.changes.is_empty(), "{baseline:?} {remote:?} {tag}");
        assert_eq!(
          conflict_kinds(&diff),
          vec![("a".to_string(), ConflictKind::DisabledLocally)]
        );
      }
    }
    // The pack did not touch it, so the player's choice is simply respected.
    let diff = three_way(
      &map(&[("a", "a1")]),
      &local(&[("a", disabled("a1"))]),
      &map(&[("a", "a1")]),
    );
    assert!(diff.is_empty());
  }

  #[test]
  fn unlisted_paths_are_never_part_of_the_comparison() {
    let locals = local(&[("mods/mine", present("mine"))]);
    let diff = three_way(&map(&[("a", "a1")]), &locals, &map(&[("a", "a1")]));
    assert!(diff.is_empty());
  }

  #[test]
  fn resolve_keeps_local_files_unless_the_remote_one_is_chosen() {
    let baseline = map(&[("a", "a1"), ("gone", "g1"), ("off", "o1")]);
    let locals = local(&[
      ("a", present("mine")),
      ("gone", present("mine")),
      ("off", LocalState::Disabled(key("o1"))),
      ("new", present("mine")),
    ]);
    let remote = map(&[("a", "a2"), ("off", "o2"), ("new", "n1"), ("fresh", "f1")]);
    let diff = three_way(&baseline, &locals, &remote);

    let default = resolve(&diff, &BTreeMap::new());
    assert_eq!(
      default
        .iter()
        .map(|op| op.path.as_str())
        .collect::<Vec<_>>(),
      vec!["fresh"]
    );

    let mut choices = BTreeMap::new();
    choices.insert("a".to_string(), ConflictChoice::TakeRemote);
    choices.insert("gone".to_string(), ConflictChoice::TakeRemote);
    choices.insert("off".to_string(), ConflictChoice::TakeRemote);
    choices.insert("unknown".to_string(), ConflictChoice::TakeRemote);
    let operations = resolve(&diff, &choices);
    assert_eq!(
      operations,
      vec![
        Operation {
          path: "a".into(),
          write: Some(key("a2")),
          clear_disabled: false
        },
        Operation {
          path: "fresh".into(),
          write: Some(key("f1")),
          clear_disabled: false
        },
        Operation {
          path: "gone".into(),
          write: None,
          clear_disabled: false
        },
        Operation {
          path: "off".into(),
          write: Some(key("o2")),
          clear_disabled: true
        },
      ]
    );
  }

  #[test]
  fn local_states_distinguish_present_disabled_and_absent() {
    let root = std::env::temp_dir().join(format!("ustbl-diff-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("mods/on.jar"), b"abc").unwrap();
    std::fs::write(root.join("mods/off.jar.disabled"), b"abc").unwrap();
    std::fs::create_dir_all(root.join("mods/dir")).unwrap();

    let abc = FileKey {
      sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
      size: 3,
    };
    let paths: BTreeSet<String> = ["mods/on.jar", "mods/off.jar", "mods/none.jar"]
      .iter()
      .map(|p| p.to_string())
      .collect();
    let states = collect_local_states(&root, &paths).unwrap();
    assert_eq!(states["mods/on.jar"], LocalState::Present(abc.clone()));
    assert_eq!(states["mods/off.jar"], LocalState::Disabled(abc));
    assert_eq!(states["mods/none.jar"], LocalState::Absent);

    let in_the_way: BTreeSet<String> = ["mods/dir".to_string()].into();
    assert!(collect_local_states(&root, &in_the_way).is_err());
    let _ = std::fs::remove_dir_all(root);
  }
}
