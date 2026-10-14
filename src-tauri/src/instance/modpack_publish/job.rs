//! The background half of publishing: sends the delta pack, commits the version
//! and waits for the platform to publish it.
//!
//! The command prepares the plan and the version and returns; this runs on its
//! own and shows on the task list next to the downloads. Everything it needs to
//! start over after a failure or a restart is in the [`JobRecord`].

use super::commands::{
  adopt_published, authed_json, failure, failure_code, instance_of, PublishOutcome,
};
use super::manifest::{LocalFile, PackObject};
use super::pack::build_pack;
use super::record::{jobs_dir, paths, JobRecord, Phase, RecordedUpload};
use super::transfer::{report, SentBytes};
use crate::account::helpers::vustb;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::models::misc::InstanceError;
use crate::instance::modpack_update::diff::FileKey;
use crate::instance::modpack_update::sources::CAMPUS_API;
use crate::tasks::monitor::TaskMonitor;
use crate::tasks::upload::upload_handle;
use crate::tasks::PTaskHandle;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::collections::{HashSet, VecDeque};
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
/// Chunks in flight at once. Fixed: the platform takes them in parallel and the
/// cloud behind it is not the limit chunks of this size run into.
const CHUNK_CONCURRENCY: usize = 3;
const CHUNK_ATTEMPTS: u32 = 4;
/// Bytes handed to the HTTP stack at a time, so progress moves smoothly.
const SLICE: usize = 64 * 1024;
/// Matches the reporting interval of the task list, which shows speed per tick.
const TICK: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_secs(2);

type Handle = Arc<RwLock<PTaskHandle>>;

/// Jobs that run right now; the rest of the records are waiting for the user.
static ACTIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(super) fn is_running(job_id: &str) -> bool {
  ACTIVE.lock().unwrap().iter().any(|id| id == job_id)
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

fn chunk_length(total: u64, chunk_size: u64, count: u32, n: u32) -> u64 {
  if n + 1 == count {
    total - chunk_size * (count as u64 - 1)
  } else {
    chunk_size
  }
}

fn retriable(status: reqwest::StatusCode) -> bool {
  status.is_server_error()
    || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    || status == reqwest::StatusCode::REQUEST_TIMEOUT
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
        handed_over.lock().unwrap().get_or_insert_with(Instant::now);
        return None;
      }
      let end = (offset + SLICE).min(data.len());
      let part = data.slice(offset..end);
      sent.add(part.len() as u64);
      counted.fetch_add(part.len() as u64, Ordering::Relaxed);
      Some((Ok::<_, io::Error>(part), end))
    }
  });
  reqwest::Body::wrap_stream(stream)
}

async fn read_chunk(pack: &Path, offset: u64, length: u64) -> USTBLResult<Bytes> {
  let mut file = tokio::fs::File::open(pack).await?;
  file.seek(SeekFrom::Start(offset)).await?;
  let mut buffer = vec![0u8; length as usize];
  file.read_exact(&mut buffer).await?;
  Ok(Bytes::from(buffer))
}

pub(super) struct PublishJob {
  pub app: AppHandle,
  pub uploader: reqwest::Client,
  pub record: JobRecord,
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
      format!("发布版本 {}", self.record.version_number),
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
      Ok(_) => JobRecord::remove(&dir, &self.record.id),
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
    let (outcome, error) = match result {
      Ok(outcome) => (Some(outcome), None),
      Err(error) => (None, Some(error.0)),
    };
    let _ = app.emit_to(
      "main",
      FINISHED_EVENT,
      PublishFinished {
        instance_id: self.record.instance_id.clone(),
        group: tasks.group.clone(),
        job_id: self.record.id.clone(),
        outcome,
        error,
      },
    );
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

