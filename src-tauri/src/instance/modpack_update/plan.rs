//! Comparing an instance with the version it is being updated to.

use super::decide::{
  decide, Action, Base, DecisionConflict, DecisionInput, Local, NextBaseline, Policy,
};
use super::diff::{
  collect_local_states, ChangeKind, Conflict, ConflictChoice, ConflictKind, Diff, FileChange,
  FileKey, LocalState, Operation, DISABLED_SUFFIX,
};
use super::local_scan::{collect_states_cached, record_states, LocalScanCache};
use super::merge::{merge_text, text_conflicts, TextMergeError};
use super::remote::{RemoteModpack, RemoteRule, RuleKind, RuleState};
use super::text_diff::{unified_diff, DiffHunk, CONTEXT_LINES};
use crate::error::{USTBLError, USTBLResult};
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
  /// The baseline entries the update records when it commits.
  pub next_baseline: Vec<BaselineFile>,
  /// The variant each written path ends up at (`true` = `.disabled`).
  pub variants: BTreeMap<String, bool>,
  /// Delete-rule directories pruned after a deletion, by deleted path.
  pub prune_dirs: BTreeMap<String, Vec<String>>,
}

/// The pieces of one comparison, so the conflict setup can finish the work
/// without hashing the local files a second time.
pub struct Compared {
  pub diff: Diff,
  /// The baseline entries the update records when it commits.
  pub next_baseline: Vec<BaselineFile>,
  /// The variant each written path ends up at (`true` = `.disabled`).
  pub variants: BTreeMap<String, bool>,
  /// Paths with both variants on disk that the remote or the baseline
  /// involves. Planning refuses these outright.
  pub both_variants: Vec<String>,
  /// Delete-rule directories pruned after a deletion, by deleted path.
  pub prune_dirs: BTreeMap<String, Vec<String>>,
}

/// The baseline entries by path, managed and dormant alike.
fn baseline_map(baseline: &ModpackBaseline) -> BTreeMap<String, BaselineFile> {
  baseline
    .files
    .iter()
    .map(|file| (file.path.clone(), file.clone()))
    .collect()
}

