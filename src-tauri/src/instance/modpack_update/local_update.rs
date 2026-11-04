//! Updating a local import from an archive the player picked.
//!
//! A local import has no remote to follow, so the engine treats it as an
//! instance without an origin. Picking a file stages that archive next to the
//! instance and records it as the pending target; while the record is valid
//! the engine handles the instance and offers the archive as its newest version.

use super::remote::UPDATE_DIR;
use super::sources::{archive_path, LatestVersion, Payload};
use crate::error::USTBLResult;
use crate::file_error::AtPath;
use crate::instance::models::misc::{InstanceError, InstanceOrigin, OriginPlatform, PackFormat};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const TARGET_FILE: &str = "local-target.json";

/// The archive picked to update a local import with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalTarget {
  pub sha1: String,
  pub label: String,
}

impl LocalTarget {
  /// The archive is the version: its SHA-1 is the version id, like in the
  /// origin of a local import.
  pub fn latest(&self) -> LatestVersion {
    LatestVersion {
      version_id: self.sha1.clone(),
      label: self.label.clone(),
      payload: Some(Payload::Archive {
        url: String::new(),
        sha1: self.sha1.clone(),
      }),
    }
  }
}

/// Kept outside `remote/`, which every finished or rolled back update clears.
fn target_path(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join(TARGET_FILE)
}

pub(crate) fn save_target(root: &Path, target: &LocalTarget) -> USTBLResult<()> {
  let path = target_path(root);
  let parent = path.parent().unwrap();
  fs::create_dir_all(parent).at_in(parent, root)?;
  let temporary = path.with_extension("json.tmp");
  fs::write(&temporary, serde_json::to_vec_pretty(target)?).at_in(&temporary, root)?;
  fs::rename(&temporary, &path).at_in(&path, root)?;
  Ok(())
}

pub(crate) fn load_target(root: &Path) -> Option<LocalTarget> {
  serde_json::from_slice(&fs::read(target_path(root)).ok()?).ok()
}

pub(crate) fn clear_target(root: &Path) {
  let _ = fs::remove_file(target_path(root));
}

/// The picked archive that updates this instance, if there is one. A record
/// whose archive is gone (an update cleaned the folder) or that names the
/// installed version does not count.
pub(crate) fn pending_target(root: &Path, origin: &InstanceOrigin) -> Option<LocalTarget> {
  if origin.platform != OriginPlatform::Local || origin.version_id.is_empty() {
    return None;
  }
  load_target(root)
    .filter(|target| target.sha1 != origin.version_id && archive_path(root, &target.sha1).is_file())
}

/// Drop the pending target together with its staged archive.
pub(crate) fn discard_target(root: &Path) {
  if let Some(target) = load_target(root) {
    let _ = fs::remove_file(archive_path(root, &target.sha1));
  }
  clear_target(root);
}

/// Copy the picked archive to where the engine reads it from. The original
/// may be moved or deleted while the dialog is open.
pub(crate) fn stage_archive(source: &Path, root: &Path, sha1: &str) -> USTBLResult<()> {
  let target = archive_path(root, sha1);
  let parent = target.parent().unwrap();
  fs::create_dir_all(parent).at_in(parent, root)?;
  let temporary = target.with_extension("zip.part");
  fs::copy(source, &temporary).at_in(&temporary, root)?;
  fs::rename(&temporary, &target).at_in(&target, root)?;
  Ok(())
}

