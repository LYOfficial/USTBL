//! What a publish needs to be picked up again after the launcher was closed.
//!
//! The record is written once the platform has accepted the version and is kept
//! until the version is published or given up. It holds the plan (the manifest
//! and what the instance looked like), not the file contents: the delta pack is
//! rebuilt from the recorded sources if it is gone.

use super::manifest::PackObject;
use crate::error::{USTBLError, USTBLResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const RECORD_SCHEMA: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
  /// The version exists on the platform and the pack is being sent.
  Uploading,
  /// Committed; the platform is processing the pack.
  Processing,
  /// The platform ended with an error; sending again may help.
  Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedFile {
  pub path: String,
  pub sha1: String,
  pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedObject {
  pub sha1: String,
  pub path: PathBuf,
  pub stored: bool,
}

impl From<&PackObject> for RecordedObject {
  fn from(object: &PackObject) -> Self {
    Self {
      sha1: object.sha1.clone(),
      path: object.path.clone(),
      stored: object.stored,
    }
  }
}

impl From<&RecordedObject> for PackObject {
  fn from(object: &RecordedObject) -> Self {
    Self {
      sha1: object.sha1.clone(),
      path: object.path.clone(),
      stored: object.stored,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedPack {
  pub sha1: String,
  pub size: u64,
}

/// §9.1's exact persisted shape. Concurrency is not part of it: it is a live,
/// server-tunable value (§14.1) handed down fresh by `POST /versions` and by
/// `GET /uploads/{id}` on resume, not a fact about the upload worth freezing
/// into the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedUpload {
  pub id: String,
  pub part_size: u64,
  pub part_count: u32,
  pub generation: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRecord {
  pub schema: u32,
  pub id: String,
  pub phase: Phase,
  pub error: Option<String>,
  pub created_at: i64,
  pub instance_id: String,
  pub instance_name: String,
  pub project_slug: String,
  pub version_id: i64,
  pub version_number: String,
  /// The files the version lists; the instance is compared with them once the
  /// version is published.
  pub files: Vec<RecordedFile>,
  /// `mmc-pack.json` of a component profile, which the new baseline needs.
  pub mmc_pack: Option<Value>,
  pub skipped: Vec<String>,
  /// Where each object of the pack comes from.
  pub objects: Vec<RecordedObject>,
  pub pack: Option<RecordedPack>,
  pub upload: Option<RecordedUpload>,
}

pub fn jobs_dir() -> PathBuf {
  crate::APP_DATA_DIR
    .get()
    .expect("APP_DATA_DIR is set at startup")
    .join("modpack-publish")
}

fn valid_id(id: &str) -> bool {
  !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Where the pieces of one job live inside `dir`.
pub struct JobPaths {
  pub record: PathBuf,
  pub pack: PathBuf,
  pub work: PathBuf,
}

pub fn paths(dir: &Path, id: &str) -> USTBLResult<JobPaths> {
  if !valid_id(id) {
    return Err(USTBLError("无效的发布任务".into()));
  }
  Ok(JobPaths {
    record: dir.join(format!("{id}.json")),
    pack: dir.join(format!("{id}.zip")),
    work: dir.join(format!("{id}.work")),
  })
}

impl JobRecord {
  /// Written through a temporary file, so a crash never leaves half a record.
  pub fn save(&self, dir: &Path) -> USTBLResult<()> {
    fs::create_dir_all(dir)?;
    let target = paths(dir, &self.id)?.record;
    let temporary = target.with_extension("json.tmp");
    fs::write(
      &temporary,
      serde_json::to_vec_pretty(self).map_err(|e| USTBLError(e.to_string()))?,
    )?;
    fs::rename(&temporary, &target)?;
    Ok(())
  }

  pub fn load(dir: &Path, id: &str) -> USTBLResult<Self> {
    let bytes = fs::read(paths(dir, id)?.record)
      .map_err(|_| USTBLError("找不到这个发布任务，可能已被清理".into()))?;
    let record: Self =
      serde_json::from_slice(&bytes).map_err(|_| USTBLError("发布任务的记录已损坏".into()))?;
    if record.schema != RECORD_SCHEMA {
      return Err(USTBLError("发布任务来自不兼容的版本".into()));
    }
    Ok(record)
  }

  /// Every readable record, oldest first. Unreadable ones are left alone.
  pub fn list(dir: &Path) -> Vec<Self> {
    let Ok(entries) = fs::read_dir(dir) else {
      return Vec::new();
    };
    let mut records: Vec<Self> = entries
      .flatten()
      .filter_map(|entry| {
        let path = entry.path();
        if path.extension()? != "json" {
          return None;
        }
        Self::load(dir, path.file_stem()?.to_str()?).ok()
      })
      .collect();
    records.sort_by_key(|record| record.created_at);
    records
  }

  /// Forget the job and everything it left on disk.
  pub fn remove(dir: &Path, id: &str) {
    if let Ok(paths) = paths(dir, id) {
      let _ = fs::remove_file(paths.record);
      let _ = fs::remove_file(paths.pack);
      let _ = fs::remove_dir_all(paths.work);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn record(id: &str, created_at: i64) -> JobRecord {
    JobRecord {
      schema: RECORD_SCHEMA,
      id: id.into(),
      phase: Phase::Uploading,
      error: None,
      created_at,
      instance_id: "instance".into(),
      instance_name: "GTNH".into(),
      project_slug: "gtnh".into(),
      version_id: 7,
      version_number: "2.8.0".into(),
      files: vec![RecordedFile {
        path: "mods/a.jar".into(),
        sha1: "a".repeat(40),
        size: 3,
      }],
      mmc_pack: None,
      skipped: vec![],
      objects: vec![RecordedObject {
        sha1: "a".repeat(40),
        path: PathBuf::from("/x/mods/a.jar"),
        stored: true,
      }],
      pack: Some(RecordedPack {
        sha1: "b".repeat(40),
        size: 9,
      }),
      upload: Some(RecordedUpload {
        id: "u1".into(),
        part_size: 8,
        part_count: 2,
        generation: 1,
      }),
    }
  }

  fn dir() -> PathBuf {
    std::env::temp_dir().join(format!("ustbl-record-test-{}", uuid::Uuid::new_v4()))
  }

  #[test]
  fn a_record_survives_a_restart_and_lists_oldest_first() {
    let dir = dir();
    record("new", 20).save(&dir).unwrap();
    record("old", 10).save(&dir).unwrap();

    let loaded = JobRecord::load(&dir, "old").unwrap();
    assert_eq!(loaded.version_id, 7);
    assert_eq!(loaded.upload.unwrap().part_count, 2);
    let ids: Vec<_> = JobRecord::list(&dir).into_iter().map(|r| r.id).collect();
    assert_eq!(ids, ["old", "new"]);
    let _ = fs::remove_dir_all(dir);
  }

  #[test]
  fn removing_a_job_takes_its_pack_and_work_files_too() {
    let dir = dir();
    record("job", 1).save(&dir).unwrap();
    let paths = paths(&dir, "job").unwrap();
    fs::write(&paths.pack, "zip").unwrap();
    fs::create_dir_all(&paths.work).unwrap();

    JobRecord::remove(&dir, "job");

    assert!(!paths.record.exists() && !paths.pack.exists() && !paths.work.exists());
    assert!(JobRecord::list(&dir).is_empty());
    let _ = fs::remove_dir_all(dir);
  }

  #[test]
  fn ids_cannot_point_outside_the_jobs_directory() {
    let dir = dir();
    assert!(JobRecord::load(&dir, "../secrets").is_err());
    assert!(paths(&dir, "a/b").is_err());
    assert!(paths(&dir, "").is_err());
  }

  #[test]
  fn a_broken_or_foreign_record_is_not_listed() {
    let dir = dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("broken.json"), "{").unwrap();
    let mut foreign = record("foreign", 1);
    foreign.schema = RECORD_SCHEMA + 1;
    foreign.save(&dir).unwrap();
    assert!(JobRecord::list(&dir).is_empty());
    let _ = fs::remove_dir_all(dir);
  }

  /// Schema 1 (v2's chunk-based record) is not read back; a publish on an old
  /// record must fail outright and tell the user to start over.
  #[test]
  fn an_old_schema_record_fails_outright_instead_of_being_read_as_the_new_shape() {
    let dir = dir();
    let mut old = record("old-shape", 1);
    old.schema = 1;
    old.save(&dir).unwrap();
    assert!(JobRecord::load(&dir, "old-shape").is_err());
    assert!(JobRecord::list(&dir).is_empty());
    let _ = fs::remove_dir_all(dir);
  }
}
