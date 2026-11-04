//! The one classification behind every "local files against a version" list:
//! the publish tree's marks (against the line's previous release) and the
//! "view changes" list and count (against the release an instance matched).
//!
//! Both ask the same questions of a file the version lists: is its content
//! other than the version's, is it switched on/off the other way, is it a
//! first-install file. They differ in where content is judged (hashing the file
//! against the version, or the installed baseline's normalized comparison), in
//! what a missing file means, and in how files the version does not list are
//! told apart. `Judge` carries those differences; everything else is shared.

use super::diff::{FileKey, LocalState, DISABLED_SUFFIX};
use super::local_scan::{collect_states_cached, LocalScanCache};
use super::plan::{LocalChange, LocalChangeKind, NoDiffReason};
use super::remote::{RemoteRule, RuleKind, RuleState};
use crate::instance::helpers::modpack::baseline::hash_file;
use crate::instance::helpers::modpack::export::CONTENT_DIRS;
use crate::instance::modpack_publish::commands::PreviousFile;
use crate::instance::modpack_publish::defaults::{self, DiskFile};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeClass {
  /// The version lists it with other content.
  Modified,
  /// The version lists it, and it is gone.
  Deleted,
  /// Same content, other enabled/disabled state.
  Toggled,
  /// A first-install file of the version whose content differs.
  OnceChanged,
  /// The version does not list it.
  Added,
  /// Added, but the starting state would not publish it.
  AddedExcluded,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDetailEntry {
  pub path: String,
  pub class: ChangeClass,
  /// Whether the entry counts toward the instance's local-change number.
  pub counted: bool,
  /// The nearest previous-release rule covering an added file.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub rule: Option<RuleState>,
  /// The size of the file on disk.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<u64>,
  /// What the file is compared with: the installed content when the baseline
  /// has it, the version's otherwise.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub baseline: Option<FileKey>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub local: Option<FileKey>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub hunks: Option<Vec<super::text_diff::DiffHunk>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub no_diff: Option<NoDiffReason>,
  #[serde(default)]
  pub toggled: bool,
  /// The file sits at `<path>.disabled` on disk.
  #[serde(default)]
  pub disabled: bool,
}

impl ChangeDetailEntry {
  fn new(path: &str, class: ChangeClass, counted: bool) -> Self {
    Self {
      path: path.to_string(),
      class,
      counted,
      rule: None,
      size: None,
      baseline: None,
      local: None,
      hunks: None,
      no_diff: None,
      toggled: false,
      disabled: false,
    }
  }
}

/// How the local files are judged against the version.
pub enum Judge<'a> {
  /// The publish tree: each listed file is hashed against the version (a size
  /// that differs is a change without hashing), a listed file that is missing
  /// is left out, and every unlisted file is added. Entries follow `files`.
  Release,
  /// The instance's change list: content was judged against the installed
  /// baseline beforehand, a listed file that is missing is deleted, and an
  /// unlisted file is added or excluded. Entries follow the version's files,
  /// then its first-install files, then the added ones.
  Baseline(Baseline<'a>),
}

pub struct Baseline<'a> {
  /// The baseline's judgement of managed files, by lowercase path.
  pub changes: &'a HashMap<String, LocalChange>,
  /// Managed entries' installed content, by lowercase path.
  pub managed: &'a HashMap<String, FileKey>,
  /// Lowercase paths of files the player ignores whole; they are skipped.
  pub ignored_whole: &'a HashSet<String>,
  /// Whether the player keeps an unlisted file as their own.
  pub ignored_added: &'a dyn Fn(&str) -> bool,
  /// Whether the starting state would publish an unlisted file.
  pub published: &'a dyn Fn(&str) -> bool,
  /// The version's rules, for the one covering an added file.
  pub rules: &'a [RemoteRule],
  /// Compare first-install files, and measure the added ones.
  pub with_diff: bool,
}

