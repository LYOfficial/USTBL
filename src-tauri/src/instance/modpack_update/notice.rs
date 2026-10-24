//! Per-instance record of the modpack update the launcher already announced,
//! so a startup check announces each new version only once.

use super::remote::UPDATE_DIR;
use crate::error::USTBLResult;
use crate::instance::models::misc::InstanceError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const NOTICE_FILE: &str = "update-notice.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateNotice {
  #[serde(rename = "versionId")]
  pub version_id: String,
}

fn notice_path(instance_dir: &std::path::Path) -> PathBuf {
  instance_dir.join(UPDATE_DIR).join(NOTICE_FILE)
}

/// Read the version id the launcher already announced for this instance.
/// Returns `None` when no notice was recorded yet or the file is unreadable.
pub fn read_update_notice(instance_dir: &std::path::Path) -> Option<UpdateNotice> {
  let path = notice_path(instance_dir);
  let data = fs::read_to_string(path).ok()?;
  serde_json::from_str(&data).ok()
}

/// Record that the launcher announced `version_id` for this instance.
pub fn write_update_notice(
  instance_dir: &std::path::Path,
  notice: &UpdateNotice,
) -> USTBLResult<()> {
  let dir = instance_dir.join(UPDATE_DIR);
  fs::create_dir_all(&dir).map_err(|_| InstanceError::FileOperationError)?;
  let path = notice_path(instance_dir);
  let data = serde_json::to_string_pretty(notice).map_err(|_| InstanceError::FileOperationError)?;
  fs::write(path, data).map_err(|_| InstanceError::FileOperationError)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let path =
        std::env::temp_dir().join(format!("ustbl-notice-test-{name}-{}", std::process::id()));
      let _ = fs::remove_dir_all(&path);
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn read_returns_none_when_missing() {
    let dir = TestDir::new("missing");
    assert_eq!(read_update_notice(&dir.0), None);
  }

  #[test]
  fn write_then_read_roundtrip() {
    let dir = TestDir::new("roundtrip");
    let notice = UpdateNotice {
      version_id: "v-123".to_string(),
    };
    write_update_notice(&dir.0, &notice).unwrap();
    assert_eq!(read_update_notice(&dir.0), Some(notice));
  }

  #[test]
  fn read_returns_none_on_invalid_json() {
    let dir = TestDir::new("invalid");
    let path = dir.0.join(UPDATE_DIR).join(NOTICE_FILE);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "not json").unwrap();
    assert_eq!(read_update_notice(&dir.0), None);
  }

  #[test]
  fn write_overwrites_previous_notice() {
    let dir = TestDir::new("overwrite");
    let first = UpdateNotice {
      version_id: "v-1".to_string(),
    };
    let second = UpdateNotice {
      version_id: "v-2".to_string(),
    };
    write_update_notice(&dir.0, &first).unwrap();
    write_update_notice(&dir.0, &second).unwrap();
    assert_eq!(read_update_notice(&dir.0), Some(second));
  }
}
