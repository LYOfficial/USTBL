use super::ResourceAccelerationService;
use crate::error::USTBLResult;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAccelerationStatus {
  pub running: bool,
  pub loading: bool,
  pub github_enabled: bool,
  pub hosts_total: usize,
  pub hosts_completed: usize,
  pub started_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAccelerationLatency {
  pub host: String,
  pub latency_ms: Option<u64>,
  pub available: bool,
}

#[tauri::command]
pub async fn start_resource_acceleration(
  app: AppHandle,
  github_enabled: bool,
) -> USTBLResult<ResourceAccelerationStatus> {
  let service = app.state::<Mutex<ResourceAccelerationService>>();
  let mut service = service.lock().await;
  service
    .start(&app, github_enabled)
    .await
    .map_err(Into::into)
}

#[tauri::command]
pub async fn stop_resource_acceleration(app: AppHandle) -> USTBLResult<ResourceAccelerationStatus> {
  let service = app.state::<Mutex<ResourceAccelerationService>>();
  let mut service = service.lock().await;
  service.stop().await.map_err(Into::into)
}

#[tauri::command]
pub async fn retrieve_resource_acceleration_status(
  app: AppHandle,
) -> USTBLResult<ResourceAccelerationStatus> {
  let service = app.state::<Mutex<ResourceAccelerationService>>();
  let status = service.lock().await.status();
  Ok(status)
}

#[tauri::command]
pub async fn test_resource_acceleration_latency(
  github_enabled: bool,
) -> USTBLResult<Vec<ResourceAccelerationLatency>> {
  Ok(super::service::test_latency(github_enabled).await)
}
