//! Optional files of a platform modpack.
//!
//! Modrinth (`env.client: optional`) and CurseForge (`required: false`) let a
//! pack author mark files the player may leave out. The player's choice is
//! kept in `.ustbl-update/optional.json` and applied to the remote every time
//! one is built, so an install and every later update agree on it:
//!
//! - a chosen file is installed like any other;
//! - an unchosen file under `mods/` is installed disabled (`x.jar.disabled`),
//!   where the mod page can enable it;
//! - an unchosen file anywhere else is left out of the remote, so it is neither
//!   installed nor recorded in the baseline.
//!
//! Which files the player chose is read, in this order, from: the disk (a mod
//! the player enabled or disabled on the mod page counts as chosen or not), the
//! record, the baseline (a file installed before this feature was required, so
//! it stays), and finally the default, which is not chosen.

use super::diff::DISABLED_SUFFIX;
use super::remote::{safe_relative_path, FileSource, RemoteModpack, UPDATE_DIR};
use crate::instance::helpers::modpack::baseline::{BaselineFileState, ModpackBaseline};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub const OPTIONAL_FILE: &str = "optional.json";
const SCHEMA: u32 = 1;

/// `path` without a trailing `.disabled`, whatever its case.
pub fn without_disabled_suffix(mut path: String) -> String {
  if path.to_lowercase().ends_with(DISABLED_SUFFIX) {
    path.truncate(path.len() - DISABLED_SUFFIX.len());
  }
  path
}

/// The record's name relative to the instance, as the update transaction
/// saves and restores it.
pub fn meta_name() -> String {
  format!("{UPDATE_DIR}/{OPTIONAL_FILE}")
}

fn record_path(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join(OPTIONAL_FILE)
}

/// A file the pack author marked optional.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionalFile {
  /// Stays the same when the file is replaced by a newer one of the same
  /// project; see `ModpackDownload::select_key`.
  pub key: String,
  /// Relative to the instance, always with `/`. With `.disabled` taken off.
  pub path: String,
  /// What the interface shows: the file name.
  pub name: String,
}

/// What the player was offered and what they chose.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionalRecord {
  #[serde(default)]
  pub schema: u32,
  /// Every optional file of the installed version, chosen or not.
  #[serde(default)]
  pub known: Vec<OptionalFile>,
  /// Keys of the chosen files. Before the first install finishes, `known` is
  /// empty and this is what the player picked in the install dialog.
  #[serde(default)]
  pub selected: Vec<String>,
}

/// Read the record. A missing or unreadable file is an instance without one.
pub fn read(root: &Path) -> OptionalRecord {
  let path = record_path(root);
  match fs::read(&path) {
    Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
      log::warn!("ignoring unreadable optional file record {path:?}: {error}");
      OptionalRecord::default()
    }),
    Err(error) => {
      if error.kind() != io::ErrorKind::NotFound {
        log::warn!("cannot read optional file record {path:?}: {error}");
      }
      OptionalRecord::default()
    }
  }
}

/// Store `record`; a record that knows no file removes the file instead.
pub fn write(root: &Path, record: &OptionalRecord) -> io::Result<()> {
  let path = record_path(root);
  if record.known.is_empty() && record.selected.is_empty() {
    return match fs::remove_file(&path) {
      Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
      _ => Ok(()),
    };
  }
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let record = OptionalRecord {
    schema: SCHEMA,
    ..record.clone()
  };
  let temp = path.with_extension("json.tmp");
  fs::write(&temp, serde_json::to_vec_pretty(&record)?)?;
  fs::rename(temp, path)
}

/// The player's picks before an instance is installed.
pub fn write_picks(root: &Path, selected: &[String]) -> io::Result<()> {
  let mut selected = selected.to_vec();
  selected.sort();
  selected.dedup();
  write(
    root,
    &OptionalRecord {
      schema: SCHEMA,
      known: Vec::new(),
      selected,
    },
  )
}

/// The optional files among `downloads`, one per file, with distinct keys: a
/// project that ships two files keeps its key on the first and takes the
/// path for the others.
pub fn table(entries: impl IntoIterator<Item = (String, String)>) -> Vec<OptionalFile> {
  let mut used = HashSet::new();
  let mut table = Vec::new();
  for (key, path) in entries {
    let key = if key.is_empty() || !used.insert(key.clone()) {
      let by_path = crate::instance::helpers::modpack::misc::select_key_of_path(&path);
      used.insert(by_path.clone());
      by_path
    } else {
      key
    };
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    table.push(OptionalFile { key, path, name });
  }
  table
}

