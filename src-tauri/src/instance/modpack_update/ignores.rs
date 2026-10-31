//! Local ignores: spots of a managed file the player decided to keep as their
//! own. An ignored spot is no local change, and an update merges around it.
//!
//! Spots are line ranges of the normalized installed text (`N(B)`, 0-based,
//! half-open), recorded against the installed content's sha1. A record whose
//! sha1 no longer matches the baseline is dropped when read.

use super::diff::{scan_path, Conflict, ConflictChoice, LocalState, DISABLED_SUFFIX};
use super::merge::MergeConflict;
use super::plan::Prepared;
use super::release::Release;
use super::remote::{RemoteModpack, UPDATE_DIR};
use super::text_diff::{edits, lines_of, Edit};
use crate::instance::helpers::modpack::baseline::{BaselineFile, ModpackBaseline};
use crate::instance::helpers::modpack::{base_store, text};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const IGNORES_FILE: &str = "ignores.json";
const SCHEMA: u32 = 1;

/// The record's name relative to the instance, as the update transaction
/// backs it up.
pub fn meta_name() -> String {
  format!("{UPDATE_DIR}/{IGNORES_FILE}")
}

fn ignores_path(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join(IGNORES_FILE)
}

/// A line range of `N(B)`. An insertion point has `start == end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
  pub start: usize,
  pub end: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoreEntry {
  /// The installed content the ranges refer to.
  pub base_sha1: String,
  /// The whole file is the player's own; `regions` stays empty.
  #[serde(default)]
  pub whole: bool,
  #[serde(default)]
  pub regions: Vec<Region>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ignores {
  pub schema: u32,
  #[serde(default)]
  pub files: BTreeMap<String, IgnoreEntry>,
  /// Newly added files have no installed baseline to anchor their ignore.
  #[serde(default)]
  pub added: BTreeSet<String>,
}

impl Default for Ignores {
  fn default() -> Self {
    Self {
      schema: SCHEMA,
      files: BTreeMap::new(),
      added: BTreeSet::new(),
    }
  }
}

/// Written out by an update: the file as it was and as it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoresUpdate {
  pub from: Ignores,
  pub to: Ignores,
}

impl Ignores {
  pub fn is_empty(&self) -> bool {
    self.files.is_empty() && self.added.is_empty()
  }

  pub fn parse(bytes: &[u8]) -> Self {
    serde_json::from_slice::<Ignores>(bytes)
      .ok()
      .filter(|parsed| parsed.schema == SCHEMA)
      .unwrap_or_default()
  }

  /// What is stored for these ignores; `None` when there is nothing to keep.
  pub fn to_bytes(&self) -> Option<Vec<u8>> {
    if self.is_empty() {
      return None;
    }
    serde_json::to_vec_pretty(self).ok()
  }

  /// The read-and-check of §6: only records of a managed file whose installed
  /// content is still the one they were made against survive, in canonical
  /// form.
  pub fn validated(&self, baseline: &ModpackBaseline) -> Self {
    let mut files = BTreeMap::new();
    for (path, entry) in &self.files {
      let current = baseline
        .files
        .iter()
        .find(|file| file.path == *path && file.is_managed());
      let Some(current) = current else { continue };
      if current.sha1 != entry.base_sha1 {
        continue;
      }
      let entry = canonical(entry);
      if entry.whole || !entry.regions.is_empty() {
        files.insert(path.clone(), entry);
      }
    }
    Self {
      schema: SCHEMA,
      files,
      added: self.added.clone(),
    }
  }

  /// The added-file ignores that still apply: the file is not in the release
  /// and some variant of it is on disk. Records keep the spelling they were
  /// made with; a record that does not match the disk spelling is stale.
  pub fn validated_added(&self, root: &Path, release: &Release) -> BTreeSet<String> {
    let listed = release.index();
    self
      .added
      .iter()
      .filter(|path| !listed.contains_key(&path.to_lowercase()) && on_disk(root, path))
      .cloned()
      .collect()
  }

  /// Whether the added file is ignored. Paths compare without case.
  pub fn ignores_added(&self, path: &str) -> bool {
    self
      .added
      .iter()
      .any(|added| added.eq_ignore_ascii_case(path))
  }
}

fn on_disk(root: &Path, path: &str) -> bool {
  root.join(path).is_file() || root.join(format!("{path}{DISABLED_SUFFIX}")).is_file()
}

fn canonical(entry: &IgnoreEntry) -> IgnoreEntry {
  if entry.whole {
    return IgnoreEntry {
      base_sha1: entry.base_sha1.clone(),
      whole: true,
      regions: Vec::new(),
    };
  }
  IgnoreEntry {
    base_sha1: entry.base_sha1.clone(),
    whole: false,
    regions: merge_regions(
      entry
        .regions
        .iter()
        .copied()
        .filter(|r| r.start <= r.end)
        .collect(),
    ),
  }
}

