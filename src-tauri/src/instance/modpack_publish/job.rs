//! The background half of publishing: sends the delta pack, commits the version
//! and waits for the platform to publish it.
//!
//! The command prepares the plan and the version and returns; this runs on its
//! own and shows on the task list next to the downloads. Everything it needs to
//! start over after a failure or a restart is in the [`JobRecord`].

use super::cancel::{apply_cancel, cancel_remote, CancelOutcome};
use super::commands::{
  adopt_published, authed_json, failure, failure_code, failure_detail, instance_of, PublishOutcome,
};
use super::manifest::{LocalFile, PackObject};
use super::pack::{is_verified, mark_verified, rebuild_pack, verify_pack};
use super::record::{jobs_dir, paths, JobRecord, Phase, RecordedUpload};
use super::transfer::{report, SentBytes};
use crate::error::{USTBLError, USTBLResult};
use crate::instance::models::misc::InstanceError;
use crate::instance::modpack_update::diff::FileKey;
use crate::tasks::monitor::TaskMonitor;
use crate::tasks::upload::upload_handle;
use crate::tasks::PTaskHandle;
use futures::stream::{FuturesUnordered, StreamExt};
use md5::{Digest, Md5};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, SeekFrom};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::time::{interval, sleep, MissedTickBehavior};
use tokio_util::bytes::Bytes;

const FINISHED_EVENT: &str = "modpack-publish:finished";
const PART_ATTEMPTS: u32 = 4;
/// Consecutive attempts answered with a server error before the part uploader
/// gives up and probes the upload session for a silent restart (§9.3).
const PROBE_AFTER_FAILURES: u32 = 3;
/// Consecutive clock-skew re-signs tolerated for the same part before one is
/// counted as an ordinary failed attempt instead: re-signing must not be
/// able to loop forever without ever waiting.
const SKEW_RESIGN_LIMIT: u32 = 3;
/// Bytes handed to the HTTP stack at a time, so progress moves smoothly.
const SLICE: usize = 64 * 1024;
/// Matches the reporting interval of the task list, which shows speed per tick.
const TICK: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_secs(2);
/// Consecutive status checks that may go unanswered while the platform is busy
/// processing the pack before the job gives up (it stays resumable).
const POLL_MISSES: u32 = 30;
const COMMIT_ATTEMPTS: u32 = 3;
/// A batch of signatures is only used within this long after `signed_at`
/// (§13.2, §14.1: 10 minutes of the 900 s skew allowance, with margin).
const SIGNATURE_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Parts signed by one request (§9.1/§14.1: `MAX_SIGN_PARTS` on the server).
const SIGN_BATCH: usize = 8;
/// Generation restarts tolerated within one upload before it is reported as
/// stuck instead of retried again (§9.1, §9.3).
const MAX_RESTARTS: u32 = 10;

type Handle = Arc<RwLock<PTaskHandle>>;

/// Jobs that run right now; the rest of the records are waiting for the user.
static ACTIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn is_running(job_id: &str) -> bool {
  ACTIVE.lock().unwrap().iter().any(|id| id == job_id)
}

pub(super) fn ensure_not_running(job_id: &str) -> USTBLResult<()> {
  if is_running(job_id) {
    return Err(USTBLError("任务正在进行，请先在任务列表中取消".into()));
  }
  Ok(())
}

/// After a restart, a record the platform is still processing gets its task
/// card back (§3.2.2): the card starts right at the platform stage instead
/// of the publish being watched invisibly in the background.
pub(crate) fn resume_monitors(app: &AppHandle) {
  for record in JobRecord::list(&jobs_dir()) {
    if record.phase == Phase::Processing {
      start_monitor(app, record);
    }
  }
}

/// Jobs whose cancel cleanup or discard is still on the wire. Process-local
/// only: nothing about it is persisted. While a job id is listed here,
/// `resume_campus_modpack_publish` and `discard_campus_modpack_publish`
/// refuse to touch it, so neither can race the cleanup's own read of the
/// record or the watch it may start.
static CANCELLING: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn is_cancelling(job_id: &str) -> bool {
  CANCELLING.lock().unwrap().iter().any(|id| id == job_id)
}

/// Registered synchronously before any cancel cleanup or discard reaches
/// the network, and released only when the whole exchange — remote answer,
/// record action, and the watch it may start — is fully done. Never
/// inferred from the job's own `ActiveToken`, which drops with the
/// cancelled task itself, long before the cleanup's remote calls finish.
pub(super) struct CancellingToken(String);

impl CancellingToken {
  pub(super) fn take(job_id: &str) -> Option<Self> {
    let mut cancelling = CANCELLING.lock().unwrap();
    if cancelling.iter().any(|id| id == job_id) {
      return None;
    }
    cancelling.push(job_id.to_string());
    Some(Self(job_id.to_string()))
  }
}

impl Drop for CancellingToken {
  fn drop(&mut self) {
    CANCELLING.lock().unwrap().retain(|id| id != &self.0);
  }
}

/// The cancel guard's cleanup, run as its own task so it does not outlive
/// the app. The remote answer decides through the shared action; the record
/// it applies to is re-read from disk at that point, never the snapshot the
/// guard was created with. The `cancelling` registration outlives the whole
/// exchange and is released last, after the watch (if any) is running and
/// `ACTIVE` can take over the duplicate protection.
async fn cleanup_cancelled(app: &AppHandle, job_id: String, version_id: i64) {
  let remote = cancel_remote(app, version_id).await;
  let app_for_watch = app.clone();
  let outcome = apply_cancel(&jobs_dir(), &job_id, remote, move |record| {
    start_monitor(&app_for_watch, record);
  });
  match outcome {
    CancelOutcome::Removed => {}
    CancelOutcome::Kept(reason) => {
      log::warn!(
        "Modpack publish {job_id} was cancelled but the platform could not confirm \
         the version is gone ({}); keeping the local record for a later continue or discard",
        reason.0
      );
    }
    CancelOutcome::Watching => {}
    CancelOutcome::RecordKept(error) => {
      log::warn!(
        "Modpack publish {job_id} was cancelled but its record could not be read back or saved: {error:?}"
      );
    }
  }
}

/// Watch the platform stage of `record` through the ordinary job path —
/// the same one `resume_monitors` and a 409-conflicted discard use. A
/// failed start (the job already runs) only means the watch is picked up
/// again later; the record itself is never touched here.
pub(super) fn start_monitor(app: &AppHandle, record: JobRecord) {
  let _ = PublishJob {
    app: app.clone(),
    uploader: crate::utils::web::build_part_upload_client(app),
    record,
    // Only platform processing is watched here, never a part upload.
    concurrency: None,
  }
  .start();
}

struct ActiveToken(String);

impl ActiveToken {
  fn take(job_id: &str) -> Option<Self> {
    let mut active = ACTIVE.lock().unwrap();
    if active.iter().any(|id| id == job_id) {
      return None;
    }
    active.push(job_id.to_string());
    Some(Self(job_id.to_string()))
  }
}

impl Drop for ActiveToken {
  fn drop(&mut self) {
    ACTIVE.lock().unwrap().retain(|id| id != &self.0);
  }
}

/// The three stages one publish shows on the task list (§3.2.2): the pack
/// going to the cloud drive, the version being committed, and the platform
/// processing it. The group stays until the publish is published or failed.
struct Tasks {
  group: String,
  pack: Handle,
  commit: Handle,
  platform: Handle,
}

impl Tasks {
  fn all(&self) -> impl Iterator<Item = &Handle> {
    [&self.pack, &self.commit, &self.platform].into_iter()
  }