/// Which optional files of a remote are chosen, and which of them the player
/// has not been asked about yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
  /// Keys of the chosen files.
  pub selected: BTreeSet<String>,
  /// Files the installed version did not have (all of them on a fresh
  /// install), whether or not `choice` picked them.
  pub new: Vec<OptionalFile>,
}

impl Resolution {
  /// What the record holds once the remote is installed.
  pub fn record(&self, optional: &[OptionalFile]) -> OptionalRecord {
    OptionalRecord {
      schema: SCHEMA,
      known: optional.to_vec(),
      selected: optional
        .iter()
        .filter(|file| self.selected.contains(&file.key))
        .map(|file| file.key.clone())
        .collect(),
    }
  }
}

/// Whether the mod at `path` is enabled on disk: `Some(true)` when only the
/// plain file exists, `Some(false)` when only the `.disabled` one does. Files
/// outside `mods/` have no disabled state, and a missing file (or both
/// variants) says nothing.
fn enabled_on_disk(root: &Path, path: &str) -> Option<bool> {
  if !path.to_lowercase().starts_with("mods/") || safe_relative_path(path).is_err() {
    return None;
  }
  let enabled = root.join(path).is_file();
  let disabled = root.join(format!("{path}{DISABLED_SUFFIX}")).is_file();
  match (enabled, disabled) {
    (true, false) => Some(true),
    (false, true) => Some(false),
    _ => None,
  }
}

/// Decide which of `optional` are chosen. `choice` holds the keys the player
/// picked just now among the new ones.
pub fn resolve(
  root: &Path,
  baseline: &ModpackBaseline,
  choice: &[String],
  optional: &[OptionalFile],
) -> Resolution {
  if optional.is_empty() {
    return Resolution::default();
  }
  let record = read(root);
  let known: HashMap<&str, &OptionalFile> = record
    .known
    .iter()
    .map(|file| (file.key.as_str(), file))
    .collect();
  let picked: HashSet<&str> = record
    .selected
    .iter()
    .chain(choice)
    .map(String::as_str)
    .collect();
  let installed: HashSet<String> = baseline
    .files
    .iter()
    .filter(|file| file.state == BaselineFileState::Managed)
    .map(|file| file.path.to_lowercase())
    .collect();

  let mut resolution = Resolution::default();
  for file in optional {
    let selected = if let Some(before) = known.get(file.key.as_str()) {
      // Offered before. What the player did to the mod since wins, so the next
      // update does not undo it; the file may have had another name then.
      enabled_on_disk(root, &before.path).unwrap_or(picked.contains(file.key.as_str()))
    } else if picked.contains(file.key.as_str()) {
      resolution.new.push(file.clone());
      true
    } else if installed.contains(&file.path.to_lowercase()) {
      // Installed as a required file by an update that did not know the
      // distinction: it stays, as the player left it.
      enabled_on_disk(root, &file.path).unwrap_or(true)
    } else {
      resolution.new.push(file.clone());
      false
    };
    if selected {
      resolution.selected.insert(file.key.clone());
    }
  }
  resolution
}

/// Put the choice into the remote's file list.
pub fn apply(remote: &mut RemoteModpack, resolution: &Resolution) {
  if remote.optional.is_empty() {
    return;
  }
  // Only a file the manifest asks to download is optional: an override that
  // happens to share the path wins over the download and is not touched.
  let by_path: HashMap<String, usize> = remote
    .files
    .iter()
    .enumerate()
    .filter(|(_, file)| matches!(file.source, FileSource::Download(_)))
    .map(|(index, file)| (file.path.to_lowercase(), index))
    .collect();
  let mut left_out = HashSet::new();
  for file in &remote.optional {
    if resolution.selected.contains(&file.key) {
      continue;
    }
    let lowered = file.path.to_lowercase();
    let Some(&index) = by_path.get(&lowered) else {
      continue;
    };
    if lowered.starts_with("mods/") {
      remote.files[index].disabled = true;
    } else {
      left_out.insert(index);
    }
  }
  if !left_out.is_empty() {
    let mut index = 0;
    remote.files.retain(|_| {
      index += 1;
      !left_out.contains(&(index - 1))
    });
  }
  remote.path_index = Arc::new(OnceLock::new());
}

/// [`resolve`] then [`apply`]: the remote as it is installed for this player.
pub fn select(
  remote: &mut RemoteModpack,
  root: &Path,
  baseline: &ModpackBaseline,
  choice: &[String],
) -> Resolution {
  let resolution = resolve(root, baseline, choice, &remote.optional);
  apply(remote, &resolution);
  resolution
}