/// Sort, and join ranges that overlap or touch.
pub fn merge_regions(mut regions: Vec<Region>) -> Vec<Region> {
  regions.sort();
  let mut merged: Vec<Region> = Vec::new();
  for region in regions {
    match merged.last_mut() {
      Some(last) if region.start <= last.end => last.end = last.end.max(region.end),
      _ => merged.push(region),
    }
  }
  merged
}

pub fn read(root: &Path) -> Ignores {
  fs::read(ignores_path(root))
    .map(|bytes| Ignores::parse(&bytes))
    .unwrap_or_default()
}

/// The stored records that still apply to this baseline.
pub fn read_valid(root: &Path, baseline: &ModpackBaseline) -> Ignores {
  read(root).validated(baseline)
}

pub fn read_valid_for_release(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
) -> Ignores {
  let mut ignores = read_valid(root, baseline);
  ignores.added = ignores.validated_added(root, release);
  ignores
}

pub fn write(root: &Path, ignores: &Ignores) -> io::Result<()> {
  let path = ignores_path(root);
  match ignores.to_bytes() {
    Some(bytes) => {
      if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
      }
      fs::write(path, bytes)
    }
    None => match fs::remove_file(path) {
      Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
      _ => Ok(()),
    },
  }
}

pub fn remove(root: &Path) {
  let _ = write(root, &Ignores::default());
}

/// Whether the range of a change lies inside an ignored range. A pure
/// insertion on an endpoint of a range counts as inside.
pub fn covers(regions: &[Region], start: usize, end: usize) -> bool {
  regions
    .iter()
    .any(|region| region.start <= start && end <= region.end)
}

/// Whether one change of the file is ignored.
pub fn is_ignored(entry: &IgnoreEntry, edit: &Edit) -> bool {
  entry.whole || covers(&entry.regions, edit.start, edit.end)
}

/// Whether every change is ignored; no change at all counts.
pub fn all_ignored(entry: &IgnoreEntry, changes: &[Edit]) -> bool {
  changes.iter().all(|edit| is_ignored(entry, edit))
}

/// Closed-interval contact: ranges that merely meet count as touching, since
/// the merge treats adjacent changes of both sides as one conflict.
fn touches(region: &Region, start: usize, end: usize) -> bool {
  region.start <= end && start <= region.end
}

/// Add a range, joining it with those it overlaps or meets.
pub fn add_region(regions: &mut Vec<Region>, region: Region) {
  regions.push(region);
  *regions = merge_regions(std::mem::take(regions));
}

/// Remove exactly this range. Returns whether it was there.
pub fn remove_region(regions: &mut Vec<Region>, region: Region) -> bool {
  let before = regions.len();
  regions.retain(|known| *known != region);
  regions.len() != before
}

/// Where the ranges sit in the new installed text.
///
/// `remote_edits` are the published version's changes against the installed
/// text. A range that no change touches moves by the lines inserted or removed
/// before it. A range a change touches follows that change: it becomes the
/// union of itself and the touched changes, mapped to the new text, so the
/// lines it ignored stay inside. A range touching a block the player settled
/// as "new version" (`taken`, in installed-text lines) is dropped: the local
/// text there is gone.
pub fn map_regions(
  regions: &[Region],
  remote_edits: &[Edit],
  taken: &[(usize, usize)],
) -> Vec<Region> {
  let mut mapped = Vec::new();
  for region in regions {
    if taken
      .iter()
      .any(|(start, end)| touches(region, *start, *end))
    {
      continue;
    }
    let hit: Vec<&Edit> = remote_edits
      .iter()
      .filter(|edit| touches(region, edit.start, edit.end))
      .collect();
    let (Some(first), Some(last)) = (hit.first(), hit.last()) else {
      let delta: isize = remote_edits
        .iter()
        .filter(|edit| edit.end < region.start)
        .map(|edit| (edit.new_end - edit.new_start) as isize - (edit.end - edit.start) as isize)
        .sum();
      mapped.push(Region {
        start: (region.start as isize + delta) as usize,
        end: (region.end as isize + delta) as usize,
      });
      continue;
    };
    let start = region.start.min(first.start) + first.new_start - first.start;
    let end = region.end.max(last.end) + last.new_end - last.end;
    mapped.push(Region { start, end });
  }
  merge_regions(mapped)
}

