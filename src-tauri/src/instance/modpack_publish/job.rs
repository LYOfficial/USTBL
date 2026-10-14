//! The background half of publishing: uploads, the new version, the record.
//!
//! The command prepares the plan and returns; this runs on its own and shows
//! on the task list next to the downloads, one task per file.

use super::commands::{
  adopt_published, authed_json, failure, put_file, write_bundle, PublishOutcome, TempDir, TempFile,
};
use super::manifest::{LocalFile, PublishPlan};
use super::transfer::{Concurrency, Transfers};
use crate::error::{USTBLError, USTBLResult};
use crate::instance::models::misc::{Instance, InstanceError};
use crate::tasks::monitor::TaskMonitor;
use crate::tasks::upload::upload_handle;
use crate::tasks::PTaskHandle;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::{interval, MissedTickBehavior};

const FINISHED_EVENT: &str = "modpack-publish:finished";
const MISSING_BATCH: usize = 5000;
/// Matches the reporting interval of the task list, which shows speed per tick.
const TICK: Duration = Duration::from_secs(1);
/// How long one upload count is measured before trying a different one.
const PROBE_EPOCH: Duration = Duration::from_secs(4);

type Handle = Arc<RwLock<PTaskHandle>>;

pub(super) struct UploadJob {
  sha1: String,
  path: PathBuf,
  size: u64,
}

struct Queued {
  job: UploadJob,
  handle: Handle,
}

struct Tasks {
  group: String,
  files: Vec<Handle>,
  bundle: Option<Handle>,
  finish: Handle,
}

impl Tasks {
  fn all(&self) -> impl Iterator<Item = &Handle> {
    self
      .files
      .iter()
      .chain(self.bundle.iter())
      .chain(std::iter::once(&self.finish))
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
  #[serde(skip_serializing_if = "Option::is_none")]
  outcome: Option<PublishOutcome>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
}

/// Deletes the draft version on the platform if the publish does not get to
/// the end, whether it failed or was cancelled (which drops the job).
struct DraftGuard {
  app: AppHandle,
  version_id: i64,
  armed: bool,
}

impl DraftGuard {
  fn new(app: &AppHandle, version_id: i64) -> Self {
    Self {
      app: app.clone(),
      version_id,
      armed: true,
    }
  }

