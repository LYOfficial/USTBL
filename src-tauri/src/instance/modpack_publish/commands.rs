//! Publishing an instance to the campus modpack platform.

use super::components::plan_components;
use super::job::{is_running, PublishJob};
use super::manifest::{plan_files, LocalFile};
use super::pack::{build_pack, verify_pack};
use super::record::{
  jobs_dir, paths, JobRecord, Phase, RecordedFile, RecordedObject, RecordedPack, RecordedUpload,
  RECORD_SCHEMA,
};
use crate::account::helpers::vustb;
use crate::account::models::AccountInfo;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::misc::{get_instance_game_config, get_instance_subdir_paths};
use crate::instance::helpers::modpack::baseline::{
  canonical_loader_version, components_of_mmc_pack, hash_file, BaselineFile, BaselineLoader,
  ModpackBaseline,
};
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::helpers::modpack::multimc::snapshot::{
  is_component_instance, ComponentSnapshot,
};
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceOrigin, InstanceSubdirType, OriginPlatform,
};
use crate::instance::modpack_update::commands::store_instance;
use crate::instance::modpack_update::diff::FileKey;
use crate::instance::modpack_update::sources::{loader_slug, CAMPUS_API};
use crate::utils::web::build_part_upload_client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// Hashes sent per request when asking which the platform lacks.
const MISSING_BATCH: usize = 5000;
/// Part concurrency used on resume, when the record (§9.1) carries none and
/// there is no fresh `POST /versions` response to read it from (§14.1).
pub(super) const DEFAULT_PART_CONCURRENCY: usize = 3;

/// The UI hides publishing from other accounts, but the role is checked here too;
/// the platform authenticates the bearer token again.
fn ensure_manager(app: &AppHandle) -> USTBLResult<()> {
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
    Err(USTBLError("当前像素北科账号没有整合包管理权限".to_string()))
  }
}

/// Deleting a whole modpack is for administrators; the platform checks again.
fn ensure_admin(app: &AppHandle) -> USTBLResult<()> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let account_state = binding.lock()?;
  let user_group = account_state
    .vustb_account
    .as_ref()
    .map(|account| account.user_group.as_str())
    .ok_or_else(|| USTBLError("请先登录像素北科账号".to_string()))?;
  if matches!(user_group, "super_admin" | "admin") {
    Ok(())
  } else {
    Err(USTBLError("只有管理员可以删除校内整合包".to_string()))
  }
}

const PROGRESS_EVENT: &str = "modpack-publish:progress";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct CampusLine {
  pub id: i64,
  pub game_version: String,
  pub loader_type: String,
  pub loader_version: String,
  pub version_number: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct CampusProject {
  pub id: i64,
  pub slug: String,
  pub name: String,
  pub summary: String,
  pub lines: Vec<CampusLine>,
}

fn campus_platform() -> String {
  "campus".into()
}

/// What a recommend slot currently points at: a campus line, or a project on
/// another platform (then only `project_ref` is set).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct CampusRecommendation {
  pub slot: String,
  #[serde(default = "campus_platform")]
  pub platform: String,
  #[serde(default)]
  pub project: Option<CampusProject>,
  #[serde(default, rename(serialize = "line"))]
  pub version: Option<CampusLine>,
  #[serde(default)]
  pub project_ref: Option<String>,
  /// The line of that project the launcher installs.
  #[serde(default)]
  pub game_version: Option<String>,
  #[serde(default)]
  pub loader_type: Option<String>,
}

/// The upload is under way; the task group shows how it goes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishStarted {
  pub group: String,
  pub job_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishOutcome {
  pub version_id: i64,
  /// Paths left out on purpose (disabled mods, launcher files).
  pub skipped: Vec<String>,
  /// What happened to the instance's own record of the modpack.
  pub adoption: Adoption,
}

