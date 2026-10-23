//! The background half of publishing: sends the delta pack, commits the version
//! and waits for the platform to publish it.
//!
//! The command prepares the plan and the version and returns; this runs on its
//! own and shows on the task list next to the downloads. Everything it needs to
//! start over after a failure or a restart is in the [`JobRecord`].

use super::commands::{
  adopt_published, authed_json, failure, failure_code, failure_detail, instance_of, PublishOutcome,
};
use super::manifest::{LocalFile, PackObject};
use super::pack::{build_pack, verify_pack};
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
static MONITORING: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn is_running(job_id: &str) -> bool {
  ACTIVE.lock().unwrap().iter().any(|id| id == job_id)
}

pub(crate) fn resume_monitors(app: &AppHandle) {
  for record in JobRecord::list(&jobs_dir()) {
    if record.phase == Phase::Processing {
      PublishJob {
        app: app.clone(),
        uploader: crate::utils::web::build_part_upload_client(app),
        record,
        // Only platform processing is watched here, never a part upload.
        concurrency: None,
      }
      .monitor_in_background();
    }
  }
}

fn monitor_is_running(job_id: &str) -> bool {
  MONITORING.lock().unwrap().iter().any(|id| id == job_id)
}

pub(super) fn is_monitoring(job_id: &str) -> bool {
  monitor_is_running(job_id)
}

fn take_monitor(job_id: &str) -> Option<MonitorToken> {
  let mut monitoring = MONITORING.lock().unwrap();
  if monitoring.iter().any(|id| id == job_id) {
    return None;
  }
  monitoring.push(job_id.to_string());
  Some(MonitorToken(job_id.to_string()))
}

struct MonitorToken(String);

