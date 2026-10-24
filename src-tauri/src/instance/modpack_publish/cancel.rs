//! Giving a publish up: the one decision and the one action both the task
//! card's cancel and the unfinished list's "放弃" go through, so the two can
//! never drift apart.
//!
//! The rule: the local record, pack and work files are only removed once the
//! version is gone on the platform for sure — the `DELETE` answered with
//! success, or with a 404 telling us it was already gone. Anything else keeps
//! them, untouched and never silently: the reason is logged, and the record
//! stays exactly as it was for a later continue or discard. A 409 means the
//! version is the platform's now; its status then decides whether the
//! platform watch is resumed instead.
//!
//! Whatever the remote answer was, the action is applied to the record as it
//! is on disk *at that moment*, re-read after the remote call: the job may
//! have kept writing it (a generation restart, the phase flip to
//! `Processing`, a fresh error) while the answer was on the wire.

use super::commands::{authed_json, failure};
use super::record::{JobRecord, Phase};
use crate::error::USTBLError;
use serde_json::Value;
use std::path::Path;

/// The platform's answer to giving the version up, before any decision.
pub(super) enum RemoteCancel {
  /// `DELETE` succeeded: the version is gone.
  Deleted,
  /// `DELETE` answered 404: the version was already gone.
  AlreadyGone,
  /// `DELETE` answered 409 (a processing or published version refuses
  /// deletion) and `GET /versions/{id}/status` says the version is now being
  /// processed. The platform owns it; the watch resumes.
  Processing,
  /// Same, but the version is already published: the watch resumes and ends
  /// in the ordinary adoption (`adopt_published` via the platform stage).
  Published,
  /// The version is the platform's but the status query failed, or the
  /// status is not one deletion can act on (a 409 means processing or
  /// published; anything else would need the launcher's own judgement).
  PlatformOwnedUnknown,
  /// The `DELETE` itself failed: the version may or may not be gone (a
  /// network error could have happened after the platform removed it), so
  /// the record must stay either way.
  Undetermined(USTBLError),
}

/// What a caller of [`decide_cancel`] does next. The two production callers —
/// the cancel guard's cleanup task and `discard_campus_modpack_publish` —
/// each map this to their own context (fire-and-forget vs. a user-facing
/// error), but the decision itself is made exactly once, here.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CancelAction {
  /// Platform confirmed the version gone: remove record, pack and work.
  RemoveLocal,
  /// The version is already out or being processed: keep the record as
  /// `Processing` and resume the platform watch through the ordinary
  /// `PublishJob` path (a published version then ends in `adopt_published`).
  ResumeMonitoring,
  /// Nothing could be confirmed: keep the record and the local files. The
  /// publish can still be continued or given up later; it must not
  /// silently vanish.
  KeepLocal,
}

/// The whole delete-or-keep decision, pure so both callers share exactly
/// these rules and tests exercise the same mapping production runs.
pub(super) fn decide_cancel(remote: &RemoteCancel) -> CancelAction {
  match remote {
    RemoteCancel::Deleted | RemoteCancel::AlreadyGone => CancelAction::RemoveLocal,
    RemoteCancel::Processing | RemoteCancel::Published => CancelAction::ResumeMonitoring,
    RemoteCancel::PlatformOwnedUnknown | RemoteCancel::Undetermined(_) => CancelAction::KeepLocal,
  }
}

/// What applying a [`CancelAction`] left behind, for the caller to report.
/// `KeptLocal` carries the reason the platform could not confirm the
/// deletion (the guard logs it, `discard_campus_modpack_publish` shows it).
pub(super) enum CancelOutcome {
  /// Record, pack and work are gone from disk.
  Removed,
  /// The platform could not confirm the version is gone; nothing on disk
  /// was touched (the reason, for the log or the user).
  Kept(USTBLError),
  /// The record as it is on disk now is `Processing` again and the watch
  /// was handed to `watch`.
  Watching,
  /// The record could not be loaded or saved; nothing else was attempted.
  RecordKept(USTBLError),
}