/// Whether the published version became the instance's own baseline. The
/// instance only follows what it provably matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Adoption {
  /// The instance now follows the published version.
  Followed,
  /// The instance follows another modpack and is left that way.
  KeptOther,
  /// The instance shares its game directory, so it cannot own the files.
  NotIsolated,
  /// Files changed while publishing; the next check shows the difference.
  LocalChanged,
  /// The release went out but writing the instance's record failed.
  RecordFailed,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum Stage {
  Hashing,
  Packing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
  instance_id: String,
  stage: Stage,
  done: usize,
  total: usize,
}

fn report(app: &AppHandle, instance_id: &str, stage: Stage, done: usize, total: usize) {
  let _ = app.emit_to(
    "main",
    PROGRESS_EVENT,
    Progress {
      instance_id: instance_id.to_string(),
      stage,
      done,
      total,
    },
  );
}

/// The server explains refusals in `detail`; pass that on instead of a bare
/// status code. Conflicts also carry a machine readable `code`, and some
/// (such as `UPLOAD_RESTARTED`) carry further fields next to `code`/`message`.
pub(super) async fn failure_detail(response: reqwest::Response) -> (Option<Value>, USTBLError) {
  let status = response.status();
  let body = response.json::<Value>().await.ok();
  let detail = body.and_then(|body| body.get("detail").cloned());
  let message = match &detail {
    Some(Value::String(text)) => Some(text.clone()),
    Some(other) => other
      .get("message")
      .and_then(Value::as_str)
      .map(str::to_string),
    None => None,
  };
  (
    detail,
    USTBLError(message.unwrap_or_else(|| format!("HTTP {status}"))),
  )
}

pub(super) async fn failure_code(response: reqwest::Response) -> (Option<String>, USTBLError) {
  let (detail, error) = failure_detail(response).await;
  let code = detail
    .as_ref()
    .and_then(|detail| detail.get("code"))
    .and_then(Value::as_str)
    .map(str::to_string);
  (code, error)
}

pub(super) async fn failure(response: reqwest::Response) -> USTBLError {
  failure_code(response).await.1
}

pub(super) async fn authed_json(
  app: &AppHandle,
  method: reqwest::Method,
  path: &str,
  body: Option<Value>,
) -> USTBLResult<reqwest::Response> {
  let url = format!("{CAMPUS_API}{path}");
  vustb::send_authenticated(app, |client, token| {
    let request = client.request(method.clone(), &url).bearer_auth(token);
    match &body {
      Some(body) => request.json(body),
      None => request,
    }
  })
  .await
}

pub(super) fn instance_of(app: &AppHandle, instance_id: &str) -> USTBLResult<Instance> {
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let state = binding.lock()?;
  Ok(
    state
      .get(instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone(),
  )
}

#[tauri::command]
pub async fn list_campus_modpack_projects(app: AppHandle) -> USTBLResult<Vec<CampusProject>> {
  let client = app.state::<reqwest::Client>();
  let response = client.get(format!("{CAMPUS_API}/projects")).send().await?;
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  Ok(response.json().await?)
}

/// Remove a campus modpack with every line and version from the platform.
/// Local instances keep their files; their next update check reports the line
/// as gone.
#[tauri::command]
pub async fn delete_campus_modpack_project(app: AppHandle, project: String) -> USTBLResult<()> {
  ensure_admin(&app)?;
  let response = authed_json(
    &app,
    reqwest::Method::DELETE,
    &format!("/projects/{}", urlencoding::encode(&project)),
    None,
  )
  .await?;
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  Ok(())
}

fn hash_selection(root: &Path, selected: &[String]) -> USTBLResult<Vec<LocalFile>> {
  selected
    .iter()
    .map(|path| {
      safe_relative(path)?;
      let (sha1, size) = hash_file(&root.join(path))?;
      Ok(LocalFile {
        path: path.clone(),
        key: FileKey { sha1, size },
      })
    })
    .collect()
}

/// Paths the line's current release already lists, so the publish dialog can
/// keep them selected. Empty when the project has no release on this line.
#[tauri::command]
pub async fn fetch_campus_modpack_previous_paths(
  app: AppHandle,
  instance_id: String,
  project: String,
) -> USTBLResult<Vec<String>> {
  let valid = !project.is_empty()
    && project
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
  if !valid {
    return Err(InstanceError::ModpackUpdateUnsupported.into());
  }
  let instance = instance_of(&app, &instance_id)?;
  let loader_type =
    loader_slug(instance.mod_loader.loader_type).ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let response = app
    .state::<reqwest::Client>()
    .get(format!(
      "{CAMPUS_API}/projects/{project}/lines/{}/{loader_type}",
      instance.version
    ))
    .send()
    .await?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Ok(Vec::new());
  }
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  let body: Value = response.json().await?;
  Ok(
    body["manifest"]["files"]
      .as_array()
      .into_iter()
      .flatten()
      .filter_map(|file| file["path"].as_str().map(str::to_string))
      .collect(),
  )
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewProject {
  pub name: String,
  #[serde(default)]
  pub summary: String,
}

/// `None` when nothing is recommended in the slot (or the slot does not exist).
#[tauri::command]
pub async fn fetch_campus_modpack_recommendation(
  app: AppHandle,
  slot: String,
) -> USTBLResult<Option<CampusRecommendation>> {
  let client = app.state::<reqwest::Client>();
  let response = client
    .get(format!("{CAMPUS_API}/recommend/{slot}"))
    .send()
    .await?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Ok(None);
  }
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  Ok(Some(response.json().await?))
}

