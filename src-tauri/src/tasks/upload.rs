//! Uploads that run outside the download queue but show up on the task list.

use crate::launcher_config::commands::retrieve_launcher_config;
use crate::tasks::events::TauriEventSink;
use crate::tasks::streams::desc::{PDesc, PStatus};
use crate::tasks::streams::reporter::Reporter;
use crate::tasks::{PTaskHandle, PTaskParam};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use tauri::AppHandle;
use tokio::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UploadParam {
  /// What the task list shows for this entry.
  pub filename: String,
}

/// A waiting upload task. Its owner drives it: starts it, reports progress
/// once a second and ends it, the way a download task reports itself.
pub fn upload_handle(
  app: &AppHandle,
  task_id: u32,
  task_group: &str,
  filename: String,
  total: i64,
) -> Arc<RwLock<PTaskHandle>> {
  let cache_dir = retrieve_launcher_config(app.clone())
    .unwrap()
    .download
    .cache
    .directory;
  Arc::new(RwLock::new(PTaskHandle::new(
    PDesc::<PTaskParam>::new(
      task_id,
      Some(task_group.to_string()),
      total,
      PTaskParam::Upload(UploadParam { filename }),
      PStatus::Waiting,
    ),
    Duration::from_secs(1),
    cache_dir.join(format!("task-{task_id}.json")),
    Reporter::new(
      total,
      Duration::from_secs(1),
      TauriEventSink::new(app.clone()),
    ),
  )))
}
