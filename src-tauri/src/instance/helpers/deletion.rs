use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};
use walkdir::WalkDir;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceDeletionProgress {
  pub stage: &'static str,
  pub total_files: usize,
  pub deleted_files: usize,
}

// 先统计文件，再逐项删除；不跟随符号链接，并限制进度通知频率。
pub fn delete_instance_files(
  root: &Path,
  mut report: impl FnMut(InstanceDeletionProgress),
) -> io::Result<()> {
  let mut progress = InstanceDeletionProgress {
    stage: "scanning",
    total_files: 0,
    deleted_files: 0,
  };
  report(progress.clone());
  match fs::symlink_metadata(root) {
    Ok(_) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      progress.stage = "finalizing";
      report(progress);
      return Ok(());
    }
    Err(error) => return Err(error),
  }

  let mut entries = Vec::new();
  let mut last_report = Instant::now();
  for entry in WalkDir::new(root)
    .follow_links(false)
    .follow_root_links(false)
    .contents_first(true)
  {
    let entry = entry?;
    if !entry.file_type().is_dir() {
      progress.total_files += 1;
    }
    entries.push(entry);
    if last_report.elapsed() >= Duration::from_millis(50) {
      report(progress.clone());
      last_report = Instant::now();
    }
  }

  progress.stage = "deleting";
  report(progress.clone());
  last_report = Instant::now();
  for entry in entries {
    let file_type = entry.file_type();
    #[cfg(windows)]
    let is_directory = {
      use std::os::windows::fs::FileTypeExt;
      file_type.is_dir() || file_type.is_symlink_dir()
    };
    #[cfg(not(windows))]
    let is_directory = file_type.is_dir();

    let result = if is_directory {
      fs::remove_dir(entry.path())
    } else {
      fs::remove_file(entry.path())
    };
    match result {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
    if !file_type.is_dir() {
      progress.deleted_files += 1;
    }
    if last_report.elapsed() >= Duration::from_millis(50) {
      report(progress.clone());
      last_report = Instant::now();
    }
  }
  progress.stage = "finalizing";
  report(progress);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ustbl-delete-test-{}", uuid::Uuid::new_v4()))
  }

  #[test]
  fn counts_files_before_deleting_nested_and_empty_directories() {
    let root = temp_root();
    fs::create_dir_all(root.join("saves/world/empty")).unwrap();
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/example.jar"), b"mod").unwrap();
    fs::write(root.join("saves/world/level.dat"), b"save").unwrap();
    fs::write(root.join("instance.json"), b"{}").unwrap();
    let mut updates = Vec::new();
    delete_instance_files(&root, |progress| updates.push(progress)).unwrap();
    assert!(!root.exists());
    let deleting = updates.iter().find(|p| p.stage == "deleting").unwrap();
    assert_eq!((deleting.total_files, deleting.deleted_files), (3, 0));
    let final_progress = updates.last().unwrap();
    assert_eq!(final_progress.stage, "finalizing");
    assert_eq!(
      (final_progress.total_files, final_progress.deleted_files),
      (3, 3)
    );
  }

  #[test]
  fn empty_and_missing_directories_finish_with_zero_files() {
    let root = temp_root();
    fs::create_dir(&root).unwrap();
    for _ in 0..2 {
      let mut updates = Vec::new();
      delete_instance_files(&root, |progress| updates.push(progress)).unwrap();
      let progress = updates.last().unwrap();
      assert_eq!(progress.stage, "finalizing");
      assert_eq!((progress.total_files, progress.deleted_files), (0, 0));
    }
  }

  #[test]
  fn does_not_follow_directory_links_outside_the_instance() {
    let root = temp_root();
    let instance = root.join("instance");
    let shared = root.join("shared");
    fs::create_dir_all(&instance).unwrap();
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("keep.txt"), b"keep").unwrap();
    #[cfg(windows)]
    assert!(std::process::Command::new("cmd")
      .args(["/C", "mklink", "/J"])
      .arg(instance.join("linked"))
      .arg(&shared)
      .output()
      .unwrap()
      .status
      .success());
    #[cfg(unix)]
    std::os::unix::fs::symlink(&shared, instance.join("linked")).unwrap();
    delete_instance_files(&instance, |_| {}).unwrap();
    assert!(!instance.exists());
    assert_eq!(fs::read(shared.join("keep.txt")).unwrap(), b"keep");
    delete_instance_files(&root, |_| {}).unwrap();
  }
}
