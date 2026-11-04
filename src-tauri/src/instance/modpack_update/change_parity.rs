//! Pins what the two change classifiers return, as the JSON the frontend
//! reads: the publish tree's marks and the "view changes" list. These tests go
//! through the public entry points only, so they hold across a refactor of the
//! classification itself.

use super::changes::{classify as classify_detail, compute_changes, count};
use super::diff::FileKey;
use super::ignores;
use super::local_scan::LocalScanCache;
use super::release::{self, Release, ReleaseFile};
use super::remote::{RemoteRule, RuleKind, RuleState};
use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader, ModpackBaseline};
use crate::instance::helpers::modpack::{base_store, export};
use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
use crate::instance::modpack_publish::changes::classify as classify_publish;
use crate::instance::modpack_publish::commands::{PreviousFile, PreviousRelease};
use crate::instance::modpack_publish::defaults::DiskFile;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn sha1_of(content: &str) -> String {
  hex::encode(Sha1::digest(content.as_bytes()))
}

fn key(content: &str) -> FileKey {
  FileKey {
    sha1: sha1_of(content),
    size: content.len() as u64,
  }
}

fn instance(files: &[(&str, &str)]) -> PathBuf {
  let root = std::env::temp_dir().join(format!("ustbl-change-parity-{}", uuid::Uuid::new_v4()));
  fs::create_dir_all(&root).unwrap();
  for (path, content) in files {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, content).unwrap();
  }
  root
}

fn name_of(root: &Path) -> String {
  root.file_name().unwrap().to_string_lossy().into_owned()
}

fn show(label: &str, value: &Value) {
  if std::env::var("USTBL_SHOW_PARITY").is_ok() {
    println!("{label}: {}", serde_json::to_string_pretty(value).unwrap());
  }
}

/// Make a file unreadable but still stat-able, so a test can tell whether the
/// classifier opened it. Windows only: other platforms cannot do this without
/// depending on who runs the tests.
#[cfg(windows)]
fn lock(path: &Path) -> fs::File {
  use std::os::windows::fs::OpenOptionsExt;
  fs::OpenOptions::new()
    .read(true)
    .share_mode(0)
    .open(path)
    .unwrap()
}

// ---- publish tree marks -------------------------------------------------

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

fn previous(files: Vec<PreviousFile>) -> PreviousRelease {
  PreviousRelease {
    files,
    rules: Vec::new(),
    rules_manifest: true,
    version_id: Some("7".into()),
    project_id: Some("proj".into()),
  }
}

fn publish(
  root: &Path,
  files: &[DiskFile],
  previous: Option<&PreviousRelease>,
  ignored: &[&str],
) -> std::io::Result<Value> {
  let cache = Mutex::new(LocalScanCache::default());
  let ignored: HashSet<String> = ignored.iter().map(|path| path.to_string()).collect();
  let changes = classify_publish(root, &cache, files, previous, &ignored)?;
  let value = serde_json::to_value(changes).unwrap();
  show("publish", &value);
  Ok(value)
}

