use crate::account::models::AccountInfo;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::misc::get_instance_version_path_by_id;
use crate::storage::Storage;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_http::reqwest;
use tokio::io::AsyncReadExt;

const VUSTB_API: &str = "https://www.ustb.world/api/mc-instances";
const SHARED_INSTANCE_BINDINGS_FILE: &str = "ustbl.shared-instance-bindings.json";
const SHARED_INSTANCE_SYNC_STATES_FILE: &str = "ustbl.shared-instance-sync-states.json";
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
  pub folder_id: Option<u64>,
  pub file_name: String,
  pub file_size: u64,
  pub sha256: Option<String>,
  pub status: String,
  pub created_by_username: Option<String>,
  pub created_at: String,
  pub deleted_by_username: Option<String>,
  pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct SharedFolder {
  pub id: u64,
  pub parent_id: Option<u64>,
  pub name: String,
  pub created_at: String,
  pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct SharedInstanceDetail {
  pub id: u64,
  pub name: String,
  pub created_at: String,
  pub updated_at: String,
  #[serde(default)]
  pub folders: Vec<SharedFolder>,
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

#[derive(Debug, Clone, Deserialize)]
struct SharedInstanceLastUpdated {
  last_updated_at: Option<String>,
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SharedInstanceSyncState {
  pub last_updated_at: Option<String>,
  pub binding_prompt_ignored: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SharedInstanceSyncStates(pub HashMap<u64, SharedInstanceSyncState>);

impl Storage for SharedInstanceSyncStates {
  fn file_path() -> PathBuf {
    crate::APP_DATA_DIR
      .get()
      .expect("APP_DATA_DIR initialization failed")
      .join(SHARED_INSTANCE_SYNC_STATES_FILE)
  }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedInstanceStartupNotification {
  pub shared_instance_id: u64,
  pub name: String,
  pub kind: SharedInstanceStartupNotificationKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SharedInstanceStartupNotificationKind {
  Update,
  Bind,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedUpdateResult {
  pub deleted: Vec<String>,
  pub downloaded: Vec<String>,
  pub updated: Vec<String>,
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

async fn get_shared_instance_last_updated(
  app: &AppHandle,
  shared_instance_id: u64,
) -> USTBLResult<Option<String>> {
  let result: SharedInstanceLastUpdated =
    get_json(app, &format!("/{shared_instance_id}/last-updated")).await?;
  Ok(result.last_updated_at)
}

fn remote_update_is_newer(
  remote_last_updated_at: Option<&str>,
  local_last_updated_at: Option<&str>,
) -> USTBLResult<bool> {
  let Some(remote_last_updated_at) = remote_last_updated_at else {
    return Ok(false);
  };
  let Some(local_last_updated_at) = local_last_updated_at else {
    return Ok(true);
  };
  let remote = DateTime::<FixedOffset>::parse_from_rfc3339(remote_last_updated_at)
    .map_err(|err| USTBLError(format!("共享实例 API 返回了无效的更新时间：{err}")))?;
  let local = DateTime::<FixedOffset>::parse_from_rfc3339(local_last_updated_at)
    .map_err(|err| USTBLError(format!("本地共享实例更新时间记录无效：{err}")))?;
  Ok(remote > local)
}

fn clear_shared_instance_binding_prompt_ignored(shared_instance_id: u64) -> USTBLResult<()> {
  let mut states = SharedInstanceSyncStates::load().unwrap_or_default();
  states
    .0
    .entry(shared_instance_id)
    .or_default()
    .binding_prompt_ignored = false;
  states.save()?;
  Ok(())
}

fn assert_file_name(file_name: &str) -> USTBLResult<()> {
  let path = Path::new(file_name);
  let is_filename = path
    .file_name()
    .and_then(|name| name.to_str())
    .is_some_and(|name| name == file_name);
  if !is_filename
    || file_name.is_empty()
    || file_name == "."
    || file_name == ".."
    || file_name.contains('/')
    || file_name.contains('\\')
    || file_name.chars().any(char::is_control)
  {
    return Err(USTBLError("共享实例返回了不安全的文件名".to_string()));
  }
  Ok(())
}

fn assert_folder_name(folder_name: &str) -> USTBLResult<()> {
  assert_file_name(folder_name)
    .map_err(|_| USTBLError("共享实例返回了不安全的文件夹名".to_string()))
}

fn shared_file_relative_path(folders: &[SharedFolder], file: &SharedMod) -> USTBLResult<PathBuf> {
  assert_file_name(&file.file_name)?;
  let folders_by_id = folders
    .iter()
    .map(|folder| (folder.id, folder))
    .collect::<HashMap<_, _>>();
  let mut folder_names = vec![];
  let mut folder_id = file.folder_id;
  let mut visited = HashSet::new();

  while let Some(id) = folder_id {
    if !visited.insert(id) {
      return Err(USTBLError("共享实例文件夹层级存在循环".to_string()));
    }
    let folder = folders_by_id
      .get(&id)
      .ok_or_else(|| USTBLError("共享实例文件引用了不存在的文件夹".to_string()))?;
    assert_folder_name(&folder.name)?;
    folder_names.push(folder.name.as_str());
    folder_id = folder.parent_id;
  }

  folder_names.reverse();
  let mut relative_path = PathBuf::new();
  for name in folder_names {
    relative_path.push(name);
  }
  relative_path.push(&file.file_name);
  Ok(relative_path)
}

fn display_shared_file_path(relative_path: &Path) -> String {
  relative_path.to_string_lossy().replace('\\', "/")
}

fn validate_sha256(sha256: &str) -> USTBLResult<()> {
  let is_lowercase_hex = sha256
    .bytes()
    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
  if sha256.len() != 64 || !is_lowercase_hex {
    return Err(USTBLError("共享实例返回了无效的 SHA-256 摘要".to_string()));
  }
  Ok(())
}

fn sha256_of_bytes(bytes: &[u8]) -> String {
  let mut hasher = Sha256::new();
  hasher.update(bytes);
  format!("{:x}", hasher.finalize())
}

async fn sha256_of_file(file_path: &Path) -> USTBLResult<String> {
  let mut file = tokio::fs::File::open(file_path)
    .await
    .map_err(|err| USTBLError(format!("读取本地共享文件失败：{err}")))?;
  let mut hasher = Sha256::new();
  let mut buffer = [0; 64 * 1024];

  loop {
    let count = file
      .read(&mut buffer)
      .await
      .map_err(|err| USTBLError(format!("读取本地共享文件失败：{err}")))?;
    if count == 0 {
      break;
    }
    hasher.update(&buffer[..count]);
  }

  Ok(format!("{:x}", hasher.finalize()))
}

async fn local_file_matches_shared_checksum(
  file_path: &Path,
  shared_file: &SharedMod,
) -> USTBLResult<bool> {
  let Some(expected_sha256) = shared_file.sha256.as_deref() else {
    return Ok(true);
  };
  validate_sha256(expected_sha256)?;
  Ok(sha256_of_file(file_path).await? == expected_sha256)
}

async fn get_download_request(
  app: &AppHandle,
  instance_id: u64,
  mod_id: u64,
) -> USTBLResult<SharedDownloadRequest> {
  get_json(app, &format!("/{instance_id}/mods/{mod_id}/download")).await
}

async fn download_shared_file(
  app: &AppHandle,
  instance_id: u64,
  mod_info: &SharedMod,
  destination: &Path,
) -> USTBLResult<()> {
  assert_file_name(&mod_info.file_name)?;
  let request_info = get_download_request(app, instance_id, mod_info.id).await?;
  if !request_info.method.eq_ignore_ascii_case("GET") {
    return Err(USTBLError("共享实例返回了不支持的模组下载方法".to_string()));
  }
  assert_file_name(&request_info.name)?;
  if request_info.name != mod_info.file_name {
    return Err(USTBLError("共享实例下载文件名与文件档案不一致".to_string()));
  }

  let client = app.state::<reqwest::Client>();
  let mut request = client.get(request_info.url);
  for (key, value) in request_info.headers {
    request = request.header(key, value);
  }
  let response = request
    .send()
    .await
    .map_err(|err| USTBLError(format!("下载共享文件失败：{err}")))?
    .error_for_status()
    .map_err(|err| USTBLError(format!("下载共享文件失败：{err}")))?;
  let bytes = response
    .bytes()
    .await
    .map_err(|err| USTBLError(format!("读取共享文件失败：{err}")))?;
  if bytes.len() as u64 != mod_info.file_size {
    return Err(USTBLError(format!(
      "共享文件 {} 的大小校验失败",
      mod_info.file_name
    )));
  }
  if let Some(expected_sha256) = mod_info.sha256.as_deref() {
    validate_sha256(expected_sha256)?;
    if sha256_of_bytes(&bytes) != expected_sha256 {
      return Err(USTBLError(format!(
        "共享文件 {} 的 SHA-256 校验失败",
        mod_info.file_name
      )));
    }
  }

  tokio::fs::create_dir_all(destination.parent().unwrap_or(destination))
    .await
    .map_err(|err| USTBLError(format!("创建共享文件目录失败：{err}")))?;
  let partial = destination.with_file_name(format!(".{}.ustbl-downloading", mod_info.file_name));
  tokio::fs::write(&partial, bytes)
    .await
    .map_err(|err| USTBLError(format!("写入共享文件失败：{err}")))?;
  tokio::fs::rename(&partial, destination)
    .await
    .map_err(|err| USTBLError(format!("完成共享文件下载失败：{err}")))
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
pub async fn retrieve_shared_instance_startup_notifications(
  app: AppHandle,
) -> USTBLResult<Vec<SharedInstanceStartupNotification>> {
  let instances: Vec<SharedInstance> = get_json(&app, "").await?;
  let bindings = SharedInstanceBindings::load().unwrap_or_default();
  let states = SharedInstanceSyncStates::load().unwrap_or_default();
  let mut notifications = vec![];

  for instance in instances {
    let has_usable_binding = bindings
      .0
      .get(&instance.id)
      .is_some_and(|local_instance_id| {
        get_instance_version_path_by_id(&app, local_instance_id).is_some()
      });
    if !has_usable_binding {
      if !states
        .0
        .get(&instance.id)
        .is_some_and(|state| state.binding_prompt_ignored)
      {
        notifications.push(SharedInstanceStartupNotification {
          shared_instance_id: instance.id,
          name: instance.name,
          kind: SharedInstanceStartupNotificationKind::Bind,
        });
      }
      continue;
    }

    let remote_last_updated_at = match get_shared_instance_last_updated(&app, instance.id).await {
      Ok(last_updated_at) => last_updated_at,
      Err(err) => {
        log::warn!(
          "Failed to retrieve latest update time for shared instance {}: {err:?}",
          instance.id
        );
        continue;
      }
    };
    let local_last_updated_at = states
      .0
      .get(&instance.id)
      .and_then(|state| state.last_updated_at.as_deref());
    match remote_update_is_newer(remote_last_updated_at.as_deref(), local_last_updated_at) {
      Ok(true) => notifications.push(SharedInstanceStartupNotification {
        shared_instance_id: instance.id,
        name: instance.name,
        kind: SharedInstanceStartupNotificationKind::Update,
      }),
      Ok(false) => {}
      Err(err) => log::warn!(
        "Unable to compare update times for shared instance {}: {err:?}",
        instance.id
      ),
    }
  }

  Ok(notifications)
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
  clear_shared_instance_binding_prompt_ignored(shared_instance_id)?;
  Ok(())
}

#[tauri::command]
pub fn ignore_shared_instance_binding_prompt(shared_instance_id: u64) -> USTBLResult<()> {
  let mut states = SharedInstanceSyncStates::load().unwrap_or_default();
  states
    .0
    .entry(shared_instance_id)
    .or_default()
    .binding_prompt_ignored = true;
  states.save()?;
  Ok(())
}

#[tauri::command]
pub async fn update_shared_instance(
  app: AppHandle,
  shared_instance_id: u64,
  local_instance_id: String,
) -> USTBLResult<SharedUpdateResult> {
  let instance_root = get_instance_version_path_by_id(&app, &local_instance_id)
    .ok_or_else(|| USTBLError("未找到已绑定的本地实例".to_string()))?;
  // Save the update time observed before reading the file list. If an
  // administrator changes the shared instance while this sync is running,
  // the next launcher startup will still notice the newer timestamp.
  let sync_last_updated_at = get_shared_instance_last_updated(&app, shared_instance_id).await;
  let shared: SharedInstanceDetail = get_json(&app, &format!("/{shared_instance_id}")).await?;
  let mut result = SharedUpdateResult {
    deleted: vec![],
    downloaded: vec![],
    updated: vec![],
    skipped: vec![],
  };
  let total = shared
    .mods
    .iter()
    .filter(|item| matches!(item.status.as_str(), "deleted" | "used"))
    .count();
  let mut current = 0;
  emit_update_progress(&app, shared_instance_id, current, total, None);

  for mod_info in shared.mods.iter().filter(|item| item.status == "deleted") {
    let relative_path = shared_file_relative_path(&shared.folders, mod_info)?;
    let display_path = display_shared_file_path(&relative_path);
    let target = instance_root.join(relative_path);
    if target.is_file() {
      tokio::fs::remove_file(&target)
        .await
        .map_err(|err| USTBLError(format!("删除旧共享文件失败：{err}")))?;
      result.deleted.push(display_path.clone());
    }
    current += 1;
    emit_update_progress(
      &app,
      shared_instance_id,
      current,
      total,
      Some(&display_path),
    );
  }

  for mod_info in shared.mods.iter().filter(|item| item.status == "used") {
    let relative_path = shared_file_relative_path(&shared.folders, mod_info)?;
    let display_path = display_shared_file_path(&relative_path);
    let target = instance_root.join(relative_path);
    if target.is_file() && local_file_matches_shared_checksum(&target, mod_info).await? {
      result.skipped.push(display_path.clone());
    } else {
      if target.exists() {
        if target.is_file() {
          tokio::fs::remove_file(&target)
            .await
            .map_err(|err| USTBLError(format!("删除待更新共享文件失败：{err}")))?;
          download_shared_file(&app, shared_instance_id, mod_info, &target).await?;
          result.updated.push(display_path.clone());
        } else {
          return Err(USTBLError(format!(
            "无法下载共享文件 {display_path}：本地同名路径不是文件"
          )));
        }
      } else {
        download_shared_file(&app, shared_instance_id, mod_info, &target).await?;
        result.downloaded.push(display_path.clone());
      }
    }
    current += 1;
    emit_update_progress(
      &app,
      shared_instance_id,
      current,
      total,
      Some(&display_path),
    );
  }

  let mut bindings = SharedInstanceBindings::load().unwrap_or_default();
  bindings.0.insert(shared_instance_id, local_instance_id);
  bindings.save()?;

  let mut states = SharedInstanceSyncStates::load().unwrap_or_default();
  let state = states.0.entry(shared_instance_id).or_default();
  state.binding_prompt_ignored = false;
  match sync_last_updated_at {
    Ok(last_updated_at) => state.last_updated_at = last_updated_at,
    Err(err) => log::warn!(
      "Shared instance {shared_instance_id} synced, but its update time could not be recorded: {err:?}"
    ),
  }
  states.save()?;
  Ok(result)
}

async fn send_shared_file_upload(
  app: AppHandle,
  method: reqwest::Method,
  endpoint: String,
  file_path: PathBuf,
  folder_id: Option<u64>,
) -> USTBLResult<SharedMod> {
  let operation = if method == reqwest::Method::POST {
    "上传"
  } else {
    "更新"
  };
  let filename = file_path
    .file_name()
    .and_then(|name| name.to_str())
    .ok_or_else(|| USTBLError("无效的共享文件路径".to_string()))?
    .to_string();
  assert_file_name(&filename)?;
  let bytes = tokio::fs::read(&file_path)
    .await
    .map_err(|err| USTBLError(format!("读取待{operation}共享文件失败：{err}")))?;
  const MAX_FILE_SIZE: usize = 100 * 1024 * 1024;
  if bytes.len() > MAX_FILE_SIZE {
    return Err(USTBLError("共享文件不能超过 100 MiB".to_string()));
  }
  // tauri-plugin-http intentionally does not enable reqwest's multipart feature.
  // Build the tiny one-file multipart body directly so this stays compatible with
  // the launcher's existing HTTP runtime and the API's fixed `file` field.
  let boundary = format!("----USTBL{}", uuid::Uuid::new_v4().simple());
  let mut form = Vec::with_capacity(bytes.len() + filename.len() + 256);
  if let Some(folder_id) = folder_id {
    form.extend_from_slice(
      format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"folder_id\"\r\n\r\n{folder_id}\r\n"
      )
      .as_bytes(),
    );
  }
  form.extend_from_slice(
    format!(
      "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .as_bytes(),
  );
  form.extend_from_slice(&bytes);
  form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
  let client = app.state::<reqwest::Client>();
  let response = client
    .request(method, endpoint)
    .header(
      "Content-Type",
      format!("multipart/form-data; boundary={boundary}"),
    )
    .body(form)
    .send()
    .await
    .map_err(|err| USTBLError(format!("{operation}共享文件失败：{err}")))?;
  if !response.status().is_success() {
    return Err(invalid_response(response));
  }
  response
    .json::<SharedMod>()
    .await
    .map_err(|err| USTBLError(format!("共享实例 API 返回格式错误：{err}")))
}

#[tauri::command]
pub async fn upload_shared_instance_mod(
  app: AppHandle,
  shared_instance_id: u64,
  file_path: PathBuf,
  folder_id: Option<u64>,
) -> USTBLResult<SharedMod> {
  ensure_minecraft_manager(&app)?;
  send_shared_file_upload(
    app,
    reqwest::Method::POST,
    format!("{VUSTB_API}/{shared_instance_id}/mods"),
    file_path,
    folder_id,
  )
  .await
}

#[tauri::command]
pub async fn update_shared_instance_mod(
  app: AppHandle,
  shared_instance_id: u64,
  shared_mod_id: u64,
  file_path: PathBuf,
) -> USTBLResult<SharedMod> {
  ensure_minecraft_manager(&app)?;
  send_shared_file_upload(
    app,
    reqwest::Method::PUT,
    format!("{VUSTB_API}/{shared_instance_id}/mods/{shared_mod_id}"),
    file_path,
    None,
  )
  .await
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
  use super::{
    local_file_matches_shared_checksum, remote_update_is_newer, sha256_of_bytes,
    shared_file_relative_path, validate_sha256, SharedInstance, SharedInstanceDetail, SharedMod,
  };
  use std::path::PathBuf;

  fn shared_mod_with_checksum(sha256: Option<&str>) -> SharedMod {
    SharedMod {
      id: 1,
      folder_id: None,
      file_name: "example.txt".to_string(),
      file_size: 0,
      sha256: sha256.map(str::to_string),
      status: "used".to_string(),
      created_by_username: None,
      created_at: "2026-08-22T00:00:00Z".to_string(),
      deleted_by_username: None,
      deleted_at: None,
    }
  }

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
      "folders": [{
        "id": 7,
        "parent_id": null,
        "name": "config",
        "created_at": "2026-08-20T08:05:00Z",
        "updated_at": "2026-08-20T08:05:00Z"
      }, {
        "id": 8,
        "parent_id": 7,
        "name": "client",
        "created_at": "2026-08-20T08:05:00Z",
        "updated_at": "2026-08-20T08:05:00Z"
      }],
      "mods": [{
        "id": 31,
        "folder_id": 8,
        "file_name": "create-0.5.1.jar",
        "file_size": 15203423,
        "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
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
    assert_eq!(
      detail.mods[0].sha256.as_deref(),
      Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
    assert_eq!(detail.mods[0].created_by_username.as_deref(), Some("alice"));
    assert_eq!(detail.folders.len(), 2);
    assert_eq!(
      shared_file_relative_path(&detail.folders, &detail.mods[0]).unwrap(),
      PathBuf::from("config")
        .join("client")
        .join("create-0.5.1.jar")
    );
  }

  #[test]
  fn compares_last_updated_timestamps_by_time_not_text_order() {
    assert!(
      remote_update_is_newer(Some("2026-08-30T10:34:55.1Z"), Some("2026-08-30T10:34:55Z"),)
        .unwrap()
    );
    assert!(
      !remote_update_is_newer(Some("2026-08-30T10:34:55Z"), Some("2026-08-30T10:34:55.1Z"),)
        .unwrap()
    );
    assert!(remote_update_is_newer(Some("2026-08-30T10:34:55Z"), None).unwrap());
    assert!(!remote_update_is_newer(None, None).unwrap());
  }

  #[test]
  fn rejects_unsafe_or_cyclic_folder_paths() {
    let detail: SharedInstanceDetail = serde_json::from_str(
      r#"{
        "id": 1,
        "name": "Unsafe test",
        "created_at": "2026-08-20T09:30:08.146761Z",
        "updated_at": "2026-08-20T09:30:08.146761Z",
        "folders": [{
          "id": 7,
          "parent_id": 8,
          "name": "config",
          "created_at": "2026-08-20T08:05:00Z",
          "updated_at": "2026-08-20T08:05:00Z"
        }, {
          "id": 8,
          "parent_id": 7,
          "name": "client",
          "created_at": "2026-08-20T08:05:00Z",
          "updated_at": "2026-08-20T08:05:00Z"
        }],
        "mods": [{
          "id": 31,
          "folder_id": 7,
          "file_name": "settings.toml",
          "file_size": 1,
          "sha256": null,
          "status": "used",
          "created_at": "2026-08-20T08:05:00Z"
        }]
      }"#,
    )
    .unwrap();

    assert!(shared_file_relative_path(&detail.folders, &detail.mods[0]).is_err());
  }

  #[test]
  fn validates_lowercase_sha256_values() {
    let valid_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert!(validate_sha256(valid_sha256).is_ok());
    assert!(
      validate_sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85").is_err()
    );
    assert!(
      validate_sha256("E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855").is_err()
    );
    assert_eq!(
      sha256_of_bytes(b"abc"),
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
  }

  #[tokio::test]
  async fn compares_local_file_checksum_and_preserves_legacy_records() {
    let file_path =
      std::env::temp_dir().join(format!("ustbl-shared-test-{}.txt", uuid::Uuid::new_v4()));
    tokio::fs::write(&file_path, b"abc").await.unwrap();

    let matching = shared_mod_with_checksum(Some(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    ));
    let mismatching = shared_mod_with_checksum(Some(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ));
    let legacy = shared_mod_with_checksum(None);

    assert!(local_file_matches_shared_checksum(&file_path, &matching)
      .await
      .unwrap());
    assert!(
      !local_file_matches_shared_checksum(&file_path, &mismatching)
        .await
        .unwrap()
    );
    assert!(local_file_matches_shared_checksum(&file_path, &legacy)
      .await
      .unwrap());

    tokio::fs::remove_file(&file_path).await.unwrap();
  }
}