/// What an update does to the records. `remap` gives the ranges of a file
/// whose installed content moved (`None`: the ranges cannot be carried).
pub fn after_update(
  prev: &Ignores,
  next_baseline: &[BaselineFile],
  remap: &mut dyn FnMut(&str, &IgnoreEntry) -> Option<Vec<Region>>,
) -> Ignores {
  let mut files = BTreeMap::new();
  for (path, entry) in &prev.files {
    let Some(next) = next_baseline
      .iter()
      .find(|file| file.path == *path && file.is_managed())
    else {
      continue;
    };
    if next.sha1 == entry.base_sha1 {
      files.insert(path.clone(), entry.clone());
    } else if entry.whole {
      files.insert(
        path.clone(),
        IgnoreEntry {
          base_sha1: next.sha1.clone(),
          whole: true,
          regions: Vec::new(),
        },
      );
    } else if let Some(regions) = remap(path, entry).filter(|regions| !regions.is_empty()) {
      files.insert(
        path.clone(),
        IgnoreEntry {
          base_sha1: next.sha1.clone(),
          whole: false,
          regions,
        },
      );
    }
  }
  Ignores {
    schema: SCHEMA,
    files,
    added: prev.added.clone(),
  }
}

/// The ignores an update leaves behind, or `None` when the stored file stays
/// as it is. `remote_texts` are the published texts of the files that were
/// merged or conflicted; `choices` are the player's answers by `path#index`.
pub fn plan_update(
  root: &Path,
  prepared: &Prepared,
  remote: &RemoteModpack,
  remote_texts: &BTreeMap<String, Vec<u8>>,
  choices: &BTreeMap<String, ConflictChoice>,
) -> Option<IgnoresUpdate> {
  let to = after_update(
    &prepared.ignores,
    &prepared.next_baseline,
    &mut |path, entry| {
      let remote = remote_texts.get(path)?;
      let base = base_store::load(root, &entry.base_sha1).ok().flatten()?;
      if prepared
        .diff
        .auto_merged
        .iter()
        .any(|merged| merged.path == path)
      {
        return remap_entry(path, entry, &base, remote, &[]);
      }
      let conflict = prepared
        .diff
        .conflicts
        .iter()
        .find(|conflict| conflict.path == path && !conflict.hunks.is_empty())?;
      let taken: Vec<&MergeConflict> = conflict
        .hunks
        .iter()
        .enumerate()
        .filter(|(index, _)| {
          choices.get(&format!("{path}#{index}")) == Some(&ConflictChoice::TakeRemote)
        })
        .map(|(_, hunk)| hunk)
        .collect();
      remap_entry(path, entry, &base, remote, &taken)
    },
  );
  // An added file stops being one when the new version lists it, once-only
  // files included.
  let entered: HashSet<String> = remote
    .files
    .iter()
    .map(|file| file.path.to_lowercase())
    .collect();
  let mut to = to;
  to.added
    .retain(|path| !entered.contains(&path.to_lowercase()) && on_disk(root, path));
  (to != prepared.ignores_on_disk).then(|| IgnoresUpdate {
    from: prepared.ignores_on_disk.clone(),
    to,
  })
}

/// How many lines the raw text has beyond its normalized form (a stripped
/// `.properties` header). The merge counts raw lines.
pub fn merge_line_shift(path: &str, base: &[u8]) -> usize {
  let Ok(raw) = std::str::from_utf8(base) else {
    return 0;
  };
  let raw_lines = raw
    .replace("\r\n", "\n")
    .replace('\r', "\n")
    .lines()
    .count();
  let normalized = text::normalize_text(path, base).map_or(raw_lines, |t| t.lines().count());
  raw_lines.saturating_sub(normalized)
}

/// The lines of the normalized text of `bytes`, if it is such a text.
pub fn normalized_lines(path: &str, bytes: &[u8]) -> Option<Vec<String>> {
  text::normalize_text(path, bytes).map(|text| lines_of(&text))
}

/// Carry a file's ranges across an update from the installed text `base` to
/// the published `remote`. `taken` are the settled-as-new-version blocks in
/// raw merge lines.
pub fn remap_entry(
  path: &str,
  entry: &IgnoreEntry,
  base: &[u8],
  remote: &[u8],
  taken: &[&MergeConflict],
) -> Option<Vec<Region>> {
  let base_lines = normalized_lines(path, base)?;
  let remote_lines = normalized_lines(path, remote)?;
  let shift = merge_line_shift(path, base);
  let taken: Vec<(usize, usize)> = taken
    .iter()
    .map(|hunk| {
      (
        hunk.base_start.saturating_sub(shift),
        hunk.base_end.saturating_sub(shift),
      )
    })
    .collect();
  Some(map_regions(
    &entry.regions,
    &edits(&base_lines, &remote_lines),
    &taken,
  ))
}

