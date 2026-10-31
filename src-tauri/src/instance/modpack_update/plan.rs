//! Comparing an instance with the version it is being updated to.

use super::decide::{
  decide, disabled_outcome, Action, Base, DecisionConflict, DecisionInput, Local, NextBaseline,
  Policy,
};
use super::diff::{
  collect_local_states, scan_path, ChangeKind, Conflict, ConflictChoice, ConflictKind, Diff,
  FileChange, FileKey, LocalState, Operation, DISABLED_SUFFIX,
};
use super::local_scan::{collect_states_cached, record_states, LocalScanCache};
use super::ignores::{self, Ignores};
use super::merge::{merge_text, text_conflicts, TextMergeError};
use super::remote::{RemoteModpack, RemoteRule, RuleKind, RuleState};
use super::text_diff::{edits, lines_of, unified_diff_hiding, DiffHunk, CONTEXT_LINES};
use crate::error::{USTBLError, USTBLResult};
use crate::file_error::AtPath;
use crate::instance::helpers::modpack::baseline::{BaselineFile, ModpackBaseline};
use crate::instance::helpers::modpack::{base_store, text};
use serde::Serialize;
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
  /// Local text after restoring its ignored ranges, before any remote merge.
  pub restored_locals: BTreeMap<String, Vec<u8>>,
  /// The baseline entries the update records when it commits.
  pub next_baseline: Vec<BaselineFile>,
  /// The variant each `Add`/`Update` change's written path ends up at
  /// (`true` = `.disabled`). Conflict paths carry theirs on the conflict.
  pub variants: BTreeMap<String, bool>,
  /// Delete-rule directories pruned after a deletion, by deleted path.
  pub prune_dirs: BTreeMap<String, Vec<String>>,
  /// The local ignores that apply to the baseline, and the file as stored
  /// (the apply writes the former back when they differ).
  pub ignores: Ignores,
  pub ignores_on_disk: Ignores,
}

/// The pieces of one comparison, so the conflict setup can finish the work
/// without hashing the local files a second time.
pub struct Compared {
  pub diff: Diff,
  /// The baseline entries the update records when it commits.
  pub next_baseline: Vec<BaselineFile>,
  /// The variant each `Add`/`Update` change's written path ends up at
  /// (`true` = `.disabled`). Conflict paths carry theirs on the conflict.
  pub variants: BTreeMap<String, bool>,
  /// Paths with both variants on disk that the remote or the baseline
  /// involves. Planning refuses these outright.
  pub both_variants: Vec<String>,
  /// Delete-rule directories pruned after a deletion, by deleted path.
  pub prune_dirs: BTreeMap<String, Vec<String>>,
  /// Local text after restoring its ignored ranges, before any remote merge.
  pub restored_locals: BTreeMap<String, Vec<u8>>,
  /// Restored local text that is the final content without a remote merge.
  pub restored_writes: BTreeSet<String>,
  /// The local ignores that apply to the baseline, and the file as stored.
  pub ignores: Ignores,
  pub ignores_on_disk: Ignores,
}

/// The baseline entries by path, managed and dormant alike.
fn baseline_map(baseline: &ModpackBaseline) -> BTreeMap<String, BaselineFile> {
  baseline
    .files
    .iter()
    .map(|file| (file.path.clone(), file.clone()))
    .collect()
}

pub fn plan_hash(
  remote: &RemoteModpack,
  diff: &Diff,
  next_baseline: &[BaselineFile],
  ignores: &Ignores,
) -> String {
  let mut hasher = Sha1::new();
  hasher.update(remote.version_id.as_bytes());
  hasher.update(b"\0");
  hasher.update(remote.content_hash().as_bytes());
  hasher.update(b"\0");
  hasher.update(serde_json::to_vec(diff).unwrap_or_default());
  hasher.update(b"\0");
  hasher.update(serde_json::to_vec(next_baseline).unwrap_or_default());
  hasher.update(b"\0");
  hasher.update(serde_json::to_vec(ignores).unwrap_or_default());
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

/// How many local changes the instance shows: managed files that differ from
/// the release it matched (a file the player switched off counts) plus files
/// added to the content directories. The same decision as the change list.
pub async fn count_local_changes(root: &Path, baseline: &ModpackBaseline) -> USTBLResult<usize> {
  super::changes::count(root, baseline).await
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
  /// Only the disabled state differs: the file sits at the other variant
  /// with the recorded content.
  Disabled,
  /// Same as `Disabled`, the other way round.
  Enabled,
}

/// Why a modified file comes without a line diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoDiffReason {
  /// Not a text file the launcher compares by line (or too large).
  NotText,
  /// The installed text was not kept, so there is nothing to compare with.
  BaseMissing,
  /// Both variants sit on disk; delete one to see what it holds.
  BothVariants,
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
  /// A modified file whose disabled state also flipped.
  #[serde(default)]
  pub toggled: bool,
}


