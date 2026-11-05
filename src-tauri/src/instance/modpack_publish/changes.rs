//! How the files on disk differ from the line's current release, as the
//! publish tree marks them. Deleted files are not here: the tree already shows
//! them as ghosts.

use super::commands::PreviousRelease;
use super::defaults::DiskFile;
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::base_store;
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::helpers::modpack::text::{self, normalize_text, NotTextReason, MAX_TEXT_SIZE};
use crate::instance::models::misc::InstanceError;
use crate::instance::modpack_update::change_core::{self, ChangeClass, Judge, Spec};
use crate::instance::modpack_update::changes::logical_disk_path;
use crate::instance::modpack_update::diff::DISABLED_SUFFIX;
use crate::instance::modpack_update::ignores;
use crate::instance::modpack_update::local_scan::LocalScanCache;
use crate::instance::modpack_update::text_diff::{
  lines_of, unified_diff_hiding, DiffHunk, CONTEXT_LINES,
};
use serde::Serialize;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishChange {
  /// The enabled name, as the tree lists the file.
  pub path: String,
  /// Never `Deleted` or `AddedExcluded`: the tree shows deleted files as
  /// ghosts, and every unlisted file is just added.
  pub kind: ChangeClass,
  /// The previous release's content; absent for an added file.
  pub previous_sha1: Option<String>,
  pub previous_size: Option<u64>,
  pub local_size: Option<u64>,
  /// The player-side ignore list names the file. Only a mark: publishing
  /// follows the tree.
  pub local_ignored: bool,
  /// The verified platform version used to recover a missing base copy.
  pub previous_version_id: Option<String>,
  pub previous_project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishBaseCandidate {
  pub path: String,
  pub sha1: String,
  pub size: u64,
}

/// Select the one remote object eligible for recovering a missing publish base.
/// The caller performs the network operation only when this returns an index.
pub fn select_publish_base(
  has_base: bool,
  path: &str,
  sha1: &str,
  local_size: Option<u64>,
  remote: &[PublishBaseCandidate],
) -> Option<usize> {
  if has_base
    || !text::is_text_path(path, &[])
    || local_size.is_none_or(|size| size > MAX_TEXT_SIZE as u64)
  {
    return None;
  }
  remote.iter().position(|file| {
    file.path.eq_ignore_ascii_case(path)
      && file.sha1.eq_ignore_ascii_case(sha1)
      && file.size <= MAX_TEXT_SIZE as u64
  })
}

fn folded(path: &str) -> String {
  path.to_lowercase()
}

/// One entry per changed file. A file whose size differs from the previous
/// release is changed without being hashed; an added file is not read at all.
/// Hashes come from `cache`, which the local-change scan shares.
pub fn classify(
  root: &Path,
  cache: &Mutex<LocalScanCache>,
  files: &[DiskFile],
  previous: Option<&PreviousRelease>,
  locally_ignored: &HashSet<String>,
) -> io::Result<Vec<PublishChange>> {
  let entries = change_core::classify(&Spec {
    root,
    cache,
    release: previous.map_or(&[], |release| &release.files),
    files,
    judge: Judge::Release,
  })?;
  Ok(
    entries
      .into_iter()
      .map(|entry| PublishChange {
        local_ignored: locally_ignored.contains(&folded(&entry.path)),
        path: entry.path,
        kind: entry.class,
        previous_sha1: entry.baseline.as_ref().map(|key| key.sha1.clone()),
        previous_size: entry.baseline.as_ref().map(|key| key.size),
        local_size: entry.size,
        previous_version_id: previous.and_then(|release| release.version_id.clone()),
        previous_project_id: previous.and_then(|release| release.project_id.clone()),
      })
      .collect(),
  )
}
/// Paths the player ignored: records of managed files and of added files, in
/// the form the tree lists them. Empty without a baseline.
pub fn locally_ignored(root: &Path, baseline: Option<&ModpackBaseline>) -> HashSet<String> {
  let Some(baseline) = baseline else {
    return HashSet::new();
  };
  let ignored = ignores::read_valid(root, baseline);
  ignored
    .files
    .keys()
    .map(|path| folded(path))
    .chain(
      ignored
        .added
        .iter()
        .map(|path| folded(logical_disk_path(path))),
    )
    .collect()
}

fn is_sha1(value: &str) -> bool {
  value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Why the publish tree has no line diff to show for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishDiffUnavailable {
  NotTextType,
  TooLarge,
  Binary,
  NotUtf8,
  /// The file on disk is gone or cannot be read.
  LocalMissing,
  /// The previous release's text is not kept locally.
  BaseMissing,
}

impl From<NotTextReason> for PublishDiffUnavailable {
  fn from(reason: NotTextReason) -> Self {
    match reason {
      NotTextReason::NotTextType => Self::NotTextType,
      NotTextReason::TooLarge => Self::TooLarge,
      NotTextReason::Binary => Self::Binary,
      NotTextReason::NotUtf8 => Self::NotUtf8,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishFileDiff {
  /// Empty when only line endings, a BOM or a properties timestamp differ.
  pub hunks: Option<Vec<DiffHunk>>,
  pub unavailable: Option<PublishDiffUnavailable>,
}

impl PublishFileDiff {
  fn without(reason: PublishDiffUnavailable) -> Self {
    Self {
      hunks: None,
      unavailable: Some(reason),
    }
  }
}

/// The line diff of a file from the previous release's text to the one on
/// disk, or the reason it cannot be drawn. The checks run in this order: the
/// file on disk exists, its type and size, its content (NUL byte, UTF-8), the
/// previous text is kept, that text is text. The hash names a file inside the
/// instance's base store, so anything but a SHA-1 is refused, as is a path that
/// leaves the instance.
pub fn file_diff(root: &Path, path: &str, previous_sha1: &str) -> USTBLResult<PublishFileDiff> {
  use PublishDiffUnavailable::{BaseMissing, LocalMissing};
  safe_relative(path)?;
  if !is_sha1(previous_sha1) {
    return Err(InstanceError::ModpackUpdateUnsupported.into());
  }
  let enabled = root.join(path);
  let disabled = root.join(format!("{path}{DISABLED_SUFFIX}"));
  let file = if enabled.is_file() {
    enabled
  } else if disabled.is_file() {
    disabled
  } else {
    return Ok(PublishFileDiff::without(LocalMissing));
  };
  let Ok(meta) = std::fs::metadata(&file) else {
    return Ok(PublishFileDiff::without(LocalMissing));
  };
  // Size is judged before reading, so a huge file is never loaded.
  if let Err(reason) = text::size_reason(path, meta.len()) {
    return Ok(PublishFileDiff::without(reason.into()));
  }
  let Ok(local) = std::fs::read(&file) else {
    return Ok(PublishFileDiff::without(LocalMissing));
  };
  if let Err(reason) = text::text_reason(path, &local) {
    return Ok(PublishFileDiff::without(reason.into()));
  }
  let Ok(Some(before)) = base_store::load(root, &previous_sha1.to_lowercase()) else {
    return Ok(PublishFileDiff::without(BaseMissing));
  };
  let (Some(old), Some(new)) = (normalize_text(path, &before), normalize_text(path, &local)) else {
    let reason = text::text_reason(path, &before).err();
    return Ok(PublishFileDiff::without(
      reason.map_or(BaseMissing, Into::into),
    ));
  };
  Ok(PublishFileDiff {
    hunks: Some(unified_diff_hiding(
      &lines_of(&old),
      &lines_of(&new),
      CONTEXT_LINES,
      &|_| false,
    )),
    unavailable: None,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::instance::modpack_publish::commands::PreviousFile;
  use crate::instance::modpack_update::change_core::scan_targets;
  use crate::instance::modpack_update::diff::FileKey;
  use sha1::{Digest, Sha1};
  use std::collections::{BTreeMap, BTreeSet};
  use std::fs;
  use std::path::PathBuf;

  fn sha1_of(content: &str) -> String {
    hex::encode(Sha1::digest(content.as_bytes()))
  }

  fn root(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-publish-changes-{}", uuid::Uuid::new_v4()));
    for (path, content) in files {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    root
  }

  fn disk(path: &str, disabled: bool) -> DiskFile {
    DiskFile {
      path: path.into(),
      disabled,
      conflict: false,
    }
  }

  fn listed(path: &str, content: &str, disabled: bool, once: bool) -> PreviousFile {
    PreviousFile {
      path: path.into(),
      sha1: sha1_of(content),
      size: content.len() as u64,
      disabled,
      once,
    }
  }

  fn release(files: Vec<PreviousFile>) -> PreviousRelease {
    PreviousRelease {
      files,
      rules: Vec::new(),
      rules_manifest: true,
      version_id: None,
      project_id: None,
    }
  }

  fn candidate(path: &str, sha1: &str, size: u64) -> PublishBaseCandidate {
    PublishBaseCandidate {
      path: path.into(),
      sha1: sha1.into(),
      size,
    }
  }

  #[test]
  fn publish_base_selection_skips_invalid_and_unsupported_local_inputs() {
    let sha1 = "a".repeat(40);
    let remote = [candidate("config/a.cfg", &sha1, 3)];
    assert_eq!(
      select_publish_base(false, "../a.cfg", &sha1, Some(3), &remote),
      None
    );
    assert_eq!(
      select_publish_base(false, "mods/a.jar", &sha1, Some(3), &remote),
      None
    );
    assert_eq!(
      select_publish_base(
        false,
        "config/a.cfg",
        &sha1,
        Some(MAX_TEXT_SIZE as u64 + 1),
        &remote
      ),
      None
    );
  }

  #[test]
  fn publish_base_selection_skips_cached_and_oversized_remote_files() {
    let sha1 = "a".repeat(40);
    let remote = [candidate("config/a.cfg", &sha1, 3)];
    assert_eq!(
      select_publish_base(true, "config/a.cfg", &sha1, Some(3), &remote),
      None
    );
    let oversized = [candidate("config/a.cfg", &sha1, MAX_TEXT_SIZE as u64 + 1)];
    assert_eq!(
      select_publish_base(false, "config/a.cfg", &sha1, Some(3), &oversized),
      None
    );
  }

  #[test]
  fn publish_base_selection_requires_case_insensitive_path_and_matching_sha1() {
    let sha1 = "a".repeat(40);
    let remote = [candidate("Config/A.CFG", &sha1, 3)];
    assert_eq!(
      select_publish_base(false, "config/a.cfg", &sha1, Some(3), &remote),
      Some(0)
    );
    assert_eq!(
      select_publish_base(false, "config/a.cfg", &"b".repeat(40), Some(3), &remote),
      None
    );
  }

  fn run(
    root: &Path,
    files: &[DiskFile],
    previous: Option<&PreviousRelease>,
    ignored: &HashSet<String>,
  ) -> Vec<PublishChange> {
    let cache = Mutex::new(LocalScanCache::default());
    classify(root, &cache, files, previous, ignored).unwrap()
  }

  fn kinds(changes: &[PublishChange]) -> Vec<(String, ChangeClass)> {
    changes
      .iter()
      .map(|change| (change.path.clone(), change.kind))
      .collect()
  }

  #[test]
  fn a_file_with_other_content_is_modified_whether_or_not_the_size_differs() {
    let root = root(&[
      ("config/longer.cfg", "version=2222"),
      ("config/same-size.cfg", "version=2"),
    ]);
    let previous = release(vec![
      listed("config/longer.cfg", "v=1", false, false),
      listed("config/same-size.cfg", "version=1", false, false),
    ]);
    let files = [
      disk("config/longer.cfg", false),
      disk("config/same-size.cfg", false),
    ];
    let changes = run(&root, &files, Some(&previous), &HashSet::new());
    assert_eq!(
      kinds(&changes),
      vec![
        ("config/longer.cfg".to_string(), ChangeClass::Modified),
        ("config/same-size.cfg".to_string(), ChangeClass::Modified),
      ]
    );
    assert_eq!(changes[0].previous_size, Some(3));
    assert_eq!(changes[0].local_size, Some(12));
    assert_eq!(changes[0].previous_sha1, Some(sha1_of("v=1")));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_file_the_release_lacks_is_added_and_only_added_files_are_not_scanned() {
    let root = root(&[("mods/new.jar", "new"), ("mods/old.jar", "old")]);
    let previous = release(vec![listed("mods/old.jar", "old", false, false)]);
    let files = [disk("mods/new.jar", false), disk("mods/old.jar", false)];
    let targets = scan_targets(&files, &previous.files);
    assert_eq!(targets.keys().collect::<Vec<_>>(), vec!["mods/old.jar"]);
    let changes = run(&root, &files, Some(&previous), &HashSet::new());
    assert_eq!(
      kinds(&changes),
      vec![("mods/new.jar".to_string(), ChangeClass::Added)]
    );
    assert_eq!(changes[0].previous_sha1, None);
    assert_eq!(changes[0].local_size, Some(3));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn same_content_with_another_enabled_state_is_toggled() {
    let root = root(&[("mods/off.jar.disabled", "jar"), ("mods/on.jar", "jar")]);
    let previous = release(vec![
      listed("mods/off.jar", "jar", false, false),
      listed("mods/on.jar", "jar", true, false),
    ]);
    let files = [disk("mods/off.jar", true), disk("mods/on.jar", false)];
    let changes = run(&root, &files, Some(&previous), &HashSet::new());
    assert_eq!(
      kinds(&changes),
      vec![
        ("mods/off.jar".to_string(), ChangeClass::Toggled),
        ("mods/on.jar".to_string(), ChangeClass::Toggled),
      ]
    );
    assert_eq!(changes[0].local_size, Some(3));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_changed_first_install_file_is_its_own_kind() {
    let root = root(&[
      ("config/changed.cfg", "b=2"),
      ("config/same.cfg", "a=1"),
      ("config/other-size.cfg", "a=1111"),
    ]);
    let previous = release(vec![
      listed("config/changed.cfg", "b=1", false, true),
      listed("config/same.cfg", "a=1", false, true),
      listed("config/other-size.cfg", "a=1", false, true),
    ]);
    let files = [
      disk("config/changed.cfg", false),
      disk("config/same.cfg", false),
      disk("config/other-size.cfg", false),
    ];
    let changes = run(&root, &files, Some(&previous), &HashSet::new());
    assert_eq!(
      kinds(&changes),
      vec![
        ("config/changed.cfg".to_string(), ChangeClass::OnceChanged),
        (
          "config/other-size.cfg".to_string(),
          ChangeClass::OnceChanged
        ),
      ]
    );
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn files_equal_to_the_release_are_not_listed() {
    let root = root(&[("config/a.cfg", "a=1"), ("mods/b.jar.disabled", "b")]);
    let previous = release(vec![
      listed("config/a.cfg", "a=1", false, false),
      listed("mods/b.jar", "b", true, false),
    ]);
    let files = [disk("config/a.cfg", false), disk("mods/b.jar", true)];
    assert!(run(&root, &files, Some(&previous), &HashSet::new()).is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_new_project_or_a_line_without_a_version_has_only_additions() {
    let root = root(&[("config/a.cfg", "a=1"), ("mods/b.jar", "b")]);
    let files = [disk("config/a.cfg", false), disk("mods/b.jar", false)];
    let empty = release(Vec::new());
    for previous in [None, Some(&empty)] {
      let changes = run(&root, &files, previous, &HashSet::new());
      assert_eq!(
        kinds(&changes),
        vec![
          ("config/a.cfg".to_string(), ChangeClass::Added),
          ("mods/b.jar".to_string(), ChangeClass::Added),
        ]
      );
    }
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn paths_match_the_release_without_regard_to_case() {
    let root = root(&[("Config/A.cfg", "a=2")]);
    let previous = release(vec![listed("config/a.cfg", "a=1", false, false)]);
    let files = [disk("Config/A.cfg", false)];
    let changes = run(&root, &files, Some(&previous), &HashSet::new());
    assert_eq!(
      kinds(&changes),
      vec![("Config/A.cfg".to_string(), ChangeClass::Modified)]
    );
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_players_ignore_is_a_mark_on_the_entry() {
    let root = root(&[("config/a.cfg", "a=2"), ("config/b.cfg", "b=2")]);
    let previous = release(vec![listed("config/a.cfg", "a=1", false, false)]);
    let files = [disk("config/a.cfg", false), disk("config/b.cfg", false)];
    let ignored = HashSet::from(["config/a.cfg".to_string(), "config/b.cfg".to_string()]);
    let changes = run(&root, &files, Some(&previous), &ignored);
    assert!(changes.iter().all(|change| change.local_ignored));
    let plain = run(&root, &files, Some(&previous), &HashSet::new());
    assert!(plain.iter().all(|change| !change.local_ignored));
    let _ = fs::remove_dir_all(root);
  }

  fn baseline_with(path: &str, sha1: &str) -> ModpackBaseline {
    let origin = InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
      pack_format: None,
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
  fn the_ignore_list_covers_managed_and_added_files_and_needs_a_baseline() {
    let root = root(&[]);
    let baseline = baseline_with("config/Managed.cfg", "aa");
    let stored = ignores::Ignores {
      files: BTreeMap::from([(
        "config/Managed.cfg".to_string(),
        ignores::IgnoreEntry {
          base_sha1: "aa".into(),
          whole: true,
          regions: Vec::new(),
        },
      )]),
      added: BTreeSet::from(["mods/added.jar.disabled".to_string()]),
      ..Default::default()
    };
    ignores::write(&root, &stored).unwrap();
    assert_eq!(
      locally_ignored(&root, Some(&baseline)),
      HashSet::from([
        "config/managed.cfg".to_string(),
        "mods/added.jar".to_string()
      ])
    );
    assert!(locally_ignored(&root, None).is_empty());
    let _ = fs::remove_dir_all(root);
  }

  fn keep_base(root: &Path, path: &str, content: &str) -> String {
    let file = BaselineFile::managed(
      path,
      FileKey {
        sha1: sha1_of(content),
        size: content.len() as u64,
      },
      false,
    );
    base_store::store(root, &file, content.as_bytes()).unwrap();
    file.sha1
  }

  #[test]
  fn the_diff_runs_from_the_kept_previous_text_to_the_disk_text() {
    let root = root(&[("config/a.cfg", "a=1\r\nb=3\r\n")]);
    let sha1 = keep_base(&root, "config/a.cfg", "a=1\nb=2\n");
    let hunks = file_diff(&root, "config/a.cfg", &sha1)
      .unwrap()
      .hunks
      .unwrap();
    assert_eq!(hunks.len(), 1);
    let texts: Vec<_> = hunks[0]
      .lines
      .iter()
      .map(|line| line.text.as_str())
      .collect();
    assert_eq!(texts, vec!["a=1", "b=2", "b=3"]);
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn text_that_differs_only_in_normalized_noise_has_no_hunks() {
    let root = root(&[("config/a.cfg", "a=1\r\n")]);
    let sha1 = keep_base(&root, "config/a.cfg", "a=1\n");
    let hunks = file_diff(&root, "config/a.cfg", &sha1)
      .unwrap()
      .hunks
      .unwrap();
    assert!(hunks.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_reads_the_disabled_variant_of_a_switched_off_file() {
    let root = root(&[("mods/a.properties.disabled", "k=2\n")]);
    let sha1 = keep_base(&root, "mods/a.properties", "k=1\n");
    let hunks = file_diff(&root, "mods/a.properties", &sha1)
      .unwrap()
      .hunks
      .unwrap();
    assert_eq!(hunks.len(), 1);
    let _ = fs::remove_dir_all(root);
  }

  fn why(root: &Path, path: &str, sha1: &str) -> Option<PublishDiffUnavailable> {
    let diff = file_diff(root, path, sha1).unwrap();
    assert!(diff.hunks.is_none(), "{path}");
    diff.unavailable
  }

  #[test]
  fn the_diff_says_when_the_previous_text_is_not_kept() {
    let root = root(&[("config/a.cfg", "a=2\n")]);
    assert_eq!(
      why(&root, "config/a.cfg", &sha1_of("a=1\n")),
      Some(PublishDiffUnavailable::BaseMissing)
    );
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_names_the_reason_a_file_is_not_text_or_missing() {
    let root = root(&[
      ("mods/a.jar", "jar"),
      ("config/b.cfg", "x"),
      ("config/zero.cfg", "a\0b"),
    ]);
    fs::write(root.join("config/latin.cfg"), b"a=\xE4").unwrap();
    let sha1 = keep_base(&root, "config/b.cfg", "x");
    assert_eq!(
      why(&root, "mods/a.jar", &sha1),
      Some(PublishDiffUnavailable::NotTextType)
    );
    assert_eq!(
      why(&root, "config/zero.cfg", &sha1),
      Some(PublishDiffUnavailable::Binary)
    );
    assert_eq!(
      why(&root, "config/latin.cfg", &sha1),
      Some(PublishDiffUnavailable::NotUtf8)
    );
    assert_eq!(
      why(&root, "config/gone.cfg", &sha1),
      Some(PublishDiffUnavailable::LocalMissing)
    );
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_names_a_text_file_over_the_size_limit() {
    let big = "a".repeat(MAX_TEXT_SIZE + 1);
    let root = root(&[("config/big.cfg", &big)]);
    let sha1 = keep_base(&root, "config/big.cfg", "a");
    assert_eq!(
      why(&root, "config/big.cfg", &sha1),
      Some(PublishDiffUnavailable::TooLarge)
    );
    let _ = fs::remove_dir_all(root);
  }
  #[test]
  fn the_diff_refuses_paths_outside_the_instance_and_names_that_are_not_a_sha1() {
    let root = root(&[("config/a.cfg", "a")]);
    let sha1 = keep_base(&root, "config/a.cfg", "a");
    for path in [
      "../outside.cfg",
      "config/../../outside.cfg",
      "/etc/passwd",
      "C:/Windows/win.ini",
      "config\\a.cfg",
      "",
    ] {
      assert!(file_diff(&root, path, &sha1).is_err(), "{path}");
    }
    for name in ["", "../base/x", "zz", &sha1[..39], &format!("{sha1}0")] {
      assert!(file_diff(&root, "config/a.cfg", name).is_err(), "{name}");
    }
    let _ = fs::remove_dir_all(root);
  }

  /// `cargo test --lib publish::changes::tests::timing -- --ignored --nocapture`
  /// with USTBL_BENCH_FILES set: a synthetic instance, cold then warm.
  #[test]
  #[ignore]
  fn timing() {
    let count: usize = std::env::var("USTBL_BENCH_FILES")
      .ok()
      .and_then(|value| value.parse().ok())
      .unwrap_or(5000);
    let root = std::env::temp_dir().join(format!("ustbl-bench-{}", uuid::Uuid::new_v4()));
    let mut files = Vec::new();
    let mut listed_files = Vec::new();
    for index in 0..count {
      let path = format!("mods/dir{}/file{index}.jar", index % 50);
      let content = format!("{index:>8}").repeat(8192);
      let target = root.join(&path);
      fs::create_dir_all(target.parent().unwrap()).unwrap();
      fs::write(&target, &content).unwrap();
      if index % 10 != 0 {
        listed_files.push(PreviousFile {
          path: path.clone(),
          sha1: sha1_of(if index % 7 == 0 { "other" } else { &content }),
          size: content.len() as u64,
          disabled: false,
          once: false,
        });
      }
      files.push(disk(&path, false));
    }
    let previous = release(listed_files);
    let cache = Mutex::new(LocalScanCache::default());
    for round in ["cold", "warm"] {
      let started = std::time::Instant::now();
      let changes = classify(&root, &cache, &files, Some(&previous), &HashSet::new()).unwrap();
      println!(
        "{round}: {} files, {} changes, {:?}",
        count,
        changes.len(),
        started.elapsed()
      );
    }
    let _ = fs::remove_dir_all(root);
  }
}