/// The hashes among `hashes` the platform holds no content for.
async fn find_missing(app: &AppHandle, hashes: &[String]) -> USTBLResult<HashSet<String>> {
  let mut missing = HashSet::new();
  for batch in hashes.chunks(MISSING_BATCH) {
    let response = authed_json(
      app,
      reqwest::Method::POST,
      "/missing",
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
  Ok(missing)
}

/// Publish the selected files of an instance as the new version of its line:
/// either of an existing project or of a new one the platform names.
#[tauri::command]
pub async fn publish_campus_modpack(
  app: AppHandle,
  instance_id: String,
  project: Option<String>,
  new_project: Option<NewProject>,
  version_number: String,
  changelog: String,
  files: Vec<String>,
) -> USTBLResult<PublishStarted> {
  ensure_manager(&app)?;
  if project.is_some() == new_project.is_some() {
    return Err(InstanceError::ModpackUpdateUnsupported.into());
  }
  if let Some(project) = &project {
    if !is_slug(project) {
      return Err(InstanceError::ModpackUpdateUnsupported.into());
    }
  }
  let job_id = uuid::Uuid::new_v4().simple().to_string();
  let dir = jobs_dir();
  let prepared = prepare(
    &app,
    &instance_id,
    &job_id,
    &dir,
    project,
    new_project,
    version_number,
    changelog,
    files,
  )
  .await;
  let (record, concurrency) = match prepared {
    Ok(result) => result,
    Err(error) => {
      JobRecord::remove(&dir, &job_id);
      return Err(error);
    }
  };

  // A client of its own, with none of the launcher's auth or redirect
  // behavior, since it talks straight to the cloud drive's signed URLs.
  let uploader = build_part_upload_client(&app);
  let version_id = record.version_id;
  // From here on the upload runs in the background, on the task list.
  let started = PublishJob {
    app: app.clone(),
    uploader,
    record,
    // §14.1: a fresh publish's first upload round uses `POST /versions`'s
    // own concurrency; `upload_pack` takes it for that round and falls
    // back to `GET /uploads/{id}`'s value on every later one, same as a
    // resumed job does from its very first round.
    concurrency: Some(concurrency),
  }
  .start();
  match started {
    Ok(group) => Ok(PublishStarted { group, job_id }),
    Err(error) => {
      let _ = authed_json(
        &app,
        reqwest::Method::DELETE,
        &format!("/versions/{version_id}"),
        None,
      )
      .await;
      JobRecord::remove(&dir, &job_id);
      Err(error)
    }
  }
}

fn is_slug(value: &str) -> bool {
  !value.is_empty()
    && value
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Everything before the background part: scan, ask what the platform lacks,
/// build the delta pack and open the version. Leaves the record on disk.
#[allow(clippy::too_many_arguments)]
async fn prepare(
  app: &AppHandle,
  instance_id: &str,
  job_id: &str,
  dir: &Path,
  project: Option<String>,
  new_project: Option<NewProject>,
  version_number: String,
  changelog: String,
  files: Vec<String>,
) -> USTBLResult<(JobRecord, usize)> {
  let instance = instance_of(app, instance_id)?;
  let root = instance.version_path.clone();
  let loader_type =
    loader_slug(instance.mod_loader.loader_type).ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let job_paths = paths(dir, job_id)?;

  report(app, instance_id, Stage::Hashing, 0, files.len());
  let selected = files.clone();
  let hash_root = root.clone();
  let hashed = tokio::task::spawn_blocking(move || hash_selection(&hash_root, &selected))
    .await
    .map_err(|e| USTBLError(e.to_string()))??;
  report(app, instance_id, Stage::Hashing, files.len(), files.len());
  let mut plan = plan_files(hashed, &instance.name);
  if plan.files.is_empty() {
    return Err(USTBLError("没有可发布的文件".into()));
  }
  if is_component_instance(&root) {
    let snapshot = ComponentSnapshot::load(&root)
      .await
      .map_err(|_| InstanceError::ModpackExportProfileMissing)?
      .ok_or(InstanceError::ModpackExportProfileMissing)?;
    let libraries_dir =
      get_instance_subdir_paths(app, &instance, &[&InstanceSubdirType::Libraries])
        .ok_or(InstanceError::InstanceNotFoundByID)?
        .remove(0);
    // The patches are written here and stay until the job is over.
    let work = job_paths.work.clone();
    plan.components = Some(
      tokio::task::spawn_blocking(move || plan_components(&snapshot, &libraries_dir, &work))
        .await
        .map_err(|e| USTBLError(e.to_string()))??,
    );
  }

  // The published version of this line the new one replaces.
  let parent_version_id = match &project {
    None => None,
    Some(project) => {
      let current = app
        .state::<reqwest::Client>()
        .get(format!(
          "{CAMPUS_API}/projects/{project}/lines/{}/{loader_type}",
          instance.version
        ))
        .send()
        .await?;
      match current.status() {
        reqwest::StatusCode::NOT_FOUND => None,
        status if status.is_success() => current.json::<Value>().await?["id"].as_i64(),
        _ => return Err(failure(current).await),
      }
    }
  };

  // Only what the platform does not hold goes into the pack.
  let all_objects = plan.objects(&root);
  let hashes: Vec<String> = all_objects.keys().cloned().collect();
  let missing = find_missing(app, &hashes).await?;
  let needed: Vec<_> = all_objects
    .values()
    .filter(|object| missing.contains(&object.sha1))
    .cloned()
    .collect();
  let pack = if needed.is_empty() {
    None
  } else {
    report(app, instance_id, Stage::Packing, 0, 1);
    fs::create_dir_all(dir)?;
    let (objects, target) = (needed.clone(), job_paths.pack.clone());
    let built = tokio::task::spawn_blocking(move || build_pack(&objects, &target))
      .await
      .map_err(|e| USTBLError(e.to_string()))??;
    // §13.2: the self-check runs before anything is sent to the platform, so
    // a bad local pack never opens a version or an upload session that would
    // then need to be torn down again (no `POST /versions` has happened yet
    // at this point, so there is nothing on the platform to clean up).
    let (check_path, check_sha1) = (job_paths.pack.clone(), built.sha1.clone());
    tokio::task::spawn_blocking(move || verify_pack(&check_path, &check_sha1))
      .await
      .map_err(|e| USTBLError(e.to_string()))??;
    report(app, instance_id, Stage::Packing, 1, 1);
    Some(RecordedPack {
      sha1: built.sha1,
      size: built.size,
    })
  };

  let created = authed_json(
    app,
    reqwest::Method::POST,
    "/versions",
    Some(json!({
      "project": project,
      "new_project": new_project.as_ref().map(|new| json!({ "name": new.name, "summary": new.summary })),
      "game_version": instance.version,
      "loader_type": loader_type,
      "loader_version": canonical_loader_version(
        instance.mod_loader.loader_type,
        &instance.mod_loader.version
      ),
      "version_number": version_number,
      "changelog": changelog,
      "parent_version_id": parent_version_id,
      "manifest": plan.manifest(),
      "pack": pack.as_ref().map(|pack| json!({ "sha1": pack.sha1, "size": pack.size })),
    })),
  )
  .await?;
  if !created.status().is_success() {
    let (code, error) = failure_code(created).await;
    return Err(if code.as_deref() == Some("PARENT_MISMATCH") {
      InstanceError::ModpackPublishStale.into()
    } else {
      error
    });
  }
  let body: Value = created.json().await?;
  let version_id = body["id"]
    .as_i64()
    .ok_or_else(|| USTBLError("平台返回了无法识别的版本".into()))?;
  let upload = body["upload"].as_object().and_then(|upload| {
    Some(RecordedUpload {
      id: upload.get("id")?.as_str()?.to_string(),
      part_size: upload.get("part_size")?.as_u64()?,
      part_count: u32::try_from(upload.get("part_count")?.as_u64()?).ok()?,
      generation: u32::try_from(upload.get("generation")?.as_u64()?).ok()?,
    })
  });
  // §14.1: the server's call, fresh for this run; not part of the persisted
  // record (§9.1), so a resumed job falls back to the suggested default.
  let concurrency = body["upload"]["concurrency"]
    .as_u64()
    .map_or(DEFAULT_PART_CONCURRENCY, |n| n as usize);
  let slug = body["project_slug"]
    .as_str()
    .or(project.as_deref())
    .unwrap_or_default()
    .to_string();

  let record = JobRecord {
    schema: RECORD_SCHEMA,
    id: job_id.to_string(),
    phase: Phase::Uploading,
    error: None,
    created_at: chrono::Utc::now().timestamp_millis(),
    instance_id: instance_id.to_string(),
    instance_name: instance.name.clone(),
    project_slug: slug,
    version_id,
    version_number,
    files: plan
      .files
      .iter()
      .map(|file| RecordedFile {
        path: file.path.clone(),
        sha1: file.key.sha1.clone(),
        size: file.key.size,
      })
      .collect(),
    mmc_pack: plan.components.as_ref().map(|c| c.mmc_pack.clone()),
    skipped: plan.skipped.clone(),
    objects: needed.iter().map(RecordedObject::from).collect(),
    pack,
    upload,
  };
  if pack_without_upload(&record) {
    let _ = authed_json(
      app,
      reqwest::Method::DELETE,
      &format!("/versions/{version_id}"),
      None,
    )
    .await;
    return Err(USTBLError("平台没有为增量包开启上传".into()));
  }
  if let Err(error) = record.save(dir) {
    let _ = authed_json(
      app,
      reqwest::Method::DELETE,
      &format!("/versions/{version_id}"),
      None,
    )
    .await;
    return Err(error);
  }
  Ok((record, concurrency))
}

/// A pack was promised but the platform opened no session for it.
fn pack_without_upload(record: &JobRecord) -> bool {
  record.pack.is_some() && record.upload.is_none()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnfinishedPublish {
  pub job_id: String,
  pub instance_id: String,
  pub instance_name: String,
  pub project_slug: String,
  pub version_number: String,
  pub phase: Phase,
  pub error: Option<String>,
  /// §3.2.3: whether "继续" may be offered; a terminal failure only gets
  /// "放弃" (v3 §8.4).
  pub resumable: bool,
  pub created_at: i64,
}

/// Local publish records that need the user's attention (§3.2.3): an
/// interrupted upload, a failed publish, or a platform-side publish whose
/// watch is not on the task list right now. A record with a task group
/// running shows there instead and never appears here.
#[tauri::command]
pub fn list_campus_modpack_publish_jobs() -> Vec<UnfinishedPublish> {
  JobRecord::list(&jobs_dir())
    .into_iter()
    .filter(|record| !is_running(&record.id))
    .map(|record| UnfinishedPublish {
      resumable: record.resumable(),
      job_id: record.id,
      instance_id: record.instance_id,
      instance_name: record.instance_name,
      project_slug: record.project_slug,
      version_number: record.version_number,
      phase: record.phase,
      error: record.error,
      created_at: record.created_at,
    })
    .collect()
}

/// Continue an interrupted publish with what the platform already has.
#[tauri::command]
pub async fn resume_campus_modpack_publish(
  app: AppHandle,
  job_id: String,
) -> USTBLResult<PublishStarted> {
  ensure_manager(&app)?;
  let record = JobRecord::load(&jobs_dir(), &job_id)?;
  let uploader = build_part_upload_client(&app);
  let group = PublishJob {
    app,
    uploader,
    record,
    // §9.1: the record never carries a concurrency; `upload_pack`
    // reconciles and takes `GET /uploads/{id}`'s value for every round of
    // a resumed job instead.
    concurrency: None,
  }
  .start()?;
  Ok(PublishStarted { group, job_id })
}

/// Give an unfinished publish up: the version is dropped on the platform and
/// the local leftovers are removed.
#[tauri::command]
pub async fn discard_campus_modpack_publish(app: AppHandle, job_id: String) -> USTBLResult<()> {
  ensure_manager(&app)?;
  if is_running(&job_id) {
    return Err(USTBLError("任务正在进行，请先在任务列表中取消".into()));
  }
  let dir = jobs_dir();
  let record = JobRecord::load(&dir, &job_id)?;
  let response = authed_json(
    &app,
    reqwest::Method::DELETE,
    &format!("/versions/{}", record.version_id),
    None,
  )
  .await?;
  if response.status() == reqwest::StatusCode::CONFLICT {
    let state = authed_json(
      &app,
      reqwest::Method::GET,
      &format!("/versions/{}/status", record.version_id),
      None,
    )
    .await?;
    if state.status().is_success() && state.json::<Value>().await?["status"] == "published" {
      // Already out: the card comes back for the adoption, then the record
      // is dropped by the platform stage itself.
      PublishJob {
        app: app.clone(),
        uploader: build_part_upload_client(&app),
        record,
        // Only platform processing is watched here, never a part upload.
        concurrency: None,
      }
      .start()?;
      return Ok(());
    }
  }
  // Already gone counts as dropped; a version being processed cannot be.
  if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
    return Err(failure(response).await);
  }
  JobRecord::remove(&dir, &job_id);
  Ok(())
}

/// Fast-forward: after publishing, the instance holds exactly what the new
/// version lists. When that still holds, record it as the instance's baseline,
/// so the first check does not offer the publisher their own release.
pub(super) async fn adopt_published(
  app: &AppHandle,
  instance: &Instance,
  project: &str,
  version_id: i64,
  version_number: &str,
  files: &[LocalFile],
  mmc_pack: Option<&Value>,
) -> Adoption {
  match try_adopt(
    app,
    instance,
    project,
    version_id,
    version_number,
    files,
    mmc_pack,
  )
  .await
  {
    Ok(adoption) => adoption,
    Err(error) => {
      // The release itself succeeded; a failed record only means the next
      // check compares the instance with it the ordinary way.
      log::warn!("Could not record the published version: {error:?}");
      Adoption::RecordFailed
    }
  }
}

fn may_follow(
  origin: Option<&InstanceOrigin>,
  project: &str,
  game_version: &str,
  loader_type: crate::instance::models::misc::ModLoaderType,
) -> bool {
  match origin {
    None => true,
    Some(origin) => {
      origin.platform == OriginPlatform::Campus
        && origin.project_id == project
        && origin.game_version == game_version
        && origin.loader_type == loader_type
    }
  }
}

/// Where publishing leaves the instance's identity, given what followed before
/// and whether the platform confirmed the previous project is gone. This is the
/// whole decision of [`try_adopt`] that does not touch the disk.
fn adopt_action(
  origin: Option<&InstanceOrigin>,
  project: &str,
  game_version: &str,
  loader_type: crate::instance::models::misc::ModLoaderType,
  previous_project_gone: bool,
) -> Adoption {
  if !may_follow(origin, project, game_version, loader_type) {
    let stale_campus = matches!(origin, Some(o) if o.platform == OriginPlatform::Campus);
    if !(stale_campus && previous_project_gone) {
      return Adoption::KeptOther;
    }
  }
  Adoption::Followed
}

async fn try_adopt(
  app: &AppHandle,
  instance: &Instance,
  project: &str,
  version_id: i64,
  version_number: &str,
  files: &[LocalFile],
  mmc_pack: Option<&Value>,
) -> USTBLResult<Adoption> {
  let loader_type = instance.mod_loader.loader_type;
  let origin = instance.origin.as_ref();
  // Only ask the platform when the instance follows another campus project;
  // any answer but a confirmed deletion keeps the identity.
  let gone = match origin {
    Some(o)
      if o.platform == OriginPlatform::Campus
        && !may_follow(origin, project, &instance.version, loader_type) =>
    {
      crate::instance::modpack_update::sources::campus_project_gone(app, &o.project_id).await
        == Some(true)
    }
    _ => false,
  };
  if adopt_action(origin, project, &instance.version, loader_type, gone) == Adoption::KeptOther {
    return Ok(Adoption::KeptOther);
  }
  if !get_instance_game_config(app, instance).version_isolation {
    return Ok(Adoption::NotIsolated);
  }

  // Rescan: the game may have rewritten a file while the upload ran.
  let root = instance.version_path.clone();
  let expected: Vec<LocalFile> = files.to_vec();
  let paths: Vec<String> = expected.iter().map(|file| file.path.clone()).collect();
  let rescanned = tokio::task::spawn_blocking(move || hash_selection(&root, &paths))
    .await
    .map_err(|e| USTBLError(e.to_string()))?;
  if !rescanned.is_ok_and(|now| now == expected) {
    return Ok(Adoption::LocalChanged);
  }

  let origin = InstanceOrigin {
    platform: OriginPlatform::Campus,
    project_id: project.to_string(),
    version_id: version_id.to_string(),
    game_version: instance.version.clone(),
    loader_type,
    follow: instance.origin.as_ref().is_none_or(|origin| origin.follow),
  };
  let mut baseline = ModpackBaseline::build(
    &origin,
    BaselineLoader {
      loader_type,
      version: canonical_loader_version(loader_type, &instance.mod_loader.version),
    },
    expected
      .into_iter()
      .map(|file| BaselineFile {
        path: file.path,
        sha1: file.key.sha1,
        size: file.key.size,
      })
      .collect(),
  );
  if let Some(mmc_pack) = mmc_pack {
    baseline = baseline.with_components(components_of_mmc_pack(mmc_pack));
  }
  baseline.save(&instance.version_path).await?;
  let label = version_number.to_string();
  store_instance(app, &instance.id, move |stored| {
    stored.origin = Some(origin);
    stored.modpack_version = Some(label);
  })
  .await?;
  Ok(Adoption::Followed)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  fn instance_dir(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-publish-test-{}", uuid::Uuid::new_v4()));
    for (path, content) in files {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    root
  }

  #[test]
  fn selection_is_hashed_and_cannot_escape_the_instance() {
    let root = instance_dir(&[("config/a.toml", "hello")]);

    let hashed = hash_selection(&root, &["config/a.toml".to_string()]).unwrap();
    assert_eq!(hashed[0].key.size, 5);
    assert_eq!(
      hashed[0].key.sha1,
      "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"
    );

    assert!(hash_selection(&root, &["../outside".to_string()]).is_err());
    assert!(hash_selection(&root, &["missing.txt".to_string()]).is_err());
    let _ = fs::remove_dir_all(root);
  }

  fn followed(platform: OriginPlatform, project: &str, version: &str) -> InstanceOrigin {
    InstanceOrigin {
      platform,
      project_id: project.into(),
      version_id: "1".into(),
      game_version: version.into(),
      loader_type: crate::instance::models::misc::ModLoaderType::LegacyForge,
      follow: true,
    }
  }

  #[test]
  fn only_an_unidentified_or_same_line_instance_adopts_the_release() {
    use crate::instance::models::misc::ModLoaderType::{Fabric, LegacyForge};
    assert!(may_follow(None, "gtnh", "1.7.10", LegacyForge));

    let same = followed(OriginPlatform::Campus, "gtnh", "1.7.10");
    assert!(may_follow(Some(&same), "gtnh", "1.7.10", LegacyForge));
    assert!(!may_follow(Some(&same), "other", "1.7.10", LegacyForge));
    assert!(!may_follow(Some(&same), "gtnh", "1.7.10", Fabric));
    assert!(!may_follow(
      Some(&followed(OriginPlatform::Campus, "gtnh", "1.12.2")),
      "gtnh",
      "1.7.10",
      LegacyForge
    ));
    // A pack that follows Modrinth keeps doing so.
    assert!(!may_follow(
      Some(&followed(OriginPlatform::Modrinth, "gtnh", "1.7.10")),
      "gtnh",
      "1.7.10",
      LegacyForge
    ));
  }

  #[test]
  fn a_deleted_previous_project_lets_the_publish_take_over() {
    use crate::instance::models::misc::ModLoaderType::LegacyForge;
    let stale = followed(OriginPlatform::Campus, "old", "1.7.10");
    // Previous campus project is gone: the instance joins the new one.
    assert_eq!(
      adopt_action(Some(&stale), "gtnh", "1.7.10", LegacyForge, true),
      Adoption::Followed
    );
    // Previous campus project is still there or unknown: identity stays.
    assert_eq!(
      adopt_action(Some(&stale), "gtnh", "1.7.10", LegacyForge, false),
      Adoption::KeptOther
    );
    // A modrinth identity is never taken over, gone or not.
    let external = followed(OriginPlatform::Modrinth, "old", "1.7.10");
    assert_eq!(
      adopt_action(Some(&external), "gtnh", "1.7.10", LegacyForge, true),
      Adoption::KeptOther
    );
    // Same project, no question asked.
    let same = followed(OriginPlatform::Campus, "gtnh", "1.7.10");
    assert_eq!(
      adopt_action(Some(&same), "gtnh", "1.7.10", LegacyForge, false),
      Adoption::Followed
    );
    // No identity at all adopts too.
    assert_eq!(
      adopt_action(None, "gtnh", "1.7.10", LegacyForge, false),
      Adoption::Followed
    );
  }

  #[test]
  fn a_recommendation_keeps_the_project_and_the_recommended_line() {
    let body = serde_json::json!({
      "slot": "recommended",
      "project": {
        "id": 1, "slug": "gtnh", "name": "GTNH", "summary": "",
        "lines": [{ "id": 7, "game_version": "1.7.10", "loader_type": "forge",
                    "loader_version": "10.13.4.1614", "version_number": "2.8.0" }]
      },
      "version": { "id": 7, "project_id": 1, "game_version": "1.7.10", "loader_type": "forge",
                   "loader_version": "10.13.4.1614", "version_number": "2.8.0",
                   "changelog": "", "manifest": {} }
    });
    let parsed: CampusRecommendation = serde_json::from_value(body).unwrap();
    let out = serde_json::to_value(&parsed).unwrap();
    assert_eq!(out["slot"], "recommended");
    assert_eq!(out["platform"], "campus");
    assert_eq!(out["project"]["slug"], "gtnh");
    assert_eq!(out["line"]["gameVersion"], "1.7.10");
    assert_eq!(out["line"]["versionNumber"], "2.8.0");
  }

  #[test]
  fn a_recommendation_can_point_at_another_platform() {
    let body = serde_json::json!({
      "slot": "recommended", "platform": "modrinth", "project_ref": "UCpApD3P",
      "game_version": "1.21.11", "loader_type": "fabric",
      "project": null, "version": null
    });
    let parsed: CampusRecommendation = serde_json::from_value(body).unwrap();
    let out = serde_json::to_value(&parsed).unwrap();
    assert_eq!(out["platform"], "modrinth");
    assert_eq!(out["projectRef"], "UCpApD3P");
    assert_eq!(out["gameVersion"], "1.21.11");
    assert_eq!(out["loaderType"], "fabric");
    assert!(out["project"].is_null());
    assert!(out["line"].is_null());
  }
}
