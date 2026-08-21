use crate::account::models::AccountInfo;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::misc::get_instance_subdir_path_by_id;
use crate::instance::models::misc::InstanceSubdirType;
use crate::storage::Storage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_http::reqwest;

const VUSTB_API: &str = "https://www.ustb.world/api/mc-instances";
const SHARED_INSTANCE_BINDINGS_FILE: &str = "ustbl.shared-instance-bindings.json";
const SHARED_INSTANCE_UPDATE_PROGRESS_EVENT: &str = "shared-instance:update-progress";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct SharedInstance {
  pub id: u64,
  pub name: String,
  pub created_at: String,
  pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct SharedMod {
  pub id: u64,
  pub file_name: String,
  pub file_size: u64,
  pub status: String,
  pub created_by_username: Option<String>,
  pub created_at: String,
  pub deleted_by_username: Option<String>,
  pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct SharedInstanceDetail {
  pub id: u64,
  pub name: String,
  pub created_at: String,
  pub updated_at: String,
  pub mods: Vec<SharedMod>,
}

#[derive(Debug, Clone, Deserialize)]
struct SharedDownloadRequest {
  url: String,
  method: String,
  #[serde(default)]
  headers: HashMap<String, String>,
  name: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SharedInstanceBindings(pub HashMap<u64, String>);

impl Storage for SharedInstanceBindings {
  fn file_path() -> PathBuf {
    crate::APP_DATA_DIR
      .get()
      .expect("APP_DATA_DIR initialization failed")
      .join(SHARED_INSTANCE_BINDINGS_FILE)
  }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedUpdateResult {
  pub deleted: Vec<String>,
  pub downloaded: Vec<String>,
  pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SharedUpdateProgress {
  shared_instance_id: u64,
  current: usize,
  total: usize,
  file_name: Option<String>,
}

fn invalid_response(response: reqwest::Response) -> USTBLError {
  USTBLError(format!(
    "像素北科共享实例 API 返回 HTTP {}",
    response.status()
  ))
}

fn emit_update_progress(
  app: &AppHandle,
  shared_instance_id: u64,
  current: usize,
  total: usize,
  file_name: Option<&str>,
) {
  if let Err(err) = app.emit_to(
    "main",
    SHARED_INSTANCE_UPDATE_PROGRESS_EVENT,
    SharedUpdateProgress {
      shared_instance_id,
      current,
      total,
      file_name: file_name.map(ToOwned::to_owned),
    },
  ) {
    log::warn!("Failed to emit shared instance update progress: {err}");
  }
}

/// 管理接口本身由平台按启动器入口约定公开；这里仍在 Rust 命令层重复检查，
/// 防止普通账户通过前端以外的调用路径触发上传或删除。
fn ensure_minecraft_manager(app: &AppHandle) -> USTBLResult<()> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let account_state = binding.lock()?;
  let user_group = account_state
    .vustb_account
    .as_ref()
    .map(|account| account.user_group.as_str())
    .ok_or_else(|| USTBLError("请先登录像素北科账号".to_string()))?;
  if matches!(
    user_group,
    "super_admin" | "admin" | "platform_manager" | "server_manager"
  ) {
    Ok(())
  } else {
    Err(USTBLError(
      "当前像素北科账号没有共享实例管理权限".to_string(),
    ))
  }
}

async fn get_json<T: for<'de> Deserialize<'de>>(app: &AppHandle, path: &str) -> USTBLResult<T> {
  let client = app.state::<reqwest::Client>();
  let response = client
    .get(format!("{VUSTB_API}{path}"))
    .send()
    .await
    .map_err(|err| USTBLError(format!("无法连接像素北科共享实例 API：{err}")))?;
  if !response.status().is_success() {
    return Err(invalid_response(response));
  }
  response
    .json::<T>()
    .await
    .map_err(|err| USTBLError(format!("共享实例 API 返回格式错误：{err}")))
}

fn assert_mod_filename(file_name: &str) -> USTBLResult<()> {
  let path = Path::new(file_name);
  let is_filename = path
    .file_name()
    .and_then(|name| name.to_str())
    .is_some_and(|name| name == file_name);
  if !is_filename || !file_name.to_lowercase().ends_with(".jar") {
    return Err(USTBLError("共享实例返回了不安全的模组文件名".to_string()));
  }
  Ok(())
}

async fn get_download_request(
  app: &AppHandle,
  instance_id: u64,
  mod_id: u64,
) -> USTBLResult<SharedDownloadRequest> {
  get_json(app, &format!("/{instance_id}/mods/{mod_id}/download")).await
}

async fn download_mod(
  app: &AppHandle,
  instance_id: u64,
  mod_info: &SharedMod,
  destination: &Path,
) -> USTBLResult<()> {
  assert_mod_filename(&mod_info.file_name)?;
  let request_info = get_download_request(app, instance_id, mod_info.id).await?;
  if !request_info.method.eq_ignore_ascii_case("GET") {
    return Err(USTBLError("共享实例返回了不支持的模组下载方法".to_string()));
  }
  assert_mod_filename(&request_info.name)?;
  if request_info.name != mod_info.file_name {
    return Err(USTBLError("共享实例下载文件名与模组档案不一致".to_string()));
  }

  let client = app.state::<reqwest::Client>();
  let mut request = client.get(request_info.url);
  for (key, value) in request_info.headers {
    request = request.header(key, value);
  }
  let response = request
    .send()
    .await
    .map_err(|err| USTBLError(format!("下载共享模组失败：{err}")))?
    .error_for_status()
    .map_err(|err| USTBLError(format!("下载共享模组失败：{err}")))?;
  let bytes = response
    .bytes()
    .await
    .map_err(|err| USTBLError(format!("读取共享模组失败：{err}")))?;
  if mod_info.file_size > 0 && bytes.len() as u64 != mod_info.file_size {
    return Err(USTBLError(format!(
      "共享模组 {} 的大小校验失败",
      mod_info.file_name
    )));
  }

  tokio::fs::create_dir_all(destination.parent().unwrap_or(destination))
    .await
    .map_err(|err| USTBLError(format!("创建模组目录失败：{err}")))?;
  let partial = destination.with_extension("jar.ustbl-downloading");
  tokio::fs::write(&partial, bytes)
    .await
    .map_err(|err| USTBLError(format!("写入共享模组失败：{err}")))?;
  tokio::fs::rename(&partial, destination)
    .await
    .map_err(|err| USTBLError(format!("完成共享模组下载失败：{err}")))
}

#[tauri::command]
pub async fn retrieve_shared_instance_list(app: AppHandle) -> USTBLResult<Vec<SharedInstance>> {
  get_json(&app, "").await
}

#[tauri::command]
pub async fn retrieve_shared_instance_detail(
  app: AppHandle,
  shared_instance_id: u64,
) -> USTBLResult<SharedInstanceDetail> {
  get_json(&app, &format!("/{shared_instance_id}")).await
}

#[tauri::command]
pub fn retrieve_shared_instance_binding(shared_instance_id: u64) -> USTBLResult<Option<String>> {
  let bindings = SharedInstanceBindings::load().unwrap_or_default();
  Ok(bindings.0.get(&shared_instance_id).cloned())
}

#[tauri::command]
pub fn set_shared_instance_binding(
  shared_instance_id: u64,
  local_instance_id: String,
) -> USTBLResult<()> {
  let mut bindings = SharedInstanceBindings::load().unwrap_or_default();
  bindings.0.insert(shared_instance_id, local_instance_id);
  bindings.save()?;
  Ok(())
}

#[tauri::command]
pub async fn update_shared_instance(
  app: AppHandle,
  shared_instance_id: u64,
  local_instance_id: String,
) -> USTBLResult<SharedUpdateResult> {
  let mods_dir =
    get_instance_subdir_path_by_id(&app, &local_instance_id, &InstanceSubdirType::Mods)
      .ok_or_else(|| USTBLError("未找到已绑定的本地实例".to_string()))?;
  let shared: SharedInstanceDetail = get_json(&app, &format!("/{shared_instance_id}")).await?;
  let mut result = SharedUpdateResult {
    deleted: vec![],
    downloaded: vec![],
    skipped: vec![],
  };
  let total = shared.mods.len();
  let mut current = 0;
  emit_update_progress(&app, shared_instance_id, current, total, None);

  for mod_info in shared.mods.iter().filter(|item| item.status == "deleted") {
    assert_mod_filename(&mod_info.file_name)?;
    let target = mods_dir.join(&mod_info.file_name);
    if target.is_file() {
      tokio::fs::remove_file(&target)
        .await
        .map_err(|err| USTBLError(format!("删除旧共享模组失败：{err}")))?;
      result.deleted.push(mod_info.file_name.clone());
    }
    current += 1;
    emit_update_progress(
      &app,
      shared_instance_id,
      current,
      total,
      Some(&mod_info.file_name),
    );
  }

  for mod_info in shared.mods.iter().filter(|item| item.status == "used") {
    assert_mod_filename(&mod_info.file_name)?;
    let target = mods_dir.join(&mod_info.file_name);
    if target.is_file() {
      result.skipped.push(mod_info.file_name.clone());
    } else {
      download_mod(&app, shared_instance_id, mod_info, &target).await?;
      result.downloaded.push(mod_info.file_name.clone());
    }
    current += 1;
    emit_update_progress(
      &app,
      shared_instance_id,
      current,
      total,
      Some(&mod_info.file_name),
    );
  }

  let mut bindings = SharedInstanceBindings::load().unwrap_or_default();
  bindings.0.insert(shared_instance_id, local_instance_id);
  bindings.save()?;
  Ok(result)
}

#[tauri::command]
pub async fn upload_shared_instance_mod(
  app: AppHandle,
  shared_instance_id: u64,
  file_path: PathBuf,
) -> USTBLResult<SharedMod> {
  ensure_minecraft_manager(&app)?;
  let filename = file_path
    .file_name()
    .and_then(|name| name.to_str())
    .ok_or_else(|| USTBLError("无效的模组文件路径".to_string()))?
    .to_string();
  assert_mod_filename(&filename)?;
  let bytes = tokio::fs::read(&file_path)
    .await
    .map_err(|err| USTBLError(format!("读取待上传模组失败：{err}")))?;
  const MAX_MOD_SIZE: usize = 256 * 1024 * 1024;
  if bytes.len() > MAX_MOD_SIZE {
    return Err(USTBLError("模组文件不能超过 256 MiB".to_string()));
  }
  // tauri-plugin-http intentionally does not enable reqwest's multipart feature.
  // Build the tiny one-file multipart body directly so this stays compatible with
  // the launcher's existing HTTP runtime and the API's fixed `file` field.
  let boundary = format!("----USTBL{}", uuid::Uuid::new_v4().simple());
  let mut form = Vec::with_capacity(bytes.len() + filename.len() + 256);
  form.extend_from_slice(
    format!(
      "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/java-archive\r\n\r\n"
    )
    .as_bytes(),
  );
  form.extend_from_slice(&bytes);
  form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
  let client = app.state::<reqwest::Client>();
  let response = client
    .post(format!("{VUSTB_API}/{shared_instance_id}/mods"))
    .header(
      "Content-Type",
      format!("multipart/form-data; boundary={boundary}"),
    )
    .body(form)
    .send()
    .await
    .map_err(|err| USTBLError(format!("上传共享模组失败：{err}")))?;
  if !response.status().is_success() {
    return Err(invalid_response(response));
  }
  response
    .json::<SharedMod>()
    .await
    .map_err(|err| USTBLError(format!("共享实例 API 返回格式错误：{err}")))
}

#[tauri::command]
pub async fn delete_shared_instance_mod(
  app: AppHandle,
  shared_instance_id: u64,
  shared_mod_id: u64,
) -> USTBLResult<SharedMod> {
  ensure_minecraft_manager(&app)?;
  let client = app.state::<reqwest::Client>();
  let response = client
    .delete(format!(
      "{VUSTB_API}/{shared_instance_id}/mods/{shared_mod_id}"
    ))
    .send()
    .await
    .map_err(|err| USTBLError(format!("删除共享模组失败：{err}")))?;
  if !response.status().is_success() {
    return Err(invalid_response(response));
  }
  response
    .json::<SharedMod>()
    .await
    .map_err(|err| USTBLError(format!("共享实例 API 返回格式错误：{err}")))
}

#[cfg(test)]
mod tests {
  use super::{SharedInstance, SharedInstanceDetail};

  #[test]
  fn parses_snake_case_instance_list_and_serializes_camel_case() {
    let instances: Vec<SharedInstance> = serde_json::from_str(
      r#"[{
        "id": 1,
        "name": "Mechanomania-航空学",
        "created_at": "2026-08-20T09:30:08.146761Z",
        "updated_at": "2026-08-20T09:30:08.146761Z"
      }]"#,
    )
    .expect("latest public API list response should deserialize");

    assert_eq!(instances[0].name, "Mechanomania-航空学");
    let frontend_value = serde_json::to_value(&instances[0]).unwrap();
    assert_eq!(frontend_value["createdAt"], "2026-08-20T09:30:08.146761Z");
    assert!(frontend_value.get("created_at").is_none());
  }

  #[test]
  fn parses_snake_case_instance_detail() {
    let detail: SharedInstanceDetail = serde_json::from_str(
      r#"{
        "id": 1,
        "name": "Mechanomania-航空学",
        "created_at": "2026-08-20T09:30:08.146761Z",
        "updated_at": "2026-08-20T09:30:08.146761Z",
        "mods": [{
          "id": 31,
          "file_name": "create-0.5.1.jar",
          "file_size": 15203423,
          "status": "used",
          "created_by_username": "alice",
          "created_at": "2026-08-20T08:05:00Z",
          "deleted_by_username": null,
          "deleted_at": null
        }]
      }"#,
    )
    .expect("latest public API detail response should deserialize");

    assert_eq!(detail.mods[0].file_name, "create-0.5.1.jar");
    assert_eq!(detail.mods[0].file_size, 15_203_423);
    assert_eq!(detail.mods[0].created_by_username.as_deref(), Some("alice"));
  }
}