/// Managed files that differ from what the modpack installed, by path. Text
/// files that only differ in normalized-away noise are not changes. With
/// `with_diff` each modified text file carries its line diff. Dormant entries
/// are never changes.
pub async fn local_changes(
  root: &Path,
  baseline: &ModpackBaseline,
  with_diff: bool,
) -> USTBLResult<Vec<LocalChange>> {
  let entries = baseline_map(baseline);
  let ignores = ignores::read_valid(root, baseline);
  let keys: BTreeMap<String, FileKey> = entries
    .iter()
    .filter(|(path, file)| {
      file.is_managed() && !ignores.files.get(*path).is_some_and(|entry| entry.whole)
    })
    .filter_map(|(path, file)| file.key().map(|key| (path.clone(), key)))
    .collect();
  let disabled_map: BTreeMap<String, bool> = entries
    .iter()
    .filter(|(_, file)| file.is_managed())
    .map(|(path, file)| (path.clone(), file.disabled))
    .collect();
  let root = root.to_path_buf();
  tokio::task::spawn_blocking(move || {
    // The local scan reads the hash cache; a file whose size already differs
    // from the baseline is a change without being hashed again.
    let local = collect_states_cached(scan_cache(), &root, &keys)?;
    let mut changes = Vec::new();
    for (path, key) in keys {
      let baseline_disabled = disabled_map[&path];
      let state = local.get(&path).cloned().unwrap_or(LocalState::Absent);
      let variant = state.variant_disabled();
      let toggled = variant.is_some_and(|disabled| disabled != baseline_disabled);
      if let LocalState::Both(enabled, _) = &state {
        changes.push(LocalChange {
          path,
          kind: LocalChangeKind::Modified,
          baseline: key,
          local: Some(enabled.clone()),
          hunks: None,
          no_diff: Some(NoDiffReason::BothVariants),
          toggled,
        });
        continue;
      }
      let local_key = match state.key() {
        Some(found) if *found == key && !toggled => continue,
        Some(found) if *found == key && toggled => {
          changes.push(LocalChange {
            path,
            kind: if baseline_disabled {
              LocalChangeKind::Enabled
            } else {
              LocalChangeKind::Disabled
            },
            baseline: key,
            local: Some(found.clone()),
            hunks: None,
            no_diff: None,
            toggled: false,
          });
          continue;
        }
        Some(found) => found.clone(),
        None => {
          changes.push(LocalChange {
            path,
            kind: LocalChangeKind::Deleted,
            baseline: key,
            local: None,
            hunks: None,
            no_diff: None,
            toggled: false,
          });
          continue;
        }
      };
      let local_path = match variant {
        Some(true) => format!("{path}{DISABLED_SUFFIX}"),
        _ => path.clone(),
      };
      let local_file = root.join(&local_path);
      let local_bytes = std::fs::read(&local_file).at_in(&local_file, &root)?;
      let local_text = text::normalize_text(&path, &local_bytes);
      let base_text = match &local_text {
        Some(_) => base_store::load(&root, &key.sha1)
          .ok()
          .flatten()
          .and_then(|base| text::normalize_text(&path, &base)),
        None => None,
      };
      // Changes the player ignored do not count: a file whose every change
      // is ignored reads like an unchanged one.
      let entry = ignores.files.get(&path);
      let lines = match (&base_text, &local_text) {
        (Some(base), Some(local)) => Some((lines_of(base), lines_of(local))),
        _ => None,
      };
      let same = lines.as_ref().is_some_and(|(base, local)| {
        base == local
          || entry.is_some_and(|entry| ignores::all_ignored(entry, &edits(base, local)))
      });
      let (hunks, no_diff) = match (base_text, local_text) {
        (Some(_), Some(_)) if same && !toggled => continue,
        (Some(_), Some(_)) if same && toggled => {
          changes.push(LocalChange {
            path,
            kind: if baseline_disabled {
              LocalChangeKind::Enabled
            } else {
              LocalChangeKind::Disabled
            },
            baseline: key,
            local: Some(local_key),
            hunks: None,
            no_diff: None,
            toggled: false,
          });
          continue;
        }
        (Some(_), Some(_)) => (
          with_diff
            .then(|| {
              lines.as_ref().map(|(base, local)| {
                unified_diff_hiding(base, local, CONTEXT_LINES, &|edit| {
                  entry.is_some_and(|entry| ignores::is_ignored(entry, edit))
                })
              })
            })
            .flatten(),
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
        toggled,
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

/// The files inside a delete `dir` rule's directory that the decision has to
/// see: every regular file below it, as (disk spelling, logical path) pairs.
/// The logical path folds the `.disabled` suffix back to the enabled name.
fn enumerate_under(root: &Path, dir: &str, found: &mut BTreeMap<String, String>) -> USTBLResult<()> {
  let base = root.join(dir);
  if !base.is_dir() {
    return Ok(());
  }
  let mut stack = vec![base];
  while let Some(current) = stack.pop() {
    for entry in std::fs::read_dir(&current).at_in(&current, root)? {
      let entry = entry.at_in(&current, root)?;
      let file_type = entry.file_type().at_in(&entry.path(), root)?;
      let relative = entry
        .path()
        .strip_prefix(root)
        .map_err(|_| USTBLError("delete rule escaped the instance".into()))?
        .to_string_lossy()
        .replace('\\', "/");
      if file_type.is_dir() {
        stack.push(entry.path());
      } else if file_type.is_file() {
        let logical = relative
          .strip_suffix(DISABLED_SUFFIX)
          .unwrap_or(&relative)
          .to_string();
        found.insert(relative, logical);
      }
      // Anything else (symlinks, sockets) is not a managed file.
    }
  }
  Ok(())
}

/// The path a case-insensitive file system finds for `logical`: the baseline
/// or manifest spelling when one folds to it (§3), the disk's own otherwise.
fn canonical_path(
  logical: &str,
  baseline: &BTreeMap<String, String>,
  remote: &RemoteModpack,
) -> String {
  if let Some(path) = baseline.get(&logical.to_lowercase()) {
    return path.clone();
  }
  match remote.file_of(logical) {
    Some(file) => file.path.clone(),
    None => logical.to_string(),
  }
}

/// The candidate paths the decision runs over: the baseline, the shipped
/// files, the `file` rules, and everything a delete `dir` rule covers.
/// `rule_dirs` collects the delete `dir` rules for the prune map. Disk paths
/// that fold to a baseline entry or a shipped file keep the baseline's or the
/// manifest's spelling, so one file never appears twice.
fn candidate_paths(
  root: &Path,
  baseline: &BTreeMap<String, BaselineFile>,
  remote: &RemoteModpack,
) -> USTBLResult<(BTreeSet<String>, Vec<RemoteRule>)> {
  let mut paths: BTreeSet<String> = baseline.keys().cloned().collect();
  paths.extend(remote.files.iter().map(|file| file.path.clone()));
  let mut delete_dirs = Vec::new();
  for rule in &remote.rules {
    match (rule.kind, rule.state) {
      (RuleKind::File, _) => {
        paths.insert(rule.path.clone());
      }
      (RuleKind::Dir, RuleState::Delete) => delete_dirs.push(rule.clone()),
      (RuleKind::Dir, RuleState::Ignore) => {}
    }
  }
  // Enumerate under each delete dir rule, but only where no more specific
  // rule or shipped file shields the file.
  let baseline_folded: BTreeMap<String, String> = baseline
    .keys()
    .map(|path| (path.to_lowercase(), path.clone()))
    .collect();
  let mut found = BTreeMap::new();
  for rule in &delete_dirs {
    enumerate_under(root, &rule.path, &mut found)?;
  }
  for (_, logical) in found {
    if matches!(remote.policy_of(&logical), Policy::Delete) {
      // Only this rule family may delete it; keep the canonical spelling.
      paths.insert(canonical_path(&logical, &baseline_folded, remote));
    }
  }
  Ok((paths, delete_dirs))
}

/// Paths nothing sits at under their own spelling, paired with the disk file
/// whose name only differs in case. The comparison reads the file under its
/// canonical name afterwards, so one file is decided exactly once.
fn find_folded_matches(
  root: &Path,
  paths: &BTreeSet<String>,
  states: &BTreeMap<String, LocalState>,
) -> Vec<(String, String, LocalState)> {
  let mut matches = Vec::new();
  for path in paths {
    if states.get(path).is_some_and(|state| *state != LocalState::Absent) {
      continue;
    }
    let name = path.rsplit_once('/').map(|(_, name)| name).unwrap_or(path);
    let dir = match path.rsplit_once('/') {
      Some((dir, _)) => root.join(dir),
      None => root.to_path_buf(),
    };
    let Some(spelling) = std::fs::read_dir(&dir)
      .ok()
      .into_iter()
      .flatten()
      .flatten()
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .find(|entry| {
        entry.to_lowercase() == name.to_lowercase()
          || (entry.to_lowercase() == format!("{}{}", name, DISABLED_SUFFIX).to_lowercase())
      })
    else {
      continue;
    };
    // A `.disabled` spelling folds back to the enabled logical name;
    // `scan_path` reports it as the disabled variant of that path.
    let logical = spelling
      .strip_suffix(DISABLED_SUFFIX)
      .unwrap_or(&spelling)
      .to_string();
    let spelling = match path.rsplit_once('/') {
      Some((dir, _)) => format!("{dir}/{logical}"),
      None => logical,
    };
    if let Ok(state) = scan_path(root, &spelling) {
      if state != LocalState::Absent {
        matches.push((path.clone(), spelling, state));
      }
    }
  }
  matches
}

/// How `compare` decides one path: the baseline entry, the possibly restored
/// local state, the remote's policy and the local ignores.
pub struct DecisionContext<'a> {
  pub remote: &'a RemoteModpack,
  pub ignores: &'a Ignores,
}

/// Decide one path and fold the outcome into the diff, the next baseline, the
/// variant map and the prune map. Pure apart from the data `compare` already
/// collected.
#[allow(clippy::too_many_arguments)]
fn decide_path(
  path: &str,
  context: &DecisionContext,
  baseline: Option<&BaselineFile>,
  local: &LocalState,
  actual_local: &LocalState,
  restored_local: bool,
  delete_dirs: &[RemoteRule],
  diff: &mut Diff,
  next_baseline: &mut BTreeMap<String, BaselineFile>,
  variants: &mut BTreeMap<String, bool>,
  prune_dirs: &mut BTreeMap<String, Vec<String>>,
  restored_writes: &mut BTreeSet<String>,
) {
  let remote = context.remote;
  let policy = remote.policy_of(path);
  let ship = remote.ship_of(path);
  let base = match baseline {
    None => Base::None,
    Some(entry) if entry.is_managed() => match entry.key() {
      Some(key) => Base::Managed {
        key,
        disabled: entry.disabled,
      },
      // A managed entry without content cannot exist on this branch; treat
      // it like a dormant one defensively.
      None => Base::Dormant {
        key: None,
        disabled: entry.disabled,
      },
    },
    Some(entry) => Base::Dormant {
      key: entry.key(),
      disabled: entry.disabled,
    },
  };
  let local_decision = match local {
    LocalState::Absent => Local::Absent,
    LocalState::Present(key) => Local::Present {
      key: key.clone(),
      disabled: false,
    },
    LocalState::Disabled(key) => Local::Present {
      key: key.clone(),
      disabled: true,
    },
    LocalState::Both(enabled, disabled) => Local::Both {
      enabled: enabled.clone(),
      disabled: disabled.clone(),
    },
  };
  let managed = baseline.is_some_and(BaselineFile::is_managed);
  let restore_whole = managed
    && context
      .ignores
      .files
      .get(path)
      .is_some_and(|entry| entry.whole);
  let replace_added = !managed && context.ignores.ignores_added(path);
  let restored_added = replace_added
    && ship
      .as_ref()
      .is_some_and(|ship| actual_local.key().is_some_and(|key| *key != ship.key));
  let restored = restore_whole || restored_local || restored_added;
  let decision = decide(
    path,
    &DecisionInput {
      policy,
      remote: ship.as_ref(),
      base,
      local: local_decision,
      restore_whole,
      replace_added,
    },
  );
  let local_key = local.key().cloned();
  let local_size = actual_local.key().map(|key| key.size);
  match &decision.action {
    Action::WriteRemote { key, variant } => {
      let kind = match baseline {
        Some(entry) if entry.is_managed() || entry.key().is_some() => ChangeKind::Update,
        _ => ChangeKind::Add,
      };
      diff.changes.push(FileChange {
        path: path.to_string(),
        kind,
        remote: Some(key.clone()),
        local_size,
        restored,
      });
      variants.insert(path.to_string(), variant.disabled());
    }
    Action::Rename { variant, .. } => {
      if restored_local {
        let remote = ship.as_ref().expect("a restored text file is shipped");
        diff.changes.push(FileChange {
          path: path.to_string(),
          kind: ChangeKind::Update,
          remote: Some(remote.key.clone()),
          local_size,
          restored: true,
        });
        restored_writes.insert(path.to_string());
      } else {
        diff.changes.push(FileChange {
          path: path.to_string(),
          kind: match variant {
            super::decide::Variant::Disabled => ChangeKind::Disable,
            super::decide::Variant::Enabled => ChangeKind::Enable,
          },
          remote: None,
          local_size,
          restored,
        });
      }
      variants.insert(path.to_string(), variant.disabled());
    }
    Action::Delete => {
      diff.changes.push(FileChange {
        path: path.to_string(),
        kind: ChangeKind::Delete,
        remote: None,
        local_size,
        restored: false,
      });
      if policy == Policy::Delete {
        let dirs: Vec<String> = delete_dirs
          .iter()
          .filter(|rule| {
            path
              .to_lowercase()
              .starts_with(&format!("{}/", rule.path.to_lowercase()))
          })
          .map(|rule| rule.path.clone())
          .collect();
        if !dirs.is_empty() {
          prune_dirs.insert(path.to_string(), dirs);
        }
      }
    }
    Action::Retire => {
      diff.changes.push(FileChange {
        path: path.to_string(),
        kind: ChangeKind::Retire,
        remote: None,
        local_size,
        restored: false,
      });
    }
    Action::Nothing => {
      if restored_local && decision.conflict.is_none() && policy == Policy::Ship {
        let remote = ship.as_ref().expect("a restored text file is shipped");
        let disabled = disabled_outcome(
          remote.disabled,
          baseline.and_then(|entry| entry.key().map(|_| entry.disabled)),
          local.variant_disabled(),
        );
        diff.changes.push(FileChange {
          path: path.to_string(),
          kind: ChangeKind::Update,
          remote: Some(remote.key.clone()),
          local_size,
          restored: true,
        });
        variants.insert(path.to_string(), disabled);
        restored_writes.insert(path.to_string());
      }
    }
  }
  if let Some(conflict) = &decision.conflict {
    let kind = match conflict {
      DecisionConflict::UnmanagedFileInTheWay => ConflictKind::UnmanagedFileInTheWay,
      DecisionConflict::Remanage => ConflictKind::Remanage,
      DecisionConflict::ModifiedRemovedRemotely => ConflictKind::ModifiedLocallyRemovedRemotely,
      DecisionConflict::DeletedChangedRemotely => ConflictKind::DeletedLocallyChangedRemotely,
      DecisionConflict::ModifiedChangedRemotely => ConflictKind::ModifiedLocallyChangedRemotely,
    };
    let baseline_key = baseline.and_then(BaselineFile::key);
    // The variant the path's file ends up at, however the conflict is
    // answered: "take remote" writes the new file there, and a kept-local
    // file sitting at the other variant is renamed (§5.2: the disabled state
    // is decided separately from the content, a content conflict does not
    // suspend it). `None` when the path has no disabled state to settle.
    // A file in the way never had a recorded state to compare against, so
    // its local variant wins; every other conflict takes the three-way.
    let remote_disabled = ship.as_ref().map(|ship| {
      if matches!(conflict, DecisionConflict::UnmanagedFileInTheWay) {
        local.variant_disabled().unwrap_or_else(|| ship.disabled.unwrap_or(false))
      } else {
        disabled_outcome(
          ship.disabled,
          baseline.and_then(|entry| entry.key().map(|_| entry.disabled)),
          local.variant_disabled(),
        )
      }
    });
    diff.conflicts.push(Conflict {
      path: path.to_string(),
      kind,
      baseline: baseline_key,
      local: local_key,
      remote: ship.as_ref().map(|ship| ship.key.clone()),
      remote_disabled,
      local_disabled: local.variant_disabled(),
      hunks: Vec::new(),
    });
  }
  match decision.next {
    NextBaseline::Unchanged => {
      if let Some(entry) = baseline {
        next_baseline.insert(path.to_string(), entry.clone());
      }
    }
    NextBaseline::Remove => {}
    NextBaseline::Set(entry) => {
      next_baseline.insert(path.to_string(), entry);
    }
  }
}

/// Hash the local files both versions care about once, then run the decision
/// over every candidate path. Merging needs the remote bytes, which the
/// caller fetches after this first look.
pub async fn compare(
  root: &Path,
  baseline: &ModpackBaseline,
  remote: &RemoteModpack,
) -> USTBLResult<Compared> {
  let baseline_map = baseline_map(baseline);
  let (paths, delete_dirs) = candidate_paths(root, &baseline_map, remote)?;
  let root = root.to_path_buf();
  let local_root = root.clone();
  let scan_paths = paths.clone();
  let local = tokio::task::spawn_blocking(move || collect_local_states(&local_root, &scan_paths))
    .await
    .map_err(|e| USTBLError(e.to_string()))??;
  record_states(scan_cache(), &root, &local);
  // The baseline's or the manifest's spelling may not be the disk's on a
  // case-insensitive file system; a path with nothing at its own spelling
  // falls back to the file whose name only differs in case (§3).
  let mut local = local;
  let folded_root = root.clone();
  let folded_paths = paths.clone();
  let folded_states = local.clone();
  let folded = tokio::task::spawn_blocking(move || {
    find_folded_matches(&folded_root, &folded_paths, &folded_states)
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))?;
  for (path, spelling, state) in folded {
    record_states(scan_cache(), &root, &BTreeMap::from([(spelling, state.clone())]));
    local.insert(path, state);
  }
  // A local file that only differs in normalized-away noise counts as the
  // recorded content. This folds the key before the decision sees it, for
  // managed entries and dormant ones carrying content alike (§5.1: ≡B means
  // the update fires without a remanage prompt).
  for (path, entry) in &baseline_map {
    let Some(baseline_key) = entry.key() else {
      continue;
    };
    let Some(state) = local.get(path) else {
      continue;
    };
    let Some(local_key) = state.key() else {
      continue;
    };
    if *local_key == baseline_key {
      continue;
    }
    let variant_path = match state.variant_disabled() {
      Some(true) => format!("{path}{DISABLED_SUFFIX}"),
      _ => path.clone(),
    };
    let equivalent = base_store::load(&root, &baseline_key.sha1)
      .ok()
      .flatten()
      .and_then(|base| {
        let local_bytes = std::fs::read(root.join(&variant_path)).ok()?;
        Some(text::normalize_text(path, &base)? == text::normalize_text(path, &local_bytes)?)
      })
      .unwrap_or(false);
    if equivalent {
      local.insert(
        path.clone(),
        match state.variant_disabled() {
          Some(true) => LocalState::Disabled(baseline_key),
          _ => LocalState::Present(baseline_key),
        },
      );
    }
  }
  let ignores_on_disk = ignores::read(&root);
  let ignores = ignores_on_disk.validated(baseline);
  let actual_local = local.clone();
  let mut restored_locals = BTreeMap::new();
  for (path, entry) in &ignores.files {
    let Some(state) = local.get(path).cloned() else {
      continue;
    };
    let Some(bytes) = ignores::restored_local(&root, path, entry, &state) else {
      continue;
    };
    let mut restored_key = FileKey {
      sha1: hex::encode(Sha1::digest(&bytes)),
      size: bytes.len() as u64,
    };
    if let Some(base_key) = baseline_map.get(path).and_then(BaselineFile::key) {
      let equivalent = base_store::load(&root, &base_key.sha1)
        .ok()
        .flatten()
        .and_then(|base| {
          Some(text::normalize_text(path, &base)? == text::normalize_text(path, &bytes)?)
        })
        .unwrap_or(false);
      if equivalent {
        restored_key = base_key;
      }
    }
    local.insert(
      path.clone(),
      match state.variant_disabled() {
        Some(true) => LocalState::Disabled(restored_key),
        _ => LocalState::Present(restored_key),
      },
    );
    restored_locals.insert(path.clone(), bytes);
  }
  let context = DecisionContext {
    remote,
    ignores: &ignores,
  };
  let mut diff = Diff::default();
  let mut next_baseline = BTreeMap::new();
  let mut variants = BTreeMap::new();
  let mut prune_dirs = BTreeMap::new();
  let mut restored_writes = BTreeSet::new();
  let mut both_variants = Vec::new();
  for path in &paths {
    let entry = baseline_map.get(path);
    let state = local.get(path).cloned().unwrap_or(LocalState::Absent);
    let actual_state = actual_local
      .get(path)
      .cloned()
      .unwrap_or(LocalState::Absent);
    // Both variants on disk refuses the plan when the path is in play; a
    // delete rule clears both without asking.
    if let LocalState::Both(..) = &state {
      let policy = remote.policy_of(path);
      let involves =
        policy != Policy::Delete && (policy == Policy::Ship || entry.is_some_and(|e| e.is_managed()));
      if involves {
        both_variants.push(path.clone());
        continue;
      }
    }
    decide_path(
      path,
      &context,
      entry,
      &state,
      &actual_state,
      restored_locals.contains_key(path),
      &delete_dirs,
      &mut diff,
      &mut next_baseline,
      &mut variants,
      &mut prune_dirs,
      &mut restored_writes,
    );
  }
  Ok(Compared {
    diff,
    next_baseline: next_baseline.into_values().collect(),
    variants,
    both_variants,
    prune_dirs,
    restored_locals,
    restored_writes,
    ignores,
    ignores_on_disk,
  })
}

/// The steps the apply runs, from the prepared plan and the player's answers.
/// Unanswered conflicts keep the local file; a kept-local file that needs a
/// variant flip is renamed instead of written.
pub fn resolve(prepared: &Prepared, choices: &BTreeMap<String, ConflictChoice>) -> Vec<Operation> {
  let mut operations = Vec::new();
  for change in prepared
    .diff
    .changes
    .iter()
    .chain(&prepared.diff.auto_merged)
  {
    match change.kind {
      ChangeKind::Add | ChangeKind::Update => operations.push(Operation {
        path: change.path.clone(),
        write: change.remote.clone(),
        final_disabled: prepared
          .variants
          .get(&change.path)
          .copied()
          .unwrap_or(false),
        rename: false,
        write_local: false,
        prune_dirs: Vec::new(),
      }),
      ChangeKind::Delete => operations.push(Operation {
        path: change.path.clone(),
        write: None,
        final_disabled: false,
        rename: false,
        write_local: false,
        prune_dirs: prepared
          .prune_dirs
          .get(&change.path)
          .cloned()
          .unwrap_or_default(),
      }),
      ChangeKind::Disable | ChangeKind::Enable => {
        // Rename-only: the content is the local file's; the apply moves it
        // out of its current variant into the other.
        operations.push(Operation {
          path: change.path.clone(),
          write: None,
          final_disabled: change.kind == ChangeKind::Disable,
          rename: true,
          write_local: false,
          prune_dirs: Vec::new(),
        });
      }
      ChangeKind::Retire => {}
    }
  }
  for conflict in &prepared.diff.conflicts {
    let take_remote =
      choices.get(&conflict.path).copied().unwrap_or_default() == ConflictChoice::TakeRemote;
    if take_remote {
      operations.push(Operation {
        path: conflict.path.clone(),
        write: conflict.remote.clone(),
        final_disabled: conflict.remote_disabled.unwrap_or(false),
        rename: false,
        write_local: false,
        prune_dirs: Vec::new(),
      });
    } else if prepared.restored_locals.contains_key(&conflict.path) {
      operations.push(Operation {
        path: conflict.path.clone(),
        write: conflict.remote.clone(),
        final_disabled: conflict
          .remote_disabled
          .or(conflict.local_disabled)
          .unwrap_or(false),
        rename: false,
        write_local: true,
        prune_dirs: Vec::new(),
      });
    } else if let (true, Some(outcome)) = (
      conflict.remote.is_some() && conflict.local.is_some(),
      conflict.remote_disabled,
    ) {
      // Keeping the local content does not keep the variant: the file still
      // moves to the outcome the disabled three-way settled (§5.2), so the
      // baseline entry and the disk agree. Already there, it simply stays.
      if conflict.local_disabled != Some(outcome) {
        operations.push(Operation {
          path: conflict.path.clone(),
          write: None,
          final_disabled: outcome,
          rename: true,
          write_local: false,
          prune_dirs: Vec::new(),
        });
      }
    }
  }
  operations.sort_by(|a, b| a.path.cmp(&b.path));
  operations
}

/// Finish a comparison after the remote bytes of conflicting text files were
/// fetched: merge what merges itself and give the rest its line conflicts.
pub fn finish(
  compared: Compared,
  root: &Path,
  remote: &RemoteModpack,
  remote_contents: &BTreeMap<String, Vec<u8>>,
) -> USTBLResult<Prepared> {
  let Compared {
    mut diff,
    next_baseline,
    variants,
    prune_dirs,
    restored_locals,
    restored_writes,
    ignores,
    ignores_on_disk,
    ..
  } = compared;
  let mut merged_contents = BTreeMap::new();
  for path in &restored_writes {
    if let Some(bytes) = restored_locals.get(path) {
      merged_contents.insert(path.clone(), bytes.clone());
    }
  }
  let mut conflict_sources = BTreeMap::new();
  let local_content = |path: &str| {
    restored_locals
      .get(path)
      .cloned()
      .or_else(|| std::fs::read(root.join(path)).ok())
  };
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
        let local_bytes = local_content(&conflict.path);
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
        path: conflict.path.clone(),
        kind: super::diff::ChangeKind::Update,
        remote: Some(remote_key),
        local_size: conflict.local.map(|key| key.size),
        restored: restored_locals.contains_key(&conflict.path),
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
        if let (Some(base_bytes), Some(local_bytes)) = (
          base_store::load(root, &base.sha1).ok().flatten(),
          local_content(&conflict.path),
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
  let plan_hash = plan_hash(remote, &diff, &next_baseline, &ignores);
  Ok(Prepared {
    diff,
    plan_hash,
    ignores,
    ignores_on_disk,
    merged_contents,
    conflict_sources,
    restored_locals,
    next_baseline,
    variants,
    prune_dirs,
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
        .map(|(path, content)| BaselineFile::managed(path, key(content), false))
        .collect(),
    )
  }

  fn remote(files: &[(&str, &str)]) -> RemoteModpack {
    remote_with_rules(files, Vec::new())
  }

  fn remote_with_rules(files: &[(&str, &str)], rules: Vec<RemoteRule>) -> RemoteModpack {
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
          disabled: false,
          once: false,
        })
        .collect(),
      rules,
      rules_manifest: true,
      disabled_authoritative: true,
      path_index: std::sync::Arc::new(std::sync::OnceLock::new()),
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

  fn rule(path: &str, kind: RuleKind, state: RuleState) -> RemoteRule {
    RemoteRule {
      path: path.into(),
      kind,
      state,
    }
  }

  #[tokio::test]
  async fn fresh_install_single_comparison_preserves_rules_and_whole_file_choices() {
    let root = instance(&[
      ("config/a.toml", "value = 1\n"),
      ("config/once.toml", "keep = 1\n"),
      ("cache/old", "delete"),
      ("ignored/local", "keep"),
    ]);
    let baseline = baseline(&[]);
    let mut remote = remote_with_rules(
      &[
        ("config/a.toml", "value = 2\n"),
        ("config/once.toml", "keep = 2\n"),
        ("mods/new.jar", "jar"),
      ],
      vec![
        rule("cache", RuleKind::Dir, RuleState::Delete),
        rule("ignored", RuleKind::Dir, RuleState::Ignore),
      ],
    );
    remote
      .files
      .iter_mut()
      .find(|file| file.path == "config/once.toml")
      .unwrap()
      .once = true;
    let compared = compare(&root, &baseline, &remote).await.unwrap();
    let prepared = finish(compared, &root, &remote, &BTreeMap::new()).unwrap();
    assert!(prepared
      .diff
      .conflicts
      .iter()
      .all(|conflict| conflict.baseline.is_none() && conflict.hunks.is_empty()));
    let choices = prepared
      .diff
      .conflicts
      .iter()
      .map(|conflict| (conflict.path.clone(), ConflictChoice::TakeRemote))
      .collect();
    let operations = resolve(&prepared, &choices);
    assert!(operations
      .iter()
      .any(|op| op.path == "config/a.toml" && op.write == Some(key("value = 2\n"))));
    assert!(operations
      .iter()
      .any(|op| op.path == "mods/new.jar" && op.write.is_some()));
    assert!(operations
      .iter()
      .any(|op| op.path == "cache/old" && op.write.is_none()));
    assert!(!operations
      .iter()
      .any(|op| op.path == "config/once.toml" || op.path == "ignored/local"));
    let _ = fs::remove_dir_all(root);
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

    let prepared = prepare_with_contents(&root, &baseline, &remote, &BTreeMap::new())
      .await
      .unwrap();

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

    let prepared = prepare_with_contents(&root, &baseline, &remote_v2, &BTreeMap::new())
      .await
      .unwrap();
    let plain = plan_hash(
      &remote_v2,
      &prepared.diff,
      &prepared.next_baseline,
      &Ignores::default(),
    );
    let mut with_added_ignore = Ignores::default();
    with_added_ignore.added.insert("mods/unmanaged.jar".into());
    assert_ne!(
      plain,
      plan_hash(
        &remote_v2,
        &prepared.diff,
        &prepared.next_baseline,
        &with_added_ignore
      )
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
    // Not listed in the baseline: counted as an added mod.
    fs::write(root.join("mods/mine.jar"), "mine").unwrap();
    let installed = baseline(&[
      ("mods/same.jar", "same"),
      ("mods/edited.jar", "original"),
      ("mods/off.jar", "off"),
      ("mods/gone.jar", "gone"),
    ]);

    // edited, switched off, deleted, and the mod the player added
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 4);
    fs::remove_file(root.join("mods/mine.jar")).unwrap();
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 3);
    let clean = baseline(&[
      ("mods/same.jar", "same"),
      ("mods/edited.jar", "edited-by-player"),
      ("mods/off.jar", "off"),
    ]);
    // `off.jar` still sits at the other variant than the baseline recorded.
    assert_eq!(count_local_changes(&root, &clean).await.unwrap(), 1);
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

  #[tokio::test]
  async fn ignore_rules_retire_managed_files_and_keep_dormant_ones() {
    let root = instance(&[("mods/cache.jar", "c1"), ("mods/keep.jar", "k1")]);
    let installed = baseline(&[("mods/cache.jar", "c1"), ("mods/keep.jar", "k1")]);
    let mut remote = remote(&[("mods/keep.jar", "k1")]);
    remote.rules = vec![rule("mods/cache.jar", RuleKind::File, RuleState::Ignore)];
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    assert_eq!(
      prepared
        .diff
        .changes
        .iter()
        .map(|change| (change.path.as_str(), change.kind))
        .collect::<Vec<_>>(),
      [("mods/cache.jar", ChangeKind::Retire)]
    );
    let entries: BTreeMap<_, _> = prepared
      .next_baseline
      .iter()
      .map(|entry| (entry.path.as_str(), entry))
      .collect();
    assert!(!entries["mods/cache.jar"].is_managed());
    assert_eq!(entries["mods/cache.jar"].key(), Some(key("c1")));
    assert!(entries["mods/keep.jar"].is_managed());
    // The file is still on disk afterwards; the next update keeps ignoring it.
    let mut dormant_baseline = installed.clone();
    dormant_baseline.files = prepared.next_baseline.clone();
    let again = prepare_with_contents(&root, &dormant_baseline, &remote, &BTreeMap::new())
      .await
      .unwrap();
    assert!(again.diff.changes.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn delete_rules_clear_covered_files_and_spare_the_rest() {
    let root = instance(&[
      ("mods/old/a.jar", "a1"),
      ("mods/old/b.jar.disabled", "b1"),
      ("mods/old/keep.jar", "k1"),
      ("mods/ship.jar", "s1"),
    ]);
    let installed = baseline(&[("mods/ship.jar", "s1")]);
    let mut remote = remote(&[("mods/ship.jar", "s2"), ("mods/old/keep.jar", "k1")]);
    remote.rules = vec![
      rule("mods/old", RuleKind::Dir, RuleState::Delete),
      // A shipped file and a more specific file rule beat the dir rule.
      rule("mods/old/keep.jar", RuleKind::File, RuleState::Ignore),
    ];
    // keep.jar is shipped, so the file rule never fires; ship the ignore one
    // off the files list for the coverage check.
    remote.files.retain(|file| file.path != "mods/old/keep.jar");
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    let changes: BTreeMap<_, _> = prepared
      .diff
      .changes
      .iter()
      .map(|change| (change.path.as_str(), change.kind))
      .collect();
    assert_eq!(changes["mods/old/a.jar"], ChangeKind::Delete);
    assert_eq!(changes["mods/old/b.jar"], ChangeKind::Delete);
    // keep.jar sits under an ignore rule without a baseline entry: nothing to
    // retire and it is never deleted.
    assert!(!changes.contains_key("mods/old/keep.jar"));
    assert_eq!(changes["mods/ship.jar"], ChangeKind::Update);
    // The prune map points at the rule directory.
    assert_eq!(
      prepared.prune_dirs["mods/old/a.jar"],
      vec!["mods/old".to_string()]
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn unmentioned_managed_files_are_dropped_while_dormant_ones_stay() {
    let root = instance(&[("mods/a.jar", "a1"), ("mods/edited.jar", "mine")]);
    let mut installed = baseline(&[("mods/a.jar", "a1"), ("mods/edited.jar", "e1")]);
    installed.files.push(BaselineFile::dormant(
      "mods/gone.jar",
      Some(key("g1")),
      false,
    ));
    let remote = remote(&[]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    let changes: BTreeMap<_, _> = prepared
      .diff
      .changes
      .iter()
      .map(|change| (change.path.as_str(), change.kind))
      .collect();
    assert_eq!(changes["mods/a.jar"], ChangeKind::Delete);
    assert!(!changes.contains_key("mods/gone.jar"));
    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert_eq!(
      prepared.diff.conflicts[0].kind,
      ConflictKind::ModifiedLocallyRemovedRemotely
    );
    assert_eq!(prepared.diff.conflicts[0].path, "mods/edited.jar");
    // The dormant entry survives untouched.
    assert!(prepared
      .next_baseline
      .iter()
      .any(|entry| entry.path == "mods/gone.jar" && !entry.is_managed()));
    assert!(prepared
      .next_baseline
      .iter()
      .all(|entry| entry.path != "mods/a.jar" && entry.path != "mods/edited.jar"));
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn both_variants_are_reported_and_block_the_involved_paths() {
    let root = std::env::temp_dir().join(format!("ustbl-plan-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/a.jar"), "a1").unwrap();
    fs::write(root.join("mods/a.jar.disabled"), "a1-off").unwrap();
    fs::write(root.join("mods/free.jar"), "f1").unwrap();
    fs::write(root.join("mods/free.jar.disabled"), "f1-off").unwrap();
    let installed = baseline(&[("mods/a.jar", "a1")]);
    let mut remote = remote(&[("mods/a.jar", "a2")]);
    remote.rules = vec![rule("mods/free.jar", RuleKind::File, RuleState::Delete)];
    let compared = compare(&root, &installed, &remote).await.unwrap();
    // The shipped path with both variants is refused; the delete-rule one is
    // still cleared.
    assert_eq!(compared.both_variants, ["mods/a.jar".to_string()]);
    assert!(compared
      .diff
      .changes
      .iter()
      .any(|change| change.path == "mods/free.jar" && change.kind == ChangeKind::Delete));
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_dormant_file_managed_again_conflicts_or_updates_by_content() {
    let root = instance(&[("mods/a.jar", "mine"), ("mods/b.jar", "b1")]);
    let mut installed = baseline(&[]);
    installed.files = vec![
      BaselineFile::dormant("mods/a.jar", Some(key("a1")), false),
      BaselineFile::dormant("mods/b.jar", Some(key("b1")), false),
    ];
    let remote = remote(&[("mods/a.jar", "a2"), ("mods/b.jar", "b2")]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    // a.jar differs from both recorded and shipped: remanage conflict.
    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert_eq!(prepared.diff.conflicts[0].kind, ConflictKind::Remanage);
    assert_eq!(prepared.diff.conflicts[0].path, "mods/a.jar");
    // b.jar kept the recorded content: updated without asking.
    assert_eq!(prepared.diff.changes.len(), 1);
    assert_eq!(prepared.diff.changes[0].path, "mods/b.jar");
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Update);
    // Both become managed entries of the new version.
    for entry in &prepared.next_baseline {
      assert!(entry.is_managed());
      assert_eq!(
        entry.key(),
        Some(key(match entry.path.as_str() {
          "mods/a.jar" => "a2",
          "mods/b.jar" => "b2",
          other => panic!("{other}"),
        }))
      );
    }
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn first_install_skips_ignored_files_and_marks_once_ones_dormant() {
    let root = instance(&[]);
    let installed = baseline(&[]);
    let mut remote = remote(&[
      ("mods/a.jar", "a1"),
      ("mods/skip.jar", "s1"),
      ("config/first.toml", "f1"),
    ]);
    remote.files[1].disabled = true;
    remote.files[2].once = true;
    remote.rules = vec![rule("mods/skip.jar", RuleKind::File, RuleState::Ignore)];
    // A file rule may not overlap a shipped file; drop skip.jar from files.
    remote.files.retain(|file| file.path != "mods/skip.jar");
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    let changes: BTreeMap<_, _> = prepared
      .diff
      .changes
      .iter()
      .map(|change| (change.path.as_str(), change.kind))
      .collect();
    assert_eq!(changes["mods/a.jar"], ChangeKind::Add);
    assert_eq!(changes["config/first.toml"], ChangeKind::Add);
    assert!(!changes.contains_key("mods/skip.jar"));
    let entries: BTreeMap<_, _> = prepared
      .next_baseline
      .iter()
      .map(|entry| (entry.path.as_str(), entry))
      .collect();
    assert!(entries["mods/a.jar"].is_managed());
    // once: written by the install, recorded dormant.
    let once = entries["config/first.toml"];
    assert!(!once.is_managed());
    assert_eq!(once.key(), Some(key("f1")));
    // The ignored file never enters the baseline.
    assert!(!entries.contains_key("mods/skip.jar"));
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_disabled_ship_writes_the_disabled_variant_and_records_it() {
    let root = instance(&[]);
    let installed = baseline(&[]);
    let mut remote = remote(&[("mods/a.jar", "a1")]);
    remote.files[0].disabled = true;
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    assert_eq!(prepared.variants["mods/a.jar"], true);
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "mods/a.jar")
      .unwrap();
    assert!(entry.disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_disabled_flip_is_a_change_without_a_content_write() {
    let root = instance(&[("mods/a.jar", "a1")]);
    let installed = baseline(&[("mods/a.jar", "a1")]);
    // The new version ships the same content but disabled.
    let mut remote = remote(&[("mods/a.jar", "a1")]);
    remote.files[0].disabled = true;
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();
    assert_eq!(prepared.diff.changes.len(), 1);
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Disable);
    assert_eq!(prepared.diff.changes[0].remote, None);
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "mods/a.jar")
      .unwrap();
    assert!(entry.disabled);
    let _ = fs::remove_dir_all(root);
  }

  /// A Modrinth/CurseForge archive: not a `/3` manifest, but its `disabled`
  /// flags are read from `.disabled` files and count.
  fn platform_remote(files: &[(&str, &str)], disabled: &[&str]) -> RemoteModpack {
    let mut remote = remote(files);
    remote.rules_manifest = false;
    for file in &mut remote.files {
      file.disabled = disabled.contains(&file.path.as_str());
    }
    remote.validate("pack").unwrap();
    remote
  }

  fn entry<'a>(prepared: &'a Prepared, path: &str) -> &'a BaselineFile {
    prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == path)
      .unwrap()
  }

  #[tokio::test]
  async fn a_platform_pack_installs_a_disabled_mod_at_the_disabled_name() {
    let root = instance(&[]);
    let installed = baseline(&[]);
    let remote = platform_remote(
      &[("mods/a.jar", "a1"), ("mods/b.jar", "b1")],
      &["mods/a.jar"],
    );
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert_eq!(prepared.variants["mods/a.jar"], true);
    assert_eq!(prepared.variants["mods/b.jar"], false);
    assert!(entry(&prepared, "mods/a.jar").disabled);
    assert!(!entry(&prepared, "mods/b.jar").disabled);
    let operations = resolve(&prepared, &BTreeMap::new());
    let operation = |path: &str| operations.iter().find(|op| op.path == path).unwrap();
    assert_eq!(operation("mods/a.jar").write, Some(key("a1")));
    assert!(operation("mods/a.jar").final_disabled);
    assert!(!operation("mods/b.jar").final_disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_platform_update_enables_a_mod_the_player_left_alone() {
    let root = instance(&[("mods/a.jar.disabled", "a1")]);
    let installed = vec![BaselineFile::managed("mods/a.jar", key("a1"), true)];
    let mut installed_pack = baseline(&[]);
    installed_pack.files = installed;
    let remote = platform_remote(&[("mods/a.jar", "a2")], &[]);
    let prepared = prepare_with_contents(&root, &installed_pack, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert_eq!(prepared.variants["mods/a.jar"], false);
    assert!(!entry(&prepared, "mods/a.jar").disabled);
    let operations = resolve(&prepared, &BTreeMap::new());
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].write, Some(key("a2")));
    assert!(!operations[0].final_disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_platform_update_keeps_the_state_the_player_chose() {
    // (file the player has, disabled in the pack's old version, disabled in the new one)
    for (disk, was_disabled, now_disabled) in [
      ("mods/a.jar", true, true),
      ("mods/a.jar", true, false),
      ("mods/a.jar.disabled", false, false),
      ("mods/a.jar.disabled", false, true),
    ] {
      let root = instance(&[(disk, "a1")]);
      let mut installed = baseline(&[]);
      installed.files = vec![BaselineFile::managed("mods/a.jar", key("a1"), was_disabled)];
      let new_disabled: &[&str] = if now_disabled { &["mods/a.jar"] } else { &[] };
      let remote = platform_remote(&[("mods/a.jar", "a2")], new_disabled);
      let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
        .await
        .unwrap();

      let player_disabled = disk.ends_with(".disabled");
      assert_eq!(prepared.variants["mods/a.jar"], player_disabled, "{disk}");
      let operations = resolve(&prepared, &BTreeMap::new());
      assert_eq!(operations.len(), 1);
      assert_eq!(operations[0].write, Some(key("a2")));
      assert_eq!(operations[0].final_disabled, player_disabled, "{disk}");
      let _ = fs::remove_dir_all(root);
    }
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

  /// The disabled state is settled however the content conflict is answered:
  /// keeping the local content still renames the file to the outcome variant,
  /// and the baseline records it (§5.2).
  async fn prepare_content_conflict(root: &Path) -> Prepared {
    let installed = baseline(&[("mods/a.jar", "a1")]);
    let mut remote = remote(&[("mods/a.jar", "a2")]);
    remote.files[0].disabled = true;
    prepare_with_contents(root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap()
  }

  fn content_conflict(prepared: &Prepared) -> &Conflict {
    assert_eq!(prepared.diff.conflicts.len(), 1);
    let conflict = &prepared.diff.conflicts[0];
    assert_eq!(conflict.path, "mods/a.jar");
    assert_eq!(conflict.kind, ConflictKind::ModifiedLocallyChangedRemotely);
    assert_eq!(conflict.remote_disabled, Some(true));
    conflict
  }

  #[tokio::test]
  async fn a_kept_local_conflict_still_settles_the_disabled_variant() {
    let root = instance(&[("mods/a.jar", "a1-edited")]);
    let prepared = prepare_content_conflict(&root).await;
    content_conflict(&prepared);
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "mods/a.jar")
      .unwrap();
    assert!(entry.is_managed() && entry.disabled);

    for choices in [
      BTreeMap::new(),
      BTreeMap::from([("mods/a.jar".to_string(), ConflictChoice::KeepLocal)]),
    ] {
      let operations = resolve(&prepared, &choices);
      assert_eq!(operations.len(), 1);
      assert!(operations[0].rename);
      assert_eq!(operations[0].write, None);
      assert!(operations[0].final_disabled);
    }
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_taken_remote_conflict_writes_the_disabled_variant() {
    let root = instance(&[("mods/a.jar", "a1-edited")]);
    let prepared = prepare_content_conflict(&root).await;
    content_conflict(&prepared);
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "mods/a.jar")
      .unwrap();
    assert!(entry.is_managed() && entry.disabled);

    let choices = BTreeMap::from([("mods/a.jar".to_string(), ConflictChoice::TakeRemote)]);
    let operations = resolve(&prepared, &choices);
    assert_eq!(operations.len(), 1);
    assert!(!operations[0].rename);
    assert_eq!(operations[0].write, Some(key("a2")));
    assert!(operations[0].final_disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_remanage_conflict_settles_the_disabled_variant_too() {
    let root = instance(&[("mods/a.jar.disabled", "mine")]);
    let mut installed = baseline(&[]);
    installed.files = vec![BaselineFile::dormant("mods/a.jar", Some(key("a1")), true)];
    let mut remote = remote(&[("mods/a.jar", "a2")]);
    remote.files[0].disabled = false;
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert_eq!(prepared.diff.conflicts.len(), 1);
    let conflict = &prepared.diff.conflicts[0];
    assert_eq!(conflict.kind, ConflictKind::Remanage);
    // The player kept the recorded state, so the remote's enabled one wins.
    assert_eq!(conflict.remote_disabled, Some(false));
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "mods/a.jar")
      .unwrap();
    assert!(entry.is_managed() && !entry.disabled);

    let operations = resolve(&prepared, &BTreeMap::new());
    assert_eq!(operations.len(), 1);
    assert!(operations[0].rename);
    assert!(!operations[0].final_disabled);
    let taken = resolve(
      &prepared,
      &BTreeMap::from([("mods/a.jar".to_string(), ConflictChoice::TakeRemote)]),
    );
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].write, Some(key("a2")));
    assert!(!taken[0].final_disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_file_in_the_way_keeps_its_variant_and_records_the_remotes_opinion() {
    // a.jar sits on disk but the modpack never recorded it; the new version
    // ships it, disabled. Never recorded means the local variant wins for the
    // file itself, while the baseline records the remote's opinion (§5.2).
    let root = instance(&[("mods/a.jar", "mine"), ("mods/b.jar", "b1")]);
    let installed = baseline(&[("mods/b.jar", "b1")]);
    let mut remote = remote(&[("mods/a.jar", "a2"), ("mods/b.jar", "b1")]);
    remote.files[0].disabled = true;
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert_eq!(prepared.diff.conflicts.len(), 1);
    let conflict = &prepared.diff.conflicts[0];
    assert_eq!(conflict.path, "mods/a.jar");
    assert_eq!(conflict.kind, ConflictKind::UnmanagedFileInTheWay);
    assert_eq!(conflict.remote_disabled, Some(false));
    assert_eq!(conflict.local_disabled, Some(false));
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "mods/a.jar")
      .unwrap();
    assert!(entry.is_managed() && entry.disabled);

    // The local file already sits at the outcome: keeping it is a plain keep.
    assert!(resolve(&prepared, &BTreeMap::new()).is_empty());
    // Taking the remote writes the new file there too.
    let taken = resolve(
      &prepared,
      &BTreeMap::from([("mods/a.jar".to_string(), ConflictChoice::TakeRemote)]),
    );
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].write, Some(key("a2")));
    assert!(!taken[0].final_disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn an_enumerated_path_casefold_matching_the_manifest_keeps_its_spelling() {
    // The installed pack recorded `config/foo.toml`; the disk has it as
    // `config/Foo.toml`. The new version does not ship the file anymore but
    // deletes everything under `config`: the enumeration must find the file
    // once, under the manifest's spelling, and never delete it twice.
    let root = instance(&[("config/Foo.toml", "c1"), ("config/other.toml", "o1")]);
    let installed = baseline(&[("config/foo.toml", "c1")]);
    let mut remote = remote(&[]);
    remote.rules = vec![rule("config", RuleKind::Dir, RuleState::Delete)];
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    let changes: BTreeMap<_, _> = prepared
      .diff
      .changes
      .iter()
      .map(|change| (change.path.as_str(), change.kind))
      .collect();
    assert_eq!(changes["config/foo.toml"], ChangeKind::Delete);
    assert_eq!(changes["config/other.toml"], ChangeKind::Delete);
    assert!(!changes.contains_key("config/Foo.toml"));
    assert!(prepared.diff.conflicts.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_dormant_file_differing_only_in_line_endings_updates_without_asking() {
    let root = instance(&[("config/a.properties", "a=1\r\n")]);
    let mut installed = baseline(&[]);
    installed.files = vec![BaselineFile::dormant(
      "config/a.properties",
      Some(key("a=1\n")),
      false,
    )];
    store_base(&root, &installed, "a=1\n");
    let remote = remote(&[("config/a.properties", "a=2\n")]);

    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    // ≡B after normalization: the file updates to the new content instead of
    // asking to be managed again (§5.1).
    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes.len(), 1);
    assert_eq!(prepared.diff.changes[0].path, "config/a.properties");
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Update);
    let entry = prepared
      .next_baseline
      .iter()
      .find(|entry| entry.path == "config/a.properties")
      .unwrap();
    assert!(entry.is_managed());
    let _ = fs::remove_dir_all(root);
  }

  const TEN: &str = "a=1\nb=2\nc=3\nd=4\ne=5\nf=6\ng=7\nh=8\ni=9\nj=10\n";

  fn ignore_range(root: &Path, installed: &ModpackBaseline, start: usize, end: usize) {
    let sha1 = installed.files[0].sha1.clone();
    ignores::add(
      root,
      installed,
      &installed.files[0].path,
      &sha1,
      Some(ignores::Region { start, end }),
    )
    .unwrap();
  }

  #[tokio::test]
  async fn ignored_changes_leave_the_local_change_list_and_count() {
    let local = TEN.replace("b=2", "b=20").replace("i=9", "i=90");
    let root = instance(&[("config/a.toml", &local)]);
    let installed = baseline(&[("config/a.toml", TEN)]);
    store_base(&root, &installed, TEN);
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 1);

    ignore_range(&root, &installed, 1, 2);
    let changes = local_changes(&root, &installed, true).await.unwrap();
    assert_eq!(changes.len(), 1);
    let hunks = changes[0].hunks.as_ref().unwrap();
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].edits.len(), 1);
    assert_eq!((hunks[0].edits[0].start, hunks[0].edits[0].end), (8, 9));

    ignore_range(&root, &installed, 8, 9);
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 0);
    assert!(local_changes(&root, &installed, true).await.unwrap().is_empty());

    ignores::undo(&root, &installed, "config/a.toml", Some(ignores::Region { start: 8, end: 9 }))
      .unwrap();
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 1);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_whole_file_ignore_hides_the_file_and_an_edited_record_is_dropped() {
    let root = instance(&[("config/a.toml", "x=2\n"), ("mods/a.jar", "edited")]);
    let installed = baseline(&[("config/a.toml", "x=1\n"), ("mods/a.jar", "orig")]);
    for file in &installed.files {
      ignores::add(&root, &installed, &file.path, &file.sha1, None).unwrap();
    }
    assert_eq!(count_local_changes(&root, &installed).await.unwrap(), 0);
    // A baseline whose installed content moved no longer matches the record.
    let moved = baseline(&[("config/a.toml", "x=9\n"), ("mods/a.jar", "orig")]);
    assert_eq!(count_local_changes(&root, &moved).await.unwrap(), 1);
    // A range of a file without a kept text cannot be recorded.
    let sha1 = installed.files[1].sha1.clone();
    assert_eq!(
      ignores::add(&root, &installed, "mods/a.jar", &sha1, Some(ignores::Region { start: 0, end: 1 })),
      Err(ignores::IgnoreError::Mismatch)
    );
    let _ = fs::remove_dir_all(root);
  }

  fn ignore_whole(root: &Path, installed: &ModpackBaseline) {
    let file = &installed.files[0];
    ignores::add(root, installed, &file.path, &file.sha1, None).unwrap();
  }

  fn planned_change<'a>(prepared: &'a Prepared, path: &str) -> &'a FileChange {
    prepared
      .diff
      .changes
      .iter()
      .chain(&prepared.diff.auto_merged)
      .find(|change| change.path == path)
      .unwrap()
  }

  #[tokio::test]
  async fn a_whole_file_ignore_restores_changed_and_unchanged_remote_files() {
    for remote_text in ["a=3\n", "a=1\n"] {
      let root = instance(&[("config/a.toml", "a=2\n")]);
      let installed = baseline(&[("config/a.toml", "a=1\n")]);
      ignore_whole(&root, &installed);
      let remote = remote(&[("config/a.toml", remote_text)]);
      let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
        .await
        .unwrap();

      assert!(prepared.diff.conflicts.is_empty());
      let change = planned_change(&prepared, "config/a.toml");
      assert_eq!(change.kind, ChangeKind::Update);
      assert_eq!(change.remote, Some(key(remote_text)));
      assert!(change.restored);
      assert!(!prepared.merged_contents.contains_key("config/a.toml"));
      assert!(ignores::plan_update(&root, &prepared, &remote)
        .unwrap()
        .to
        .files
        .is_empty());
      let _ = fs::remove_dir_all(root);
    }
  }

  #[tokio::test]
  async fn a_whole_file_ignore_reinstalls_a_deleted_file() {
    let root = instance(&[]);
    let installed = baseline(&[("mods/a.jar", "a1")]);
    ignore_whole(&root, &installed);
    let remote = remote(&[("mods/a.jar", "a1")]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert!(prepared.diff.conflicts.is_empty());
    let change = planned_change(&prepared, "mods/a.jar");
    assert_eq!(change.kind, ChangeKind::Update);
    assert!(change.restored);
    assert_eq!(
      resolve(&prepared, &BTreeMap::new())[0].write,
      Some(key("a1"))
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_whole_file_ignore_restores_the_manifest_disabled_state() {
    let root = instance(&[("mods/a.jar.disabled", "a1")]);
    let installed = baseline(&[("mods/a.jar", "a1")]);
    ignore_whole(&root, &installed);
    let remote = remote(&[("mods/a.jar", "a1")]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    let change = planned_change(&prepared, "mods/a.jar");
    assert_eq!(change.kind, ChangeKind::Enable);
    assert!(change.restored);
    let operations = resolve(&prepared, &BTreeMap::new());
    assert_eq!(operations.len(), 1);
    assert!(operations[0].rename && !operations[0].final_disabled);
    assert!(!prepared.next_baseline[0].disabled);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_whole_file_ignore_still_deletes_or_retires_with_the_manifest() {
    let root = instance(&[("config/a.toml", "a=2\n")]);
    let installed = baseline(&[("config/a.toml", "a=1\n")]);
    ignore_whole(&root, &installed);
    let removed = remote(&[]);
    let prepared = prepare_with_contents(&root, &installed, &removed, &BTreeMap::new())
      .await
      .unwrap();
    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Delete);
    assert!(!prepared.diff.changes[0].restored);
    assert!(ignores::plan_update(&root, &prepared, &removed)
      .unwrap()
      .to
      .is_empty());
    let _ = fs::remove_dir_all(&root);

    for once in [false, true] {
      let root = instance(&[("config/a.toml", "a=2\n")]);
      let installed = baseline(&[("config/a.toml", "a=1\n")]);
      ignore_whole(&root, &installed);
      let remote = if once {
        let mut remote = remote(&[("config/a.toml", "a=3\n")]);
        remote.files[0].once = true;
        remote
      } else {
        remote_with_rules(
          &[],
          vec![rule("config/a.toml", RuleKind::File, RuleState::Ignore)],
        )
      };
      let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
        .await
        .unwrap();
      assert!(prepared.diff.conflicts.is_empty());
      assert_eq!(prepared.diff.changes.len(), 1);
      assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Retire);
      assert!(resolve(&prepared, &BTreeMap::new()).is_empty());
      assert!(ignores::plan_update(&root, &prepared, &remote)
        .unwrap()
        .to
        .files
        .is_empty());
      let _ = fs::remove_dir_all(root);
    }
  }

  #[tokio::test]
  async fn ignored_spots_restore_before_deciding_and_change_the_plan_hash() {
    let local = TEN.replace("b=2", "b=20");
    let remote_text = TEN.replace("b=2", "b=22");
    let root = instance(&[("config/a.toml", &local)]);
    let installed = baseline(&[("config/a.toml", TEN)]);
    store_base(&root, &installed, TEN);
    let remote = remote(&[("config/a.toml", &remote_text)]);
    let contents = BTreeMap::from([(
      "config/a.toml".to_string(),
      remote_text.clone().into_bytes(),
    )]);
    let plain = prepare_with_contents(&root, &installed, &remote, &contents)
      .await
      .unwrap();
    assert_eq!(plain.diff.conflicts.len(), 1);

    ignore_range(&root, &installed, 1, 2);
    let restored = prepare_with_contents(&root, &installed, &remote, &contents)
      .await
      .unwrap();
    assert!(restored.diff.conflicts.is_empty());
    let change = planned_change(&restored, "config/a.toml");
    assert_eq!(change.remote, Some(key(&remote_text)));
    assert!(change.restored);
    assert!(!restored.merged_contents.contains_key("config/a.toml"));
    assert_ne!(plain.plan_hash, restored.plan_hash);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn ignored_spots_restore_only_their_lines_before_an_auto_merge() {
    let local = TEN.replace("b=2", "b=20").replace("d=4", "d=40");
    let remote_text = TEN.replace("i=9", "i=99");
    let root = instance(&[("config/a.toml", &local)]);
    let installed = baseline(&[("config/a.toml", TEN)]);
    store_base(&root, &installed, TEN);
    ignore_range(&root, &installed, 1, 2);
    let remote = remote(&[("config/a.toml", &remote_text)]);
    let contents = BTreeMap::from([(
      "config/a.toml".to_string(),
      remote_text.clone().into_bytes(),
    )]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &contents)
      .await
      .unwrap();

    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.auto_merged.len(), 1);
    assert!(prepared.diff.auto_merged[0].restored);
    let merged = String::from_utf8(prepared.merged_contents["config/a.toml"].clone()).unwrap();
    assert!(merged.contains("b=2\n") && !merged.contains("b=20"));
    assert!(merged.contains("d=40") && merged.contains("i=99"));
    assert!(ignores::plan_update(&root, &prepared, &remote)
      .unwrap()
      .to
      .files
      .is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn ignored_spots_write_lprime_with_local_line_endings_when_remote_is_unchanged() {
    let local = TEN
      .replace("b=2", "b=20")
      .replace("d=4", "d=40")
      .replace('\n', "\r\n");
    let root = instance(&[("config/a.toml", &local)]);
    let installed = baseline(&[("config/a.toml", TEN)]);
    store_base(&root, &installed, TEN);
    ignore_range(&root, &installed, 1, 2);
    let remote = remote(&[("config/a.toml", TEN)]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert!(prepared.diff.conflicts.is_empty());
    let change = planned_change(&prepared, "config/a.toml");
    assert_eq!(change.kind, ChangeKind::Update);
    assert_eq!(change.remote, Some(key(TEN)));
    assert!(change.restored);
    let expected = TEN
      .replace("d=4", "d=40")
      .replace('\n', "\r\n")
      .into_bytes();
    assert_eq!(prepared.merged_contents["config/a.toml"], expected);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn ignored_spots_leave_unignored_conflicts_and_removed_file_choices() {
    let local = TEN.replace("b=2", "b=20").replace("d=4", "d=40");
    let remote_text = TEN.replace("d=4", "d=44");
    let root = instance(&[("config/a.toml", &local)]);
    let installed = baseline(&[("config/a.toml", TEN)]);
    store_base(&root, &installed, TEN);
    ignore_range(&root, &installed, 1, 2);
    let changed = remote(&[("config/a.toml", &remote_text)]);
    let contents = BTreeMap::from([("config/a.toml".to_string(), remote_text.into_bytes())]);
    let prepared = prepare_with_contents(&root, &installed, &changed, &contents)
      .await
      .unwrap();

    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert!(!prepared.diff.conflicts[0].hunks.is_empty());
    let local_source = &prepared.conflict_sources["config/a.toml"].1;
    let local_source = String::from_utf8(local_source.clone()).unwrap();
    assert!(local_source.contains("b=2\n") && !local_source.contains("b=20"));
    assert!(local_source.contains("d=40"));
    let mut choices = BTreeMap::new();
    let merged =
      crate::instance::modpack_update::commands::selected_hunk_contents(&prepared, &mut choices)
        .unwrap();
    let merged = String::from_utf8(merged["config/a.toml"].clone()).unwrap();
    assert!(merged.contains("b=2\n") && !merged.contains("b=20"));
    assert!(merged.contains("d=40"));
    let _ = fs::remove_dir_all(&root);

    let root = instance(&[("config/a.toml", &local)]);
    store_base(&root, &installed, TEN);
    ignore_range(&root, &installed, 1, 2);
    let removed = remote(&[]);
    let prepared = prepare_with_contents(&root, &installed, &removed, &BTreeMap::new())
      .await
      .unwrap();
    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert_eq!(
      prepared.diff.conflicts[0].kind,
      ConflictKind::ModifiedLocallyRemovedRemotely
    );
    let operations = resolve(&prepared, &BTreeMap::new());
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].write, None);
    assert!(operations[0].write_local);
    let mut choices = BTreeMap::new();
    let contents =
      crate::instance::modpack_update::commands::selected_hunk_contents(&prepared, &mut choices)
        .unwrap();
    let restored = String::from_utf8(contents["config/a.toml"].clone()).unwrap();
    assert!(restored.contains("b=2\n") && !restored.contains("b=20"));
    assert!(restored.contains("d=40"));
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn ignored_spots_equivalent_to_base_are_deleted_when_remote_drops_the_file() {
    let local = TEN.replace("b=2", "b=20");
    let root = instance(&[("config/a.toml", &local)]);
    let installed = baseline(&[("config/a.toml", TEN)]);
    store_base(&root, &installed, TEN);
    ignore_range(&root, &installed, 1, 2);
    let remote = remote(&[]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes.len(), 1);
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Delete);
    assert!(!prepared.diff.changes[0].restored);
    let update = ignores::plan_update(&root, &prepared, &remote).unwrap();
    assert!(update.from.files.contains_key("config/a.toml"));
    assert!(update.to.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn ignored_added_files_are_replaced_only_when_the_new_content_differs() {
    let root = instance(&[
      ("mods/mine.jar", "mine"),
      ("mods/same.jar", "same"),
      ("mods/other.jar", "other"),
    ]);
    let installed = baseline(&[]);
    let current = crate::instance::modpack_update::release::Release::from_baseline(&installed, "1");
    for path in ["mods/mine.jar", "mods/same.jar", "mods/other.jar"] {
      ignores::add_added(&root, &installed, &current, path).unwrap();
    }
    let remote = remote(&[("mods/mine.jar", "new"), ("mods/same.jar", "same")]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes.len(), 1);
    let change = planned_change(&prepared, "mods/mine.jar");
    assert_eq!(change.kind, ChangeKind::Add);
    assert_eq!(change.remote, Some(key("new")));
    assert!(change.restored);
    let update = ignores::plan_update(&root, &prepared, &remote).unwrap();
    assert_eq!(update.to.files.len(), 0);
    assert_eq!(
      update.to.added.into_iter().collect::<Vec<_>>(),
      vec!["mods/other.jar".to_string()]
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn an_ignored_added_file_follows_a_delete_rule_and_its_record_is_consumed() {
    let root = instance(&[("mods/mine.jar", "mine")]);
    let installed = baseline(&[]);
    let current = crate::instance::modpack_update::release::Release::from_baseline(&installed, "1");
    ignores::add_added(&root, &installed, &current, "mods/mine.jar").unwrap();
    let remote = remote_with_rules(&[], vec![rule("mods", RuleKind::Dir, RuleState::Delete)]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.changes.len(), 1);
    assert_eq!(prepared.diff.changes[0].kind, ChangeKind::Delete);
    assert!(!prepared.diff.changes[0].restored);
    assert!(ignores::plan_update(&root, &prepared, &remote)
      .unwrap()
      .to
      .is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn an_ignored_added_file_is_otherwise_left_alone_with_its_record() {
    let root = instance(&[("mods/mine.jar", "mine")]);
    let installed = baseline(&[]);
    let current = crate::instance::modpack_update::release::Release::from_baseline(&installed, "1");
    ignores::add_added(&root, &installed, &current, "mods/mine.jar").unwrap();
    let remote = remote(&[]);
    let prepared = prepare_with_contents(&root, &installed, &remote, &BTreeMap::new())
      .await
      .unwrap();

    assert!(prepared.diff.is_empty());
    assert!(ignores::plan_update(&root, &prepared, &remote).is_none());
    assert!(root.join("mods/mine.jar").is_file());
    let _ = fs::remove_dir_all(root);
  }
}
