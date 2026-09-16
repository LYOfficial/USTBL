use crate::account::helpers::vustb;
use crate::account::models::AccountError;
use crate::error::USTBLResult;
use crate::APP_DATA_DIR;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;
use tauri::AppHandle;
use uuid::Uuid;

const CLIENT_ID_FILE: &str = "ustbl.vustb-client-id";
const PRESENCE_INTERVAL: Duration = Duration::from_secs(120);
static CLIENT_ID: OnceLock<String> = OnceLock::new();
static ACTIVE_GAMES: LazyLock<Mutex<BTreeMap<u64, String>>> =
  LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn valid_client_id(value: &str) -> bool {
  (8..=64).contains(&value.len())
    && value
      .bytes()
      .all(|value| value.is_ascii_alphanumeric() || value == b'_' || value == b'-')
}

fn load_or_create_client_id() -> USTBLResult<String> {
  if let Some(value) = CLIENT_ID.get() {
    return Ok(value.clone());
  }
  let path = APP_DATA_DIR
    .get()
    .ok_or(AccountError::SaveError)?
    .join(CLIENT_ID_FILE);
  let stored = fs::read_to_string(&path)
    .ok()
    .map(|value| value.trim().to_string());
  let (value, should_write) = match stored.filter(|value| valid_client_id(value)) {
    Some(value) => (value, false),
    None => (Uuid::new_v4().simple().to_string(), true),
  };
  if should_write {
    fs::write(&path, &value).map_err(|_| AccountError::SaveError)?;
  }
  let _ = CLIENT_ID.set(value.clone());
  Ok(value)
}

fn current_instance_name() -> Option<String> {
  ACTIVE_GAMES
    .lock()
    .ok()
    .and_then(|games| games.last_key_value().map(|(_, name)| name.clone()))
}

pub fn track_game_started(id: u64, instance_name: String) {
  if let Ok(mut games) = ACTIVE_GAMES.lock() {
    games.insert(id, instance_name);
  }
}

pub fn track_game_stopped(id: u64) {
  if let Ok(mut games) = ACTIVE_GAMES.lock() {
    games.remove(&id);
  }
}

pub async fn sync(app: &AppHandle) -> USTBLResult<()> {
  let payload = serde_json::json!({
    "client_id": load_or_create_client_id()?,
    "instance_name": current_instance_name(),
  });
  let _: Value =
    vustb::put_authenticated(app, "/api/community/launcher/presence", &payload).await?;
  Ok(())
}

pub async fn clear(app: &AppHandle) -> USTBLResult<()> {
  let client_id = load_or_create_client_id()?;
  let endpoint = format!("/api/community/launcher/presence?client_id={client_id}");
  let _: Value = vustb::delete_authenticated(app, &endpoint).await?;
  Ok(())
}

pub async fn monitor(app: AppHandle) {
  loop {
    if let Err(error) = sync(&app).await {
      log::debug!("vUSTB presence heartbeat skipped: {error:?}");
    }
    tokio::time::sleep(PRESENCE_INTERVAL).await;
  }
}

#[cfg(test)]
mod tests {
  use super::valid_client_id;

  #[test]
  fn presence_client_id_is_restricted_to_api_safe_characters() {
    assert!(valid_client_id("12345678"));
    assert!(valid_client_id("launcher_client-1"));
    assert!(!valid_client_id("short"));
    assert!(!valid_client_id("client id with spaces"));
  }
}