pub fn plan_hash(remote: &RemoteModpack, diff: &Diff, next_baseline: &[BaselineFile]) -> String {
  let mut hasher = Sha1::new();
  hasher.update(remote.version_id.as_bytes());
  hasher.update(b"\0");
  hasher.update(remote.content_hash().as_bytes());
  hasher.update(b"\0");
  hasher.update(serde_json::to_vec(diff).unwrap_or_default());
  hasher.update(b"\0");
  hasher.update(serde_json::to_vec(next_baseline).unwrap_or_default());
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

fn diff_lines(text: &str) -> Vec<String> {
  text.lines().map(str::to_owned).collect()
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
  let keys: BTreeMap<String, FileKey> = entries
    .iter()
    .filter(|(_, file)| file.is_managed())
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
      let local_bytes = std::fs::read(root.join(&local_path))?;
      let local_text = text::normalize_text(&path, &local_bytes);
      let base_text = match &local_text {
        Some(_) => base_store::load(&root, &key.sha1)
          .ok()
          .flatten()
          .and_then(|base| text::normalize_text(&path, &base)),
        None => None,
      };
      let (hunks, no_diff) = match (base_text, local_text) {
        (Some(base), Some(local)) if base == local && !toggled => continue,
        (Some(base), Some(local)) if base == local && toggled => {
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
        (Some(base), Some(local)) => (
          with_diff.then(|| unified_diff(&diff_lines(&base), &diff_lines(&local), CONTEXT_LINES)),
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
/// see: every regular file below it, as logical paths (the `.disabled`
/// suffix folds back to the enabled name).
fn enumerate_under(root: &Path, dir: &str, found: &mut BTreeSet<String>) -> USTBLResult<()> {
  let base = root.join(dir);
  if !base.is_dir() {
    return Ok(());
  }
  let mut stack = vec![base];
  while let Some(current) = stack.pop() {
    for entry in std::fs::read_dir(&current)? {
      let entry = entry?;
      let file_type = entry.file_type()?;
      let relative = entry
        .path()
        .strip_prefix(root)
        .map_err(|_| USTBLError("delete rule escaped the instance".into()))?
        .to_string_lossy()
        .replace('\\', "/");
      if file_type.is_dir() {
        stack.push(entry.path());
      } else if file_type.is_file() {
        found.insert(
          relative
            .strip_suffix(DISABLED_SUFFIX)
            .unwrap_or(&relative)
            .to_string(),
        );
      }
      // Anything else (symlinks, sockets) is not a managed file.
    }
  }
  Ok(())
}

/// The candidate paths the decision runs over: the baseline, the shipped
/// files, the `file` rules, and everything a delete `dir` rule covers.
/// `rule_dirs` collects the delete `dir` rules for the prune map.
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
  let mut found = BTreeSet::new();
  for rule in &delete_dirs {
    enumerate_under(root, &rule.path, &mut found)?;
  }
  for path in found {
    if matches!(remote.policy_of(&path), Policy::Delete) {
      // Only this rule family may delete it; keep the exact rule for pruning.
      paths.insert(path);
    }
  }
  Ok((paths, delete_dirs))
}

/// How `compare` decides one path: the baseline entry, the local state and
/// the remote's policy become a [`DecisionInput`]; the effectively-unchanged
/// verdict stays a caller-provided closure U3 will feed with local ignores.
pub struct DecisionContext<'a> {
  pub remote: &'a RemoteModpack,
  /// Whether the local content of `path` counts as unchanged although its
  /// key differs. Always `false` on this branch.
  pub effectively_unchanged: &'a dyn Fn(&str) -> bool,
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
  delete_dirs: &[RemoteRule],
  diff: &mut Diff,
  next_baseline: &mut BTreeMap<String, BaselineFile>,
  variants: &mut BTreeMap<String, bool>,
  prune_dirs: &mut BTreeMap<String, Vec<String>>,
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
  let decision = decide(
    path,
    &DecisionInput {
      policy,
      remote: ship.as_ref(),
      base,
      local: local_decision,
      effectively_unchanged: (context.effectively_unchanged)(path),
    },
  );
  let local_key = local.key().cloned();
  let local_size = local_key.as_ref().map(|key| key.size);
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
      });
      variants.insert(path.to_string(), variant.disabled());
    }
    Action::Rename { variant, .. } => {
      diff.changes.push(FileChange {
        path: path.to_string(),
        kind: match variant {
          super::decide::Variant::Disabled => ChangeKind::Disable,
          super::decide::Variant::Enabled => ChangeKind::Enable,
        },
        remote: None,
        local_size,
      });
    }
    Action::Delete => {
      diff.changes.push(FileChange {
        path: path.to_string(),
        kind: ChangeKind::Delete,
        remote: None,
        local_size,
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
      });
    }
    Action::Nothing => {}
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
    diff.conflicts.push(Conflict {
      path: path.to_string(),
      kind,
      baseline: baseline_key,
      local: local_key,
      remote: ship.as_ref().map(|ship| ship.key.clone()),
      hunks: Vec::new(),
    });
    // A conflict answered "take remote" writes the new file at the variant
    // the disabled three-way settled.
    if let Some(ship) = &ship {
      let outcome = super::decide::disabled_outcome(
        ship.disabled,
        baseline.and_then(|entry| entry.key().map(|_| entry.disabled)),
        local.variant_disabled(),
      );
      variants.insert(path.to_string(), outcome);
    }
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
  // A local file that only differs in normalized-away noise counts as the
  // baseline content. This folds the key before the decision sees it.
  let mut local = local;
  for (path, entry) in &baseline_map {
    if !entry.is_managed() {
      continue;
    }
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
  let context = DecisionContext {
    remote,
    effectively_unchanged: &|_| false,
  };
  let mut diff = Diff::default();
  let mut next_baseline = BTreeMap::new();
  let mut variants = BTreeMap::new();
  let mut prune_dirs = BTreeMap::new();
  let mut both_variants = Vec::new();
  for path in &paths {
    let entry = baseline_map.get(path);
    let state = local.get(path).cloned().unwrap_or(LocalState::Absent);
    // Both variants on disk refuses the plan when the path is in play; a
    // delete rule clears both without asking.
    if let LocalState::Both(..) = &state {
      let involves = remote.policy_of(path) != Policy::Delete
        && (remote.policy_of(path) == Policy::Ship
          || entry.is_some_and(|entry| entry.is_managed()));
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
      &delete_dirs,
      &mut diff,
      &mut next_baseline,
      &mut variants,
      &mut prune_dirs,
    );
  }
  Ok(Compared {
    diff,
    next_baseline: next_baseline.into_values().collect(),
    variants,
    both_variants,
    prune_dirs,
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
        prune_dirs: Vec::new(),
      }),
      ChangeKind::Delete => operations.push(Operation {
        path: change.path.clone(),
        write: None,
        final_disabled: false,
        rename: false,
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
          prune_dirs: Vec::new(),
        });
      }
      ChangeKind::Retire => {}
    }
  }
  for conflict in &prepared.diff.conflicts {
    if choices.get(&conflict.path).copied().unwrap_or_default() != ConflictChoice::TakeRemote {
      continue;
    }
    operations.push(Operation {
      path: conflict.path.clone(),
      write: conflict.remote.clone(),
      final_disabled: prepared
        .variants
        .get(&conflict.path)
        .copied()
        .unwrap_or(false),
      rename: false,
      prune_dirs: Vec::new(),
    });
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
    ..
  } = compared;
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
  let plan_hash = plan_hash(remote, &diff, &next_baseline);
  Ok(Prepared {
    diff,
    plan_hash,
    merged_contents,
    conflict_sources,
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