/// Whether an archive in format `new` for Minecraft `game_version` can update
/// the instance. `installed` is the format recorded at import. Instances
/// imported before it was recorded have none: a component chain in the
/// baseline says MultiMC, and any other pack was a Modrinth or CurseForge one
/// without saying which, so either of those two is accepted.
pub(crate) fn check_compatible(
  installed: Option<PackFormat>,
  has_components: bool,
  origin: &InstanceOrigin,
  new: PackFormat,
  game_version: &str,
) -> USTBLResult<()> {
  let same_format = match installed {
    Some(format) => format == new,
    None => has_components == (new == PackFormat::MultiMc),
  };
  if !same_format {
    return Err(InstanceError::ModpackUpdateLocalFormatMismatch.into());
  }
  if game_version != origin.game_version {
    return Err(InstanceError::ModpackUpdateLocalGameVersionMismatch.into());
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::ModLoaderType;

  struct TempRoot(PathBuf);

  impl TempRoot {
    fn new(name: &str) -> Self {
      let root =
        std::env::temp_dir().join(format!("ustbl-local-update-{name}-{}", std::process::id()));
      let _ = fs::remove_dir_all(&root);
      fs::create_dir_all(&root).unwrap();
      Self(root)
    }
  }

  impl Drop for TempRoot {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn local(version_id: &str) -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Local,
      project_id: "pack".into(),
      version_id: version_id.into(),
      game_version: "1.7.10".into(),
      loader_type: ModLoaderType::LegacyForge,
      follow: false,
      pack_format: None,
    }
  }

  fn target(sha1: &str) -> LocalTarget {
    LocalTarget {
      sha1: sha1.into(),
      label: "2.9.0".into(),
    }
  }

  fn error_of(result: USTBLResult<()>) -> String {
    result.unwrap_err().0
  }

  #[test]
  fn the_target_is_written_read_and_cleared() {
    let root = TempRoot::new("record");
    assert_eq!(load_target(&root.0), None);
    save_target(&root.0, &target("aaa")).unwrap();
    assert_eq!(load_target(&root.0), Some(target("aaa")));
    save_target(&root.0, &target("bbb")).unwrap();
    assert_eq!(load_target(&root.0), Some(target("bbb")));
    clear_target(&root.0);
    assert_eq!(load_target(&root.0), None);
    clear_target(&root.0);
  }

  #[test]
  fn a_target_counts_only_for_an_installed_local_import_with_its_archive() {
    let root = TempRoot::new("pending");
    save_target(&root.0, &target("bbb")).unwrap();
    // The record alone is not enough: the staged archive has to be there.
    assert_eq!(pending_target(&root.0, &local("aaa")), None);

    stage_archive(&root.0.join(UPDATE_DIR).join(TARGET_FILE), &root.0, "bbb").unwrap();
    assert_eq!(pending_target(&root.0, &local("aaa")), Some(target("bbb")));

    // Not while the import is still installing, and never for another platform.
    assert_eq!(pending_target(&root.0, &local("")), None);
    let platform = InstanceOrigin {
      platform: OriginPlatform::Modrinth,
      ..local("aaa")
    };
    assert_eq!(pending_target(&root.0, &platform), None);
    // The installed version is not an update to itself.
    assert_eq!(pending_target(&root.0, &local("bbb")), None);
  }

  #[test]
  fn the_target_becomes_the_latest_version_by_its_archive() {
    let latest = target("bbb").latest();
    assert_eq!(latest.version_id, "bbb");
    assert_eq!(latest.label, "2.9.0");
    assert_eq!(latest.archive_ref(), Some(("bbb", "")));
  }

  #[test]
  fn discarding_removes_the_record_and_the_staged_archive() {
    let root = TempRoot::new("discard");
    save_target(&root.0, &target("bbb")).unwrap();
    stage_archive(&root.0.join(UPDATE_DIR).join(TARGET_FILE), &root.0, "bbb").unwrap();
    assert!(archive_path(&root.0, "bbb").is_file());
    discard_target(&root.0);
    assert!(!archive_path(&root.0, "bbb").exists());
    assert_eq!(load_target(&root.0), None);
    discard_target(&root.0);
  }

  #[test]
  fn a_pack_of_another_format_is_refused() {
    let origin = local("aaa");
    for (installed, new) in [
      (PackFormat::MultiMc, PackFormat::Modrinth),
      (PackFormat::MultiMc, PackFormat::CurseForge),
      (PackFormat::Modrinth, PackFormat::MultiMc),
      (PackFormat::Modrinth, PackFormat::CurseForge),
      (PackFormat::CurseForge, PackFormat::Modrinth),
      (PackFormat::CurseForge, PackFormat::MultiMc),
    ] {
      assert_eq!(
        error_of(check_compatible(
          Some(installed),
          false,
          &origin,
          new,
          "1.7.10"
        )),
        "MODPACK_UPDATE_LOCAL_FORMAT_MISMATCH",
        "{installed:?} -> {new:?}"
      );
    }
    for format in [
      PackFormat::MultiMc,
      PackFormat::Modrinth,
      PackFormat::CurseForge,
    ] {
      check_compatible(Some(format), false, &origin, format, "1.7.10").unwrap();
    }
  }

  #[test]
  fn another_minecraft_version_is_refused() {
    let origin = local("aaa");
    assert_eq!(
      error_of(check_compatible(
        Some(PackFormat::MultiMc),
        true,
        &origin,
        PackFormat::MultiMc,
        "1.12.2"
      )),
      "MODPACK_UPDATE_LOCAL_GAME_VERSION_MISMATCH"
    );
  }

  #[test]
  fn the_format_is_checked_before_the_game_version() {
    assert_eq!(
      error_of(check_compatible(
        Some(PackFormat::MultiMc),
        true,
        &local("aaa"),
        PackFormat::Modrinth,
        "1.12.2"
      )),
      "MODPACK_UPDATE_LOCAL_FORMAT_MISMATCH"
    );
  }

  #[test]
  fn an_import_without_a_recorded_format_is_told_by_its_component_chain() {
    let origin = local("aaa");
    check_compatible(None, true, &origin, PackFormat::MultiMc, "1.7.10").unwrap();
    assert_eq!(
      error_of(check_compatible(
        None,
        true,
        &origin,
        PackFormat::Modrinth,
        "1.7.10"
      )),
      "MODPACK_UPDATE_LOCAL_FORMAT_MISMATCH"
    );
    check_compatible(None, false, &origin, PackFormat::Modrinth, "1.7.10").unwrap();
    check_compatible(None, false, &origin, PackFormat::CurseForge, "1.7.10").unwrap();
    assert_eq!(
      error_of(check_compatible(
        None,
        false,
        &origin,
        PackFormat::MultiMc,
        "1.7.10"
      )),
      "MODPACK_UPDATE_LOCAL_FORMAT_MISMATCH"
    );
  }
}