  /// After a failure: whatever has not ended is cancelled.
  fn abandon(&self) {
    for handle in self.all() {
      let mut handle = handle.write().unwrap();
      let status = handle.status().clone();
      if status.is_waiting() || status.is_in_progress() || status.is_stopped() {
        handle.mark_cancelled();
      }
    }
  }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishFinished {
  instance_id: String,
  group: String,
  job_id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  outcome: Option<PublishOutcome>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
}

/// Cancelling the task drops the job; that gives the version up on the
/// platform and — only once the platform confirms the version is gone —
/// forgets the record (§3.2, v3 §12). Closing the launcher drops the job
/// too but must keep both, so the cleanup is a task of its own that does
/// not outlive the app. The guard holds only the job's identity: the record
/// is re-read from disk once the remote answer is in, since the job may
/// have kept writing it until the very moment it was cancelled.
struct CancelGuard {
  app: AppHandle,
  job_id: String,
  version_id: i64,
  armed: bool,
}

impl Drop for CancelGuard {
  fn drop(&mut self) {
    if !self.armed {
      return;
    }
    let (app, job_id, version_id) = (self.app.clone(), self.job_id.clone(), self.version_id);
    // Registered synchronously, before the cleanup ever reaches the
    // runtime: a resume or discard coming in right after the cancel must
    // already see the job as busy. The token lives in the cleanup task
    // and drops when it is fully done.
    let Some(token) = CancellingToken::take(&job_id) else {
      return;
    };
    tauri::async_runtime::spawn(async move {
      let _token = token;
      cleanup_cancelled(&app, job_id, version_id).await;
    });
  }
}

fn complete(handle: &Handle) {
  let mut handle = handle.write().unwrap();
  handle.desc.current = handle.desc.total;
  handle.mark_completed();
}

/// Why `execute` ended early. `Fresh` means the record on disk was not
/// touched by this run and still carries whatever it had (possibly an old,
/// unrelated error); `Recorded` means the platform stage already saved the
/// record itself — `Failed` for a real failure, still `Processing` with the
/// error set when the watch lost the platform — so `run` must leave it
/// alone instead of overwriting it with the same or a stale message.
enum ExecuteFailure {
  Fresh(USTBLError),
  Recorded,
}

impl From<USTBLError> for ExecuteFailure {
  fn from(error: USTBLError) -> Self {
    ExecuteFailure::Fresh(error)
  }
}

/// What `run` does with an execute failure: a fresh one is written to the
/// record and handed back to be told to the frontend; a recorded one needs
/// nothing, the record already says it all.
fn record_failure(record: &mut JobRecord, failure: &ExecuteFailure) -> Option<String> {
  match failure {
    ExecuteFailure::Fresh(error) => {
      record.phase = Phase::Failed;
      record.error = Some(error.0.clone());
      Some(error.0.clone())
    }
    ExecuteFailure::Recorded => None,
  }
}

/// Length of part `n` (1-based, as the server numbers them): every part is
/// `part_size` long except the last, which takes whatever remains.
fn part_length(total: u64, part_size: u64, count: u32, n: u32) -> u64 {
  if n == count {
    total - part_size * (count as u64 - 1)
  } else {
    part_size
  }
}

/// `GET /uploads/{id}`'s own generation, plus what its `parts` map means
/// against `expected_generation` (§9.1): the part numbers already reported
/// there when the server is still on the generation the launcher expected,
/// or an empty set when it has already moved to a different one (nothing of
/// the old generation counts towards the new one, and the launcher must
/// adopt the server's generation before signing or reporting anything
/// else).
fn received_parts(body: &Value, expected_generation: u32) -> (u32, HashSet<u32>) {
  let server_generation = body["generation"]
    .as_u64()
    .unwrap_or(expected_generation as u64) as u32;
  if server_generation != expected_generation {
    return (server_generation, HashSet::new());
  }
  let parts = body["parts"]
    .as_object()
    .into_iter()
    .flatten()
    .filter_map(|(n, _etag)| n.parse::<u32>().ok())
    .collect();
  (server_generation, parts)
}

/// The generation to restart under, from a 409 `UPLOAD_RESTARTED` response's
/// `detail` object (§13.2). `None` when `detail` is not that conflict at all,
/// so the caller still has the original error to report. The server's own
/// `generation` field is used when present; `current + 1` is a reasonable
/// stand-in otherwise, since that conflict only ever fires from a newer one.
fn restart_generation(detail: Option<&Value>, current: u32) -> Option<u32> {
  let detail = detail?;
  if detail.get("code").and_then(Value::as_str) != Some("UPLOAD_RESTARTED") {
    return None;
  }
  Some(
    detail
      .get("generation")
      .and_then(Value::as_u64)
      .map(|generation| generation as u32)
      .unwrap_or(current + 1),
  )
}

fn retriable(status: reqwest::StatusCode) -> bool {
  status.is_server_error()
    || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

/// The longest `<Code>` kept by [`cloud_error_code`], in characters.
const CLOUD_ERROR_CODE_LIMIT: usize = 64;

/// The S3-compatible cloud answers errors with XML, not JSON:
/// `<Error><Code>...</Code>...</Error>`. Only the code is worth keeping
/// around (§13.4: the rest of the body must never end up in an error shown
/// to the user or written to the log, since it may carry request details),
/// and even that is capped at [`CLOUD_ERROR_CODE_LIMIT`] characters in case
/// the cloud ever answers with something unexpectedly long. `None` when the
/// body does not look like that at all.
fn cloud_error_code(body: &str) -> Option<&str> {
  let start = body.find("<Code>")? + "<Code>".len();
  let end = body[start..].find("</Code>")? + start;
  let code = &body[start..end];
  match code.char_indices().nth(CLOUD_ERROR_CODE_LIMIT) {
    Some((cut, _)) => Some(&code[..cut]),
    None => Some(code),
  }
}

/// Signed cloud PUTs expire 900 s after `x-amz-date`, well past a part's
/// expected transfer time; this tells that apart from an ordinary server
/// error (§3, §13.2).
fn is_signature_expired(body: &str) -> bool {
  cloud_error_code(body) == Some("RequestTimeTooSkewed")
}

/// When the last slice of a request body was taken by the HTTP stack.
type HandedOver = Arc<Mutex<Option<Instant>>>;

fn counting_body(
  data: Bytes,
  sent: Arc<SentBytes>,
  counted: Arc<AtomicU64>,
  handed_over: HandedOver,
) -> reqwest::Body {
  let stream = futures::stream::unfold(0usize, move |offset| {
    let (data, sent, counted) = (data.clone(), sent.clone(), counted.clone());
    let handed_over = handed_over.clone();
    async move {
      if offset >= data.len() {
        return None;
      }
      let end = (offset + SLICE).min(data.len());
      if end >= data.len() {
        handed_over.lock().unwrap().get_or_insert_with(Instant::now);
      }
      let part = data.slice(offset..end);
      sent.add(part.len() as u64);
      counted.fetch_add(part.len() as u64, Ordering::Relaxed);
      Some((Ok::<_, io::Error>(part), end))
    }
  });
  reqwest::Body::wrap_stream(stream)
}

/// Reads one part off the local pack and hashes it on the way in, so the MD5
/// used for the ETag self-check never requires a second pass over the bytes
/// (§13.2).
async fn read_part(pack: &Path, offset: u64, length: u64) -> USTBLResult<(Bytes, [u8; 16])> {
  let mut file = tokio::fs::File::open(pack).await?;
  file.seek(SeekFrom::Start(offset)).await?;
  let mut buffer = vec![0u8; length as usize];
  file.read_exact(&mut buffer).await?;
  let digest: [u8; 16] = Md5::digest(&buffer).into();
  Ok((Bytes::from(buffer), digest))
}

/// The task card is titled after the project's display name, not the slug the
/// server generated. `?` and `@` are the group id's own separators and cannot
/// appear in the name.
fn display_project(record: &JobRecord) -> String {
  let name = record
    .project_name
    .as_deref()
    .map(str::trim)
    .filter(|name| !name.is_empty())
    .unwrap_or(record.project_slug.as_str());
  name
    .chars()
    .map(|c| {
      if matches!(c, '?' | '@' | '\n' | '\r') {
        ' '
      } else {
        c
      }
    })
    .collect::<String>()
    .trim()
    .to_string()
}

pub(super) struct PublishJob {
  pub app: AppHandle,
  pub uploader: reqwest::Client,
  pub record: JobRecord,
  /// How many parts to PUT at once (§13.2, §14.1). `Some` only right after
  /// `POST /versions`, whose response carries the value a fresh upload
  /// should use for its first round; `None` on resume (the record itself
  /// never carries it), so every round instead takes whatever
  /// `GET /uploads/{id}` suggests during reconciliation — which is also
  /// what a restart mid-`upload_pack` falls back to once the first round
  /// is behind it.
  pub concurrency: Option<usize>,
}

impl PublishJob {
  /// Puts the job on the task list and runs it in the background. Returns the
  /// name of the task group. The group lives until the version is published
  /// or the publish fails (§3.2.2): platform processing is the group's own
  /// last stage, never a separate background watch.
  pub(super) fn start(self) -> USTBLResult<String> {
    let token = ActiveToken::take(&self.record.id)
      .ok_or_else(|| USTBLError("这个发布任务已在进行中".into()))?;
    let app = self.app.clone();
    let monitor = app.state::<Pin<Box<TaskMonitor>>>();
    let group = format!(
      "modpack-publish?{} {}@{}",
      display_project(&self.record),
      self.record.version_number,
      chrono::Utc::now().timestamp_millis()
    );

    // A publish without new files has nothing to upload (§3.2.2.1): the
    // stage stays on the card, already done, so the "瞬间完成" case reads
    // as "平台已有全部文件，无需上传" instead of a missing step.
    let pack = match &self.record.pack {
      Some(pack) => upload_handle(
        &app,
        monitor.get_new_id(),
        &group,
        "增量包".to_string(),
        pack.size as i64,
      ),
      None => {
        let handle = upload_handle(
          &app,
          monitor.get_new_id(),
          &group,
          "平台已有全部文件，无需上传".to_string(),
          1,
        );
        complete(&handle);
        handle
      }
    };
    let commit = upload_handle(
      &app,
      monitor.get_new_id(),
      &group,
      format!("提交版本 {}", self.record.version_number),
      1,
    );
    // Total 0 keeps the stage out of the group's percentage; the card shows
    // an indeterminate bar while the platform works (§3.2.2.2).
    let platform = upload_handle(&app, monitor.get_new_id(), &group, "平台处理".to_string(), 0);
    let tasks = Tasks {
      group: group.clone(),
      pack,
      commit,
      platform,
    };
    let handles: Vec<Handle> = tasks.all().cloned().collect();
    monitor.register_job_group(&group, &handles);

    let anchor_id = tasks.commit.read().unwrap().desc.task_id;
    let job = tauri::async_runtime::spawn(self.run(tasks, token));
    monitor.bind_job(anchor_id, job);
    Ok(group)
  }

  async fn run(mut self, tasks: Tasks, _token: ActiveToken) {
    let app = self.app.clone();
    let dir = jobs_dir();
    let anchor_id = tasks.commit.read().unwrap().desc.task_id;
    let mut guard = CancelGuard {
      app: app.clone(),
      job_id: self.record.id.clone(),
      version_id: self.record.version_id,
      armed: true,
    };
    let result = self.execute(&tasks, &mut guard).await;
    guard.armed = false;

    match &result {
      Ok(_) => {}
      Err(failure) => {
        tasks.abandon();
        // A fresh failure is written to the record and told to the
        // frontend; a recorded one is left exactly as the platform stage
        // saved it (§3.2.2: a lost watch stays `Processing`).
        if let Some(message) = record_failure(&mut self.record, failure) {
          if let Err(error) = self.record.save(&dir) {
            log::warn!("Modpack publish {} could not save its record: {error:?}", self.record.id);
          }
          emit_finished(&app, &tasks.group, &self.record, None, Some(message));
        }
      }
    }
    app
      .state::<Pin<Box<TaskMonitor>>>()
      .finish_job_group(&tasks.group, anchor_id, result.is_ok());
  }

  async fn status(&self) -> USTBLResult<Value> {
    let response = authed_json(
      &self.app,
      reqwest::Method::GET,
      &format!("/versions/{}/status", self.record.version_id),
      None,
    )
    .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
      return Err(USTBLError(
        "平台上已找不到这个版本，请放弃这个任务后重新发布".into(),
      ));
    }
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    Ok(response.json().await?)
  }

  /// One status check while the platform processes the pack. `None` means the
  /// platform did not answer (it is busy with the pack), which is no reason to
  /// give the job up.
  async fn poll_status(&self) -> USTBLResult<Option<Value>> {
    let response = match authed_json(
      &self.app,
      reqwest::Method::GET,
      &format!("/versions/{}/status", self.record.version_id),
      None,
    )
    .await
    {
      Ok(response) => response,
      Err(error) => {
        log::warn!("Status check unanswered: {}", error.0);
        return Ok(None);
      }
    };
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
      return Err(USTBLError(
        "平台上已找不到这个版本，请放弃这个任务后重新发布".into(),
      ));
    }
    if retriable(status) {
      log::warn!("Status check answered {status}");
      return Ok(None);
    }
    if !status.is_success() {
      return Err(failure(response).await);
    }
    match response.json().await {
      Ok(state) => Ok(Some(state)),
      Err(error) => {
        log::warn!("Status check unreadable: {error}");
        Ok(None)
      }
    }
  }