impl Drop for MonitorToken {
  fn drop(&mut self) {
    MONITORING.lock().unwrap().retain(|id| id != &self.0);
  }
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

struct Tasks {
  group: String,
  pack: Option<Handle>,
  finish: Handle,
}

impl Tasks {
  fn all(&self) -> impl Iterator<Item = &Handle> {
    self.pack.iter().chain(std::iter::once(&self.finish))
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

/// Cancelling the task drops the job; that gives the version up on the platform
/// and forgets the record. Closing the launcher drops it too but must keep
/// both, so the cleanup is a task of its own that does not outlive the app.
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
    tauri::async_runtime::spawn(async move {
      let _ = authed_json(
        &app,
        reqwest::Method::DELETE,
        &format!("/versions/{version_id}"),
        None,
      )
      .await;
      JobRecord::remove(&jobs_dir(), &job_id);
    });
  }
}

fn complete(handle: &Handle) {
  let mut handle = handle.write().unwrap();
  handle.desc.current = handle.desc.total;
  handle.mark_completed();
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

/// What `GET /uploads/{id}`'s `parts` map means for `expected_generation`
/// (§9.1): the part numbers already reported there, or an empty set when the
/// server has already moved to a different generation (nothing of the old
/// one counts towards the new).
fn received_parts(body: &Value, expected_generation: u32) -> HashSet<u32> {
  let server_generation = body["generation"].as_u64().unwrap_or(0) as u32;
  if server_generation != expected_generation {
    return HashSet::new();
  }
  body["parts"]
    .as_object()
    .into_iter()
    .flatten()
    .filter_map(|(n, _etag)| n.parse::<u32>().ok())
    .collect()
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

/// Signed cloud PUTs expire 900 s after `x-amz-date`, well past a part's
/// expected transfer time; this tells that apart from an ordinary server
/// error (§3, §13.2). The S3-compatible cloud answers with XML, not JSON.
fn is_signature_expired(body: &str) -> bool {
  body.contains("RequestTimeTooSkewed")
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
/// (§13.2: "边读边算 MD5").
async fn read_part(pack: &Path, offset: u64, length: u64) -> USTBLResult<(Bytes, [u8; 16])> {
  let mut file = tokio::fs::File::open(pack).await?;
  file.seek(SeekFrom::Start(offset)).await?;
  let mut buffer = vec![0u8; length as usize];
  file.read_exact(&mut buffer).await?;
  let digest: [u8; 16] = Md5::digest(&buffer).into();
  Ok((Bytes::from(buffer), digest))
}

pub(super) struct PublishJob {
  pub app: AppHandle,
  pub uploader: reqwest::Client,
  pub record: JobRecord,
  /// How many parts to PUT at once (§13.2, §14.1). `Some` only right after
  /// `POST /versions`, whose response carries the value a fresh upload
  /// should use for its first round; `None` on resume (10-23: the record
  /// itself never carries it), so every round instead takes whatever
  /// `GET /uploads/{id}` suggests during reconciliation — which is also
  /// what a restart mid-`upload_pack` falls back to once the first round
  /// is behind it. A later commit changes `upload_pack` itself to act on
  /// this; for now the field only changes shape.
  pub concurrency: Option<usize>,
}

impl PublishJob {
  /// Puts the job on the task list and runs it in the background. Returns the
  /// name of the task group.
  pub(super) fn start(self) -> USTBLResult<String> {
    let token = ActiveToken::take(&self.record.id)
      .ok_or_else(|| USTBLError("这个发布任务已在进行中".into()))?;
    let app = self.app.clone();
    let monitor = app.state::<Pin<Box<TaskMonitor>>>();
    let group = format!(
      "modpack-publish?{} {}@{}",
      self.record.project_slug,
      self.record.version_number,
      chrono::Utc::now().timestamp_millis()
    );

    let pack = self.record.pack.as_ref().map(|pack| {
      upload_handle(
        &app,
        monitor.get_new_id(),
        &group,
        "增量包".to_string(),
        pack.size as i64,
      )
    });
    let finish = upload_handle(
      &app,
      monitor.get_new_id(),
      &group,
      format!("提交版本 {}", self.record.version_number),
      1,
    );
    let tasks = Tasks {
      group: group.clone(),
      pack,
      finish,
    };
    let handles: Vec<Handle> = tasks.all().cloned().collect();
    monitor.register_job_group(&group, &handles);

    let finish_id = tasks.finish.read().unwrap().desc.task_id;
    let job = tauri::async_runtime::spawn(self.run(tasks, token));
    monitor.bind_job(finish_id, job);
    Ok(group)
  }

  async fn run(mut self, tasks: Tasks, _token: ActiveToken) {
    let app = self.app.clone();
    let dir = jobs_dir();
    let finish_id = tasks.finish.read().unwrap().desc.task_id;
    let mut guard = CancelGuard {
      app: app.clone(),
      job_id: self.record.id.clone(),
      version_id: self.record.version_id,
      armed: true,
    };
    let result = self.execute(&tasks).await;
    guard.armed = false;

    match &result {
      Ok(_) => {}
      Err(error) => {
        tasks.abandon();
        // Kept, so the user can send it again or give it up.
        self.record.phase = Phase::Failed;
        self.record.error = Some(error.0.clone());
        let _ = self.record.save(&dir);
      }
    }
    app
      .state::<Pin<Box<TaskMonitor>>>()
      .finish_job_group(&tasks.group, finish_id, result.is_ok());
    if let Err(error) = result {
      let _ = app.emit_to(
        "main",
        FINISHED_EVENT,
        PublishFinished {
          instance_id: self.record.instance_id.clone(),
          group: tasks.group.clone(),
          job_id: self.record.id.clone(),
          outcome: None,
          error: Some(error.0),
        },
      );
    }
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

  async fn execute(&mut self, tasks: &Tasks) -> USTBLResult<()> {
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
        let pack = tasks
          .pack
          .as_ref()
          .ok_or_else(|| USTBLError("缺少增量包任务".into()))?;
        let upload_started = Instant::now();
        if let Err(error) = self.upload_pack(pack, &tasks.group).await {
          pack.write().unwrap().mark_failed(error.0.clone());
          return Err(error);
        }
        log::info!(
          "Modpack publish {} upload completed in {:.2}s",
          self.record.id,
          upload_started.elapsed().as_secs_f64()
        );
      }
      tasks.finish.write().unwrap().mark_started();
      self.record.phase = Phase::Processing;
      self.record.error = None;
      self.record.save(&jobs_dir())?;
      let commit_started = Instant::now();
      if let Err(error) = self.commit().await {
        tasks.finish.write().unwrap().mark_failed(error.0.clone());
        return Err(error);
      }
      log::info!(
        "Modpack publish {} commit accepted in {:.2}s",
        self.record.id,
        commit_started.elapsed().as_secs_f64()
      );
    } else {
      tasks.finish.write().unwrap().mark_started();
    }

    // The upload task ends once the platform has accepted the commit. Platform
    // processing is monitored independently and survives launcher restarts.
    self.record.phase = Phase::Processing;
    self.record.error = None;
    self.record.save(&jobs_dir())?;
    complete(&tasks.finish);
    log::info!(
      "Modpack publish {} submitted in {:.2}s; platform processing continues in background",
      self.record.id,
      started.elapsed().as_secs_f64()
    );
    self.clone_for_monitor().monitor_in_background();
    Ok(())
  }

  fn clone_for_monitor(&self) -> Self {
    Self {
      app: self.app.clone(),
      uploader: self.uploader.clone(),
      record: self.record.clone(),
      concurrency: self.concurrency,
    }
  }

  pub(super) fn monitor_in_background(self) {
    if monitor_is_running(&self.record.id) {
      return;
    }
    tauri::async_runtime::spawn(async move {
      monitor_platform(self).await;
    });
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
  /// write.
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
    if !exists {
      let objects: Vec<PackObject> = self.record.objects.iter().map(PackObject::from).collect();
      let target = path.clone();
      let built = tokio::task::spawn_blocking(move || build_pack(&objects, &target))
        .await
        .map_err(|e| USTBLError(e.to_string()))??;
      if built.sha1 != pack.sha1 || built.size != pack.size {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(USTBLError(
          "实例中的文件在上次发布后发生了变化，无法继续，请放弃这个任务后重新发布".into(),
        ));
      }
    }
    let (check_path, check_sha1) = (path.clone(), pack.sha1.clone());
    tokio::task::spawn_blocking(move || verify_pack(&check_path, &check_sha1))
      .await
      .map_err(|e| USTBLError(e.to_string()))??;
    Ok(path)
  }

  async fn upload_pack(&self, handle: &Handle, group: &str) -> USTBLResult<()> {
    let total = self.record.pack.as_ref().map_or(0, |pack| pack.size);
    let path = self.ensure_pack().await?;
    let mut upload: RecordedUpload = self
      .record
      .upload
      .clone()
      .ok_or_else(|| USTBLError("这个版本没有上传会话".into()))?;

    handle.write().unwrap().mark_started();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Reconciliation (§9.1) may hand back a restarted (newer-generation)
    // upload whose queue starts over from part 1; keep going until every
    // part of the *current* generation is reported. A session that keeps
    // restarting instead of making progress is reported as such rather than
    // retried forever.
    let mut restarts = 0u32;
    loop {
      let reconciled = self.reconcile(&upload).await?;
      match self
        .upload_parts(handle, group, &upload, reconciled, &path, total, &mut tick)
        .await?
      {
        UploadRoundOutcome::Done => return Ok(()),
        UploadRoundOutcome::Restarted(generation) => {
          restarts += 1;
          if restarts > MAX_RESTARTS {
            return Err(InstanceError::ModpackUploadRestarted.into());
          }
          upload.generation = generation;
          self.record_generation(generation)?;
        }
      }
    }
  }

  fn record_generation(&self, generation: u32) -> USTBLResult<()> {
    let mut record = self.record.clone();
    if let Some(upload) = &mut record.upload {
      upload.generation = generation;
    }
    record.save(&jobs_dir())
  }

  /// `GET /uploads/{id}` against the current generation (§9.1): what is
  /// already reported there, and whether the launcher must restart from part
  /// 1 because the server moved to a newer generation behind its back (a
  /// probe-triggered restart from an earlier run, e.g.).
  async fn reconcile(&self, upload: &RecordedUpload) -> USTBLResult<HashSet<u32>> {
    let response = authed_json(
      &self.app,
      reqwest::Method::GET,
      &format!("/uploads/{}", upload.id),
      None,
    )
    .await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
      return Err(USTBLError(
        "平台上的上传会话已过期，请放弃这个任务后重新发布".into(),
      ));
    }
    if !status.is_success() {
      return Err(failure(response).await);
    }
    let body: Value = response.json().await?;
    Ok(received_parts(&body, upload.generation))
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
    let signatures = SignatureCache::default();
    let concurrency = self
      .concurrency
      .unwrap_or(super::commands::DEFAULT_PART_CONCURRENCY)
      .max(1);
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
            Some(Ok(PartOutcome::Reported)) => {}
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
    let mut attempt = 0u32;
    loop {
      if attempt >= PART_ATTEMPTS {
        return Err(last);
      }
      if attempt > 0 {
        sleep(Duration::from_secs(1 << (attempt - 1).min(3))).await;
      }
      attempt += 1;

      let signed = match signatures.get_or_sign(&self.app, upload, n).await {
        Ok(signed) => signed,
        Err(error) => {
          last = error;
          continue;
        }
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
          // what was sent. One resend before giving the job up outright.
          if attempt < PART_ATTEMPTS {
            signatures.discard(n);
            last = USTBLError("云盘回报的 ETag 与本地 MD5 不一致，正在重传".into());
            continue;
          }
          return Err(InstanceError::ModpackUploadVerifyFailed.into());
        }
        return self.report_part(upload, n, &md5_hex, length).await.map(
          |restarted| match restarted {
            Some(generation) => PartOutcome::Restarted(generation),
            None => PartOutcome::Reported,
          },
        );
      }

      sent.take_back(sent_now);
      let body = response.text().await.unwrap_or_default();
      if status == reqwest::StatusCode::FORBIDDEN && is_signature_expired(&body) {
        // §13.2/§9.3: re-signing on an expired signature is not a failed
        // attempt.
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
      return Err(USTBLError(format!("HTTP {status}: {body}")));
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
    Err(failure(response).await)
  }

  /// `POST /uploads/{id}/probe` (§9.3): tells a session the cloud silently
  /// expired from ordinary cloud trouble, after repeated plain 500s on one
  /// part. `Ok(Some(generation))` means the server already restarted the
  /// session; `Ok(None)` means the cloud is just acting up and normal retries
  /// should continue.
  async fn probe(&self, upload: &RecordedUpload) -> USTBLResult<Option<u32>> {
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
    if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
      let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
      sleep(Duration::from_secs(retry_after)).await;
      return Ok(None);
    }
    Err(failure(response).await)
  }
}

enum UploadRoundOutcome {
  Done,
  Restarted(u32),
}

enum PartOutcome {
  Reported,
  Restarted(u32),
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

/// Signatures for parts not yet sent, fetched up to [`SIGN_BATCH`] at a time
/// and shared by every part in flight so a prefetch for one part can be used
/// by another (§13.2: "签名在当前批次用完前预取").
#[derive(Default)]
struct SignatureCache {
  parts: Mutex<HashMap<u32, SignedPart>>,
}

impl SignatureCache {
  fn discard(&self, n: u32) {
    self.parts.lock().unwrap().remove(&n);
  }

  async fn get_or_sign(
    &self,
    app: &AppHandle,
    upload: &RecordedUpload,
    n: u32,
  ) -> USTBLResult<SignedPart> {
    if let Some(signed) = self.parts.lock().unwrap().get(&n) {
      if signed.received_at.elapsed() < SIGNATURE_LIFETIME {
        return Ok(signed.clone());
      }
    }
    self.sign_batch(app, upload, n).await?;
    self
      .parts
      .lock()
      .unwrap()
      .get(&n)
      .cloned()
      .ok_or_else(|| USTBLError("平台没有为这一片签名".into()))
  }

  async fn sign_batch(&self, app: &AppHandle, upload: &RecordedUpload, n: u32) -> USTBLResult<()> {
    // `n` plus whichever of the next few parts still need a fresh signature,
    // up to the batch limit, so one request covers more than just `n`.
    let mut wanted = vec![n];
    {
      let parts = self.parts.lock().unwrap();
      for m in (n + 1)..upload.part_count.min(n + SIGN_BATCH as u32) {
        if wanted.len() >= SIGN_BATCH {
          break;
        }
        let fresh = parts
          .get(&m)
          .is_some_and(|signed| signed.received_at.elapsed() < SIGNATURE_LIFETIME);
        if !fresh {
          wanted.push(m);
        }
      }
    }

    let response = authed_json(
      app,
      reqwest::Method::POST,
      &format!("/uploads/{}/parts/sign", upload.id),
      Some(json!({ "generation": upload.generation, "parts": wanted })),
    )
    .await?;
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    let received_at = Instant::now();
    let body: Value = response.json().await?;
    let requests = body["requests"]
      .as_object()
      .ok_or_else(|| USTBLError("平台返回了无法识别的签名".into()))?;
    let mut parts = self.parts.lock().unwrap();
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
      parts.insert(
        part_n,
        SignedPart {
          method,
          url,
          headers,
          received_at,
        },
      );
    }
    Ok(())
  }
}

async fn monitor_platform(job: PublishJob) {
  let Some(_token) = take_monitor(&job.record.id) else {
    return;
  };
  let started = Instant::now();
  let mut misses = 0;
  let mut last_status = String::new();
  loop {
    let state = match job.poll_status().await {
      Ok(Some(state)) => {
        misses = 0;
        state
      }
      Ok(None) => {
        misses += 1;
        log::warn!(
          "Modpack publish {} status check missed ({}/{})",
          job.record.id,
          misses,
          POLL_MISSES
        );
        if misses >= POLL_MISSES {
          let mut record = job.record.clone();
          record.phase = Phase::Processing;
          record.error = Some("暂时无法确认平台处理进度，请稍后重新启动启动器继续".into());
          let _ = record.save(&jobs_dir());
          return;
        }
        sleep(POLL).await;
        continue;
      }
      Err(error) => {
        let mut record = job.record.clone();
        record.phase = Phase::Failed;
        record.error = Some(error.0.clone());
        let _ = record.save(&jobs_dir());
        emit_finished(&job.app, &record, None, Some(error.0));
        return;
      }
    };
    let status = state["status"].as_str().unwrap_or_default();
    if status != last_status {
      log::info!(
        "Modpack publish {} status changed: {} -> {} after {:.2}s processing",
        job.record.id,
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
      let mut record = job.record.clone();
      record.phase = Phase::Failed;
      record.error = Some(message.clone());
      let _ = record.save(&jobs_dir());
      emit_finished(&job.app, &record, None, Some(message));
      return;
    }
    if status == "published" {
      let files: Vec<LocalFile> = job
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
        .unwrap_or(&job.record.project_slug)
        .to_string();
      let adoption = match instance_of(&job.app, &job.record.instance_id) {
        Ok(instance) => {
          adopt_published(
            &job.app,
            &instance,
            &slug,
            job.record.version_id,
            &job.record.version_number,
            &files,
            job.record.mmc_pack.as_ref(),
          )
          .await
        }
        Err(_) => super::commands::Adoption::RecordFailed,
      };
      JobRecord::remove(&jobs_dir(), &job.record.id);
      let outcome = PublishOutcome {
        version_id: job.record.version_id,
        skipped: job.record.skipped.clone(),
        adoption,
      };
      log::info!(
        "Modpack publish {} platform processing completed in {:.2}s",
        job.record.id,
        started.elapsed().as_secs_f64()
      );
      emit_finished(&job.app, &job.record, Some(outcome), None);
      return;
    }
    sleep(POLL).await;
  }
}

fn emit_finished(
  app: &AppHandle,
  record: &JobRecord,
  outcome: Option<PublishOutcome>,
  error: Option<String>,
) {
  let _ = app.emit_to(
    "main",
    FINISHED_EVENT,
    PublishFinished {
      instance_id: record.instance_id.clone(),
      group: format!("modpack-publish-monitor: {}", record.version_number),
      job_id: record.id.clone(),
      outcome,
      error,
    },
  );
}

#[cfg(test)]
mod tests {
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
  fn reconciliation_picks_up_only_the_current_generations_parts() {
    let body = json!({
      "status": "uploading",
      "generation": 3,
      "parts": { "1": "a".repeat(32), "3": "b".repeat(32) },
    });
    let mut received: Vec<u32> = received_parts(&body, 3).into_iter().collect();
    received.sort_unstable();
    assert_eq!(received, [1, 3]);
  }

  #[test]
  fn reconciliation_against_a_newer_generation_restarts_from_scratch() {
    // The server moved on behind the launcher's back (e.g. a previous run's
    // probe restarted it); nothing already reported under the stale
    // generation counts towards the new one.
    let body = json!({
      "status": "uploading",
      "generation": 4,
      "parts": { "1": "a".repeat(32) },
    });
    assert!(received_parts(&body, 3).is_empty());
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
}
