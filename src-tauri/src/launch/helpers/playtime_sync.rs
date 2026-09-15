use crate::account::helpers::vustb;
use crate::account::models::AccountInfo;
use crate::error::USTBLResult;
use crate::storage::Storage;
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

const PLAYTIME_QUEUE_FILE: &str = "ustbl.vustb-playtime.json";
const PLAYTIME_SYNC_INTERVAL_SECONDS: u64 = 600;
static PLAYTIME_STORAGE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static PLAYTIME_FLUSH_LOCK: LazyLock<tokio::sync::Mutex<()>> =
  LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PlaytimeEvent {
  event_id: String,
  client_id: String,
  subject: String,
  seconds: u32,
  instance_id: Option<String>,
  instance_name: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct PlaytimeQueue {
  client_id: String,
  events: Vec<PlaytimeEvent>,
}

impl Storage for PlaytimeQueue {
  fn file_path() -> std::path::PathBuf {
    crate::APP_DATA_DIR
      .get()
      .expect("APP_DATA_DIR initialization failed")
      .join(PLAYTIME_QUEUE_FILE)
  }
}

fn truncate(value: &str, max_chars: usize) -> String {
  value.chars().take(max_chars).collect()
}

fn split_playtime_seconds(seconds: u64) -> Vec<u32> {
  let mut remaining = seconds;
  let mut chunks = Vec::new();
  while remaining > 0 {
    let chunk = remaining.min(PLAYTIME_SYNC_INTERVAL_SECONDS) as u32;
    chunks.push(chunk);
    remaining -= u64::from(chunk);
  }
  chunks
}

fn current_subject(app: &AppHandle) -> USTBLResult<Option<String>> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let state = binding.lock()?;
  Ok(
    state
      .vustb_account
      .as_ref()
      .map(|account| account.subject.clone())
      .filter(|subject| !subject.is_empty()),
  )
}

fn load_queue() -> PlaytimeQueue {
  let mut queue = PlaytimeQueue::load().unwrap_or_default();
  if queue.client_id.len() < 8 {
    queue.client_id = uuid::Uuid::new_v4().simple().to_string();
  }
  queue
}

pub async fn queue_playtime_delta(
  app: &AppHandle,
  instance_id: &str,
  instance_name: Option<&str>,
  seconds: u64,
) -> USTBLResult<()> {
  let Some(subject) = current_subject(app)? else {
    return Ok(());
  };
  if seconds == 0 {
    return Ok(());
  }

  {
    let _guard = PLAYTIME_STORAGE_LOCK.lock()?;
    let mut queue = load_queue();
    let client_id = queue.client_id.clone();
    for chunk in split_playtime_seconds(seconds) {
      queue.events.push(PlaytimeEvent {
        event_id: uuid::Uuid::new_v4().simple().to_string(),
        client_id: client_id.clone(),
        subject: subject.clone(),
        seconds: chunk,
        instance_id: Some(truncate(instance_id, 64)),
        instance_name: instance_name.map(|name| truncate(name, 128)),
      });
    }
    queue.save()?;
  }

  if let Err(error) = flush_playtime_queue(app).await {
    log::warn!("Queued vUSTB playtime for a later retry: {error:?}");
  }
  Ok(())
}

pub async fn flush_playtime_queue(app: &AppHandle) -> USTBLResult<()> {
  let _flush_guard = PLAYTIME_FLUSH_LOCK.lock().await;
  let Some(subject) = current_subject(app)? else {
    return Ok(());
  };
  let events = {
    let _guard = PLAYTIME_STORAGE_LOCK.lock()?;
    load_queue()
      .events
      .into_iter()
      .filter(|event| event.subject == subject)
      .collect::<Vec<_>>()
  };

  for event in events {
    let event_id = event.event_id.clone();
    let payload = serde_json::json!({
      "event_id": event.event_id.clone(),
      "client_id": event.client_id.clone(),
      "seconds": event.seconds,
      "instance_id": event.instance_id.clone(),
      "instance_name": event.instance_name.clone(),
    });
    let result =
      vustb::post_authenticated::<_, serde_json::Value>(app, "/api/launcher/playtime", &payload)
        .await;
    if let Err(error) = result {
      log::warn!("vUSTB playtime sync will retry later: {error:?}");
      break;
    }

    let _guard = PLAYTIME_STORAGE_LOCK.lock()?;
    let mut queue = load_queue();
    queue.events.retain(|item| item.event_id != event_id);
    queue.save()?;
  }
  Ok(())
}

pub async fn monitor_playtime_sync(app: AppHandle) {
  let mut interval = tokio::time::interval(Duration::from_secs(PLAYTIME_SYNC_INTERVAL_SECONDS));
  loop {
    interval.tick().await;
    if let Err(error) = flush_playtime_queue(&app).await {
      log::debug!("Background vUSTB playtime sync skipped: {error:?}");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{split_playtime_seconds, truncate};

  #[test]
  fn playtime_is_split_into_api_sized_events() {
    assert_eq!(split_playtime_seconds(0), Vec::<u32>::new());
    assert_eq!(split_playtime_seconds(1), vec![1]);
    assert_eq!(split_playtime_seconds(600), vec![600]);
    assert_eq!(split_playtime_seconds(1201), vec![600, 600, 1]);
  }

  #[test]
  fn playtime_metadata_is_truncated_by_characters() {
    assert_eq!(truncate("像素北科启动器", 4), "像素北科");
  }
}