  async fn execute(
    &mut self,
    tasks: &Tasks,
    guard: &mut CancelGuard,
  ) -> Result<(), ExecuteFailure> {
    let started = Instant::now();
    log::info!(
      "Modpack publish {} started for version {}",
      self.record.id,
      self.record.version_id
    );
    let state = self.status().await?;
    let current = state["status"].as_str().unwrap_or_default().to_string();
    log::info!(
      "Modpack publish {} initial status: {} after {:.2}s",
      self.record.id,
      current,
      started.elapsed().as_secs_f64()
    );
    if current != "published" && current != "processing" {
      if self.record.upload.is_some() {
        let upload_started = Instant::now();
        if let Err(error) = self.upload_pack(&tasks.pack, &tasks.group).await {
          tasks.pack.write().unwrap().mark_failed(error.0.clone());
          return Err(ExecuteFailure::Fresh(error));
        }
        log::info!(
          "Modpack publish {} upload completed in {:.2}s",
          self.record.id,
          upload_started.elapsed().as_secs_f64()
        );
      }
      tasks.commit.write().unwrap().mark_started();
      self.record.phase = Phase::Processing;
      self.record.error = None;
      self.record.save(&jobs_dir())?;
      let commit_started = Instant::now();
      if let Err(error) = self.commit().await {
        tasks.commit.write().unwrap().mark_failed(error.0.clone());
        return Err(ExecuteFailure::Fresh(error));
      }
      log::info!(
        "Modpack publish {} commit accepted in {:.2}s",
        self.record.id,
        commit_started.elapsed().as_secs_f64()
      );
    } else {
      // Resumed while the platform was already at work: the first two
      // stages are done, only the platform stage is watched.
      complete(&tasks.pack);
    }
    complete(&tasks.commit);

    // The group stays on the list for the platform stage (§3.2.2): the card
    // shows an indeterminate bar until the version is published or fails.
    self.record.phase = Phase::Processing;
    self.record.error = None;
    self.record.save(&jobs_dir())?;
    // The version is the platform's now: a processing version refuses
    // DELETE, so from here on cancelling the card only stops the watch and
    // the record stays `Processing` for the next resume — the guard that
    // would have deleted the version and the record must not fire.
    guard.armed = false;
    tasks.platform.write().unwrap().mark_started();
    log::info!(
      "Modpack publish {} submitted in {:.2}s; platform processing runs on the task card",
      self.record.id,
      started.elapsed().as_secs_f64()
    );
    self.monitor(tasks).await
  }

