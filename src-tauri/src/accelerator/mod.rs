mod browser_proxy;
pub mod commands;
pub mod ipv6;
mod service;
mod system_proxy;

pub(crate) use service::ResourceAccelerationService;

pub fn recover_system_proxy(app: &tauri::AppHandle) {
  use tauri::Manager;
  if let Ok(path) = app.path().app_local_data_dir() {
    if let Err(error) = system_proxy::recover_proxy(&path.join("acceleration-proxy.json")) {
      log::error!("Failed to recover USTBL system proxy: {error}");
    }
  }
}