  fn disarm(&mut self) {
    self.armed = false;
  }
}

impl Drop for DraftGuard {
  fn drop(&mut self) {
    if !self.armed {
      return;
    }
    let (app, version_id) = (self.app.clone(), self.version_id);
    tauri::async_runtime::spawn(async move {
      // A draft that never published only takes up space on the server.
      let _ = authed_json(
        &app,
        reqwest::Method::DELETE,
        &format!("/versions/{version_id}"),
        None,
      )
      .await;
    });
  }
}

/// Runs a step for a task, marking the task failed if the step fails.
async fn tracked<T>(handle: &Handle, step: impl Future<Output = USTBLResult<T>>) -> USTBLResult<T> {
  let result = step.await;
  if let Err(error) = &result {
    handle.write().unwrap().mark_failed(error.0.clone());
  }
  result
}

fn complete(handle: &Handle) {
  let mut handle = handle.write().unwrap();
  handle.desc.current = handle.desc.total;
  handle.mark_completed();
}

/// The files the platform does not have yet.
pub(super) async fn find_missing(
  app: &AppHandle,
  root: &Path,
  plan: &PublishPlan,
) -> USTBLResult<Vec<UploadJob>> {
  let blobs = plan.blob_sources(root);
  let hashes: Vec<&String> = blobs.keys().collect();
  let mut missing = Vec::new();
  for batch in hashes.chunks(MISSING_BATCH) {
    let response = authed_json(
      app,
      reqwest::Method::POST,
      "/blobs/missing",
      Some(json!({ "sha1s": batch })),
    )
    .await?;
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    let body: Value = response.json().await?;
    missing.extend(
      body["missing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_string)),
    );
  }
  let mut jobs = Vec::with_capacity(missing.len());
  for sha1 in missing {
    let path = blobs
      .get(&sha1)
      .ok_or_else(|| USTBLError("平台请求了未知的文件".into()))?
      .clone();
    let size = tokio::fs::metadata(&path).await?.len();
    jobs.push(UploadJob { sha1, path, size });
  }
  Ok(jobs)
}

pub(super) struct PublishJob {
  pub app: AppHandle,
  pub uploader: reqwest::Client,
  pub instance: Instance,
  pub project: String,
  pub version_number: String,
  pub changelog: String,
  pub root: PathBuf,
  pub plan: PublishPlan,
  pub parent_version_id: Option<i64>,
  pub loader_type: String,
  /// Patches of a component profile live here until they are uploaded.
  pub _component_work: TempDir,
}

impl PublishJob {
  /// Puts the job on the task list and runs it in the background. Returns the
  /// name of the task group.
  pub(super) fn start(self, missing: Vec<UploadJob>) -> String {
    let app = self.app.clone();
    let monitor = app.state::<Pin<Box<TaskMonitor>>>();
    let group = format!(
      "modpack-publish?{} {}@{}",
      self.project,
      self.version_number,
      chrono::Utc::now().timestamp_millis()
    );

    let mut queue = VecDeque::new();
    let mut files = Vec::new();
    for job in missing {
      let name = job
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| job.sha1.clone());
      let handle = upload_handle(&app, monitor.get_new_id(), &group, name, job.size as i64);
      files.push(handle.clone());
      queue.push_back(Queued { job, handle });
    }
    // The size of the bundle is known once it is packed.
    let bundle = (!self.plan.bundle_files().is_empty())
      .then(|| upload_handle(&app, monitor.get_new_id(), &group, "bundle.zip".into(), 0));
    let finish = upload_handle(
      &app,
      monitor.get_new_id(),
      &group,
      format!("发布版本 {}", self.version_number),
      1,
    );
    let tasks = Tasks {
      group: group.clone(),
      files,
      bundle,
      finish,
    };
    let handles: Vec<Handle> = tasks.all().cloned().collect();
    monitor.register_job_group(&group, &handles);

    let finish_id = tasks.finish.read().unwrap().desc.task_id;
    let job = tauri::async_runtime::spawn(self.run(tasks, queue));
    monitor.bind_job(finish_id, job);
    group
  }

  async fn run(self, tasks: Tasks, queue: VecDeque<Queued>) {
    let app = self.app.clone();
    let instance_id = self.instance.id.clone();
    let finish_id = tasks.finish.read().unwrap().desc.task_id;
    let result = self.execute(&tasks, queue).await;
    if result.is_err() {
      tasks.abandon();
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
        instance_id,
        group: tasks.group.clone(),
        outcome,
        error,
      },
    );
  }

  async fn execute(&self, tasks: &Tasks, queue: VecDeque<Queued>) -> USTBLResult<PublishOutcome> {
    self.upload_blobs(&tasks.group, queue).await?;

    tasks.finish.write().unwrap().mark_started();
    let version_id = tracked(&tasks.finish, self.release(tasks)).await?;
    complete(&tasks.finish);

    let adoption = adopt_published(
      &self.app,
      &self.instance,
      &self.project,
      version_id,
      &self.version_number,
      &self.plan,
    )
    .await;
    Ok(PublishOutcome {
      version_id,
      skipped: self.plan.skipped.clone(),
      adoption,
    })
  }

  /// Creates the version, sends the bundle and publishes. Returns the id.
  async fn release(&self, tasks: &Tasks) -> USTBLResult<i64> {
    let created = authed_json(
      &self.app,
      reqwest::Method::POST,
      &format!("/projects/{}/versions", self.project),
      Some(json!({
        "game_version": self.instance.version,
        "loader_type": self.loader_type,
        "loader_version": self.instance.mod_loader.version,
        "version_number": self.version_number,
        "changelog": self.changelog,
        "parent_version_id": self.parent_version_id,
        "manifest": self.plan.manifest(),
      })),
    )
    .await?;
    if !created.status().is_success() {
      return Err(failure(created).await);
    }
    let version_id = created.json::<Value>().await?["id"]
      .as_i64()
      .ok_or_else(|| USTBLError("平台返回了无法识别的版本".into()))?;

    let mut draft = DraftGuard::new(&self.app, version_id);
    if let Some(bundle) = &tasks.bundle {
      tracked(bundle, self.send_bundle(bundle, version_id)).await?;
    }
    let response = authed_json(
      &self.app,
      reqwest::Method::POST,
      &format!("/versions/{version_id}/publish"),
      None,
    )
    .await?;
    if response.status() == reqwest::StatusCode::CONFLICT {
      return Err(InstanceError::ModpackPublishStale.into());
    }
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    draft.disarm();
    Ok(version_id)
  }