  /// The platform stage (§3.2.2.2): polls the version's status and writes
  /// what happens onto the card. Returns `Ok` once the version is published
  /// (the record is gone by then); every other ending is a `Recorded`
  /// failure, the record already saved — `Failed` for a real failure,
  /// still `Processing` with the error set when the platform stopped
  /// answering, so the next launcher start picks the watch up again (§3.2.2).
  async fn monitor(&mut self, tasks: &Tasks) -> Result<(), ExecuteFailure> {
    let started = Instant::now();
    let mut misses = 0;
    let mut last_status = String::new();
    loop {
      // A paused group stops polling: the watch resumes with the group.
      while self
        .app
        .state::<Pin<Box<TaskMonitor>>>()
        .is_group_stopped(&tasks.group)
      {
        sleep(Duration::from_secs(1)).await;
      }
      let state = match self.poll_status().await {
        Ok(Some(state)) => {
          misses = 0;
          state
        }
        Ok(None) => {
          misses += 1;
          log::warn!(
            "Modpack publish {} status check missed ({}/{})",
            self.record.id,
            misses,
            POLL_MISSES
          );
          if misses >= POLL_MISSES {
            // Shown on the card (§3.2.2), not as a separate entry: the
            // record stays `Processing`, and the next launcher start
            // resumes the watch through `resume_monitors`.
            let message = "暂时无法确认平台处理进度，请稍后重新启动启动器继续".to_string();
            tasks.platform.write().unwrap().mark_failed(message.clone());
            self.record.phase = Phase::Processing;
            self.record.error = Some(message.clone());
            if let Err(error) = self.record.save(&jobs_dir()) {
              log::warn!("Modpack publish {} could not save its record: {error:?}", self.record.id);
            }
            return Err(ExecuteFailure::Recorded);
          }
          sleep(POLL).await;
          continue;
        }
        Err(error) => {
          tasks.platform.write().unwrap().mark_failed(error.0.clone());
          self.record.phase = Phase::Failed;
          self.record.error = Some(error.0.clone());
          if let Err(error) = self.record.save(&jobs_dir()) {
            log::warn!("Modpack publish {} could not save its record: {error:?}", self.record.id);
          }
          emit_finished(&self.app, &tasks.group, &self.record, None, Some(error.0.clone()));
          return Err(ExecuteFailure::Recorded);
        }
      };
      let status = state["status"].as_str().unwrap_or_default();
      if status != last_status {
        log::info!(
          "Modpack publish {} status changed: {} -> {} after {:.2}s processing",
          self.record.id,
          last_status,
          status,
          started.elapsed().as_secs_f64()
        );
        last_status = status.to_string();
      }
      if status == "failed" {
        let message = state["error"]
          .as_str()
          .filter(|text| !text.is_empty())
          .unwrap_or("平台处理增量包失败")
          .to_string();
        tasks.platform.write().unwrap().mark_failed(message.clone());
        self.record.phase = Phase::Failed;
        self.record.error = Some(message.clone());
        if let Err(error) = self.record.save(&jobs_dir()) {
          log::warn!("Modpack publish {} could not save its record: {error:?}", self.record.id);
        }
        emit_finished(&self.app, &tasks.group, &self.record, None, Some(message.clone()));
        return Err(ExecuteFailure::Recorded);
      }
      // §8.4: the platform can revert the version to `uploading` during the
      // complete phase when its own probe finds the cloud upload session
      // gone (3 consecutive 500s) — the pack has to be sent again under a
      // new generation. The task becomes resumable; resuming re-enters
      // `upload_pack`, whose reconciliation adopts the server's new
      // generation before anything signs or reports under it (§9.1).
      if status == "uploading" {
        let message = state["error"]
          .as_str()
          .filter(|text| !text.is_empty())
          .unwrap_or("云盘上传会话已失效，需要重新上传")
          .to_string();
        tasks.platform.write().unwrap().mark_failed(message.clone());
        self.record.phase = Phase::Failed;
        self.record.error = Some(message.clone());
        if let Err(error) = self.record.save(&jobs_dir()) {
          log::warn!("Modpack publish {} could not save its record: {error:?}", self.record.id);
        }
        emit_finished(&self.app, &tasks.group, &self.record, None, Some(message.clone()));
        return Err(ExecuteFailure::Recorded);
      }
      if status == "published" {
        let files: Vec<LocalFile> = self
          .record
          .files
          .iter()
          .map(|file| LocalFile {
            path: file.path.clone(),
            key: FileKey {
              sha1: file.sha1.clone(),
              size: file.size,
            },
          })
          .collect();
        let slug = state["project_slug"]
          .as_str()
          .unwrap_or(&self.record.project_slug)
          .to_string();
        let adoption = match instance_of(&self.app, &self.record.instance_id) {
          Ok(instance) => {
            adopt_published(
              &self.app,
              &instance,
              &slug,
              self.record.version_id,
              &self.record.version_number,
              &files,
              self.record.mmc_pack.as_ref(),
            )
            .await
          }
          Err(_) => super::commands::Adoption::RecordFailed,
        };
        JobRecord::remove(&jobs_dir(), &self.record.id);
        let outcome = PublishOutcome {
          version_id: self.record.version_id,
          skipped: self.record.skipped.clone(),
          adoption,
        };
        complete(&tasks.platform);
        log::info!(
          "Modpack publish {} platform processing completed in {:.2}s",
          self.record.id,
          started.elapsed().as_secs_f64()
        );
        emit_finished(&self.app, &tasks.group, &self.record, Some(outcome), None);
        return Ok(());
      }
      sleep(POLL).await;
    }
  }

  /// A request that never got an answer may still have been queued, so each
  /// retry first looks at what the platform says.
  async fn commit(&self) -> USTBLResult<()> {
    let mut last = USTBLError("提交失败".into());
    for attempt in 0..COMMIT_ATTEMPTS {
      if attempt > 0 {
        sleep(POLL * 2).await;
        if let Ok(Some(state)) = self.poll_status().await {
          if matches!(state["status"].as_str(), Some("processing" | "published")) {
            return Ok(());
          }
        }
      }
      let response = match authed_json(
        &self.app,
        reqwest::Method::POST,
        &format!("/versions/{}/commit", self.record.version_id),
        None,
      )
      .await
      {
        Ok(response) => response,
        Err(error) => {
          last = error;
          continue;
        }
      };
      let status = response.status();
      if status.is_success() {
        return Ok(());
      }
      if status == reqwest::StatusCode::GONE {
        return Err(InstanceError::ModpackUploadSessionGone.into());
      }
      let (code, error) = failure_code(response).await;
      if code.as_deref() == Some("PARENT_MISMATCH") {
        return Err(InstanceError::ModpackPublishStale.into());
      }
      if !retriable(status) {
        return Err(error);
      }
      last = error;
    }
    Err(last)
  }

  /// The pack is built again from the recorded sources when it is gone, and has
  /// to come out as the pack the platform was promised. Either way the result
  /// passes the same self-check the upload itself relies on (§13.2): on a
  /// rebuild, because the delta pack never touched disk before; when it is
  /// already there, because a previous run may have been interrupted mid
  /// write. A pack this process just built and verified skips the read-back
  /// (its bytes are known); one resumed from disk after a restart always gets
  /// the full check.
  async fn ensure_pack(&self) -> USTBLResult<std::path::PathBuf> {
    let pack = self
      .record
      .pack
      .clone()
      .ok_or_else(|| USTBLError("这个版本没有增量包".into()))?;
    let path = paths(&jobs_dir(), &self.record.id)?.pack;
    let exists = tokio::fs::metadata(&path)
      .await
      .is_ok_and(|meta| meta.len() == pack.size);
    if exists && is_verified(&path) {
      return Ok(path);
    }
    let check_path = path.clone();
    if exists {
      tokio::task::spawn_blocking(move || verify_pack(&check_path, Some(&pack.sha1)))
        .await
        .map_err(|e| USTBLError(e.to_string()))??;
    } else {
      let objects: Vec<PackObject> = self.record.objects.iter().map(PackObject::from).collect();
      tokio::task::spawn_blocking(move || {
        rebuild_pack(&objects, &check_path, &pack.sha1, pack.size)
      })
      .await
      .map_err(|e| USTBLError(e.to_string()))??;
    }
    if let Ok(meta) = tokio::fs::metadata(&path).await {
      if let Ok(modified) = meta.modified() {
        mark_verified(&path, meta.len(), modified);
      }
    }
    Ok(path)
  }

  async fn upload_pack(&mut self, handle: &Handle, group: &str) -> USTBLResult<()> {
    let total = self.record.pack.as_ref().map_or(0, |pack| pack.size);
    let path = self.ensure_pack().await?;

    handle.write().unwrap().mark_started();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Reconciliation (§9.1) may find the server already on a newer
    // generation (a probe from this or an earlier run) or hand back a
    // restart while parts are in flight; keep going until every part of the
    // *current* generation is reported. A session that keeps restarting
    // instead of making progress is reported as such rather than retried
    // forever.
    let mut restarts = 0u32;
    loop {
      let upload = self
        .record
        .upload
        .clone()
        .ok_or_else(|| USTBLError("这个版本没有上传会话".into()))?;
      let (generation, received, reconciled_concurrency) = self.reconcile(&upload).await?;
      // The server's generation is adopted and saved *before* anything
      // signs or reports under it (§9.1, §9.3): otherwise a stale local
      // generation would keep being used for every signature and report
      // this round, even though the server has already moved on.
      if generation != upload.generation {
        self.record_generation(generation)?;
      }
      let upload = self.record.upload.clone().unwrap_or(upload);
      // §14.1: a brand new publish uploads its first round at the
      // concurrency `POST /versions` returned; `self.concurrency` is taken
      // (set to `None`) right after that round, so every later round —
      // including a resumed job, which never had a `POST /versions`
      // response to take it from in the first place — uses whatever
      // `GET /uploads/{id}` suggests instead.
      let concurrency = self.concurrency.take().unwrap_or(reconciled_concurrency);
      match self
        .upload_parts(
          handle,
          group,
          &upload,
          received,
          concurrency,
          &path,
          total,
          &mut tick,
        )
        .await?
      {
        UploadRoundOutcome::Done => return Ok(()),
        UploadRoundOutcome::Restarted(generation) => {
          restarts += 1;
          if restarts > MAX_RESTARTS {
            return Err(InstanceError::ModpackUploadRestarted.into());
          }
          self.record_generation(generation)?;
        }
      }
    }
  }

  /// Persists a new generation to the job's own record (§9.1), so every
  /// save from here on — including a crash right after this point —
  /// carries it, and a resumed run never signs or reports under a
  /// generation the server has already moved past.
  fn record_generation(&mut self, generation: u32) -> USTBLResult<()> {
    if let Some(upload) = &mut self.record.upload {
      upload.generation = generation;
    }
    self.record.save(&jobs_dir())
  }