#[cfg(test)]
mod tests {
  use super::super::diff::FileKey;
  use super::super::remote::{FileSource, RemoteFile};
  use super::*;
  use crate::instance::helpers::modpack::baseline::{
    BaselineFile, BaselineLoader, ModpackBaseline,
  };
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::tasks::download::DownloadParam;

  struct Root(PathBuf);

  impl Root {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-optional-{}", uuid::Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn write(&self, relative: &str, content: &str) {
      let path = self.0.join(relative);
      fs::create_dir_all(path.parent().unwrap()).unwrap();
      fs::write(path, content).unwrap();
    }
  }

  impl Drop for Root {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn loader() -> BaselineLoader {
    BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: "0.16.0".into(),
    }
  }

  fn baseline(paths: &[&str]) -> ModpackBaseline {
    ModpackBaseline::build(
      &InstanceOrigin {
        platform: OriginPlatform::Modrinth,
        project_id: "p".into(),
        version_id: "1".into(),
        game_version: "1.21.1".into(),
        loader_type: ModLoaderType::Fabric,
        follow: true,
      },
      loader(),
      paths
        .iter()
        .map(|path| {
          BaselineFile::managed(
            path,
            FileKey {
              sha1: "aa".into(),
              size: 1,
            },
            false,
          )
        })
        .collect(),
    )
  }

  fn file(key: &str, path: &str) -> OptionalFile {
    OptionalFile {
      key: key.into(),
      path: path.into(),
      name: path.rsplit('/').next().unwrap().into(),
    }
  }

  fn remote(files: &[(&str, bool)], optional: Vec<OptionalFile>) -> RemoteModpack {
    let make = |path: &str, download: bool| RemoteFile {
      path: path.into(),
      key: FileKey {
        sha1: "bb".into(),
        size: 2,
      },
      source: if download {
        FileSource::Download(Box::new(DownloadParam {
          src: "https://example.com/a".parse().unwrap(),
          dest: PathBuf::new(),
          filename: None,
          sha1: None,
          custom_headers: None,
          transfer_options: Default::default(),
        }))
      } else {
        FileSource::ArchiveEntry {
          archive: PathBuf::new(),
          entry: path.into(),
        }
      },
      disabled: false,
      once: false,
    };
    RemoteModpack {
      version_id: "2".into(),
      label: "2.0".into(),
      game_version: "1.21.1".into(),
      loader: loader(),
      components: None,
      component_pack: None,
      recommended_memory_mb: None,
      files: files
        .iter()
        .map(|(path, download)| make(path, *download))
        .collect(),
      rules: Vec::new(),
      rules_manifest: false,
      disabled_authoritative: true,
      optional,
      path_index: Arc::new(OnceLock::new()),
    }
  }

  fn state(remote: &RemoteModpack) -> Vec<(String, bool)> {
    remote
      .files
      .iter()
      .map(|file| (file.path.clone(), file.disabled))
      .collect()
  }

  #[test]
  fn nothing_is_chosen_by_default_and_every_file_is_new() {
    let root = Root::new();
    let optional = vec![
      file("k:a", "mods/a.jar"),
      file("k:b", "resourcepacks/b.zip"),
    ];
    let resolution = resolve(&root.0, &baseline(&[]), &[], &optional);
    assert!(resolution.selected.is_empty());
    assert_eq!(resolution.new, optional);
  }

  #[test]
  fn an_unchosen_mod_is_disabled_and_an_unchosen_other_file_is_left_out() {
    let root = Root::new();
    let optional = vec![
      file("k:a", "mods/a.jar"),
      file("k:b", "resourcepacks/b.zip"),
      file("k:c", "mods/c.jar"),
    ];
    let mut pack = remote(
      &[
        ("config/x.toml", true),
        ("mods/a.jar", true),
        ("mods/c.jar", true),
        ("mods/req.jar", true),
        ("resourcepacks/b.zip", true),
      ],
      optional,
    );
    select(&mut pack, &root.0, &baseline(&[]), &["k:c".to_string()]);
    assert_eq!(
      state(&pack),
      [
        ("config/x.toml".to_string(), false),
        ("mods/a.jar".to_string(), true),
        ("mods/c.jar".to_string(), false),
        ("mods/req.jar".to_string(), false),
      ]
    );
    // The lookup the engine uses follows the edited list.
    assert!(pack.file_of("resourcepacks/b.zip").is_none());
    assert!(pack.file_of("mods/a.jar").unwrap().disabled);
    pack.validate("pack").unwrap();
  }

  #[test]
  fn a_chosen_non_mod_file_is_installed() {
    let root = Root::new();
    let mut pack = remote(
      &[("shaderpacks/s.zip", true)],
      vec![file("k:s", "shaderpacks/s.zip")],
    );
    select(&mut pack, &root.0, &baseline(&[]), &["k:s".to_string()]);
    assert_eq!(state(&pack), [("shaderpacks/s.zip".to_string(), false)]);
  }

  #[test]
  fn an_override_sharing_the_path_is_not_touched() {
    let root = Root::new();
    let mut pack = remote(&[("mods/a.jar", false)], vec![file("k:a", "mods/a.jar")]);
    select(&mut pack, &root.0, &baseline(&[]), &[]);
    assert_eq!(state(&pack), [("mods/a.jar".to_string(), false)]);
  }

  #[test]
  fn the_record_keeps_the_choice_across_updates() {
    let root = Root::new();
    let optional = vec![file("k:a", "mods/a.jar"), file("k:b", "mods/b.jar")];
    let first = resolve(&root.0, &baseline(&[]), &["k:a".into()], &optional);
    write(&root.0, &first.record(&optional)).unwrap();
    // The install wrote a.jar enabled and b.jar disabled.
    root.write("mods/a.jar", "a");
    root.write("mods/b.jar.disabled", "b");

    let installed = baseline(&["mods/a.jar", "mods/b.jar"]);
    let again = resolve(&root.0, &installed, &[], &optional);
    assert_eq!(again.selected, BTreeSet::from(["k:a".to_string()]));
    assert!(again.new.is_empty());

    let mut next = remote(
      &[("mods/a.jar", true), ("mods/b.jar", true)],
      optional.clone(),
    );
    select(&mut next, &root.0, &installed, &[]);
    assert_eq!(
      state(&next),
      [
        ("mods/a.jar".to_string(), false),
        ("mods/b.jar".to_string(), true)
      ]
    );
  }

  #[test]
  fn a_mod_the_player_toggled_on_the_mod_page_is_not_changed_back() {
    let root = Root::new();
    let optional = vec![file("k:a", "mods/a.jar"), file("k:b", "mods/b.jar")];
    write(
      &root.0,
      &OptionalRecord {
        schema: SCHEMA,
        known: optional.clone(),
        selected: vec!["k:a".into()],
      },
    )
    .unwrap();
    // The player disabled a and enabled b.
    root.write("mods/a.jar.disabled", "a");
    root.write("mods/b.jar", "b");

    let installed = baseline(&["mods/a.jar", "mods/b.jar"]);
    let resolution = resolve(&root.0, &installed, &[], &optional);
    assert_eq!(resolution.selected, BTreeSet::from(["k:b".to_string()]));
    assert_eq!(
      resolution.record(&optional).selected,
      vec!["k:b".to_string()]
    );
  }

  #[test]
  fn a_renamed_file_follows_the_state_of_the_old_one() {
    let root = Root::new();
    let before = vec![file("k:a", "mods/a-1.0.jar")];
    write(
      &root.0,
      &OptionalRecord {
        schema: SCHEMA,
        known: before,
        selected: Vec::new(),
      },
    )
    .unwrap();
    // The player enabled the unchosen mod; the next version renames it.
    root.write("mods/a-1.0.jar", "a");
    let next = vec![file("k:a", "mods/a-2.0.jar")];
    let resolution = resolve(&root.0, &baseline(&["mods/a-1.0.jar"]), &[], &next);
    assert_eq!(resolution.selected, BTreeSet::from(["k:a".to_string()]));
    assert!(resolution.new.is_empty());
  }

  #[test]
  fn files_missing_on_disk_follow_the_record() {
    let root = Root::new();
    let optional = vec![file("k:a", "mods/a.jar"), file("k:s", "shaderpacks/s.zip")];
    write(
      &root.0,
      &OptionalRecord {
        schema: SCHEMA,
        known: optional.clone(),
        selected: vec!["k:a".into()],
      },
    )
    .unwrap();
    let resolution = resolve(&root.0, &baseline(&["mods/a.jar"]), &[], &optional);
    assert_eq!(resolution.selected, BTreeSet::from(["k:a".to_string()]));
  }

  #[test]
  fn a_file_unknown_to_the_record_is_new_and_unchosen() {
    let root = Root::new();
    let known = vec![file("k:a", "mods/a.jar")];
    write(
      &root.0,
      &OptionalRecord {
        schema: SCHEMA,
        known: known.clone(),
        selected: vec!["k:a".into()],
      },
    )
    .unwrap();
    root.write("mods/a.jar", "a");
    let added = file("k:n", "mods/new.jar");
    let optional = vec![known[0].clone(), added.clone()];
    let installed = baseline(&["mods/a.jar"]);

    let resolution = resolve(&root.0, &installed, &[], &optional);
    assert_eq!(resolution.selected, BTreeSet::from(["k:a".to_string()]));
    assert_eq!(resolution.new, [added.clone()]);

    let chosen = resolve(&root.0, &installed, &["k:n".to_string()], &optional);
    assert_eq!(
      chosen.selected,
      BTreeSet::from(["k:a".to_string(), "k:n".to_string()])
    );
    assert_eq!(chosen.new, [added]);

    let mut pack = remote(
      &[("mods/a.jar", true), ("mods/new.jar", true)],
      optional.clone(),
    );
    select(&mut pack, &root.0, &installed, &[]);
    assert_eq!(
      state(&pack),
      [
        ("mods/a.jar".to_string(), false),
        ("mods/new.jar".to_string(), true)
      ]
    );
  }

  #[test]
  fn an_instance_without_a_record_keeps_what_it_has() {
    let root = Root::new();
    let optional = vec![
      file("k:on", "mods/on.jar"),
      file("k:off", "mods/off.jar"),
      file("k:gone", "mods/gone.jar"),
      file("k:new", "mods/new.jar"),
    ];
    // Installed by an older launcher: on is enabled, off was disabled by the
    // player, gone was deleted, new is not in the baseline.
    root.write("mods/on.jar", "x");
    root.write("mods/off.jar.disabled", "x");
    let installed = baseline(&["mods/on.jar", "mods/off.jar", "mods/gone.jar"]);

    let resolution = resolve(&root.0, &installed, &[], &optional);
    assert_eq!(
      resolution.selected,
      BTreeSet::from(["k:on".to_string(), "k:gone".to_string()])
    );
    assert_eq!(resolution.new, [optional[3].clone()]);

    let mut pack = remote(
      &[
        ("mods/gone.jar", true),
        ("mods/new.jar", true),
        ("mods/off.jar", true),
        ("mods/on.jar", true),
      ],
      optional,
    );
    select(&mut pack, &root.0, &installed, &[]);
    assert_eq!(
      state(&pack),
      [
        ("mods/gone.jar".to_string(), false),
        ("mods/new.jar".to_string(), true),
        ("mods/off.jar".to_string(), true),
        ("mods/on.jar".to_string(), false),
      ]
    );
  }

  #[test]
  fn the_picks_made_before_the_install_are_chosen() {
    let root = Root::new();
    write_picks(&root.0, &["k:b".into(), "k:b".into()]).unwrap();
    assert_eq!(read(&root.0).selected, ["k:b"]);
    let optional = vec![file("k:a", "mods/a.jar"), file("k:b", "mods/b.jar")];
    let resolution = resolve(&root.0, &baseline(&[]), &[], &optional);
    assert_eq!(resolution.selected, BTreeSet::from(["k:b".to_string()]));
  }

  #[test]
  fn the_record_round_trips_and_an_empty_one_removes_the_file() {
    let root = Root::new();
    assert_eq!(read(&root.0), OptionalRecord::default());
    let record = OptionalRecord {
      schema: SCHEMA,
      known: vec![file("k:a", "mods/a.jar")],
      selected: vec!["k:a".into()],
    };
    write(&root.0, &record).unwrap();
    assert_eq!(read(&root.0), record);
    assert!(root.0.join(UPDATE_DIR).join(OPTIONAL_FILE).is_file());
    write(&root.0, &OptionalRecord::default()).unwrap();
    assert!(!root.0.join(UPDATE_DIR).join(OPTIONAL_FILE).exists());

    root.write(".ustbl-update/optional.json", "not json");
    assert_eq!(read(&root.0), OptionalRecord::default());
  }

  #[test]
  fn keys_stay_distinct_within_a_table() {
    let table = table([
      ("curseforge:1".to_string(), "mods/a.jar".to_string()),
      ("curseforge:1".to_string(), "mods/b.jar".to_string()),
      (String::new(), "mods/C.jar".to_string()),
    ]);
    let keys: Vec<_> = table.iter().map(|file| file.key.as_str()).collect();
    assert_eq!(keys, ["curseforge:1", "path:mods/b.jar", "path:mods/c.jar"]);
    assert_eq!(table[1].name, "b.jar");
  }
}