#[test]
fn publish_marks_every_kind_in_disk_order_with_the_exact_fields() {
  let root = instance(&[
    ("config/mod.cfg", "version=2222"),
    ("config/same.cfg", "version=2"),
    ("config/new.cfg", "n"),
    ("mods/off.jar.disabled", "jar"),
    ("mods/on.jar", "jar"),
    ("config/once-changed.cfg", "b=2"),
    ("config/once-size.cfg", "a=1111"),
    ("config/once-same.cfg", "a=1"),
    ("config/once-toggle.cfg.disabled", "t=1"),
    ("config/equal.cfg", "e=1"),
    ("config/Case.cfg", "c=2"),
    ("mods/added-off.jar.disabled", "xyz"),
  ]);
  let files = [
    disk("config/mod.cfg", false),
    disk("config/gone.cfg", false),
    disk("config/same.cfg", false),
    disk("config/new.cfg", false),
    disk("mods/off.jar", true),
    disk("mods/on.jar", false),
    disk("config/once-changed.cfg", false),
    disk("config/once-size.cfg", false),
    disk("config/once-same.cfg", false),
    disk("config/once-toggle.cfg", true),
    disk("config/once-gone.cfg", false),
    disk("config/equal.cfg", false),
    disk("config/Case.cfg", false),
    disk("config/gone-new.cfg", false),
    disk("mods/added-off.jar", true),
  ];
  let release = previous(vec![
    listed("config/mod.cfg", "v=1", false, false),
    listed("config/gone.cfg", "g=1", false, false),
    listed("config/same.cfg", "version=1", false, false),
    listed("mods/off.jar", "jar", false, false),
    listed("mods/on.jar", "jar", true, false),
    listed("config/once-changed.cfg", "b=1", false, true),
    listed("config/once-size.cfg", "a=1", false, true),
    listed("config/once-same.cfg", "a=1", false, true),
    listed("config/once-toggle.cfg", "t=1", false, true),
    listed("config/once-gone.cfg", "o=1", false, true),
    listed("config/equal.cfg", "e=1", false, false),
    listed("config/case.cfg", "c=1", false, false),
    listed("config/not-on-disk.cfg", "x", false, false),
  ]);
  let actual = publish(
    &root,
    &files,
    Some(&release),
    &["config/new.cfg", "config/mod.cfg", "config/case.cfg"],
  )
  .unwrap();
  let _ = fs::remove_dir_all(root);

  let entry =
    |path: &str, kind: &str, prev: Option<(&str, u64)>, local: Option<u64>, ignored: bool| {
      json!({
        "path": path,
        "kind": kind,
        "previousSha1": prev.map(|(content, _)| sha1_of(content)),
        "previousSize": prev.map(|(_, size)| size),
        "localSize": local,
        "localIgnored": ignored,
        "previousVersionId": "7",
        "previousProjectId": "proj",
      })
    };
  let expected = json!([
    entry(
      "config/mod.cfg",
      "modified",
      Some(("v=1", 3)),
      Some(12),
      true
    ),
    entry(
      "config/same.cfg",
      "modified",
      Some(("version=1", 9)),
      Some(9),
      false
    ),
    entry("config/new.cfg", "added", None, Some(1), true),
    entry("mods/off.jar", "toggled", Some(("jar", 3)), Some(3), false),
    entry("mods/on.jar", "toggled", Some(("jar", 3)), Some(3), false),
    entry(
      "config/once-changed.cfg",
      "onceChanged",
      Some(("b=1", 3)),
      Some(3),
      false
    ),
    entry(
      "config/once-size.cfg",
      "onceChanged",
      Some(("a=1", 3)),
      Some(6),
      false
    ),
    entry(
      "config/once-toggle.cfg",
      "toggled",
      Some(("t=1", 3)),
      Some(3),
      false
    ),
    entry(
      "config/Case.cfg",
      "modified",
      Some(("c=1", 3)),
      Some(3),
      true
    ),
    entry("config/gone-new.cfg", "added", None, None, false),
    entry("mods/added-off.jar", "added", None, Some(3), false),
  ]);
  assert_eq!(actual, expected);
}

#[test]
fn publish_without_a_previous_release_lists_every_file_as_added() {
  let root = instance(&[("config/a.cfg", "a=1")]);
  let files = [disk("config/a.cfg", false), disk("config/b.cfg", false)];
  let expected = json!([
    {
      "path": "config/a.cfg", "kind": "added", "previousSha1": null,
      "previousSize": null, "localSize": 3, "localIgnored": false,
      "previousVersionId": null, "previousProjectId": null
    },
    {
      "path": "config/b.cfg", "kind": "added", "previousSha1": null,
      "previousSize": null, "localSize": null, "localIgnored": false,
      "previousVersionId": null, "previousProjectId": null
    }
  ]);
  assert_eq!(publish(&root, &files, None, &[]).unwrap(), expected);
  let _ = fs::remove_dir_all(root);
}