  /// `GET /uploads/{id}` against the current generation (§9.1): the
  /// server's own generation (which the launcher must adopt before signing
  /// or reporting anything else), what is already reported there, and the
  /// concurrency to upload with, falling back to the suggested default when
  /// a pre-4c server omits it.
  async fn reconcile(&self, upload: &RecordedUpload) -> USTBLResult<(u32, HashSet<u32>, usize)> {
    let response = authed_json(
      &self.app,
      reqwest::Method::GET,
      &format!("/uploads/{}", upload.id),
      None,
    )
    .await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
      return Err(InstanceError::ModpackUploadSessionGone.into());
    }
    if !status.is_success() {
      return Err(failure(response).await);
    }
    let body: Value = response.json().await?;
    let (generation, received) = received_parts(&body, upload.generation);
    let concurrency = body["concurrency"]
      .as_u64()
      .map_or(super::commands::DEFAULT_PART_CONCURRENCY, |n| n as usize);
    Ok((generation, received, concurrency))
  }

  /// Uploads whatever `received` is missing from `upload.part_count`. Returns
  /// once every part of `upload.generation` is reported, or once the server
  /// restarted the session under a new generation (§9.1, §13.2).
  async fn upload_parts(
    &self,
    handle: &Handle,
    group: &str,
    upload: &RecordedUpload,
    received: HashSet<u32>,
    concurrency: usize,
    path: &Path,
    total: u64,
    tick: &mut tokio::time::Interval,
  ) -> USTBLResult<UploadRoundOutcome> {
    let stored: u64 = received
      .iter()
      .map(|n| part_length(total, upload.part_size, upload.part_count, *n))
      .sum();
    let mut queue: VecDeque<u32> = (1..=upload.part_count)
      .filter(|n| !received.contains(n))
      .collect();

    let sent = SentBytes::new(total, stored);
    let monitor = self.app.state::<Pin<Box<TaskMonitor>>>();
    let signatures = SignatureCache::new(queue.iter().copied());
    let concurrency = concurrency.max(1);
    let mut running = FuturesUnordered::new();

    loop {
      // A paused group starts no new part.
      if !monitor.is_group_stopped(group) {
        while running.len() < concurrency {
          let Some(n) = queue.pop_front() else { break };
          let length = part_length(total, upload.part_size, upload.part_count, n);
          running.push(self.send_part(upload, path, n, length, &sent, &signatures));
        }
      }
      if running.is_empty() && queue.is_empty() {
        break;
      }
      tokio::select! {
        finished = running.next(), if !running.is_empty() => {
          match finished {
            Some(Ok(PartOutcome::Reported(n))) => signatures.reported(n),
            Some(Ok(PartOutcome::Restarted(generation))) => {
              return Ok(UploadRoundOutcome::Restarted(generation));
            }
            Some(Err(error)) => return Err(error),
            None => {}
          }
        }
        _ = tick.tick() => report(handle, sent.done()),
      }
    }
    complete(handle);
    Ok(UploadRoundOutcome::Done)
  }

  /// Signs (if needed), PUTs to the cloud and reports one part, retrying on
  /// transient trouble and probing the session after repeated 500s (§9.3,
  /// §13.2). `Ok(PartOutcome::Restarted)` means the whole upload must start
  /// over from part 1 under a new generation; the caller does not retry this
  /// part itself in that case.
  async fn send_part(
    &self,
    upload: &RecordedUpload,
    pack: &Path,
    n: u32,
    length: u64,
    sent: &Arc<SentBytes>,
    signatures: &SignatureCache,
  ) -> USTBLResult<PartOutcome> {
    let prepared = Instant::now();
    let (data, digest) = read_part(pack, (n - 1) as u64 * upload.part_size, length).await?;
    let md5_hex = hex::encode(digest);
    let prepare_time = prepared.elapsed();

    let mut last = USTBLError("上传失败".into());
    let mut server_error_streak = 0u32;
    // Re-signing on a clock-skew rejection does not count as a failed
    // attempt (§9.3), but it must still be bounded: past this many
    // consecutive re-signs for the same part it is treated as an ordinary
    // failure instead of retried without end.
    let mut skew_resigns = 0u32;
    let mut etag_resent = false;
    let mut attempt = 0u32;
    loop {
      if attempt >= PART_ATTEMPTS {
        return Err(last);
      }
      if attempt > 0 {
        // §13.2: 2, 4, 8 s between retries for 5xx/429/408.
        sleep(Duration::from_secs(1 << attempt.min(3))).await;
      }
      attempt += 1;

      let signed = match signatures.get_or_sign(&self.app, upload, n).await {
        Ok(signed) => signed,
        Err(error) => match SignStep::from_sign_error(error) {
          SignStep::Restarted(generation) => return Ok(PartOutcome::Restarted(generation)),
          SignStep::Fail(error) => return Err(error),
          SignStep::Retry(error) => {
            last = error;
            continue;
          }
        },
      };

      let counted = Arc::new(AtomicU64::new(0));
      let handed_over: HandedOver = Arc::new(Mutex::new(None));
      let started = Instant::now();
      let mut request = self.uploader.request(signed.method.clone(), &signed.url);
      for (name, value) in &signed.headers {
        request = request.header(name, value);
      }
      let response = request
        .header("Content-Length", length)
        .body(counting_body(
          data.clone(),
          sent.clone(),
          counted.clone(),
          handed_over.clone(),
        ))
        .send()
        .await;
      let sent_now = counted.load(Ordering::Relaxed);
      let body_time = handed_over
        .lock()
        .unwrap()
        .map(|done| done.duration_since(started).as_secs_f64());

      let response = match response {
        Ok(response) => response,
        Err(error) => {
          sent.take_back(sent_now);
          // §13.4: never log a URL carrying a cloud signature.
          let error = error.without_url();
          log::warn!(
            "Part {n}/{} (attempt {attempt}): request failed: {error}",
            upload.part_count
          );
          last = USTBLError(error.to_string());
          continue;
        }
      };
      let status = response.status();
      log::info!(
        "Part {n}/{} (attempt {attempt}): prepared in {:.2}s, body handed over after {}, answered after {:.2}s ({status})",
        upload.part_count,
        prepare_time.as_secs_f64(),
        body_time.map_or("never".to_string(), |time| format!("{time:.2}s")),
        started.elapsed().as_secs_f64(),
      );

      if status.is_success() {
        if sent_now < length {
          sent.add(length - sent_now);
        } else {
          sent.take_back(sent_now - length);
        }
        let cloud_etag = response
          .headers()
          .get(reqwest::header::ETAG)
          .and_then(|value| value.to_str().ok())
          .unwrap_or_default()
          .trim_matches('"')
          .to_ascii_lowercase();
        if cloud_etag != md5_hex {
          // Self-check (§13.2): what the cloud says it stored does not match
          // what was sent. Exactly one resend before giving the job up
          // outright, independent of the part's own attempt budget.
          if !etag_resent {
            etag_resent = true;
            signatures.discard(n);
            last = USTBLError("云盘回报的 ETag 与本地 MD5 不一致，正在重传".into());
            continue;
          }
          return Err(InstanceError::ModpackUploadVerifyFailed.into());
        }
        return self.report_part(upload, n, &md5_hex, length).await.map(
          |restarted| match restarted {
            Some(generation) => PartOutcome::Restarted(generation),
            None => PartOutcome::Reported(n),
          },
        );
      }

      sent.take_back(sent_now);
      let body = response.text().await.unwrap_or_default();
      if status == reqwest::StatusCode::FORBIDDEN
        && is_signature_expired(&body)
        && skew_resigns < SKEW_RESIGN_LIMIT
      {
        // §13.2/§9.3: re-signing on an expired signature is not a failed
        // attempt, up to a bounded number of times.
        skew_resigns += 1;
        signatures.discard(n);
        attempt -= 1;
        continue;
      }
      if retriable(status) {
        signatures.discard(n);
        server_error_streak = if status.is_server_error() {
          server_error_streak + 1
        } else {
          0
        };
        // A plain cloud error keeps its own message for a while, but once the
        // attempts run out it is reported as the typed, user-facing code
        // instead of a bare status (§13.4).
        last = InstanceError::ModpackUploadCloudUnavailable.into();
        if server_error_streak >= PROBE_AFTER_FAILURES {
          match self.probe(upload).await? {
            Some(generation) => return Ok(PartOutcome::Restarted(generation)),
            None => server_error_streak = 0,
          }
        }
        continue;
      }
      // §13.4: the cloud's own error code only, never the raw body (which
      // may carry request details) in a message the user can see.
      let code = cloud_error_code(&body).unwrap_or("UNKNOWN");
      return Err(USTBLError(format!("HTTP {status}: {code}")));
    }
  }

  /// `PUT /uploads/{id}/parts/{n}`; `Ok(Some(generation))` means the server
  /// rejected the report because it is already on a newer generation
  /// (§13.2: 409 `UPLOAD_RESTARTED`).
  async fn report_part(
    &self,
    upload: &RecordedUpload,
    n: u32,
    md5_hex: &str,
    length: u64,
  ) -> USTBLResult<Option<u32>> {
    let response = authed_json(
      &self.app,
      reqwest::Method::PUT,
      &format!("/uploads/{}/parts/{n}", upload.id),
      Some(json!({
        "generation": upload.generation,
        "etag": md5_hex,
        "size": length,
      })),
    )
    .await?;
    if response.status().is_success() {
      return Ok(None);
    }
    if response.status() == reqwest::StatusCode::CONFLICT {
      let (detail, error) = failure_detail(response).await;
      return match restart_generation(detail.as_ref(), upload.generation) {
        Some(generation) => Ok(Some(generation)),
        None => Err(error),
      };
    }
    if response.status() == reqwest::StatusCode::GONE {
      return Err(InstanceError::ModpackUploadSessionGone.into());
    }
    Err(failure(response).await)
  }

  /// `POST /uploads/{id}/probe` (§9.3): tells a session the cloud silently
  /// expired from ordinary cloud trouble, after repeated plain 500s on one
  /// part. `Ok(Some(generation))` means the server already restarted the
  /// session; `Ok(None)` means the cloud is just acting up and normal
  /// retries should continue (either the probe itself succeeded without a
  /// restart, or the 503 backoff schedule ran out).
  ///
  /// The backoff on a 503 (§9.3, §14.1: 1, 2, 5, 10 minutes, the larger of
  /// that and `Retry-After`) happens here, in a loop of its own: it must not
  /// consume [`PART_ATTEMPTS`], since it is the cloud as a whole acting up
  /// rather than one failed attempt at one part. Only `log::warn!`, not a
  /// user-visible task message, marks the wait (the task list has no
  /// non-terminal "still trying" state to show one in); once the schedule is
  /// exhausted the caller reports the typed, user-facing
  /// `ModpackUploadCloudUnavailable` instead of waiting forever.
  async fn probe(&self, upload: &RecordedUpload) -> USTBLResult<Option<u32>> {
    const BACKOFF: [Duration; 4] = [
      Duration::from_secs(60),
      Duration::from_secs(2 * 60),
      Duration::from_secs(5 * 60),
      Duration::from_secs(10 * 60),
    ];
    let mut backoff = BACKOFF.iter();
    loop {
      let response = authed_json(
        &self.app,
        reqwest::Method::POST,
        &format!("/uploads/{}/probe", upload.id),
        Some(json!({ "generation": upload.generation })),
      )
      .await?;
      if response.status().is_success() {
        let body: Value = response.json().await?;
        if body["result"] == "restarted" {
          let generation = body["generation"].as_u64().unwrap_or(0) as u32;
          return Ok(Some(generation));
        }
        return Ok(None);
      }
      if response.status() != reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Err(failure(response).await);
      }
      let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);
      let Some(&scheduled) = backoff.next() else {
        return Ok(None);
      };
      let wait = retry_after.map_or(scheduled, |retry_after| retry_after.max(scheduled));
      log::warn!(
        "Upload {} probe found the cloud drive unavailable; waiting {:.0}s before trying again",
        upload.id,
        wait.as_secs_f64()
      );
      sleep(wait).await;
    }
  }
}