/// Mark the conflict blocks the player ignored. A whole-file record flags the
/// file and every block; a range record flags a block when all local changes
/// inside it are ignored.
pub fn annotate_conflict(
  conflict: &mut Conflict,
  entry: &IgnoreEntry,
  base: Option<&[u8]>,
  local: Option<&[u8]>,
) {
  if entry.whole {
    conflict.ignored = true;
    conflict.ignored_hunks = (0..conflict.hunks.len()).collect();
    return;
  }
  let (Some(base), Some(local)) = (base, local) else {
    return;
  };
  let (Some(base_lines), Some(local_lines)) = (
    normalized_lines(&conflict.path, base),
    normalized_lines(&conflict.path, local),
  ) else {
    return;
  };
  let shift = merge_line_shift(&conflict.path, base);
  let local_edits = edits(&base_lines, &local_lines);
  for (index, hunk) in conflict.hunks.iter().enumerate() {
    let (start, end) = (
      hunk.base_start.saturating_sub(shift),
      hunk.base_end.saturating_sub(shift),
    );
    let inside: Vec<&Edit> = local_edits
      .iter()
      .filter(|edit| edit.start <= end && start <= edit.end)
      .collect();
    if !inside.is_empty() && inside.iter().all(|edit| is_ignored(entry, edit)) {
      conflict.ignored_hunks.push(index);
    }
  }
  conflict.ignored =
    !conflict.hunks.is_empty() && conflict.ignored_hunks.len() == conflict.hunks.len();
}

/// Paths whose local content counts as unchanged for the decision: every
/// difference from the installed content is ignored.
pub fn unchanged_paths(
  root: &Path,
  ignores: &Ignores,
  local: &BTreeMap<String, LocalState>,
) -> BTreeSet<String> {
  let mut found = BTreeSet::new();
  for (path, entry) in &ignores.files {
    let Some(state) = local.get(path) else {
      continue;
    };
    if state.key().is_none() || matches!(state, LocalState::Both(..)) {
      continue;
    }
    if entry.whole {
      found.insert(path.clone());
      continue;
    }
    let Some(changes) = local_edits(root, path, entry, state) else {
      continue;
    };
    if all_ignored(entry, &changes) {
      found.insert(path.clone());
    }
  }
  found
}

fn variant_file(path: &str, state: &LocalState) -> String {
  match state.variant_disabled() {
    Some(true) => format!("{path}{DISABLED_SUFFIX}"),
    _ => path.to_string(),
  }
}

/// The installed text and the local text as lines, when both can be read.
fn both_texts(
  root: &Path,
  path: &str,
  base_sha1: &str,
  state: &LocalState,
) -> Option<(Vec<String>, Vec<String>)> {
  let base = base_store::load(root, base_sha1).ok().flatten()?;
  let local = fs::read(root.join(variant_file(path, state))).ok()?;
  Some((
    normalized_lines(path, &base)?,
    normalized_lines(path, &local)?,
  ))
}