#[test]
fn publish_gives_the_same_answer_from_a_warm_scan_cache() {
  let root = instance(&[
    ("config/a.cfg", "a=2"),
    ("config/b.cfg", "b=1"),
    ("config/c.cfg", "cccc"),
  ]);
  let files = [
    disk("config/a.cfg", false),
    disk("config/b.cfg", false),
    disk("config/c.cfg", false),
  ];
  let release = previous(vec![
    listed("config/a.cfg", "a=1", false, false),
    listed("config/b.cfg", "b=1", false, false),
    listed("config/c.cfg", "c", false, false),
  ]);
  let cache = Mutex::new(LocalScanCache::default());
  let ignored = HashSet::new();
  let cold = classify_publish(&root, &cache, &files, Some(&release), &ignored).unwrap();
  let warm = classify_publish(&root, &cache, &files, Some(&release), &ignored).unwrap();
  assert_eq!(
    serde_json::to_value(&cold).unwrap(),
    serde_json::to_value(&warm).unwrap()
  );
  assert_eq!(cold.len(), 2);
  let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn publish_hashes_equal_sized_files_only_and_never_reads_added_ones() {
  let root = instance(&[
    ("config/longer.cfg", "version=2222"),
    ("config/same-size.cfg", "version=2"),
    ("config/new.cfg", "new"),
  ]);
  let release = previous(vec![
    listed("config/longer.cfg", "v=1", false, false),
    listed("config/same-size.cfg", "version=1", false, false),
  ]);
  let longer = [
    disk("config/longer.cfg", false),
    disk("config/new.cfg", false),
  ];
  let _longer = lock(&root.join("config/longer.cfg"));
  let _new = lock(&root.join("config/new.cfg"));
  let value = publish(&root, &longer, Some(&release), &[]).unwrap();
  assert_eq!(value[0]["kind"], "modified");
  assert_eq!(value[0]["localSize"], 12);
  assert_eq!(value[1]["kind"], "added");
  assert_eq!(value[1]["localSize"], 3);

  let _same = lock(&root.join("config/same-size.cfg"));
  let same = [disk("config/same-size.cfg", false)];
  assert!(publish(&root, &same, Some(&release), &[]).is_err());
  drop((_longer, _new, _same));
  let _ = fs::remove_dir_all(root);
}

// ---- "view changes" list ------------------------------------------------

fn origin() -> InstanceOrigin {
  InstanceOrigin {
    platform: OriginPlatform::Campus,
    project_id: "p".into(),
    version_id: "7".into(),
    game_version: "1.21.1".into(),
    loader_type: ModLoaderType::Fabric,
    follow: true,
    pack_format: None,
  }
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

fn managed(path: &str, content: &str, disabled: bool) -> BaselineFile {
  BaselineFile::managed(path, key(content), disabled)
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

async fn detail(root: &Path, baseline: &ModpackBaseline, release: &Release) -> Value {
  let listed = export::list_files_at(root, &name_of(root)).unwrap();
  let entries = classify_detail(root, baseline, release, &listed)
    .await
    .unwrap();
  let value = serde_json::to_value(entries).unwrap();
  show("detail", &value);
  value
}

async fn without_diff(root: &Path, baseline: &ModpackBaseline, release: &Release) -> Value {
  let listed = export::list_content_files(root, &name_of(root)).unwrap();
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
  .unwrap();
  let value = serde_json::to_value(entries).unwrap();
  show("count", &value);
  value
}

const BASE_TEXT: &str = "a=1\nb=2\nc=3\n";

/// One instance that has every case of the list in it. Returns the root with
/// the baseline and the release.
fn busy_instance() -> (PathBuf, ModpackBaseline, Release) {
  let root = instance(&[
    ("config/edited.toml", "a=1\nb=20\nc=3\n"),
    ("mods/switched.jar.disabled", "sw-changed"),
    ("mods/toggle.jar.disabled", "toggle"),
    ("mods/on-off.jar", "on-off"),
    ("config/once-changed.toml", "changed"),
    ("config/once-same.toml", "same"),
    ("config/once-size.toml", "longer than the release"),
    ("config/once-ignored.toml", "different"),
    ("config/once-toggle.toml.disabled", "toggle-once"),
    ("mods/ignored-new.jar", "ign-new"),
    ("mods/new.jar", "new"),
    ("mods/new-off.jar.disabled", "new-off"),
    ("config/new.toml", "new-config"),
    ("saves/world.dat", "save"),
    ("config/cache/new.toml", "cache"),
    ("config/rule/one.toml", "r1"),
    ("config/rule/two.toml", "r2"),
    ("mods/kept.jar", "kept"),
  ]);
  let baseline = baseline_with(vec![
    managed("config/edited.toml", BASE_TEXT, false),
    managed("mods/switched.jar", "sw", false),
    managed("mods/toggle.jar", "toggle", false),
    managed("mods/on-off.jar", "on-off", true),
    managed("config/deleted.toml", "installed-del", false),
    managed("config/ignored.toml", "ign", false),
    managed("config/once-ignored.toml", "orig", false),
  ]);
  // The baseline remembers different content for the deleted file than the
  // release lists now.
  let release = release_of(
    vec![
      release_file("config/edited.toml", BASE_TEXT, false, false),
      release_file("mods/switched.jar", "sw", false, false),
      release_file("mods/toggle.jar", "toggle", false, false),
      release_file("mods/on-off.jar", "on-off", false, false),
      release_file("config/deleted.toml", "release-del", false, false),
      release_file("config/ignored.toml", "ign", false, false),
      release_file("config/once-changed.toml", "original", false, true),
      release_file("config/once-same.toml", "same", false, true),
      release_file("config/once-size.toml", "short", false, true),
      release_file("config/once-ignored.toml", "orig", false, true),
      release_file("config/once-gone.toml", "gone", false, true),
      release_file("config/once-toggle.toml", "toggle-once", false, true),
      release_file("mods/kept.jar", "kept", false, false),
    ],
    vec![
      RemoteRule {
        path: "config/cache".into(),
        kind: RuleKind::Dir,
        state: RuleState::Ignore,
      },
      RemoteRule {
        path: "config/rule".into(),
        kind: RuleKind::Dir,
        state: RuleState::Delete,
      },
      RemoteRule {
        path: "config/rule/two.toml".into(),
        kind: RuleKind::File,
        state: RuleState::Ignore,
      },
    ],
  );
  let edited = baseline
    .files
    .iter()
    .find(|file| file.path == "config/edited.toml")
    .unwrap();
  base_store::store(&root, edited, BASE_TEXT.as_bytes()).unwrap();
  (root, baseline, release)
}

fn ignore_whole(root: &Path, baseline: &ModpackBaseline, path: &str) {
  let file = baseline
    .files
    .iter()
    .find(|file| file.path == path)
    .unwrap();
  ignores::add(root, baseline, path, &file.sha1, None).unwrap();
}

#[tokio::test]
async fn detail_lists_every_class_in_release_then_first_install_then_added_order() {
  let (root, baseline, release) = busy_instance();
  ignore_whole(&root, &baseline, "config/ignored.toml");
  ignore_whole(&root, &baseline, "config/once-ignored.toml");
  ignores::add_added(&root, &baseline, &release, "mods/ignored-new.jar").unwrap();
  let actual = detail(&root, &baseline, &release).await;

  let _ = fs::remove_dir_all(root);
  assert_eq!(
    actual,
    json!([
      {
        "baseline": {
          "sha1": "0344f8ad14c452b02bd2c34172bdde569f3daaea",
          "size": 12
        },
        "class": "modified",
        "counted": true,
        "disabled": false,
        "hunks": [
          {
            "edits": [
              {
                "end": 2,
                "start": 1
              }
            ],
            "lines": [
              {
                "kind": "context",
                "newNumber": 1,
                "oldNumber": 1,
                "text": "a=1"
              },
              {
                "edit": 0,
                "kind": "delete",
                "newNumber": null,
                "oldNumber": 2,
                "text": "b=2"
              },
              {
                "edit": 0,
                "kind": "add",
                "newNumber": 2,
                "oldNumber": null,
                "text": "b=20"
              },
              {
                "kind": "context",
                "newNumber": 3,
                "oldNumber": 3,
                "text": "c=3"
              }
            ],
            "newLines": 3,
            "newStart": 1,
            "oldLines": 3,
            "oldStart": 1
          }
        ],
        "local": {
          "sha1": "",
          "size": 13
        },
        "path": "config/edited.toml",
        "size": 13,
        "toggled": false
      },
      {
        "baseline": {
          "sha1": "8d4c780fcfdc41841e5070f4c43da8958ba6aec0",
          "size": 2
        },
        "class": "modified",
        "counted": true,
        "disabled": true,
        "local": {
          "sha1": "df0dcaf65c12307eff962a51ea3abd7f41491383",
          "size": 10
        },
        "noDiff": "notText",
        "path": "mods/switched.jar",
        "size": 10,
        "toggled": true
      },
      {
        "baseline": {
          "sha1": "f9c5942b1f55cb12b8f2b5fae21a9f1706f9d367",
          "size": 6
        },
        "class": "toggled",
        "counted": true,
        "disabled": true,
        "local": {
          "sha1": "f9c5942b1f55cb12b8f2b5fae21a9f1706f9d367",
          "size": 6
        },
        "path": "mods/toggle.jar",
        "size": 6,
        "toggled": true
      },
      {
        "baseline": {
          "sha1": "0e3ef94ce2b6cea39de68a58d33532f73e44a45c",
          "size": 13
        },
        "class": "deleted",
        "counted": true,
        "disabled": false,
        "path": "config/deleted.toml",
        "toggled": false
      },
      {
        "class": "onceChanged",
        "counted": false,
        "disabled": false,
        "path": "config/once-changed.toml",
        "size": 7,
        "toggled": false
      },
      {
        "class": "onceChanged",
        "counted": false,
        "disabled": false,
        "path": "config/once-gone.toml",
        "toggled": false
      },
      {
        "class": "onceChanged",
        "counted": false,
        "disabled": false,
        "path": "config/once-size.toml",
        "size": 23,
        "toggled": false
      },
      {
        "class": "addedExcluded",
        "counted": false,
        "disabled": false,
        "path": "config/cache/new.toml",
        "rule": "ignore",
        "size": 5,
        "toggled": false
      },
      {
        "class": "added",
        "counted": false,
        "disabled": false,
        "path": "config/new.toml",
        "size": 10,
        "toggled": false
      },
      {
        "class": "addedExcluded",
        "counted": false,
        "disabled": false,
        "path": "config/rule/one.toml",
        "rule": "delete",
        "size": 2,
        "toggled": false
      },
      {
        "class": "addedExcluded",
        "counted": false,
        "disabled": false,
        "path": "config/rule/two.toml",
        "rule": "ignore",
        "size": 2,
        "toggled": false
      },
      {
        "class": "added",
        "counted": true,
        "disabled": true,
        "path": "mods/new-off.jar",
        "size": 7,
        "toggled": false
      },
      {
        "class": "added",
        "counted": true,
        "disabled": false,
        "path": "mods/new.jar",
        "size": 3,
        "toggled": false
      },
      {
        "class": "addedExcluded",
        "counted": false,
        "disabled": false,
        "path": "saves/world.dat",
        "size": 4,
        "toggled": false
      }
    ])
  );
}

#[tokio::test]
async fn the_count_pass_omits_first_install_files_and_the_size_of_added_ones() {
  let (root, baseline, release) = busy_instance();
  release::write(&root, &release).await.unwrap();
  ignore_whole(&root, &baseline, "config/ignored.toml");
  ignore_whole(&root, &baseline, "config/once-ignored.toml");
  ignores::add_added(&root, &baseline, &release, "mods/ignored-new.jar").unwrap();
  let actual = without_diff(&root, &baseline, &release).await;
  let counted = count(&root, &baseline).await.unwrap();
  let _ = fs::remove_dir_all(root);
  assert_eq!(
    actual,
    json!([
      {
        "baseline": {
          "sha1": "0344f8ad14c452b02bd2c34172bdde569f3daaea",
          "size": 12
        },
        "class": "modified",
        "counted": true,
        "disabled": false,
        "local": {
          "sha1": "",
          "size": 13
        },
        "path": "config/edited.toml",
        "size": 13,
        "toggled": false
      },
      {
        "baseline": {
          "sha1": "8d4c780fcfdc41841e5070f4c43da8958ba6aec0",
          "size": 2
        },
        "class": "modified",
        "counted": true,
        "disabled": true,
        "local": {
          "sha1": "df0dcaf65c12307eff962a51ea3abd7f41491383",
          "size": 10
        },
        "noDiff": "notText",
        "path": "mods/switched.jar",
        "size": 10,
        "toggled": true
      },
      {
        "baseline": {
          "sha1": "f9c5942b1f55cb12b8f2b5fae21a9f1706f9d367",
          "size": 6
        },
        "class": "toggled",
        "counted": true,
        "disabled": true,
        "local": {
          "sha1": "f9c5942b1f55cb12b8f2b5fae21a9f1706f9d367",
          "size": 6
        },
        "path": "mods/toggle.jar",
        "size": 6,
        "toggled": true
      },
      {
        "baseline": {
          "sha1": "0e3ef94ce2b6cea39de68a58d33532f73e44a45c",
          "size": 13
        },
        "class": "deleted",
        "counted": true,
        "disabled": false,
        "path": "config/deleted.toml",
        "toggled": false
      },
      {
        "class": "added",
        "counted": true,
        "disabled": true,
        "path": "mods/new-off.jar",
        "toggled": false
      },
      {
        "class": "added",
        "counted": true,
        "disabled": false,
        "path": "mods/new.jar",
        "toggled": false
      }
    ])
  );
  assert_eq!(counted, 6);
}

#[cfg(windows)]
#[tokio::test]
async fn first_install_files_are_hashed_only_when_their_size_matches() {
  let root = instance(&[
    ("config/size.toml", "longer than the release"),
    ("config/same-size.toml", "abcd"),
  ]);
  let baseline = baseline_with(vec![]);
  let release = release_of(
    vec![
      release_file("config/size.toml", "short", false, true),
      release_file("config/same-size.toml", "wxyz", false, true),
    ],
    vec![],
  );
  let listed = export::list_files_at(&root, &name_of(&root)).unwrap();
  {
    // A file of the release's size has to be hashed, which the lock prevents.
    // (The scan cache is process-wide, so this runs before anything is cached.)
    let _same = lock(&root.join("config/same-size.toml"));
    assert!(classify_detail(&root, &baseline, &release, &listed)
      .await
      .is_err());
  }
  // The file of another size is judged by its size alone.
  let _size = lock(&root.join("config/size.toml"));
  let entries = classify_detail(&root, &baseline, &release, &listed).await;
  let value = serde_json::to_value(entries.unwrap()).unwrap();
  assert_eq!(value[0]["path"], "config/same-size.toml");
  assert_eq!(value[1]["path"], "config/size.toml");
  assert_eq!(value[1]["class"], "onceChanged");
  assert_eq!(value[1]["size"], 23);
  drop(_size);
  let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn paths_of_the_baseline_and_the_release_meet_without_regard_to_case() {
  let root = instance(&[("config/edit.toml", "local"), ("config/Del.toml", "keep")]);
  let baseline = baseline_with(vec![
    managed("CONFIG/Edit.toml", "base", false),
    managed("config/del.TOML", "keep", false),
  ]);
  let release = release_of(
    vec![
      release_file("config/edit.toml", "base", false, false),
      release_file("config/Del.toml", "keep", false, false),
    ],
    vec![],
  );
  let value = detail(&root, &baseline, &release).await;
  let _ = fs::remove_dir_all(root);
  assert_eq!(value.as_array().unwrap().len(), 1);
  assert_eq!(value[0]["path"], "config/edit.toml");
  assert_eq!(value[0]["class"], "modified");
  assert_eq!(value[0]["baseline"]["sha1"], sha1_of("base"));
}