enum UploadRoundOutcome {
  Done,
  Restarted(u32),
}

enum PartOutcome {
  Reported(u32),
  Restarted(u32),
}

/// Signing can fail for an ordinary, transient reason (`Retry` — spent
/// against the part's `PART_ATTEMPTS` budget the same way a failed PUT
/// would), because the session already restarted under a newer generation
/// (`Restarted` — a 409 `UPLOAD_RESTARTED` on `/parts/sign` is handled
/// exactly like one on reporting, §13.2), or because signing can never
/// succeed against this session (`Fatal` — 410, or a 409 that is not
/// `UPLOAD_RESTARTED`; the launcher must stop rather than spend attempts
/// retrying).
#[derive(Debug)]
enum SignError {
  Restarted(u32),
  Fatal(USTBLError),
  Retry(USTBLError),
}

impl From<USTBLError> for SignError {
  fn from(error: USTBLError) -> Self {
    SignError::Retry(error)
  }
}

impl From<reqwest::Error> for SignError {
  fn from(error: reqwest::Error) -> Self {
    SignError::Retry(error.into())
  }
}

/// What `send_part` should do next after a signing failure, decided
/// purely from the [`SignError`] itself. Split out so the classification
/// rules — restart, fail outright, spend an attempt and retry — can be
/// tested without driving the whole part loop.
#[derive(Debug, PartialEq, Eq)]
enum SignStep {
  Restarted(u32),
  Fail(USTBLError),
  Retry(USTBLError),
}

impl SignStep {
  fn from_sign_error(error: SignError) -> Self {
    match error {
      SignError::Restarted(generation) => SignStep::Restarted(generation),
      SignError::Fatal(error) => SignStep::Fail(error),
      SignError::Retry(error) => SignStep::Retry(error),
    }
  }
}

/// One signed cloud request plus when it was signed, so the cache can tell an
/// expired signature apart from one still inside its 10-minute window
/// (§13.2, §14.1) without trusting the server's own `signed_at`/`expires_at`
/// (clock skew between launcher and server would otherwise leak in).
#[derive(Clone)]
struct SignedPart {
  method: reqwest::Method,
  url: String,
  headers: HashMap<String, String>,
  received_at: Instant,
}

/// Picks ascending part numbers out of `remaining` that lack a fresh
/// signature according to `fresh`, starting from `n`, up to [`SIGN_BATCH`].
/// `remaining.range(n..)` already reaches the last part still outstanding
/// whenever `n` is close enough to it; nothing before `n` is ever included,
/// since those parts are either already reported or already being worked
/// on by another request. Always returns at least `[n]`.
fn select_sign_batch(
  remaining: &std::collections::BTreeSet<u32>,
  n: u32,
  fresh: impl Fn(u32) -> bool,
) -> Vec<u32> {
  let wanted: Vec<u32> = remaining
    .range(n..)
    .copied()
    .filter(|&m| !fresh(m))
    .take(SIGN_BATCH)
    .collect();
  if wanted.is_empty() {
    vec![n]
  } else {
    wanted
  }
}

/// Signatures for parts not yet sent, fetched up to [`SIGN_BATCH`] at a time
/// and shared by every part in flight so a prefetch for one part can be used
/// by another (§13.2).
///
/// Only one `/parts/sign` request is ever in flight at a time: a
/// `tokio::sync::Mutex` serializes the actual signing call, and every caller
/// re-checks the cache after acquiring it, so a request that arrives while
/// another is already signing just picks up its result instead of issuing
/// its own. `remaining` is the set of part numbers this round still needs a
/// report for; the batch is chosen from there so it naturally skips parts
/// already reported (removed via [`Self::reported`]) and can always reach
/// the last part still outstanding.
struct SignatureCache {
  parts: Mutex<HashMap<u32, SignedPart>>,
  remaining: Mutex<std::collections::BTreeSet<u32>>,
  sign_lock: tokio::sync::Mutex<()>,
}

impl SignatureCache {
  /// `parts` is the initial queue (§9.1's reconciled missing parts) this
  /// round will try to report; every one of them starts out "remaining"
  /// until [`Self::reported`] removes it.
  fn new(parts: impl Iterator<Item = u32>) -> Self {
    Self {
      parts: Mutex::new(HashMap::new()),
      remaining: Mutex::new(parts.collect()),
      sign_lock: tokio::sync::Mutex::new(()),
    }
  }

  fn discard(&self, n: u32) {
    self.parts.lock().unwrap().remove(&n);
  }

  /// A part that is done needs no more signature and must not be picked for
  /// a future batch.
  fn reported(&self, n: u32) {
    self.parts.lock().unwrap().remove(&n);
    self.remaining.lock().unwrap().remove(&n);
  }

  fn fresh(&self, n: u32) -> bool {
    self
      .parts
      .lock()
      .unwrap()
      .get(&n)
      .is_some_and(|signed| signed.received_at.elapsed() < SIGNATURE_LIFETIME)
  }

