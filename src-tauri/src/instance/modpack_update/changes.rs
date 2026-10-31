//! Local changes against the release the instance last matched.
//!
//! One function decides every entry, for the detail list and for the count of
//! the status badge alike: the count is the entries marked `counted`.

use super::diff::{FileKey, LocalState, DISABLED_SUFFIX};
use super::ignores::{self, IgnoreError, Ignores};
use super::local_scan::collect_states_cached;
use super::plan::{local_changes, scan_cache, LocalChange, LocalChangeKind, NoDiffReason};
use super::release::Release;
use super::remote::{RuleKind, RuleState};
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::baseline::{hash_file, ModpackBaseline};
use crate::instance::helpers::modpack::export::CONTENT_DIRS;
use crate::instance::models::misc::{ModpackFileList, SuggestedEntry};
use crate::instance::modpack_publish::defaults::{self, create_initial_model};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeClass {
  Modified,
  Deleted,
  Toggled,
  OnceChanged,
  Added,
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
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<u64>,
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

/// The manifest name represented by a disk path. Only mods use `.disabled` as
/// a file attribute; a file with that suffix elsewhere is an ordinary path.
pub(crate) fn logical_disk_path(path: &str) -> &str {
  let folded = path.to_lowercase();
  if folded.starts_with("mods/") && folded.ends_with(DISABLED_SUFFIX) {
    &path[..path.len() - DISABLED_SUFFIX.len()]
  } else {
    path
  }
}

fn in_content_dir(path: &str) -> bool {
  let folded = path.to_lowercase();
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

/// Whether the content of a first-install file differs from the release.
/// The scan reports a size mismatch without hashing, so an empty hash on an
/// equal size means "not hashed here" and the file is hashed once.
fn once_differs(root: &Path, path: &str, state: &LocalState, wanted: &FileKey) -> bool {
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

fn suggested_of(listed: &[SuggestedEntry]) -> Vec<defaults::SuggestedEntry> {
  listed
    .iter()
    .map(|entry| defaults::SuggestedEntry {
      path: entry.path.clone(),
      kind: if entry.kind == "dir" {
        RuleKind::Dir
      } else {
        RuleKind::File
      },
    })
    .collect()
}

/// Every local change of the instance against `release`.
///
/// - `disk` is the file listing as `list_files` gives it (`.disabled` names
///   still in place); `suggested` is its suggested-blacklist part.
/// - Modified and deleted files follow `local_changes`: content is judged
///   against the installed baseline, normalized text, with the player's
///   ignores applied. Nothing else compares content.
/// - A switched file is one whose variant on disk differs from the release's
///   `disabled`; only existence of the variants is looked at.
/// - First-install (`once`) files are skipped unless `with_diff` is set, which
///   also adds line diffs. They are never counted.
/// - Files on disk that the release does not list are added; whether they
///   would be published decides `Added` against `AddedExcluded`.
pub async fn compute_changes(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
  ignored: &Ignores,
  disk: &[String],
  suggested: &[SuggestedEntry],
  with_diff: bool,
) -> USTBLResult<Vec<ChangeDetailEntry>> {
  let listed = release.index();
  let whole_ignored: HashSet<String> = ignored
    .files
    .iter()
    .filter(|(_, entry)| entry.whole)
    .map(|(path, _)| path.to_lowercase())
    .collect();
  let local: HashMap<String, LocalChange> = local_changes(root, baseline, with_diff)
    .await?
    .into_iter()
    .map(|change| (change.path.to_lowercase(), change))
    .collect();
  // A whole-file ignore is anchored on the managed baseline entry.
  let managed: HashMap<String, FileKey> = baseline
    .files
    .iter()
    .filter(|file| file.is_managed())
    .filter_map(|file| file.key().map(|key| (file.path.to_lowercase(), key)))
    .collect();

  let mut entries = Vec::new();
  let mut once_keys = BTreeMap::new();
  for file in &release.files {
    let folded = file.path.to_lowercase();
    if whole_ignored.contains(&folded) {
      continue;
    }
    if file.once {
      if with_diff {
        once_keys.insert(file.path.clone(), file.key());
      }
      continue;
    }
    let change = local.get(&folded);
    let base_key = managed.get(&folded).cloned().unwrap_or_else(|| file.key());
    let Some((disabled, _)) = variant_on_disk(root, &file.path) else {
      let mut entry = ChangeDetailEntry::new(&file.path, ChangeClass::Deleted, true);
      entry.baseline = Some(base_key);
      entries.push(entry);
      continue;
    };
    let switched = disabled != file.disabled;
    let modified = change.filter(|change| change.kind == LocalChangeKind::Modified);
    if modified.is_none() && !switched {
      continue;
    }
    let class = if modified.is_some() {
      ChangeClass::Modified
    } else {
      ChangeClass::Toggled
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

  if !once_keys.is_empty() {
    let states = {
      let root = root.to_path_buf();
      let keys = once_keys.clone();
      tokio::task::spawn_blocking(move || collect_states_cached(scan_cache(), &root, &keys))
        .await
        .map_err(|error| crate::error::USTBLError(error.to_string()))??
    };
    for (path, wanted) in &once_keys {
      let state = states.get(path).unwrap_or(&LocalState::Absent);
      if once_differs(root, path, state, wanted) {
        let mut entry = ChangeDetailEntry::new(path, ChangeClass::OnceChanged, false);
        entry.size = state.key().map(|key| key.size);
        entry.disabled = state.variant_disabled() == Some(true);
        entries.push(entry);
      }
    }
  }

  let defaults = create_initial_model(disk, Some(&release.to_previous()), &suggested_of(suggested));
  for file in &defaults.files {
    if listed.contains_key(&file.path.to_lowercase()) || ignored.ignores_added(&file.path) {
      continue;
    }
    let published = defaults.publishes(&file.path);
    let class = if published {
      ChangeClass::Added
    } else {
      ChangeClass::AddedExcluded
    };
    let mut entry =
      ChangeDetailEntry::new(&file.path, class, published && in_content_dir(&file.path));
    entry.rule = release
      .rules
      .iter()
      .find(|rule| rule.kind == RuleKind::File && rule.path.eq_ignore_ascii_case(&file.path))
      .or_else(|| defaults::covering_rule(&release.rules, &file.path))
      .map(|rule| rule.state);
    entry.disabled = file.disabled;
    if with_diff {
      entry.size = size_on_disk(root, &file.path, file.disabled);
    }
    entries.push(entry);
  }
  Ok(entries)
}

/// The detail list: every entry for the files `list_files` found.
pub async fn classify(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
  listed: &ModpackFileList,
) -> USTBLResult<Vec<ChangeDetailEntry>> {
  let ignored = ignores::read_valid_for_release(root, baseline, release);
  compute_changes(
    root,
    baseline,
    release,
    &ignored,
    &listed.all,
    &listed.suggested,
    true,
  )
  .await
}

/// How many entries count toward the local-change number.
pub async fn count(root: &Path, baseline: &ModpackBaseline) -> USTBLResult<usize> {
  let (release, _) = super::release::release_for(root, baseline);
  let ignored = ignores::read_valid_for_release(root, baseline, &release);
  let name = root
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default();
  let listed = {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
      crate::instance::helpers::modpack::export::list_content_files(&root, &name)
    })
    .await
    .map_err(|error| crate::error::USTBLError(error.to_string()))??
  };
  let entries = compute_changes(
    root,
    baseline,
    &release,
    &ignored,
    &listed.all,
    &listed.suggested,
    false,
  )
  .await?;
  Ok(entries.iter().filter(|entry| entry.counted).count())
}

/// Keep an added file as the player's own. The file must currently be listed
/// as added (or added-but-excluded), otherwise the request is stale.
pub async fn ignore_added_file(
  root: &Path,
  baseline: &ModpackBaseline,
  release: &Release,
  listed: &ModpackFileList,
  path: &str,
) -> Result<(), IgnoreError> {
  let ignored = ignores::read_valid_for_release(root, baseline, release);
  let entries = compute_changes(
    root,
    baseline,
    release,
    &ignored,
    &listed.all,
    &listed.suggested,
    false,
  )
  .await
  .map_err(|error| IgnoreError::Io(error.0))?;
  let wanted = logical_disk_path(path);
  let entry = entries.iter().find(|entry| {
    matches!(entry.class, ChangeClass::Added | ChangeClass::AddedExcluded)
      && entry.path.eq_ignore_ascii_case(wanted)
  });
  match entry {
    Some(entry) => ignores::add_added(root, baseline, release, &entry.path),
    None => Err(IgnoreError::Mismatch),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::helpers::modpack::{base_store, export};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::instance::modpack_update::release::{self, ReleaseFile};
  use crate::instance::modpack_update::remote::{RemoteRule, RuleKind, RuleState};
  use sha1::{Digest, Sha1};
  use std::collections::BTreeSet;
  use std::fs;
  use std::path::PathBuf;

  const TEN: &str = "a=1\nb=2\nc=3\nd=4\ne=5\nf=6\ng=7\nh=8\ni=9\nj=10\n";

  fn key(content: &str) -> FileKey {
    FileKey {
      sha1: hex::encode(Sha1::digest(content.as_bytes())),
      size: content.len() as u64,
    }
  }

  fn origin() -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "7".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    }
  }

  fn instance(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-change-detail-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    for (path, content) in files {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    root
  }

  fn baseline_with(files: Vec<BaselineFile>) -> ModpackBaseline {
    ModpackBaseline::build(
      &origin(),
      BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16".into(),
      },
      files,
    )
  }

  fn baseline_of(managed: &[(&str, &str)]) -> ModpackBaseline {
    baseline_with(
      managed
        .iter()
        .map(|(path, content)| BaselineFile::managed(path, key(content), false))
        .collect(),
    )
  }

  fn release_file(path: &str, content: &str, disabled: bool, once: bool) -> ReleaseFile {
    let key = key(content);
    ReleaseFile {
      path: path.into(),
      sha1: key.sha1,
      size: key.size,
      disabled,
      once,
    }
  }

  fn release_of(files: Vec<ReleaseFile>, rules: Vec<RemoteRule>) -> Release {
    Release {
      schema: release::RELEASE_SCHEMA,
      version_id: "7".into(),
      label: "v7".into(),
      files,
      rules,
      rules_manifest: true,
    }
  }

  fn name_of(root: &Path) -> String {
    root.file_name().unwrap().to_string_lossy().into_owned()
  }

  async fn detail(
    root: &Path,
    baseline: &ModpackBaseline,
    release: &Release,
  ) -> Vec<ChangeDetailEntry> {
    let listed = export::list_files_at(root, &name_of(root)).unwrap();
    classify(root, baseline, release, &listed).await.unwrap()
  }

  fn find<'a>(entries: &'a [ChangeDetailEntry], path: &str) -> Option<&'a ChangeDetailEntry> {
    entries.iter().find(|entry| entry.path == path)
  }

  fn ignore_range(root: &Path, baseline: &ModpackBaseline, start: usize, end: usize) {
    let file = &baseline.files[0];
    ignores::add(
      root,
      baseline,
      &file.path,
      &file.sha1,
      Some(ignores::Region { start, end }),
    )
    .unwrap();
  }

  #[tokio::test]
  async fn classifies_every_release_and_added_file_kind() {
    let root = instance(&[
      ("config/edited.toml", "local"),
      ("config/once.toml", "changed"),
      ("mods/toggle.jar.disabled", "toggle"),
      ("mods/new.jar", "new"),
      ("config/new.toml", "new-config"),
      ("saves/world.dat", "save"),
      ("config/cache/new.toml", "cache"),
    ]);
    let baseline = baseline_of(&[
      ("config/edited.toml", "base"),
      ("mods/toggle.jar", "toggle"),
      ("config/deleted.toml", "deleted"),
    ]);
    let release = release_of(
      vec![
        release_file("config/edited.toml", "base", false, false),
        release_file("config/once.toml", "original", false, true),
        release_file("mods/toggle.jar", "toggle", false, false),
        release_file("config/deleted.toml", "deleted", false, false),
      ],
      vec![RemoteRule {
        path: "config/cache".into(),
        kind: RuleKind::Dir,
        state: RuleState::Ignore,
      }],
    );

    let entries = detail(&root, &baseline, &release).await;
    assert_eq!(
      find(&entries, "config/edited.toml").unwrap().class,
      ChangeClass::Modified
    );
    let once = find(&entries, "config/once.toml").unwrap();
    assert_eq!(once.class, ChangeClass::OnceChanged);
    assert!(!once.counted);
    assert_eq!(
      find(&entries, "config/deleted.toml").unwrap().class,
      ChangeClass::Deleted
    );
    let toggled = find(&entries, "mods/toggle.jar").unwrap();
    assert_eq!(toggled.class, ChangeClass::Toggled);
    assert!(toggled.counted && toggled.toggled);
    let added = find(&entries, "mods/new.jar").unwrap();
    assert_eq!(added.class, ChangeClass::Added);
    assert!(added.counted);
    let config = find(&entries, "config/new.toml").unwrap();
    assert_eq!(config.class, ChangeClass::Added);
    assert!(!config.counted);
    assert_eq!(
      find(&entries, "saves/world.dat").unwrap().class,
      ChangeClass::AddedExcluded
    );
    assert_eq!(
      find(&entries, "config/cache/new.toml").unwrap().class,
      ChangeClass::AddedExcluded
    );
    assert!(find(&entries, "mods/toggle.jar.disabled").is_none());

    let edited = &baseline
      .files
      .iter()
      .find(|file| file.path == "config/edited.toml")
      .unwrap();
    ignores::add(&root, &baseline, "config/edited.toml", &edited.sha1, None).unwrap();
    let entries = detail(&root, &baseline, &release).await;
    assert!(find(&entries, "config/edited.toml").is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_toggle_is_judged_against_the_release_not_the_baseline() {
    // The baseline recorded the player's switched-off state; the release
    // says the file ships enabled.
    let root = instance(&[("mods/a.jar.disabled", "a"), ("mods/b.jar", "b")]);
    let baseline = baseline_with(vec![
      BaselineFile::managed("mods/a.jar", key("a"), true),
      BaselineFile::managed("mods/b.jar", key("b"), true),
    ]);
    let release = release_of(
      vec![
        release_file("mods/a.jar", "a", false, false),
        release_file("mods/b.jar", "b", true, false),
      ],
      vec![],
    );
    let entries = detail(&root, &baseline, &release).await;
    assert_eq!(
      find(&entries, "mods/a.jar").unwrap().class,
      ChangeClass::Toggled
    );
    assert_eq!(
      find(&entries, "mods/b.jar").unwrap().class,
      ChangeClass::Toggled
    );
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_toggle_only_the_release_sees_carries_the_baseline_and_can_be_ignored() {
    // The baseline recorded the player's switched-off state, so the old local
    // change list has nothing here; only the release says it ships enabled.
    let root = instance(&[("mods/a.jar.disabled", "a")]);
    let baseline = baseline_with(vec![BaselineFile::managed("mods/a.jar", key("a"), true)]);
    let release = release_of(vec![release_file("mods/a.jar", "a", false, false)], vec![]);
    release::write(&root, &release).await.unwrap();
    assert!(local_changes(&root, &baseline, false)
      .await
      .unwrap()
      .is_empty());

    let entries = detail(&root, &baseline, &release).await;
    let toggled = find(&entries, "mods/a.jar").unwrap();
    assert_eq!(toggled.class, ChangeClass::Toggled);
    assert!(toggled.disabled);
    let base = toggled.baseline.clone().unwrap();
    assert_eq!(base, key("a"));
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);

    ignores::add(&root, &baseline, "mods/a.jar", &base.sha1, None).unwrap();
    assert!(find(&detail(&root, &baseline, &release).await, "mods/a.jar").is_none());
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_file_that_only_differs_in_line_endings_is_neither_listed_nor_counted() {
    let root = instance(&[("config/a.toml", "a=1\r\nb=2\r\n")]);
    let baseline = baseline_of(&[("config/a.toml", "a=1\nb=2\n")]);
    base_store::store(&root, &baseline.files[0], b"a=1\nb=2\n").unwrap();
    let release = Release::from_baseline(&baseline, "7");
    assert!(detail(&root, &baseline, &release).await.is_empty());
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn ignored_spots_hide_a_change_only_when_they_cover_all_of_it() {
    let local = TEN.replace("b=2", "b=20").replace("i=9", "i=90");
    let root = instance(&[("config/a.toml", &local)]);
    let baseline = baseline_of(&[("config/a.toml", TEN)]);
    base_store::store(&root, &baseline.files[0], TEN.as_bytes()).unwrap();
    let release = Release::from_baseline(&baseline, "7");
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);

    ignore_range(&root, &baseline, 1, 2);
    let entries = detail(&root, &baseline, &release).await;
    let entry = find(&entries, "config/a.toml").unwrap();
    assert_eq!(entry.class, ChangeClass::Modified);
    let hunks = entry.hunks.as_ref().unwrap();
    assert_eq!(hunks.len(), 1);
    assert_eq!((hunks[0].edits[0].start, hunks[0].edits[0].end), (8, 9));
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);

    ignore_range(&root, &baseline, 8, 9);
    assert!(detail(&root, &baseline, &release).await.is_empty());
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn files_under_the_hard_blacklist_are_not_listed_or_counted() {
    let root = instance(&[("mods/.connector/lib.jar", "x"), ("mods/new.jar", "n")]);
    let baseline = baseline_of(&[]);
    let release = Release::from_baseline(&baseline, "7");
    let entries = detail(&root, &baseline, &release).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "mods/new.jar");
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_suggested_path_the_last_release_mentioned_is_added_not_excluded() {
    let root = instance(&[
      ("saves/old.dat", "old"),
      ("saves/new.dat", "new"),
      ("fonts/new.ttf", "font"),
    ]);
    let baseline = baseline_of(&[]);
    let release = release_of(
      vec![release_file("saves/old.dat", "old", false, false)],
      vec![],
    );
    let entries = detail(&root, &baseline, &release).await;
    assert_eq!(
      find(&entries, "saves/new.dat").unwrap().class,
      ChangeClass::Added
    );
    assert_eq!(
      find(&entries, "fonts/new.ttf").unwrap().class,
      ChangeClass::AddedExcluded
    );
    assert!(find(&entries, "saves/old.dat").is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_dormant_baseline_entry_that_reappears_is_added_and_can_be_ignored() {
    let root = instance(&[("mods/a.jar", "a"), ("mods/dormant.jar", "d")]);
    let baseline = baseline_with(vec![
      BaselineFile::managed("mods/a.jar", key("a"), false),
      BaselineFile::dormant("mods/dormant.jar", Some(key("d")), false),
    ]);
    let release = Release::from_baseline(&baseline, "7");
    let entries = detail(&root, &baseline, &release).await;
    let added = find(&entries, "mods/dormant.jar").unwrap();
    assert_eq!(added.class, ChangeClass::Added);
    assert!(added.counted);
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);

    let listed = export::list_files_at(&root, &name_of(&root)).unwrap();
    ignore_added_file(&root, &baseline, &release, &listed, "mods/dormant.jar")
      .await
      .unwrap();
    assert!(find(
      &detail(&root, &baseline, &release).await,
      "mods/dormant.jar"
    )
    .is_none());
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn without_a_release_record_added_mods_are_counted() {
    let root = instance(&[
      ("mods/a.jar", "a"),
      ("mods/extra.jar", "e"),
      ("config/x.toml", "x"),
    ]);
    let baseline = baseline_of(&[("mods/a.jar", "a")]);
    assert!(release::read(&root).is_none());
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_release_record_of_another_version_is_treated_as_missing() {
    let root = instance(&[("mods/a.jar", "a")]);
    let baseline = baseline_of(&[("mods/a.jar", "a")]);
    let mut stale = release_of(
      vec![release_file("mods/ghost.jar", "g", false, false)],
      vec![],
    );
    stale.version_id = "6".into();
    release::write(&root, &stale).await.unwrap();
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    stale.version_id = "7".into();
    release::write(&root, &stale).await.unwrap();
    // Now the ghost is a deleted file of the current release, and `a.jar` is new.
    assert_eq!(count(&root, &baseline).await.unwrap(), 2);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn the_count_and_the_list_agree_on_which_added_files_count() {
    let root = instance(&[
      ("mods/new.jar", "n"),
      ("mods/off.jar.disabled", "o"),
      ("mods/VoxelMods/v.json", "v"),
      ("mods/.connector/c.jar", "c"),
      ("resourcepacks/p.zip", "p"),
      ("shaderpacks/s.zip", "s"),
      ("config/x.toml", "x"),
      ("saves/w.dat", "w"),
    ]);
    let baseline = baseline_of(&[]);
    let release = Release::from_baseline(&baseline, "7");
    let counted_added = |entries: &[ChangeDetailEntry]| -> BTreeSet<String> {
      entries
        .iter()
        .filter(|entry| entry.counted && entry.class == ChangeClass::Added)
        .map(|entry| entry.path.clone())
        .collect()
    };
    let from_list = counted_added(&detail(&root, &baseline, &release).await);
    let listed = export::list_content_files(&root, &name_of(&root)).unwrap();
    let ignored = ignores::read_valid_for_release(&root, &baseline, &release);
    let from_count = counted_added(
      &compute_changes(
        &root,
        &baseline,
        &release,
        &ignored,
        &listed.all,
        &listed.suggested,
        false,
      )
      .await
      .unwrap(),
    );
    assert_eq!(from_list, from_count);
    assert_eq!(
      from_list,
      BTreeSet::from([
        "mods/new.jar".to_string(),
        "mods/off.jar".to_string(),
        "resourcepacks/p.zip".to_string(),
        "shaderpacks/s.zip".to_string(),
      ])
    );
    assert_eq!(count(&root, &baseline).await.unwrap(), 4);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn an_added_file_ignore_keeps_the_disk_spelling_and_needs_an_added_file() {
    let root = instance(&[("mods/Foo.jar", "f"), ("mods/a.jar", "a")]);
    let baseline = baseline_of(&[("mods/a.jar", "a")]);
    let release = Release::from_baseline(&baseline, "7");
    let listed = export::list_files_at(&root, &name_of(&root)).unwrap();

    for refused in ["mods/a.jar", "mods/missing.jar"] {
      assert_eq!(
        ignore_added_file(&root, &baseline, &release, &listed, refused).await,
        Err(IgnoreError::Mismatch)
      );
    }
    ignore_added_file(&root, &baseline, &release, &listed, "mods/foo.jar.disabled")
      .await
      .unwrap();
    assert_eq!(
      ignores::read(&root).added,
      BTreeSet::from(["mods/Foo.jar".to_string()])
    );
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    // Ignoring it twice is stale: it is no longer an added file in the list.
    assert_eq!(
      ignore_added_file(&root, &baseline, &release, &listed, "mods/Foo.jar").await,
      Err(IgnoreError::Mismatch)
    );
    ignores::undo(&root, &baseline, "MODS/FOO.JAR", None).unwrap();
    assert_eq!(count(&root, &baseline).await.unwrap(), 1);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn added_entries_carry_the_nearest_ignore_or_delete_rule() {
    let root = instance(&[
      ("config/ignored/a.toml", "i"),
      ("config/deleted/a.toml", "d"),
      ("other/free.toml", "f"),
    ]);
    let baseline = baseline_of(&[]);
    let release = release_of(
      vec![],
      vec![
        RemoteRule {
          path: "config".into(),
          kind: RuleKind::Dir,
          state: RuleState::Ignore,
        },
        RemoteRule {
          path: "config/deleted".into(),
          kind: RuleKind::Dir,
          state: RuleState::Delete,
        },
        RemoteRule {
          path: "config/ignored/a.toml".into(),
          kind: RuleKind::File,
          state: RuleState::Ignore,
        },
      ],
    );
    let entries = detail(&root, &baseline, &release).await;
    assert_eq!(
      find(&entries, "config/ignored/a.toml").unwrap().rule,
      Some(RuleState::Ignore)
    );
    assert_eq!(
      find(&entries, "config/deleted/a.toml").unwrap().rule,
      Some(RuleState::Delete)
    );
    assert_eq!(find(&entries, "other/free.toml").unwrap().rule, None);
    assert_eq!(count(&root, &baseline).await.unwrap(), 0);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn schema_one_records_are_invisible_to_lists_details_and_counts() {
    let root = instance(&[("config/a.toml", "local"), ("mods/new.jar", "new")]);
    let baseline = baseline_of(&[("config/a.toml", "base")]);
    let release = Release::from_baseline(&baseline, "7");
    let old = serde_json::json!({
      "schema": 1,
      "files": {
        "config/a.toml": {
          "baseSha1": baseline.files[0].sha1,
          "whole": true,
          "regions": []
        }
      },
      "added": ["mods/new.jar"]
    });
    fs::create_dir_all(root.join(".ustbl-update")).unwrap();
    fs::write(
      root.join(".ustbl-update/ignores.json"),
      serde_json::to_vec_pretty(&old).unwrap(),
    )
    .unwrap();

    assert!(ignores::read(&root).is_empty());
    assert!(ignores::list_for_release(&root, &baseline, &release).is_empty());
    let entries = detail(&root, &baseline, &release).await;
    assert_eq!(
      find(&entries, "config/a.toml").unwrap().class,
      ChangeClass::Modified
    );
    assert_eq!(
      find(&entries, "mods/new.jar").unwrap().class,
      ChangeClass::Added
    );
    assert_eq!(count(&root, &baseline).await.unwrap(), 2);

    ignores::add(
      &root,
      &baseline,
      "config/a.toml",
      &baseline.files[0].sha1,
      None,
    )
    .unwrap();
    let overwritten = ignores::read(&root);
    assert_eq!(overwritten.schema, 2);
    assert!(overwritten.files.contains_key("config/a.toml"));
    assert!(overwritten.added.is_empty());
    let _ = fs::remove_dir_all(root);
  }
}
