//! Giving a publish up: the one decision both the task card's cancel and the
//! unfinished list's "放弃" go through, so the two can never drift apart.
//!
//! The rule: the local record, pack and work files are only removed once the
//! version is gone on the platform for sure — the `DELETE` answered with
//! success, or with a 404 telling us it was already gone. Anything else keeps
//! them, and never silently: the reason is logged, and the record stays
//! available for a later continue or discard. A 409 means the version is the
//! platform's now; its status then decides whether the platform watch is
//! resumed instead.

use super::commands::{authed_json, failure};
use crate::error::USTBLError;
use serde_json::Value;

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
  use super::super::record::{JobRecord, Phase, RecordedUpload, RECORD_SCHEMA};
  use super::*;
  use crate::error::USTBLError;

  /// Every verdict the platform can hand back, mapped by the same function
  /// the production callers use — not a separate enumeration that could
  /// drift from it.
  #[test]
  fn only_a_confirmed_deletion_removes_the_local_record() {
    assert_eq!(
      decide_cancel(&RemoteCancel::Deleted),
      CancelAction::RemoveLocal
    );
    assert_eq!(
      decide_cancel(&RemoteCancel::AlreadyGone),
      CancelAction::RemoveLocal
    );
  }

  #[test]
  fn network_or_server_trouble_keeps_the_record_and_files() {
    let network = RemoteCancel::Undetermined(USTBLError("error sending request".into()));
    assert_eq!(decide_cancel(&network), CancelAction::KeepLocal);
    let server_error = RemoteCancel::Undetermined(USTBLError("HTTP 500".into()));
    assert_eq!(decide_cancel(&server_error), CancelAction::KeepLocal);
  }

  #[test]
  fn a_processing_or_published_conflict_resumes_the_platform_watch() {
    assert_eq!(
      decide_cancel(&RemoteCancel::Processing),
      CancelAction::ResumeMonitoring
    );
    assert_eq!(
      decide_cancel(&RemoteCancel::Published),
      CancelAction::ResumeMonitoring
    );
  }

  #[test]
  fn an_unanswerable_conflict_keeps_the_record_instead_of_guessing() {
    assert_eq!(
      decide_cancel(&RemoteCancel::PlatformOwnedUnknown),
      CancelAction::KeepLocal
    );
  }

  /// The state the guard must leave behind when it hands a cancelled job
  /// back to the platform watch — asserted here so the hand-off stays
  /// deliberate, not an accident of whichever caller ran last. `Processing`
  /// with no error is what `resume_monitors` picks up on the next start
  /// and what the unfinished list offers "继续" for.
  #[test]
  fn the_record_a_resumed_watch_keeps_is_processing_without_an_error() {
    let record = JobRecord {
      schema: RECORD_SCHEMA,
      id: "job".into(),
      phase: Phase::Processing,
      error: None,
      created_at: 1,
      instance_id: "i".into(),
      instance_name: "n".into(),
      project_slug: "p".into(),
      project_name: None,
      version_id: 1,
      version_number: "1".into(),
      files: vec![],
      mmc_pack: None,
      skipped: vec![],
      objects: vec![],
      pack: None,
      upload: Some(RecordedUpload {
        id: "u".into(),
        part_size: 8,
        part_count: 1,
        generation: 1,
      }),
    };
    assert!(record.resumable());
  }
}