  async fn get_or_sign(
    &self,
    app: &AppHandle,
    upload: &RecordedUpload,
    n: u32,
  ) -> Result<SignedPart, SignError> {
    self
      .get_or_sign_via(n, |wanted| sign_request(app, upload, wanted))
      .await
  }

  /// Looks up a fresh signature for `n`, signing a batch starting at it if
  /// there is none, with `request` standing in for the actual
  /// `/parts/sign` call — production uses [`sign_request`]; tests can
  /// supply something else, so this coordination (fresh check, take
  /// `sign_lock`, re-check, batch-select, insert) is exercised the same way
  /// in both.
  async fn get_or_sign_via<F, Fut>(&self, n: u32, request: F) -> Result<SignedPart, SignError>
  where
    F: FnOnce(Vec<u32>) -> Fut,
    Fut: std::future::Future<Output = Result<HashMap<u32, SignedPart>, SignError>>,
  {
    if self.fresh(n) {
      return Ok(self.parts.lock().unwrap().get(&n).cloned().unwrap());
    }
    // Only one signing request runs at a time; everyone else just waits on
    // this lock and re-checks the cache once they get it, rather than
    // issuing a redundant request of their own for the same or an
    // overlapping batch.
    let _guard = self.sign_lock.lock().await;
    if !self.fresh(n) {
      let wanted = {
        let remaining = self.remaining.lock().unwrap();
        select_sign_batch(&remaining, n, |m| self.fresh(m))
      };
      let signed = request(wanted).await?;
      self.parts.lock().unwrap().extend(signed);
    }
    self
      .parts
      .lock()
      .unwrap()
      .get(&n)
      .cloned()
      .ok_or_else(|| SignError::Retry(USTBLError("平台没有为这一片签名".into())))
  }
}

/// `POST /uploads/{id}/parts/sign` for exactly `wanted`, turned into the
/// signatures the cache stores. This is where the HTTP-level outcome is
/// classified into the three [`SignError`] kinds: a 409
/// `UPLOAD_RESTARTED` restarts the round under the server's generation;
/// a 410, or a 409 that is not `UPLOAD_RESTARTED`, means the session
/// cannot be signed against and fails outright; anything else — network
/// trouble, a platform 5xx/429/408, a body that does not parse — is
/// ordinary transient trouble and is retried against the part's attempt
/// budget (§13.2).
async fn sign_request(
  app: &AppHandle,
  upload: &RecordedUpload,
  wanted: Vec<u32>,
) -> Result<HashMap<u32, SignedPart>, SignError> {
  let response = authed_json(
    app,
    reqwest::Method::POST,
    &format!("/uploads/{}/parts/sign", upload.id),
    Some(json!({ "generation": upload.generation, "parts": wanted })),
  )
  .await?;
  let status = response.status();
  if status == reqwest::StatusCode::CONFLICT {
    // A 409 on signing means the same thing a 409 on reporting does — the
    // session is already on a newer generation — and is handled the same
    // way: stop in-flight parts, reconcile, and restart from scratch under
    // it (§13.2). A 409 that is not that code is not transient either:
    // the session's own identity no longer lines up with what the
    // launcher has, and retrying will not change that.
    let (detail, error) = failure_detail(response).await;
    return match restart_generation(detail.as_ref(), upload.generation) {
      Some(generation) => Err(SignError::Restarted(generation)),
      None => Err(SignError::Fatal(error)),
    };
  }
  if status == reqwest::StatusCode::GONE {
    return Err(SignError::Fatal(
      InstanceError::ModpackUploadSessionGone.into(),
    ));
  }
  if !status.is_success() {
    return Err(SignError::Retry(failure(response).await));
  }
  let received_at = Instant::now();
  let body: Value = response.json().await?;
  let requests = body["requests"]
    .as_object()
    .ok_or_else(|| SignError::Retry(USTBLError("平台返回了无法识别的签名".into())))?;
  let mut signed = HashMap::new();
  for (key, value) in requests {
    let Ok(part_n) = key.parse::<u32>() else {
      continue;
    };
    let method = value["method"]
      .as_str()
      .and_then(|m| m.parse().ok())
      .unwrap_or(reqwest::Method::PUT);
    let url = value["url"].as_str().unwrap_or_default().to_string();
    if !url.starts_with("https://") {
      // §13.2: the upload client only ever accepts https URLs.
      continue;
    }
    let headers = value["headers"]
      .as_object()
      .into_iter()
      .flatten()
      .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
      .collect();
    signed.insert(
      part_n,
      SignedPart {
        method,
        url,
        headers,
        received_at,
      },
    );
  }
  Ok(signed)
}

fn emit_finished(
  app: &AppHandle,
  group: &str,
  record: &JobRecord,
  outcome: Option<PublishOutcome>,
  error: Option<String>,
) {
  let _ = app.emit_to(
    "main",
    FINISHED_EVENT,
    PublishFinished {
      instance_id: record.instance_id.clone(),
      group: group.to_string(),
      job_id: record.id.clone(),
      outcome,
      error,
    },
  );
}

#[cfg(test)]
mod tests {
  use super::super::record::RECORD_SCHEMA;
  use super::*;

  #[test]
  fn parts_cover_the_pack_with_a_shorter_last_one() {
    // 20 bytes in parts of 8: 8 + 8 + 4.
    let lengths: Vec<u64> = (1..=3).map(|n| part_length(20, 8, 3, n)).collect();
    assert_eq!(lengths, [8, 8, 4]);
    assert_eq!(part_length(8, 8, 1, 1), 8);
    assert_eq!(part_length(5, 8, 1, 1), 5);
  }

  #[test]
  fn only_server_trouble_and_throttling_are_retried() {
    use reqwest::StatusCode;
    assert!(retriable(StatusCode::BAD_GATEWAY));
    assert!(retriable(StatusCode::TOO_MANY_REQUESTS));
    assert!(!retriable(StatusCode::UNPROCESSABLE_ENTITY));
    assert!(!retriable(StatusCode::GONE));
  }

  #[test]
  fn a_job_runs_once_at_a_time() {
    let token = ActiveToken::take("job-x").unwrap();
    assert!(is_running("job-x"));
    assert!(ActiveToken::take("job-x").is_none());
    drop(token);
    assert!(!is_running("job-x"));
  }

  #[test]
  fn discard_gate_rejects_an_active_job() {
    let token = ActiveToken::take("job-discard-active").unwrap();
    assert!(ensure_not_running("job-discard-active").is_err());
    drop(token);
    assert!(ensure_not_running("job-discard-active").is_ok());
  }

  #[test]
  fn reconciliation_picks_up_only_the_current_generations_parts() {
    let body = json!({
      "status": "uploading",
      "generation": 3,
      "parts": { "1": "a".repeat(32), "3": "b".repeat(32) },
    });
    let (generation, received) = received_parts(&body, 3);
    assert_eq!(generation, 3);
    let mut received: Vec<u32> = received.into_iter().collect();
    received.sort_unstable();
    assert_eq!(received, [1, 3]);
  }

  #[test]
  fn reconciliation_against_a_newer_generation_restarts_from_scratch() {
    // The server moved on behind the launcher's back (e.g. a previous run's
    // probe restarted it); nothing already reported under the stale
    // generation counts towards the new one, and the server's own
    // generation is handed back so the caller can adopt it instead of
    // signing or reporting under the stale one.
    let body = json!({
      "status": "uploading",
      "generation": 4,
      "parts": { "1": "a".repeat(32) },
    });
    let (generation, received) = received_parts(&body, 3);
    assert_eq!(generation, 4);
    assert!(received.is_empty());
  }

  #[test]
  fn a_signed_url_rejected_for_clock_skew_is_told_apart_from_other_cloud_errors() {
    assert!(is_signature_expired(
      "<Error><Code>RequestTimeTooSkewed</Code></Error>"
    ));
    assert!(!is_signature_expired(
      "<Error><Code>InternalError</Code></Error>"
    ));
    assert!(!is_signature_expired(""));
  }

  /// §13.4: only the `<Code>` is ever worth keeping from a cloud error
  /// body; the rest (which may carry request details) must not leak into a
  /// logged or user-facing message.
  #[test]
  fn only_the_cloud_error_code_is_pulled_out_of_the_xml_body() {
    assert_eq!(
      cloud_error_code(
        "<?xml version=\"1.0\"?><Error><Code>InternalError</Code>\
         <Message>oops, request-id abc123</Message></Error>"
      ),
      Some("InternalError")
    );
    assert_eq!(cloud_error_code("not xml at all"), None);
    assert_eq!(cloud_error_code(""), None);
  }