pub struct Spec<'a> {
  pub root: &'a Path,
  pub cache: &'a Mutex<LocalScanCache>,
  /// What the version lists.
  pub release: &'a [PreviousFile],
  /// The files to look at: the enabled names, as the tree lists them.
  pub files: &'a [DiskFile],
  pub judge: Judge<'a>,
}

fn folded(path: &str) -> String {
  path.to_lowercase()
}

fn key_of(file: &PreviousFile) -> FileKey {
  FileKey {
    sha1: file.sha1.clone(),
    size: file.size,
  }
}

fn in_content_dir(path: &str) -> bool {
  let folded = folded(path);
  CONTENT_DIRS
    .iter()
    .any(|dir| folded.starts_with(&format!("{dir}/")))
}

/// Which variant of a path is on disk, as `(disabled, both)`.
fn variant_on_disk(root: &Path, path: &str) -> Option<(bool, bool)> {
  let enabled = root.join(path).is_file();
  let disabled = root.join(format!("{path}{DISABLED_SUFFIX}")).is_file();
  match (enabled, disabled) {
    (false, false) => None,
    (true, both) => Some((false, both)),
    (false, true) => Some((true, false)),
  }
}

fn size_on_disk(root: &Path, path: &str, disabled: bool) -> Option<u64> {
  let name = if disabled {
    format!("{path}{DISABLED_SUFFIX}")
  } else {
    path.to_string()
  };
  std::fs::metadata(root.join(name))
    .ok()
    .map(|meta| meta.len())
}

/// The files to scan, with the content the version lists for them. A file the
/// version does not list has no entry, so it is never read.
pub fn scan_targets(files: &[DiskFile], release: &[PreviousFile]) -> BTreeMap<String, FileKey> {
  let listed: HashMap<String, &PreviousFile> = release
    .iter()
    .map(|file| (folded(&file.path), file))
    .collect();
  files
    .iter()
    .filter_map(|file| {
      let found = listed.get(&folded(&file.path))?;
      Some((file.path.clone(), key_of(found)))
    })
    .collect()
}

/// Whether the content of a file differs from the version's. The scan reports
/// a size mismatch without hashing, so an empty hash on an equal size means
/// "not hashed here" and the file is hashed once. A missing file differs.
fn content_differs(root: &Path, path: &str, state: &LocalState, wanted: &FileKey) -> bool {
  let Some(found) = state.key() else {
    return true;
  };
  if found.size != wanted.size {
    return true;
  }
  if found.sha1 == wanted.sha1 {
    return false;
  }
  if !found.sha1.is_empty() {
    return true;
  }
  let name = match state.variant_disabled() {
    Some(true) => format!("{path}{DISABLED_SUFFIX}"),
    _ => path.to_string(),
  };
  hash_file(&root.join(name)).map_or(true, |(sha1, _)| sha1 != wanted.sha1)
}

/// The class of a listed file. Content wins over the switch; a first-install
/// file with other content is its own class.
fn class_of(once: bool, content_differs: bool, switched: bool) -> Option<ChangeClass> {
  if content_differs {
    Some(if once {
      ChangeClass::OnceChanged
    } else {
      ChangeClass::Modified
    })
  } else if switched {
    Some(ChangeClass::Toggled)
  } else {
    None
  }
}

