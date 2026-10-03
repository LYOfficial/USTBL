use tauri::Manager;

#[tauri::command]
pub fn resize(app: tauri::AppHandle, view: String) -> Result<(), String> {
  if let Some(window) = app.get_webview_window("tray_popup") {
    crate::position_tray_popup(&app, &window, &view);
  }
  Ok(())
}