/// The one application of a cancel decision both production callers run.
/// `remote` is the platform's answer (production gets it from
/// [`cancel_remote`]; tests inject it); `watch` is what starts the platform
/// monitor with the *freshly saved* record (production: `start_monitor`;
/// tests: a counter). Only identity (`job_id`) is taken, never a record
/// snapshot: the record is re-read from `dir` after the remote call, so a
/// job that kept running in the meantime never gets its newer
/// phase/generation/error overwritten by a stale copy.
pub(super) fn apply_cancel(
  dir: &Path,
  job_id: &str,
  remote: RemoteCancel,
  watch: impl FnOnce(JobRecord),
) -> CancelOutcome {
  match decide_cancel(&remote) {
    CancelAction::RemoveLocal => {
      JobRecord::remove(dir, job_id);
      CancelOutcome::Removed
    }
    CancelAction::KeepLocal => {
      let reason = match remote {
        RemoteCancel::Undetermined(error) => error,
        _ => USTBLError("暂时无法确认这个版本是否还能取消，请稍后再试".into()),
      };
      CancelOutcome::Kept(reason)
    }
    CancelAction::ResumeMonitoring => {
      // Re-read the record as it is now: the job may have moved on (a new
      // generation, the `Processing` flip, a fresh error) while the remote
      // answer was on the wire. Only the latest record may be handed to
      // the watch, and only after it is safely on disk — starting the
      // watch with an in-memory change that never persisted would leave
      // nothing to resume from if the launcher dies right after.
      let mut record = match JobRecord::load(dir, job_id) {
        Ok(record) => record,
        Err(error) => return CancelOutcome::RecordKept(error),
      };
      record.phase = Phase::Processing;
      record.error = None;
      if let Err(error) = record.save(dir) {
        return CancelOutcome::RecordKept(error);
      }
      watch(record);
      CancelOutcome::Watching
    }
  }
}

/// `DELETE /versions/{id}` plus, on a 409, the status probe that tells why
/// the platform refuses to drop the version (§3.2, v3 §8.4).
pub(super) async fn cancel_remote(app: &tauri::AppHandle, version_id: i64) -> RemoteCancel {
  let response = match authed_json(
    app,
    reqwest::Method::DELETE,
    &format!("/versions/{version_id}"),
    None,
  )
  .await
  {
    Ok(response) => response,
    Err(error) => return RemoteCancel::Undetermined(error),
  };
  let status = response.status();
  if status.is_success() {
    return RemoteCancel::Deleted;
  }
  if status == reqwest::StatusCode::NOT_FOUND {
    return RemoteCancel::AlreadyGone;
  }
  if status != reqwest::StatusCode::CONFLICT {
    return RemoteCancel::Undetermined(failure(response).await);
  }
  // A 409 means the platform owns the version (processing or published);
  // find out which before deciding anything.
  match platform_status(app, version_id).await.as_deref() {
    Some("processing") => RemoteCancel::Processing,
    Some("published") => RemoteCancel::Published,
    // The status query failed or the status is not one deletion can act
    // on: keep the record rather than guessing.
    _ => RemoteCancel::PlatformOwnedUnknown,
  }
}