fn local_edits(
  root: &Path,
  path: &str,
  entry: &IgnoreEntry,
  state: &LocalState,
) -> Option<Vec<Edit>> {
  let (base, local) = both_texts(root, path, &entry.base_sha1, state)?;
  Some(edits(&base, &local))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreError {
  /// The request does not match what is on disk any more.
  Mismatch,
  Io(String),
}

impl From<io::Error> for IgnoreError {
  fn from(error: io::Error) -> Self {
    IgnoreError::Io(error.to_string())
  }
}

/// Ignore a spot (`range`) or the whole file (`None`) of a managed file. A
/// range must be one of the local changes as they stand now.
pub fn add(
  root: &Path,
  baseline: &ModpackBaseline,
  path: &str,
  base_sha1: &str,
  range: Option<Region>,
) -> Result<(), IgnoreError> {
  let managed = baseline
    .files
    .iter()
    .find(|file| file.path == path && file.is_managed())
    .filter(|file| file.sha1 == base_sha1)
    .ok_or(IgnoreError::Mismatch)?;
  let mut ignores = read_valid(root, baseline);
  match range {
    None => {
      ignores.files.insert(
        path.to_string(),
        IgnoreEntry {
          base_sha1: managed.sha1.clone(),
          whole: true,
          regions: Vec::new(),
        },
      );
    }
    Some(region) => {
      let state = scan_path(root, path)?;
      let entry = ignores
        .files
        .entry(path.to_string())
        .or_insert_with(|| IgnoreEntry {
          base_sha1: managed.sha1.clone(),
          whole: false,
          regions: Vec::new(),
        });
      let (base, local) = both_texts(root, path, base_sha1, &state).ok_or(IgnoreError::Mismatch)?;
      let known = edits(&base, &local)
        .iter()
        .any(|edit| edit.start == region.start && edit.end == region.end);
      if !known {
        return Err(IgnoreError::Mismatch);
      }
      if !entry.whole {
        add_region(&mut entry.regions, region);
      }
    }
  }
  write(root, &ignores)?;
  Ok(())
}

/// Keep a file the player added as the player's own. The path is the enabled
/// name; the caller has checked it is currently an added file.
pub fn add_added(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
  path: &str,
) -> Result<(), IgnoreError> {
  if release.index().contains_key(&path.to_lowercase()) || !on_disk(root, path) {
    return Err(IgnoreError::Mismatch);
  }
  let mut ignores = read_valid_for_release(root, baseline, release);
  ignores
    .added
    .retain(|added| !added.eq_ignore_ascii_case(path));
  ignores.added.insert(path.to_string());
  write(root, &ignores)?;
  Ok(())
}

/// Undo an ignore: one range, or the whole record (`None`).
pub fn undo(
  root: &Path,
  baseline: &ModpackBaseline,
  path: &str,
  range: Option<Region>,
) -> Result<(), IgnoreError> {
  let mut ignores = read_valid(root, baseline);
  match range {
    None => {
      let before = ignores.added.len();
      ignores
        .added
        .retain(|added| !added.eq_ignore_ascii_case(path));
      let was_added = ignores.added.len() != before;
      if ignores.files.remove(path).is_none() && !was_added {
        return Err(IgnoreError::Mismatch);
      }
    }
    Some(region) => {
      let entry = ignores.files.get_mut(path).ok_or(IgnoreError::Mismatch)?;
      if !remove_region(&mut entry.regions, region) {
        return Err(IgnoreError::Mismatch);
      }
      if entry.regions.is_empty() {
        ignores.files.remove(path);
      }
    }
  }
  write(root, &ignores)?;
  Ok(())
}

/// One ignored range, with the lines on both sides for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredRegion {
  pub start: usize,
  pub end: usize,
  pub base_lines: Vec<String>,
  pub local_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredItem {
  pub path: String,
  pub base_sha1: String,
  pub whole: bool,
  pub regions: Vec<IgnoredRegion>,
  #[serde(default)]
  pub added: bool,
}

fn list_from(root: &Path, ignores: Ignores) -> Vec<IgnoredItem> {
  let mut items = Vec::new();
  for (path, entry) in &ignores.files {
    let mut regions = Vec::new();
    if !entry.whole {
      let state = scan_path(root, path).unwrap_or(LocalState::Absent);
      let texts = both_texts(root, path, &entry.base_sha1, &state);
      for region in &entry.regions {
        let (base_lines, local_lines) = match &texts {
          Some((base, local)) => {
            let seen = map_regions(&[*region], &edits(base, local), &[]);
            let local_range = seen.first().copied().unwrap_or(*region);
            (
              slice(base, region.start, region.end),
              slice(local, local_range.start, local_range.end),
            )
          }
          None => (Vec::new(), Vec::new()),
        };
        regions.push(IgnoredRegion {
          start: region.start,
          end: region.end,
          base_lines,
          local_lines,
        });
      }
    }
    items.push(IgnoredItem {
      path: path.clone(),
      base_sha1: entry.base_sha1.clone(),
      whole: entry.whole,
      regions,
      added: false,
    });
  }
  items.extend(ignores.added.into_iter().map(|path| IgnoredItem {
    path,
    base_sha1: String::new(),
    whole: true,
    regions: Vec::new(),
    added: true,
  }));
  items
}

#[cfg(test)]
pub fn list(root: &Path, baseline: &ModpackBaseline) -> Vec<IgnoredItem> {
  list_from(root, read_valid(root, baseline))
}

pub fn list_for_release(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
) -> Vec<IgnoredItem> {
  list_from(root, read_valid_for_release(root, baseline, release))
}

fn slice(lines: &[String], start: usize, end: usize) -> Vec<String> {
  let end = end.min(lines.len());
  lines[start.min(end)..end].to_vec()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::modpack_update::diff::{ConflictKind, FileKey};

  fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
  }

  fn region(start: usize, end: usize) -> Region {
    Region { start, end }
  }

  fn edit(start: usize, end: usize, new_start: usize, new_end: usize) -> Edit {
    Edit {
      start,
      end,
      new_start,
      new_end,
    }
  }

  fn entry(regions: Vec<Region>) -> IgnoreEntry {
    IgnoreEntry {
      base_sha1: "aa".into(),
      whole: false,
      regions,
    }
  }

  #[test]
  fn a_block_inside_the_range_is_ignored() {
    let regions = [region(2, 6)];
    assert!(covers(&regions, 3, 5));
    assert!(covers(&regions, 2, 6));
  }

  #[test]
  fn a_block_partly_outside_the_range_is_not_ignored() {
    let regions = [region(2, 6)];
    assert!(!covers(&regions, 1, 3));
    assert!(!covers(&regions, 5, 8));
    assert!(!covers(&regions, 7, 9));
    // Two ranges do not add up to a bigger one unless they were merged.
    assert!(!covers(&[region(2, 4), region(5, 7)], 3, 6));
  }

  #[test]
  fn an_insertion_on_an_endpoint_is_inside() {
    let regions = [region(2, 6)];
    assert!(covers(&regions, 2, 2));
    assert!(covers(&regions, 6, 6));
    assert!(!covers(&regions, 7, 7));
    assert!(!covers(&regions, 1, 1));
  }

  #[test]
  fn a_whole_file_ignores_every_block_and_nothing_is_trivially_ignored() {
    let whole = IgnoreEntry {
      whole: true,
      ..entry(Vec::new())
    };
    assert!(is_ignored(&whole, &edit(40, 41, 40, 41)));
    assert!(all_ignored(&entry(vec![region(0, 1)]), &[]));
    assert!(!all_ignored(
      &entry(vec![region(0, 1)]),
      &[edit(0, 1, 0, 1), edit(3, 4, 3, 4)]
    ));
  }

  #[test]
  fn adding_a_range_joins_overlapping_and_adjacent_ones() {
    let mut regions = vec![region(1, 3), region(8, 9)];
    add_region(&mut regions, region(3, 5));
    assert_eq!(regions, vec![region(1, 5), region(8, 9)]);
    add_region(&mut regions, region(5, 8));
    assert_eq!(regions, vec![region(1, 9)]);
    add_region(&mut regions, region(20, 20));
    assert_eq!(regions, vec![region(1, 9), region(20, 20)]);
  }

  #[test]
  fn removing_a_range_needs_the_exact_range() {
    let mut regions = vec![region(1, 5), region(8, 9)];
    assert!(!remove_region(&mut regions, region(1, 4)));
    assert!(remove_region(&mut regions, region(8, 9)));
    assert_eq!(regions, vec![region(1, 5)]);
  }

  #[test]
  fn ranges_shift_by_changes_before_them() {
    // The remote adds two lines at 0 and drops one line at 3..4.
    let remote = [edit(0, 0, 0, 2), edit(3, 4, 5, 5)];
    let mapped = map_regions(&[region(6, 8), region(0, 0)], &remote, &[]);
    // (0,0) touches the insertion at 0; (6,8) comes after both: +2 -1.
    assert_eq!(mapped, vec![region(0, 2), region(7, 9)]);
  }

  #[test]
  fn ranges_touching_a_remote_change_become_the_union_in_the_new_text() {
    // old 4..10 is ignored; the remote rewrites 8..12 (4 lines) as 3 lines,
    // after having added 1 line at 0.
    let remote = [edit(0, 0, 0, 1), edit(8, 12, 9, 12)];
    let mapped = map_regions(&[region(4, 10)], &remote, &[]);
    assert_eq!(mapped, vec![region(5, 12)]);
    // A change that only meets the range at its end counts as touching.
    let meeting = [edit(10, 11, 10, 14)];
    assert_eq!(
      map_regions(&[region(4, 10)], &meeting, &[]),
      vec![region(4, 14)]
    );
    // A change one line away does not.
    let apart = [edit(11, 12, 11, 14)];
    assert_eq!(
      map_regions(&[region(4, 10)], &apart, &[]),
      vec![region(4, 10)]
    );
  }

  #[test]
  fn mapping_by_shift_and_by_intersection_differ() {
    let remote = [edit(5, 6, 5, 8)];
    // Not touching: only the shift of what lies before it applies.
    assert_eq!(
      map_regions(&[region(8, 9)], &remote, &[]),
      vec![region(10, 11)]
    );
    // Touching: the range grows with the new block.
    assert_eq!(
      map_regions(&[region(3, 6)], &remote, &[]),
      vec![region(3, 8)]
    );
  }

  #[test]
  fn settling_a_conflict_as_the_new_version_drops_the_range() {
    let remote = [edit(5, 6, 5, 7)];
    let taken = [(5, 6)];
    let mapped = map_regions(&[region(4, 6), region(9, 10)], &remote, &taken);
    assert_eq!(mapped, vec![region(10, 11)]);
  }

  #[test]
  fn mapped_ranges_that_meet_are_joined() {
    let remote = [edit(3, 4, 3, 4)];
    let mapped = map_regions(&[region(1, 3), region(4, 6)], &remote, &[]);
    assert_eq!(mapped, vec![region(1, 6)]);
  }

  fn baseline_with(path: &str, sha1: &str) -> ModpackBaseline {
    use crate::instance::helpers::modpack::baseline::{BaselineLoader, ModpackBaseline};
    use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
    let origin = InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    };
    let loader = BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: "1".into(),
    };
    ModpackBaseline::build(
      &origin,
      loader,
      vec![BaselineFile::managed(
        path,
        FileKey {
          sha1: sha1.into(),
          size: 1,
        },
        false,
      )],
    )
  }

  #[test]
  fn a_record_made_against_other_content_is_discarded_on_read() {
    let baseline = baseline_with("config/c2me.toml", "aa");
    let stored = Ignores {
      schema: SCHEMA,
      files: BTreeMap::from([
        (
          "config/c2me.toml".to_string(),
          entry(vec![region(3, 4), region(4, 6)]),
        ),
        (
          "config/other.toml".to_string(),
          IgnoreEntry {
            whole: true,
            ..entry(Vec::new())
          },
        ),
      ]),
      added: BTreeSet::new(),
    };
    let valid = stored.validated(&baseline);
    assert_eq!(
      valid.files.keys().collect::<Vec<_>>(),
      vec!["config/c2me.toml"]
    );
    assert_eq!(valid.files["config/c2me.toml"].regions, vec![region(3, 6)]);
    let moved = baseline_with("config/c2me.toml", "bb");
    assert!(stored.validated(&moved).is_empty());
  }

  #[test]
  fn the_file_round_trips_and_garbage_reads_as_empty() {
    let ignores = Ignores {
      schema: SCHEMA,
      files: BTreeMap::from([("a.toml".to_string(), entry(vec![region(1, 2)]))]),
      added: BTreeSet::new(),
    };
    let bytes = ignores.to_bytes().unwrap();
    assert_eq!(Ignores::parse(&bytes), ignores);
    assert!(Ignores::parse(b"not json").is_empty());
    assert!(Ignores::parse(br#"{"schema":9,"files":{"a":{"baseSha1":"x"}}}"#).is_empty());
    assert!(Ignores::default().to_bytes().is_none());
  }

  #[test]
  fn after_an_update_whole_records_follow_and_ranges_are_mapped_or_dropped() {
    let prev = Ignores {
      schema: SCHEMA,
      files: BTreeMap::from([
        ("keep.toml".to_string(), entry(vec![region(1, 2)])),
        ("moved.toml".to_string(), entry(vec![region(1, 2)])),
        ("lost.toml".to_string(), entry(vec![region(1, 2)])),
        ("gone.toml".to_string(), entry(vec![region(1, 2)])),
        (
          "whole.toml".to_string(),
          IgnoreEntry {
            whole: true,
            ..entry(Vec::new())
          },
        ),
      ]),
      added: BTreeSet::new(),
    };
    let next = vec![
      BaselineFile::managed(
        "keep.toml",
        FileKey {
          sha1: "aa".into(),
          size: 1,
        },
        false,
      ),
      BaselineFile::managed(
        "moved.toml",
        FileKey {
          sha1: "bb".into(),
          size: 1,
        },
        false,
      ),
      BaselineFile::managed(
        "lost.toml",
        FileKey {
          sha1: "bb".into(),
          size: 1,
        },
        false,
      ),
      BaselineFile::dormant(
        "gone.toml",
        Some(FileKey {
          sha1: "aa".into(),
          size: 1,
        }),
        false,
      ),
      BaselineFile::managed(
        "whole.toml",
        FileKey {
          sha1: "bb".into(),
          size: 1,
        },
        false,
      ),
    ];
    let mut asked = Vec::new();
    let after = after_update(&prev, &next, &mut |path, _| {
      asked.push(path.to_string());
      (path == "moved.toml").then(|| vec![region(4, 5)])
    });
    assert_eq!(asked, vec!["lost.toml", "moved.toml"]);
    assert_eq!(after.files["keep.toml"], entry(vec![region(1, 2)]));
    assert_eq!(after.files["moved.toml"].base_sha1, "bb");
    assert_eq!(after.files["moved.toml"].regions, vec![region(4, 5)]);
    assert!(after.files["whole.toml"].whole);
    assert_eq!(after.files["whole.toml"].base_sha1, "bb");
    assert!(!after.files.contains_key("lost.toml"));
    assert!(!after.files.contains_key("gone.toml"));
  }

  #[test]
  fn remapping_reads_the_texts_and_the_taken_blocks() {
    let base = b"a=1\nb=2\nc=3\nd=4\ne=5\n";
    let remote = b"top\na=1\nb=2\nc=3\nd=four\ne=5\n";
    let conflict = MergeConflict {
      base_start: 3,
      base_end: 4,
      base: vec!["d=4".into()],
      local: vec!["d=40".into()],
      remote: vec!["d=four".into()],
    };
    let ignored = entry(vec![region(1, 2), region(3, 4)]);
    // Kept local: the block's range follows the remote change.
    let kept = remap_entry("config/a.toml", &ignored, base, remote, &[]).unwrap();
    assert_eq!(kept, vec![region(2, 3), region(4, 5)]);
    // Settled as the new version: the range at the block is dropped.
    let taken = remap_entry("config/a.toml", &ignored, base, remote, &[&conflict]).unwrap();
    assert_eq!(taken, vec![region(2, 3)]);
    // Not text: nothing to carry.
    assert!(remap_entry("mods/a.jar", &ignored, base, remote, &[]).is_none());
  }

  #[test]
  fn the_merge_line_shift_is_the_stripped_properties_header() {
    let header = b"#Wed Oct 14 20:14:57 CST 2026\na=b\n";
    assert_eq!(merge_line_shift("config/a.properties", header), 1);
    assert_eq!(merge_line_shift("config/a.toml", header), 0);
    assert_eq!(merge_line_shift("config/a.toml", b"\xff"), 0);
  }

  fn conflict(path: &str, hunks: Vec<(usize, usize)>) -> Conflict {
    Conflict {
      path: path.into(),
      kind: ConflictKind::ModifiedLocallyChangedRemotely,
      baseline: None,
      local: None,
      remote: None,
      remote_disabled: None,
      local_disabled: None,
      hunks: hunks
        .into_iter()
        .map(|(start, end)| MergeConflict {
          base_start: start,
          base_end: end,
          base: Vec::new(),
          local: Vec::new(),
          remote: Vec::new(),
        })
        .collect(),
      ignored: false,
      ignored_hunks: Vec::new(),
    }
  }

  #[test]
  fn conflict_blocks_over_ignored_changes_are_flagged() {
    let base = b"a\nb\nc\nd\ne\nf\ng\n";
    // Local changed line 1 (b) and line 5 (f).
    let local = b"a\nB\nc\nd\ne\nF\ng\n";
    let ignored = entry(vec![region(1, 2)]);
    let mut found = conflict("config/a.toml", vec![(1, 2), (5, 6)]);
    annotate_conflict(&mut found, &ignored, Some(base), Some(local));
    assert_eq!(found.ignored_hunks, vec![0]);
    assert!(!found.ignored);
    let mut all = conflict("config/a.toml", vec![(1, 2)]);
    annotate_conflict(&mut all, &ignored, Some(base), Some(local));
    assert!(all.ignored);
    let whole = IgnoreEntry {
      whole: true,
      ..entry(Vec::new())
    };
    let mut every = conflict("config/a.toml", vec![(1, 2), (5, 6)]);
    annotate_conflict(&mut every, &whole, None, None);
    assert!(every.ignored);
    assert_eq!(every.ignored_hunks, vec![0, 1]);
    assert_eq!(lines("x\ny").len(), 2);
  }

  #[test]
  fn added_file_ignores_round_trip_and_are_removed_when_invalid() {
    let root = std::env::temp_dir().join(format!("ustbl-added-ignore-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("mods")).unwrap();
    std::fs::write(root.join("mods/new.jar"), b"new").unwrap();
    let baseline = baseline_with("config/base.toml", "aa");
    let release = Release {
      schema: 1,
      version_id: "1".into(),
      label: "one".into(),
      files: vec![],
      rules: vec![],
      rules_manifest: false,
    };
    assert!(add_added(&root, &baseline, &release, "mods/new.jar").is_ok());
    let stored = read(&root);
    assert_eq!(stored.added, BTreeSet::from(["mods/new.jar".into()]));
    let listed = list(&root, &baseline);
    assert_eq!(listed[0].path, "mods/new.jar");
    assert!(listed[0].added);

    let valid = read_valid_for_release(&root, &baseline, &release);
    assert_eq!(valid.added.len(), 1);
    std::fs::remove_file(root.join("mods/new.jar")).unwrap();
    assert!(read_valid_for_release(&root, &baseline, &release)
      .added
      .is_empty());
    assert!(add_added(&root, &baseline, &release, "mods/missing.jar").is_err());
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn old_ignore_json_without_added_field_still_parses() {
    let parsed = Ignores::parse(br#"{"schema":1,"files":{}}"#);
    assert!(parsed.files.is_empty());
    assert!(parsed.added.is_empty());
  }
}