pub fn classify(spec: &Spec) -> io::Result<Vec<ChangeDetailEntry>> {
  let root = spec.root;
  let listed: HashMap<String, &PreviousFile> = spec
    .release
    .iter()
    .map(|file| (folded(&file.path), file))
    .collect();

  let targets = match &spec.judge {
    Judge::Release => scan_targets(spec.files, spec.release),
    Judge::Baseline(baseline) if baseline.with_diff => spec
      .release
      .iter()
      .filter(|file| file.once && !baseline.ignored_whole.contains(&folded(&file.path)))
      .map(|file| (file.path.clone(), key_of(file)))
      .collect(),
    Judge::Baseline(_) => BTreeMap::new(),
  };
  let states = collect_states_cached(spec.cache, root, &targets)?;

  let mut entries = Vec::new();
  if let Judge::Baseline(baseline) = &spec.judge {
    for file in spec.release {
      let path = folded(&file.path);
      if baseline.ignored_whole.contains(&path) || file.once {
        continue;
      }
      let change = baseline.changes.get(&path);
      let base_key = baseline
        .managed
        .get(&path)
        .cloned()
        .unwrap_or_else(|| key_of(file));
      let Some((disabled, _)) = variant_on_disk(root, &file.path) else {
        let mut entry = ChangeDetailEntry::new(&file.path, ChangeClass::Deleted, true);
        entry.baseline = Some(base_key);
        entries.push(entry);
        continue;
      };
      let switched = disabled != file.disabled;
      let modified = change.filter(|change| change.kind == LocalChangeKind::Modified);
      let Some(class) = class_of(false, modified.is_some(), switched) else {
        continue;
      };
      let mut entry = ChangeDetailEntry::new(&file.path, class, true);
      entry.size = size_on_disk(root, &file.path, disabled);
      entry.toggled = switched;
      entry.disabled = disabled;
      entry.baseline = Some(base_key);
      if let Some(change) = change {
        entry.local = change.local.clone();
      }
      if let Some(change) = modified {
        entry.hunks = change.hunks.clone();
        entry.no_diff = change.no_diff;
      }
      entries.push(entry);
    }

    for (path, wanted) in &targets {
      let state = states.get(path).unwrap_or(&LocalState::Absent);
      if let Some(class) = class_of(true, content_differs(root, path, state, wanted), false) {
        let mut entry = ChangeDetailEntry::new(path, class, false);
        entry.size = state.key().map(|key| key.size);
        entry.disabled = state.variant_disabled() == Some(true);
        entries.push(entry);
      }
    }
  }

  for file in spec.files {
    let Some(before) = listed.get(&folded(&file.path)) else {
      if let Some(entry) = added_entry(spec, file) {
        entries.push(entry);
      }
      continue;
    };
    if !matches!(spec.judge, Judge::Release) {
      continue;
    }
    let state = states.get(&file.path).unwrap_or(&LocalState::Absent);
    let Some(found) = state.key() else {
      // Gone between the listing and the scan.
      continue;
    };
    let wanted = key_of(before);
    let switched = state.variant_disabled() != Some(before.disabled);
    let differs = content_differs(root, &file.path, state, &wanted);
    let Some(class) = class_of(before.once, differs, switched) else {
      continue;
    };
    let mut entry = ChangeDetailEntry::new(&file.path, class, true);
    entry.size = Some(found.size);
    entry.toggled = switched;
    entry.disabled = state.variant_disabled() == Some(true);
    entry.baseline = Some(wanted);
    entries.push(entry);
  }
  Ok(entries)
}

/// A file the version does not list. `None` when the player keeps it as their own.
fn added_entry(spec: &Spec, file: &DiskFile) -> Option<ChangeDetailEntry> {
  let mut entry = match &spec.judge {
    Judge::Release => ChangeDetailEntry::new(&file.path, ChangeClass::Added, false),
    Judge::Baseline(baseline) => {
      if (baseline.ignored_added)(&file.path) {
        return None;
      }
      let published = (baseline.published)(&file.path);
      let class = if published {
        ChangeClass::Added
      } else {
        ChangeClass::AddedExcluded
      };
      let mut entry =
        ChangeDetailEntry::new(&file.path, class, published && in_content_dir(&file.path));
      entry.rule = baseline
        .rules
        .iter()
        .find(|rule| rule.kind == RuleKind::File && rule.path.eq_ignore_ascii_case(&file.path))
        .or_else(|| defaults::covering_rule(baseline.rules, &file.path))
        .map(|rule| rule.state);
      entry
    }
  };
  entry.disabled = file.disabled;
  let measured = match &spec.judge {
    Judge::Release => true,
    Judge::Baseline(baseline) => baseline.with_diff,
  };
  if measured {
    entry.size = size_on_disk(spec.root, &file.path, file.disabled);
  }
  Some(entry)
}