/// `GET /versions/{id}/status`'s `status` field; `None` when the platform
/// cannot be asked or does not answer (network trouble, a non-success or an
/// unreadable body all count as "unknown", never as "gone"). One call,
/// one question: both production callers reach it only through
/// [`cancel_remote`], so the probe's failure handling cannot drift between
/// the task card's cancel and the unfinished list's "放弃" either.
async fn platform_status(app: &tauri::AppHandle, version_id: i64) -> Option<String> {
  let response = authed_json(
    app,
    reqwest::Method::GET,
    &format!("/versions/{version_id}/status"),
    None,
  )
  .await
  .ok()?;
  if !response.status().is_success() {
    return None;
  }
  let body: Value = response.json().await.ok()?;
  body["status"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
  use super::super::record::{paths, RecordedPack, RecordedUpload, RECORD_SCHEMA};
  use super::*;
  use std::fs;
  use std::path::PathBuf;
  use std::sync::{Arc, Mutex};

  fn record(id: &str) -> JobRecord {
    JobRecord {
      schema: RECORD_SCHEMA,
      id: id.into(),
      phase: Phase::Uploading,
      error: None,
      created_at: 1,
      instance_id: "i".into(),
      instance_name: "n".into(),
      project_slug: "p".into(),
      project_name: None,
      version_id: 7,
      version_number: "1".into(),
      files: vec![],
      mmc_pack: None,
      skipped: vec![],
      objects: vec![],
      pack: Some(RecordedPack {
        sha1: "b".repeat(40),
        size: 9,
      }),
      upload: Some(RecordedUpload {
        id: "u".into(),
        part_size: 8,
        part_count: 1,
        generation: 1,
      }),
    }
  }

  fn dir() -> PathBuf {
    std::env::temp_dir().join(format!("ustbl-cancel-test-{}", uuid::Uuid::new_v4()))
  }

  /// A record on disk with its pack and work files, as a real job leaves
  /// them behind.
  fn on_disk(id: &str) -> (PathBuf, super::super::record::JobPaths) {
    let dir = dir();
    record(id).save(&dir).unwrap();
    let job_paths = paths(&dir, id).unwrap();
    fs::write(&job_paths.pack, "zip").unwrap();
    fs::create_dir_all(&job_paths.work).unwrap();
    fs::write(job_paths.work.join("tmp"), "w").unwrap();
    (dir, job_paths)
  }

  fn exists(job_paths: &super::super::record::JobPaths) -> bool {
    job_paths.record.exists() || job_paths.pack.exists() || job_paths.work.exists()
  }

  /// A watch that only counts how often it fired and what it saw.
  #[derive(Clone, Default)]
  struct Watched(Arc<Mutex<Vec<JobRecord>>>);
  impl Watched {
    fn count(&self) -> usize {
      self.0.lock().unwrap().len()
    }
    fn callback(&self) -> impl FnOnce(JobRecord) {
      let watched = self.clone();
      move |record| watched.0.lock().unwrap().push(record)
    }
  }

  #[test]
  fn a_confirmed_deletion_removes_record_pack_and_work() {
    for remote in [RemoteCancel::Deleted, RemoteCancel::AlreadyGone] {
      let (dir, job_paths) = on_disk("job");
      let watched = Watched::default();
      let outcome = apply_cancel(&dir, "job", remote, watched.callback());
      assert!(matches!(outcome, CancelOutcome::Removed));
      assert!(!exists(&job_paths));
      assert_eq!(watched.count(), 0);
      let _ = fs::remove_dir_all(dir);
    }
  }

  #[test]
  fn an_undetermined_answer_keeps_everything_exactly_as_it_was() {
    let (dir, job_paths) = on_disk("job");
    // An older error and phase the cleanup must not touch.
    let mut before = JobRecord::load(&dir, "job").unwrap();
    before.phase = Phase::Failed;
    before.error = Some("HTTP 500".into());
    before.save(&dir).unwrap();

    let watched = Watched::default();
    let outcome = apply_cancel(
      &dir,
      "job",
      RemoteCancel::Undetermined(USTBLError("error sending request".into())),
      watched.callback(),
    );
    assert!(matches!(outcome, CancelOutcome::Kept(_)));
    assert!(exists(&job_paths));
    let after = JobRecord::load(&dir, "job").unwrap();
    assert_eq!(after.phase, Phase::Failed);
    assert_eq!(after.error.as_deref(), Some("HTTP 500"));
    assert_eq!(after.upload.unwrap().generation, 1);
    assert_eq!(watched.count(), 0);
    let _ = fs::remove_dir_all(dir);
  }

  #[test]
  fn an_unanswerable_conflict_keeps_everything_and_writes_no_mark() {
    let (dir, job_paths) = on_disk("job");
    let watched = Watched::default();
    let outcome = apply_cancel(
      &dir,
      "job",
      RemoteCancel::PlatformOwnedUnknown,
      watched.callback(),
    );
    assert!(matches!(outcome, CancelOutcome::Kept(_)));
    assert!(exists(&job_paths));
    let after = JobRecord::load(&dir, "job").unwrap();
    assert_eq!(after.phase, Phase::Uploading);
    assert_eq!(after.error, None);
    assert_eq!(watched.count(), 0);
    let _ = fs::remove_dir_all(dir);
  }

  #[test]
  fn a_conflict_saves_processing_and_fires_the_watch_exactly_once() {
    for remote in [RemoteCancel::Processing, RemoteCancel::Published] {
      let (dir, job_paths) = on_disk("job");
      let watched = Watched::default();
      let outcome = apply_cancel(&dir, "job", remote, watched.callback());
      assert!(matches!(outcome, CancelOutcome::Watching));
      // The record on disk is the one the watch can resume from:
      // `Processing`, no error, and the files are all still there.
      let after = JobRecord::load(&dir, "job").unwrap();
      assert_eq!(after.phase, Phase::Processing);
      assert_eq!(after.error, None);
      assert!(exists(&job_paths));
      assert_eq!(watched.count(), 1);
      // The watch got exactly the record that was saved, not an older copy.
      let handed = &watched.0.lock().unwrap()[0];
      assert_eq!(handed.phase, Phase::Processing);
      assert_eq!(handed.upload.as_ref().unwrap().generation, 1);
      let _ = fs::remove_dir_all(dir);
    }
  }

  /// The bug this whole rework fixes: the guard was cloned at job start
  /// (`Uploading`, generation 1), but by the time the remote answer came
  /// back the job had already committed (`Processing`, generation 3 on
  /// disk). Applying the action must never write the stale snapshot back.
  #[test]
  fn a_newer_record_on_disk_is_never_rolled_back_by_the_cleanup() {
    let (dir, _job_paths) = on_disk("job");
    let mut newer = JobRecord::load(&dir, "job").unwrap();
    newer.phase = Phase::Processing;
    newer.upload.as_mut().unwrap().generation = 3;
    newer.save(&dir).unwrap();

    let watched = Watched::default();
    let outcome = apply_cancel(&dir, "job", RemoteCancel::Processing, watched.callback());
    assert!(matches!(outcome, CancelOutcome::Watching));
    let after = JobRecord::load(&dir, "job").unwrap();
    assert_eq!(after.phase, Phase::Processing);
    assert_eq!(after.upload.unwrap().generation, 3);
    assert_eq!(watched.count(), 1);
    let _ = fs::remove_dir_all(dir);
  }

  /// A record that cannot be saved must not be watched either: the watch
  /// would run on something nothing can resume from. The disk record stays.
  #[test]
  fn a_record_that_cannot_be_saved_is_not_watched() {
    let (dir, _job_paths) = on_disk("job");
    // Make the record's own path unwritable by putting a directory there.
    let job_paths = paths(&dir, "job").unwrap();
    fs::remove_file(&job_paths.record).unwrap();
    fs::create_dir(&job_paths.record).unwrap();

    let watched = Watched::default();
    let outcome = apply_cancel(&dir, "job", RemoteCancel::Processing, watched.callback());
    assert!(matches!(outcome, CancelOutcome::RecordKept(_)));
    assert_eq!(watched.count(), 0);
    let _ = fs::remove_dir_all(dir);
  }

  /// A record that cannot even be loaded keeps the cleanup from doing
  /// anything at all.
  #[test]
  fn a_record_that_cannot_be_loaded_is_left_alone() {
    let dir = dir();
    let watched = Watched::default();
    let outcome = apply_cancel(
      &dir,
      "missing",
      RemoteCancel::Processing,
      watched.callback(),
    );
    assert!(matches!(outcome, CancelOutcome::RecordKept(_)));
    assert_eq!(watched.count(), 0);
    let _ = fs::remove_dir_all(dir);
  }
}