  /// A `<Code>` longer than [`CLOUD_ERROR_CODE_LIMIT`] is cut down to it,
  /// on a character boundary rather than a byte one.
  #[test]
  fn an_overlong_cloud_error_code_is_cut_to_the_limit_on_a_char_boundary() {
    let long_code = "错".repeat(CLOUD_ERROR_CODE_LIMIT + 10);
    let body = format!("<Error><Code>{long_code}</Code></Error>");
    let code = cloud_error_code(&body).unwrap();
    assert_eq!(code.chars().count(), CLOUD_ERROR_CODE_LIMIT);
    assert_eq!(code, "错".repeat(CLOUD_ERROR_CODE_LIMIT));

    let short_code = "InternalError";
    let body = format!("<Error><Code>{short_code}</Code></Error>");
    assert_eq!(cloud_error_code(&body), Some(short_code));
  }

  #[test]
  fn a_409_without_the_restart_code_is_left_for_the_caller_to_report() {
    let other = json!({ "code": "SOME_OTHER_CONFLICT", "message": "nope" });
    assert_eq!(restart_generation(Some(&other), 3), None);
    assert_eq!(restart_generation(None, 3), None);
  }

  #[test]
  fn a_409_upload_restarted_resumes_at_the_servers_generation_or_the_next_one() {
    let with_generation = json!({ "code": "UPLOAD_RESTARTED", "generation": 7 });
    assert_eq!(restart_generation(Some(&with_generation), 3), Some(7));

    // The server is expected to always include `generation`, but a fallback
    // keeps the launcher moving forward even if it someday does not.
    let without_generation = json!({ "code": "UPLOAD_RESTARTED" });
    assert_eq!(restart_generation(Some(&without_generation), 3), Some(4));
  }

  /// §13.2: concurrent requests for different parts must not each issue
  /// their own `/parts/sign` call — the second one has to wait on
  /// `sign_lock`, then find the first one's batch already covers it. Goes
  /// through [`SignatureCache::get_or_sign_via`], the exact coordination
  /// `get_or_sign` itself runs in production, with a fake `request` in
  /// place of the real HTTP call.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn concurrent_requests_for_different_parts_share_a_single_sign_call() {
    let cache = Arc::new(SignatureCache::new(1..=4));
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    // Held while the one `request` call that should ever run is in
    // progress, simulating a signing request slow enough that a second,
    // concurrent request for a different part would start its own if it
    // were not actually blocked on `sign_lock` instead. `Notify` rather
    // than a timer keeps the test deterministic.
    let release = Arc::new(tokio::sync::Notify::new());

    let sign = |n: u32,
                calls: Arc<std::sync::atomic::AtomicU32>,
                release: Arc<tokio::sync::Notify>,
                cache: Arc<SignatureCache>| {
      tokio::spawn(async move {
        cache
          .get_or_sign_via(n, |wanted| async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            release.notified().await;
            let received_at = Instant::now();
            Ok(
              wanted
                .into_iter()
                .map(|part_n| {
                  (
                    part_n,
                    SignedPart {
                      method: reqwest::Method::PUT,
                      url: "https://example.test/".into(),
                      headers: HashMap::new(),
                      received_at,
                    },
                  )
                })
                .collect(),
            )
          })
          .await
      })
    };

    // Part 1 is spawned first and its `request` call is awaited to have
    // actually started before part 2 is spawned, so part 1 deterministically
    // wins `sign_lock`. Its batch — ascending from 1 — necessarily covers
    // part 2 too (there is nothing else in `remaining` after it), so part
    // 2's request can only ever complete by finding part 1's batch
    // already covers it, never by starting a batch of its own.
    let task_a = sign(1, calls.clone(), release.clone(), cache.clone());
    while calls.load(std::sync::atomic::Ordering::SeqCst) == 0 {
      tokio::task::yield_now().await;
    }
    let task_b = sign(2, calls.clone(), release.clone(), cache.clone());
    // Give part 2's request a chance to reach (and block on) `sign_lock`
    // before letting part 1's request finish; if it did not block there it
    // would instead race ahead and start a `request` call of its own.
    tokio::time::sleep(Duration::from_millis(50)).await;
    release.notify_waiters();
    let (a, b) = tokio::time::timeout(Duration::from_secs(2), async {
      tokio::join!(task_a, task_b)
    })
    .await
    .expect("a concurrent request for a different part was not blocked on sign_lock");
    assert_eq!(a.unwrap().unwrap().url, "https://example.test/");
    assert_eq!(b.unwrap().unwrap().url, "https://example.test/");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Both parts ended up signed by that one call.
    assert!(cache.fresh(1));
    assert!(cache.fresh(2));
  }

  /// The outcome of a signing request decides what `send_part` does next:
  /// a restart restarts the round, a 410 or non-restart 409 fails
  /// outright, everything else spends an attempt and is retried.
  #[test]
  fn a_sign_failure_is_classified_into_restart_fail_or_retry() {
    let restarted = SignStep::from_sign_error(SignError::Restarted(7));
    assert_eq!(restarted, SignStep::Restarted(7));

    let fatal = SignStep::from_sign_error(SignError::Fatal(
      InstanceError::ModpackUploadSessionGone.into(),
    ));
    assert_eq!(
      fatal,
      SignStep::Fail(InstanceError::ModpackUploadSessionGone.into())
    );

    let retry = SignStep::from_sign_error(SignError::Retry(USTBLError("platform 500".into())));
    assert_eq!(retry, SignStep::Retry(USTBLError("platform 500".into())));
  }

  #[test]
  fn the_batch_never_looks_behind_n() {
    let remaining: std::collections::BTreeSet<u32> = (1..=20).collect();
    // Starting near the end, the batch reaches the actual last part (20)
    // because it is within SIGN_BATCH of n; starting near the front, it
    // must not reach backward to find it.
    let near_end = select_sign_batch(&remaining, 15, |_| false);
    assert_eq!(near_end, vec![15, 16, 17, 18, 19, 20]);

    let near_start = select_sign_batch(&remaining, 1, |_| false);
    assert_eq!(near_start.len(), SIGN_BATCH);
    assert!(!near_start.contains(&20));
  }

  #[test]
  fn a_batch_never_includes_a_part_already_reported() {
    let mut remaining: std::collections::BTreeSet<u32> = (1..=5).collect();
    // Parts 2 and 4 are done and removed from `remaining`, as `reported`
    // would do; they must not reappear in a later batch for part 1.
    remaining.remove(&2);
    remaining.remove(&4);
    let batch = select_sign_batch(&remaining, 1, |_| false);
    assert!(!batch.contains(&2));
    assert!(!batch.contains(&4));
    assert_eq!(batch, vec![1, 3, 5]);
  }

  /// A record resumed from an earlier failure carries that failure's error
  /// until the platform stage begins. When the resumed run fails early
  /// (first status check, upload, commit), `run` must still overwrite the
  /// record with the new error and tell the frontend — the stale message
  /// must not shield the fresh failure just because it exists.
  #[test]
  fn a_fresh_failure_overwrites_the_old_error_a_resumed_record_carries() {
    let mut record = JobRecord {
      schema: RECORD_SCHEMA,
      id: "job".into(),
      phase: Phase::Failed,
      error: Some("MODPACK_UPLOAD_CLOUD_UNAVAILABLE".into()),
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
      upload: None,
    };
    let fresh = ExecuteFailure::Fresh(USTBLError("平台上已找不到这个版本".into()));
    let message = record_failure(&mut record, &fresh);
    assert_eq!(message, Some("平台上已找不到这个版本".to_string()));
    assert_eq!(record.phase, Phase::Failed);
    assert_eq!(record.error.as_deref(), Some("平台上已找不到这个版本"));

    // The platform stage saved the record itself; `run` leaves it alone.
    let mut record = JobRecord { ..record };
    assert_eq!(record_failure(&mut record, &ExecuteFailure::Recorded), None);
    assert_eq!(record.error.as_deref(), Some("平台上已找不到这个版本"));
  }

  /// The `cancelling` registration a resume or a discard checks: it exists
  /// from the moment the guard fires until the cleanup is fully done, so
  /// neither command can slip in between the job's task dying and the
  /// cleanup's last remote answer.
  #[test]
  fn a_job_is_busy_while_its_cancel_cleanup_runs_and_free_after() {
    assert!(!is_cancelling("job-cancel-x"));
    let token = CancellingToken::take("job-cancel-x").unwrap();
    assert!(is_cancelling("job-cancel-x"));
    // One cleanup at a time; a second registration never happens in
    // production (the guard fires once), but the registry must refuse it.
    assert!(CancellingToken::take("job-cancel-x").is_none());
    drop(token);
    assert!(!is_cancelling("job-cancel-x"));
  }
}