  async fn execute(&mut self, tasks: &Tasks) -> USTBLResult<PublishOutcome> {
    let mut state = self.status().await?;
    let mut current = state["status"].as_str().unwrap_or_default().to_string();
    if current != "published" && current != "processing" {
      if self.record.upload.is_some() {
        let pack = tasks
          .pack
          .as_ref()
          .ok_or_else(|| USTBLError("缺少增量包任务".into()))?;
        if let Err(error) = self.upload_pack(pack, &tasks.group).await {
          pack.write().unwrap().mark_failed(error.0.clone());
          return Err(error);
        }
      }
      tasks.finish.write().unwrap().mark_started();
      self.record.phase = Phase::Processing;
      self.record.error = None;
      self.record.save(&jobs_dir())?;
      if let Err(error) = self.commit().await {
        tasks.finish.write().unwrap().mark_failed(error.0.clone());
        return Err(error);
      }
      current = "processing".into();
    } else {
      tasks.finish.write().unwrap().mark_started();
    }

    while current != "published" {
      state = self.status().await?;
      current = state["status"].as_str().unwrap_or_default().to_string();
      if current == "failed" {
        let message = state["error"]
          .as_str()
          .filter(|text| !text.is_empty())
          .unwrap_or("平台处理增量包失败");
        let error = USTBLError(message.to_string());
        tasks.finish.write().unwrap().mark_failed(error.0.clone());
        return Err(error);
      }
      if current != "published" {
        sleep(POLL).await;
      }
    }
    complete(&tasks.finish);

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
      // The instance was deleted while the version went out.
      Err(_) => super::commands::Adoption::RecordFailed,
    };
    Ok(PublishOutcome {
      version_id: self.record.version_id,
      skipped: self.record.skipped.clone(),
      adoption,
    })
  }

  async fn commit(&self) -> USTBLResult<()> {
    let response = authed_json(
      &self.app,
      reqwest::Method::POST,
      &format!("/versions/{}/commit", self.record.version_id),
      None,
    )
    .await?;
    if response.status().is_success() {
      return Ok(());
    }
    let (code, error) = failure_code(response).await;
    if code.as_deref() == Some("PARENT_MISMATCH") {
      return Err(InstanceError::ModpackPublishStale.into());
    }
    Err(error)
  }

  /// The pack is built again from the recorded sources when it is gone, and has
  /// to come out as the pack the platform was promised.
  async fn ensure_pack(&self) -> USTBLResult<std::path::PathBuf> {
    let pack = self
      .record
      .pack
      .clone()
      .ok_or_else(|| USTBLError("这个版本没有增量包".into()))?;
    let path = paths(&jobs_dir(), &self.record.id)?.pack;
    if tokio::fs::metadata(&path)
      .await
      .is_ok_and(|meta| meta.len() == pack.size)
    {
      return Ok(path);
    }
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
    Ok(path)
  }

  async fn upload_pack(&self, handle: &Handle, group: &str) -> USTBLResult<()> {
    let upload: RecordedUpload = self
      .record
      .upload
      .clone()
      .ok_or_else(|| USTBLError("这个版本没有上传会话".into()))?;
    let total = self.record.pack.as_ref().map_or(0, |pack| pack.size);
    let path = self.ensure_pack().await?;

    let received = self.received_chunks(&upload).await?;
    let stored: u64 = received
      .iter()
      .map(|n| chunk_length(total, upload.chunk_size, upload.chunk_count, *n))
      .sum();
    let mut queue: VecDeque<u32> = (0..upload.chunk_count)
      .filter(|n| !received.contains(n))
      .collect();

    handle.write().unwrap().mark_started();
    let sent = SentBytes::new(total, stored);
    let monitor = self.app.state::<Pin<Box<TaskMonitor>>>();
    let mut running = FuturesUnordered::new();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
      // A paused group starts no new chunk.
      if !monitor.is_group_stopped(group) {
        while running.len() < CHUNK_CONCURRENCY {
          let Some(n) = queue.pop_front() else { break };
          let length = chunk_length(total, upload.chunk_size, upload.chunk_count, n);
          running.push(self.send_chunk(&upload, &path, n, length, &sent));
        }
      }
      if running.is_empty() && queue.is_empty() {
        break;
      }
      tokio::select! {
        finished = running.next(), if !running.is_empty() => {
          if let Some(result) = finished {
            result?;
          }
        }
        _ = tick.tick() => report(handle, sent.done()),
      }
    }
    complete(handle);
    Ok(())
  }

  async fn received_chunks(&self, upload: &RecordedUpload) -> USTBLResult<HashSet<u32>> {
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
    Ok(
      body["received"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_u64().map(|n| n as u32))
        .collect(),
    )
  }

  async fn send_chunk(
    &self,
    upload: &RecordedUpload,
    pack: &Path,
    n: u32,
    length: u64,
    sent: &Arc<SentBytes>,
  ) -> USTBLResult<()> {
    let prepared = Instant::now();
    let data = read_chunk(pack, n as u64 * upload.chunk_size, length).await?;
    let digest = hex::encode(Sha1::digest(&data));
    let prepare_time = prepared.elapsed();
    let url = format!("{CAMPUS_API}/uploads/{}/chunks/{n}", upload.id);

    let mut last = USTBLError("上传失败".into());
    for attempt in 0..CHUNK_ATTEMPTS {
      if attempt > 0 {
        sleep(Duration::from_secs(1 << (attempt - 1))).await;
      }
      let counted = Arc::new(AtomicU64::new(0));
      let handed_over: HandedOver = Arc::new(Mutex::new(None));
      let started = Instant::now();
      let response = vustb::send_authenticated_with(&self.app, &self.uploader, |client, token| {
        client
          .put(&url)
          .bearer_auth(token)
          .header("Content-Type", "application/octet-stream")
          .header("Content-Length", length)
          .header("X-Chunk-SHA1", &digest)
          .body(counting_body(
            data.clone(),
            sent.clone(),
            counted.clone(),
            handed_over.clone(),
          ))
      })
      .await;
      let sent_now = counted.load(Ordering::Relaxed);
      // Where the time of a chunk goes: reading and hashing it, getting the
      // body into the connection, and waiting for the platform's answer.
      let body_time = handed_over
        .lock()
        .unwrap()
        .map(|done| done.duration_since(started).as_secs_f64());
      log::info!(
        "Chunk {n}/{} (attempt {}): prepared in {:.2}s, body handed over after {}, answered after {:.2}s ({})",
        upload.chunk_count,
        attempt + 1,
        prepare_time.as_secs_f64(),
        body_time.map_or("never".to_string(), |time| format!("{time:.2}s")),
        started.elapsed().as_secs_f64(),
        match &response {
          Ok(response) => format!("{:?} {}", response.version(), response.status()),
          Err(error) => format!("error: {}", error.0),
        }
      );
      match response {
        Ok(response) if response.status().is_success() => {
          // Whatever the stream did not count (it is read once per request).
          if sent_now < length {
            sent.add(length - sent_now);
          } else {
            sent.take_back(sent_now - length);
          }
          return Ok(());
        }
        Ok(response) if retriable(response.status()) => {
          sent.take_back(sent_now);
          last = failure(response).await;
        }
        Ok(response) => {
          sent.take_back(sent_now);
          return Err(failure(response).await);
        }
        Err(error) => {
          sent.take_back(sent_now);
          last = error;
        }
      }
    }
    Err(last)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn chunks_cover_the_pack_with_a_shorter_last_one() {
    // 20 bytes in chunks of 8: 8 + 8 + 4.
    let lengths: Vec<u64> = (0..3).map(|n| chunk_length(20, 8, 3, n)).collect();
    assert_eq!(lengths, [8, 8, 4]);
    assert_eq!(chunk_length(8, 8, 1, 0), 8);
    assert_eq!(chunk_length(5, 8, 1, 0), 5);
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
}
