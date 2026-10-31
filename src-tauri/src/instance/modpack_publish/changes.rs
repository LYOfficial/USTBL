//! How the files on disk differ from the line's current release, as the
//! publish tree marks them. Deleted files are not here: the tree already shows
//! them as ghosts.

use super::commands::{PreviousFile, PreviousRelease};
use super::defaults::DiskFile;
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::base_store;
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::helpers::modpack::text::{normalize_text, MAX_TEXT_SIZE};
use crate::instance::models::misc::InstanceError;
use crate::instance::modpack_update::changes::logical_disk_path;
use crate::instance::modpack_update::diff::{FileKey, LocalState, DISABLED_SUFFIX};
use crate::instance::modpack_update::ignores;
use crate::instance::modpack_update::local_scan::{collect_states_cached, LocalScanCache};
use crate::instance::modpack_update::text_diff::{
  lines_of, unified_diff_hiding, DiffHunk, CONTEXT_LINES,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PublishChangeKind {
  /// The previous release lists it with other content.
  Modified,
  /// The previous release does not list it.
  Added,
  /// Same content, other enabled/disabled state.
  Toggled,
  /// A first-install file of the previous release whose content differs.
  OnceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishChange {
  /// The enabled name, as the tree lists the file.
  pub path: String,
  pub kind: PublishChangeKind,
  /// The previous release's content; absent for an added file.
  pub previous_sha1: Option<String>,
  pub previous_size: Option<u64>,
  pub local_size: Option<u64>,
  /// The player-side ignore list names the file. Only a mark: publishing
  /// follows the tree.
  pub local_ignored: bool,
}

fn folded(path: &str) -> String {
  path.to_lowercase()
}

/// The files to look at on disk, with the content the previous release lists
/// for them. An added file has no entry, so it is never read.
pub fn scan_targets(
  files: &[DiskFile],
  previous: Option<&PreviousRelease>,
) -> BTreeMap<String, FileKey> {
  let Some(previous) = previous else {
    return BTreeMap::new();
  };
  let listed: HashMap<String, &PreviousFile> = previous
    .files
    .iter()
    .map(|file| (folded(&file.path), file))
    .collect();
  files
    .iter()
    .filter_map(|file| {
      let found = listed.get(&folded(&file.path))?;
      Some((
        file.path.clone(),
        FileKey {
          sha1: found.sha1.clone(),
          size: found.size,
        },
      ))
    })
    .collect()
}

fn local_size(root: &Path, file: &DiskFile) -> Option<u64> {
  let name = if file.disabled {
    format!("{}{DISABLED_SUFFIX}", file.path)
  } else {
    file.path.clone()
  };
  std::fs::metadata(root.join(name))
    .ok()
    .map(|meta| meta.len())
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
  let targets = scan_targets(files, previous);
  let states = collect_states_cached(cache, root, &targets)?;
  let listed: HashMap<String, &PreviousFile> = previous
    .map(|release| {
      release
        .files
        .iter()
        .map(|file| (folded(&file.path), file))
        .collect()
    })
    .unwrap_or_default();
  let mut changes = Vec::new();
  for file in files {
    let ignored = locally_ignored.contains(&folded(&file.path));
    let Some(before) = listed.get(&folded(&file.path)) else {
      changes.push(PublishChange {
        path: file.path.clone(),
        kind: PublishChangeKind::Added,
        previous_sha1: None,
        previous_size: None,
        local_size: local_size(root, file),
        local_ignored: ignored,
      });
      continue;
    };
    let state = states.get(&file.path).unwrap_or(&LocalState::Absent);
    let Some(found) = state.key() else {
      // Gone between the listing and the scan.
      continue;
    };
    let same = found.size == before.size && found.sha1 == before.sha1;
    let kind = if !same && before.once {
      PublishChangeKind::OnceChanged
    } else if !same {
      PublishChangeKind::Modified
    } else if state.variant_disabled() != Some(before.disabled) {
      PublishChangeKind::Toggled
    } else {
      continue;
    };
    changes.push(PublishChange {
      path: file.path.clone(),
      kind,
      previous_sha1: Some(before.sha1.clone()),
      previous_size: Some(before.size),
      local_size: Some(found.size),
      local_ignored: ignored,
    });
  }
  Ok(changes)
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

/// The line diff of a file from the previous release's text to the one on
/// disk. `None` when it cannot be drawn: the previous content is not kept
/// locally, or either side is not a text file. The hash names a file inside the
/// instance's base store, so anything but a SHA-1 is refused, as is a path that
/// leaves the instance.
pub fn file_diff(
  root: &Path,
  path: &str,
  previous_sha1: &str,
) -> USTBLResult<Option<Vec<DiffHunk>>> {
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
    return Ok(None);
  };
  let too_big = std::fs::metadata(&file).map_or(true, |meta| meta.len() > MAX_TEXT_SIZE as u64);
  if too_big {
    return Ok(None);
  }
  let Ok(local) = std::fs::read(&file) else {
    return Ok(None);
  };
  let Ok(Some(before)) = base_store::load(root, &previous_sha1.to_lowercase()) else {
    return Ok(None);
  };
  let (Some(old), Some(new)) = (normalize_text(path, &before), normalize_text(path, &local)) else {
    return Ok(None);
  };
  Ok(Some(unified_diff_hiding(
    &lines_of(&old),
    &lines_of(&new),
    CONTEXT_LINES,
    &|_| false,
  )))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use sha1::{Digest, Sha1};
  use std::collections::BTreeSet;
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
    }
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

  fn kinds(changes: &[PublishChange]) -> Vec<(String, PublishChangeKind)> {
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
        ("config/longer.cfg".to_string(), PublishChangeKind::Modified),
        (
          "config/same-size.cfg".to_string(),
          PublishChangeKind::Modified
        ),
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
    let targets = scan_targets(&files, Some(&previous));
    assert_eq!(targets.keys().collect::<Vec<_>>(), vec!["mods/old.jar"]);
    let changes = run(&root, &files, Some(&previous), &HashSet::new());
    assert_eq!(
      kinds(&changes),
      vec![("mods/new.jar".to_string(), PublishChangeKind::Added)]
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
        ("mods/off.jar".to_string(), PublishChangeKind::Toggled),
        ("mods/on.jar".to_string(), PublishChangeKind::Toggled),
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
        (
          "config/changed.cfg".to_string(),
          PublishChangeKind::OnceChanged
        ),
        (
          "config/other-size.cfg".to_string(),
          PublishChangeKind::OnceChanged
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
          ("config/a.cfg".to_string(), PublishChangeKind::Added),
          ("mods/b.jar".to_string(), PublishChangeKind::Added),
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
      vec![("Config/A.cfg".to_string(), PublishChangeKind::Modified)]
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
    let hunks = file_diff(&root, "config/a.cfg", &sha1).unwrap().unwrap();
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
    let hunks = file_diff(&root, "config/a.cfg", &sha1).unwrap().unwrap();
    assert!(hunks.is_empty());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_reads_the_disabled_variant_of_a_switched_off_file() {
    let root = root(&[("mods/a.properties.disabled", "k=2\n")]);
    let sha1 = keep_base(&root, "mods/a.properties", "k=1\n");
    let hunks = file_diff(&root, "mods/a.properties", &sha1)
      .unwrap()
      .unwrap();
    assert_eq!(hunks.len(), 1);
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_is_none_when_the_previous_text_is_not_kept() {
    let root = root(&[("config/a.cfg", "a=2\n")]);
    assert!(file_diff(&root, "config/a.cfg", &sha1_of("a=1\n"))
      .unwrap()
      .is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_is_none_for_binary_and_missing_files() {
    let root = root(&[("mods/a.jar", "jar"), ("config/b.cfg", "x")]);
    let sha1 = keep_base(&root, "config/b.cfg", "x");
    // The kept text cannot be drawn against a jar.
    assert!(file_diff(&root, "mods/a.jar", &sha1).unwrap().is_none());
    assert!(file_diff(&root, "config/gone.cfg", &sha1)
      .unwrap()
      .is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_diff_is_none_for_a_text_file_over_the_size_limit() {
    let big = "a".repeat(MAX_TEXT_SIZE + 1);
    let root = root(&[("config/big.cfg", &big)]);
    let sha1 = keep_base(&root, "config/big.cfg", "a");
    assert!(file_diff(&root, "config/big.cfg", &sha1).unwrap().is_none());
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