  async fn send_bundle(&self, handle: &Handle, version_id: i64) -> USTBLResult<()> {
    let temp =
      TempFile(std::env::temp_dir().join(format!("ustbl-bundle-{}.zip", uuid::Uuid::new_v4())));
    let (zip_root, zip_path) = (self.root.clone(), temp.0.clone());
    let entries: Vec<LocalFile> = self.plan.bundle_files().into_iter().cloned().collect();
    tokio::task::spawn_blocking(move || {
      let refs: Vec<&LocalFile> = entries.iter().collect();
      write_bundle(&zip_root, &refs, &zip_path)
    })
    .await
    .map_err(|e| USTBLError(e.to_string()))??;

    let size = tokio::fs::metadata(&temp.0).await?.len();
    {
      let mut handle = handle.write().unwrap();
      handle.set_total(size as i64);
      handle.mark_started();
    }
    let transfers = Transfers::new();
    let file = transfers.begin(handle);
    let bundle_path = format!("/versions/{version_id}/bundle");
    let upload = put_file(&self.app, &self.uploader, &bundle_path, &temp.0, &file);
    tokio::pin!(upload);
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let response = loop {
      tokio::select! {
        response = &mut upload => break response,
        _ = tick.tick() => transfers.report(),
      }
    };
    transfers.end(&file);
    let response = response?;
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    complete(handle);
    Ok(())
  }

  /// Sends the missing files, a few at a time. How many is found out on the
  /// way: files are added while the total speed rises.
  async fn upload_blobs(&self, group: &str, mut queue: VecDeque<Queued>) -> USTBLResult<()> {
    let monitor = self.app.state::<Pin<Box<TaskMonitor>>>();
    let transfers = Transfers::new();
    let mut level = Concurrency::new();
    let mut retried = HashSet::new();
    let mut running = FuturesUnordered::new();
    let mut tick = interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut epoch = (Instant::now(), 0u64);

    loop {
      // A paused group starts no new file.
      if !monitor.is_group_stopped(group) {
        while running.len() < level.level() {
          let Some(next) = queue.pop_front() else { break };
          running.push(self.upload_one(&transfers, next));
        }
      }
      if running.is_empty() && queue.is_empty() {
        return Ok(());
      }
      tokio::select! {
        finished = running.next(), if !running.is_empty() => {
          let Some((next, result)) = finished else { continue };
          match result {
            Ok(()) => {}
            // Several files at once may be what the platform or the cloud
            // objects to: try this one again on its own before giving up.
            Err(_) if level.level() > 1 && retried.insert(next.job.sha1.clone()) => {
              level.back_off();
              queue.push_front(next);
            }
            Err(error) => {
              next.handle.write().unwrap().mark_failed(error.0.clone());
              return Err(error);
            }
          }
        }
        _ = tick.tick() => {
          transfers.report();
          if epoch.0.elapsed() >= PROBE_EPOCH {
            let sent = transfers.sent_bytes();
            // Only a full set of files in flight says anything about the count.
            if running.len() >= level.level() && !queue.is_empty() {
              let seconds = epoch.0.elapsed().as_secs_f64();
              level.observe(sent.saturating_sub(epoch.1) as f64 / seconds);
            }
            epoch = (Instant::now(), sent);
          }
        }
      }
    }
  }

  async fn upload_one(&self, transfers: &Transfers, next: Queued) -> (Queued, USTBLResult<()>) {
    next.handle.write().unwrap().mark_started();
    let file = transfers.begin(&next.handle);
    let result = async {
      let path = format!("/blobs/{}", next.job.sha1);
      let response = put_file(&self.app, &self.uploader, &path, &next.job.path, &file).await?;
      if response.status().is_success() {
        Ok(())
      } else {
        Err(failure(response).await)
      }
    }
    .await;
    transfers.end(&file);
    if result.is_ok() {
      complete(&next.handle);
    }
    (next, result)
  }
}
