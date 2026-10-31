//! Publishing an instance to the campus modpack platform.

use super::cancel::{apply_cancel, cancel_remote, CancelOutcome};
use super::components::plan_components;
use super::job::{begin_discard, is_running, start_monitor, CancellingToken, PublishJob};
use super::manifest::{
  disabled_state_allowed, disk_path, partition_publishable, plan_files, LocalFile, PublishFile,
};
use super::pack::{build_pack, mark_verified, verify_pack};
use super::record::{
  jobs_dir, paths, JobRecord, Phase, RecordedFile, RecordedObject, RecordedPack, RecordedUpload,
  RECORD_SCHEMA,
};
use crate::account::helpers::vustb;
use crate::account::models::AccountInfo;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::misc::{get_instance_game_config, get_instance_subdir_paths};
use crate::instance::helpers::modpack::baseline::{
  canonical_loader_version, hash_file, BaselineLoader, ModpackBaseline,
};
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::helpers::modpack::multimc::snapshot::load_required_snapshot;
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceOrigin, InstanceSubdirType, OriginPlatform,
};
use crate::instance::modpack_update::commands::fast_forward;
use crate::instance::modpack_update::commands::store_instance;
use crate::instance::modpack_update::diff::FileKey;
use crate::instance::modpack_update::plan::{compare, scan_cache, Compared};
use crate::instance::modpack_update::release;
use crate::instance::modpack_update::remote::RemoteRule;
use crate::instance::modpack_update::sources::{
  campus_base_remote, campus_client, loader_slug, CAMPUS_API,
};
use crate::instance::modpack_update::text_diff::DiffHunk;
use crate::utils::web::build_part_upload_client;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// Hashes sent per request when asking which the platform lacks.
const MISSING_BATCH: usize = 5000;
/// Hash a few files at once without making a large instance saturate the disk.
const HASH_CONCURRENCY: usize = 4;
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
  /// Changelog of the line's newest version. Older platforms leave it out.
  #[serde(default)]
  pub changelog: Option<String>,
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
  let client = campus_client(app);
  vustb::send_authenticated_with(app, &client, |client, token| {
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

/// The publish dialog's gate on open: a component instance whose snapshot is
/// missing or from an older importer fails with `ModpackExportProfileMissing`.
/// Cheap on purpose (no hashing), so it is not part of the instance summary.
#[tauri::command]
pub async fn check_campus_modpack_publishable(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<()> {
  let instance = instance_of(&app, &instance_id)?;
  load_required_snapshot(&instance.version_path).await?;
  Ok(())
}

#[tauri::command]
pub async fn list_campus_modpack_projects(app: AppHandle) -> USTBLResult<Vec<CampusProject>> {
  let client = campus_client(&app);
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

/// The file a ticked entry is read from. A mod is listed under its enabled
/// name whichever way it sits on disk; the other variant existing as well would
/// make the choice ambiguous, so that is refused instead of picking one.
fn checked_disk_path(root: &Path, picked: &PublishFile) -> USTBLResult<String> {
  safe_relative(&picked.path)?;
  if !disabled_state_allowed(&picked.path, picked.disabled) {
    return Err(USTBLError(format!(
      "只有 mods 目录下的文件可以以禁用状态发布：{}",
      picked.path
    )));
  }
  if disabled_state_allowed(&picked.path, true)
    && root
      .join(disk_path(&picked.path, !picked.disabled))
      .exists()
  {
    return Err(USTBLError(format!(
      "{} 的启用和禁用文件同时存在，请删掉其中一个再发布",
      picked.path
    )));
  }
  Ok(disk_path(&picked.path, picked.disabled))
}

/// Keep the result in selection order, but report each file as soon as it finishes.
async fn hash_selection<F>(
  root: &Path,
  selected: &[PublishFile],
  mut on_file: F,
) -> USTBLResult<Vec<LocalFile>>
where
  F: FnMut(usize),
{
  let mut pending =
    futures::stream::iter(selected.iter().cloned().enumerate().map(|(index, picked)| {
      let root = root.to_path_buf();
      async move {
        let file = tokio::task::spawn_blocking(move || {
          let on_disk = checked_disk_path(&root, &picked)?;
          let (sha1, size) = hash_file(&root.join(on_disk))?;
          Ok::<LocalFile, USTBLError>(LocalFile {
            path: picked.path,
            key: FileKey { sha1, size },
            disabled: picked.disabled,
            once: picked.once,
          })
        })
        .await
        .map_err(|e| USTBLError(e.to_string()))??;
        Ok::<(usize, LocalFile), USTBLError>((index, file))
      }
    }))
    .buffer_unordered(HASH_CONCURRENCY);
  let mut indexed = Vec::with_capacity(selected.len());
  let mut completed = 0;
  while let Some(result) = pending.next().await {
    let (index, file) = result?;
    indexed.push((index, file));
    completed += 1;
    on_file(completed);
  }
  indexed.sort_unstable_by_key(|(index, _)| *index);
  Ok(indexed.into_iter().map(|(_, file)| file).collect())
}

/// A file of the line's current release, as the publish tree needs it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousFile {
  pub path: String,
  #[serde(default)]
  pub sha1: String,
  #[serde(default)]
  pub size: u64,
  #[serde(default)]
  pub disabled: bool,
  #[serde(default)]
  pub once: bool,
}

/// What the line's current release lists: the starting point of the publish
/// tree. Rules come back exactly as published.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousRelease {
  #[serde(default)]
  pub files: Vec<PreviousFile>,
  #[serde(default)]
  pub rules: Vec<RemoteRule>,
  #[serde(default)]
  pub rules_manifest: bool,
}

/// The files and rules the line's current release lists, so the publish dialog
/// starts from them. `None` when the project has no release on this line; any
/// other failure is an error, since publishing on top of a guess would lose rules.
pub async fn fetch_campus_modpack_previous_release(
  app: AppHandle,
  instance_id: String,
  project: String,
) -> USTBLResult<Option<PreviousRelease>> {
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
  let response = campus_client(&app)
    .get(format!(
      "{CAMPUS_API}/projects/{project}/lines/{}/{loader_type}",
      instance.version
    ))
    .send()
    .await?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Ok(None);
  }
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  let body: Value = response.json().await?;
  Ok(Some(previous_release_of(&body["manifest"])?))
}

fn previous_release_of(manifest: &Value) -> USTBLResult<PreviousRelease> {
  serde_json::from_value(manifest.clone())
    .map_err(|_| InstanceError::ModpackUpdateRemoteInvalid.into())
}

/// The publish tree's starting state, and what the files differ in from the
/// line's current release (`previous`, `None` for a new project or a line
/// without a release; then every file is an addition).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishTree {
  #[serde(flatten)]
  pub defaults: super::defaults::PublishDefaults,
  pub changes: Vec<super::changes::PublishChange>,
  pub previous: Option<PreviousRelease>,
}

#[tauri::command]
pub async fn prepare_campus_modpack_publish_tree(
  app: AppHandle,
  instance_id: String,
  project: Option<String>,
) -> USTBLResult<PublishTree> {
  let instance = instance_of(&app, &instance_id)?;
  let root = instance.version_path.clone();
  let previous = match project {
    Some(project) => {
      fetch_campus_modpack_previous_release(app.clone(), instance_id, project).await?
    }
    None => None,
  };
  let list = tokio::task::spawn_blocking(move || {
    crate::instance::helpers::modpack::export::list_files(&instance)
  })
  .await
  .map_err(|error| USTBLError(error.to_string()))??;
  let suggested = list
    .suggested
    .iter()
    .filter_map(|entry| {
      Some(super::defaults::SuggestedEntry {
        path: entry.path.clone(),
        kind: match entry.kind.as_str() {
          "file" => crate::instance::modpack_update::remote::RuleKind::File,
          "dir" => crate::instance::modpack_update::remote::RuleKind::Dir,
          _ => return None,
        },
      })
    })
    .collect::<Vec<_>>();
  let defaults = super::defaults::create_initial_model(&list.all, previous.as_ref(), &suggested);
  let baseline = ModpackBaseline::load(&root).await.ok();
  let files = defaults.files.clone();
  let listed = previous.clone();
  let changes = tokio::task::spawn_blocking(move || {
    let ignored = super::changes::locally_ignored(&root, baseline.as_ref());
    super::changes::classify(&root, scan_cache(), &files, listed.as_ref(), &ignored)
  })
  .await
  .map_err(|error| USTBLError(error.to_string()))??;
  Ok(PublishTree {
    defaults,
    changes,
    previous,
  })
}

/// The line diff of one file from the previous release's text to the file on
/// disk. `None` when the previous text is not kept locally or the file is not
/// text; the tree then only says the content differs.
#[tauri::command]
pub async fn diff_campus_modpack_publish_file(
  app: AppHandle,
  instance_id: String,
  path: String,
  previous_sha1: String,
) -> USTBLResult<Option<Vec<DiffHunk>>> {
  let root = instance_of(&app, &instance_id)?.version_path;
  tokio::task::spawn_blocking(move || super::changes::file_diff(&root, &path, &previous_sha1))
    .await
    .map_err(|error| USTBLError(error.to_string()))?
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
  let client = campus_client(&app);
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
  files: Vec<PublishFile>,
  rules: Vec<RemoteRule>,
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
    rules,
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

/// Two ticked entries for one path (case folded) would leave the manifest to
/// whichever the sort kept, so they are refused instead.
fn reject_duplicates(files: &[PublishFile]) -> USTBLResult<()> {
  let mut seen = HashSet::new();
  for file in files {
    if !seen.insert(file.path.to_lowercase()) {
      return Err(USTBLError(format!("{} 被重复选择", file.path)));
    }
  }
  Ok(())
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
  files: Vec<PublishFile>,
  rules: Vec<RemoteRule>,
) -> USTBLResult<(JobRecord, usize)> {
  let instance = instance_of(app, instance_id)?;
  let root = instance.version_path.clone();
  let loader_type =
    loader_slug(instance.mod_loader.loader_type).ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let job_paths = paths(dir, job_id)?;

  // A component instance without a usable snapshot cannot be published; fail
  // before hashing every selected file (the publish dialog checks this on open
  // with `check_campus_modpack_publishable`, this guard is the backstop).
  let snapshot = load_required_snapshot(&root).await?;

  reject_duplicates(&files)?;
  for rule in &rules {
    safe_relative(&rule.path)?;
  }
  let (files, skipped) = partition_publishable(files, &instance.name);
  let total = files.len();
  report(app, instance_id, Stage::Hashing, 0, total);
  let hashed = hash_selection(&root, &files, |done| {
    report(app, instance_id, Stage::Hashing, done, total);
  })
  .await?;
  report(app, instance_id, Stage::Hashing, total, total);
  let mut plan = plan_files(hashed, rules, &instance.name);
  plan.skipped.extend(skipped);
  plan.skipped.sort();
  // A version may list no file at all when it only deletes or ignores.
  if plan.files.is_empty() && plan.rules.is_empty() {
    return Err(USTBLError("没有可发布的文件".into()));
  }
  if let Some(snapshot) = snapshot {
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
      let current = campus_client(&app)
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
    tokio::task::spawn_blocking(move || build_pack(&objects, &target))
      .await
      .map_err(|e| USTBLError(e.to_string()))??;
    // §13.2: the self-check runs before anything is sent to the platform, so
    // a bad local pack never opens a version or an upload session that would
    // then need to be torn down again (no `POST /versions` has happened yet
    // at this point, so there is nothing on the platform to clean up). The
    // pack is new, so its sha1 is whatever this read finds.
    let check_path = job_paths.pack.clone();
    let built = tokio::task::spawn_blocking(move || verify_pack(&check_path, None))
      .await
      .map_err(|e| USTBLError(e.to_string()))??;
    // This process just verified these bytes; the upload's ensure_pack can
    // skip reading the pack back while the file is untouched.
    if let Ok(meta) = tokio::fs::metadata(&job_paths.pack).await {
      if let Ok(modified) = meta.modified() {
        mark_verified(&job_paths.pack, meta.len(), modified);
      }
    }
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
  // The task card shows the project's display name, not the generated slug.
  let project_name = match &new_project {
    Some(new) => Some(new.name.clone()),
    None => {
      let mut name = None;
      if let Some(project) = &project {
        if let Ok(response) = campus_client(&app)
          .get(format!("{CAMPUS_API}/projects/{project}"))
          .send()
          .await
        {
          if response.status().is_success() {
            name = response
              .json::<Value>()
              .await
              .ok()
              .and_then(|body| body["name"].as_str().map(str::to_string));
          }
        }
      }
      Some(name.unwrap_or_else(|| slug.clone()))
    }
  };

  let record = JobRecord {
    schema: RECORD_SCHEMA,
    id: job_id.to_string(),
    phase: Phase::Uploading,
    error: None,
    created_at: chrono::Utc::now().timestamp_millis(),
    instance_id: instance_id.to_string(),
    instance_name: instance.name.clone(),
    project_slug: slug,
    project_name,
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
  // Held from before the record is read until the task group is running:
  // a cancel cleanup that is still on the wire (or a discard in flight)
  // must never interleave with this start. Once the group runs, `ACTIVE`
  // takes over the duplicate protection and the token may go.
  let _cancelling = CancellingToken::take(&job_id)
    .ok_or_else(|| USTBLError("取消处理尚未完成，请稍后再试".into()))?;
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
/// the local leftovers are removed — but only once the platform confirms the
/// version is gone. The decision is the shared one in `cancel.rs`, the same
/// the task card's cancel makes (§3.2): a version the platform still owns
/// keeps its record and gets its watch back instead of being abandoned.
#[tauri::command]
pub async fn discard_campus_modpack_publish(app: AppHandle, job_id: String) -> USTBLResult<()> {
  ensure_manager(&app)?;
  // Held from before the remote request until the action — and the watch
  // it may start — is fully done: a second discard, a resume, or the
  // guard's own cleanup can never slip in while this one is on the wire
  // and race the record or the restarted task.
  let _cancelling = begin_discard(&job_id)?;
  let dir = jobs_dir();
  let version_id = JobRecord::load(&dir, &job_id)?.version_id;
  let remote = cancel_remote(&app, version_id).await;
  // The shared action, identical to the one the task card's cancel runs:
  // the record it applies to is re-read from disk now that the remote
  // answer is in, and the watch — when the platform turns out to own the
  // version — only ever starts from the freshly saved one.
  match apply_cancel(&dir, &job_id, remote, |record| start_monitor(&app, record)) {
    CancelOutcome::Removed | CancelOutcome::Watching => Ok(()),
    CancelOutcome::Kept(reason) | CancelOutcome::RecordKept(reason) => Err(reason),
  }
}

/// After publishing, the instance follows the new version when the update
/// side would call it a fast forward: the same decision, run against the
/// version the platform now lists. When that holds, record it as the
/// instance's baseline, so the first check does not offer the publisher their
/// own release.
pub(super) async fn adopt_published(
  app: &AppHandle,
  instance: &Instance,
  project: &str,
  version_id: i64,
  version_number: &str,
) -> Adoption {
  match try_adopt(app, instance, project, version_id, version_number).await {
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

/// Whether the comparison of the instance with the published version moves
/// nothing but the baseline. The instance is the source of the loader and the
/// component chain, so those never count as a change here.
fn is_fast_forward(compared: &Compared) -> bool {
  compared.both_variants.is_empty()
    && fast_forward(
      compared
        .diff
        .changes
        .iter()
        .chain(&compared.diff.auto_merged),
      compared.diff.conflicts.len(),
      false,
    )
}

async fn try_adopt(
  app: &AppHandle,
  instance: &Instance,
  project: &str,
  version_id: i64,
  version_number: &str,
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

  let root = instance.version_path.clone();
  let new_origin = InstanceOrigin {
    platform: OriginPlatform::Campus,
    project_id: project.to_string(),
    version_id: version_id.to_string(),
    game_version: instance.version.clone(),
    loader_type,
    follow: instance.origin.as_ref().is_none_or(|origin| origin.follow),
  };
  let loader = BaselineLoader {
    loader_type,
    version: canonical_loader_version(loader_type, &instance.mod_loader.version),
  };
  // The version as the platform lists it, the way an update would read it.
  let remote = campus_base_remote(app, &new_origin, &root, &instance.name).await?;
  // The line the instance already follows is the base; any other start (no
  // identity, or a project that is gone) is a first install.
  let following =
    origin.is_some_and(|o| may_follow(Some(o), project, &instance.version, loader_type));
  let base = match ModpackBaseline::load(&root).await {
    Ok(baseline) if following => baseline,
    _ => ModpackBaseline::build(&new_origin, loader.clone(), Vec::new()),
  };
  let compared = compare(&root, &base, &remote).await?;
  if !is_fast_forward(&compared) {
    return Ok(Adoption::LocalChanged);
  }

  let mut baseline = ModpackBaseline::build(&new_origin, loader, compared.next_baseline);
  if let Some(components) = remote.components.clone() {
    baseline = baseline.with_components(components);
  }
  baseline.save(&instance.version_path).await?;
  release::write(
    &instance.version_path,
    &release::Release::from_remote(&remote),
  )
  .await?;
  let label = version_number.to_string();
  store_instance(app, &instance.id, move |stored| {
    adopt_instance_identity(stored, new_origin, label);
  })
  .await?;
  Ok(Adoption::Followed)
}

fn adopt_instance_identity(instance: &mut Instance, origin: InstanceOrigin, version: String) {
  instance.origin = Some(origin);
  instance.modpack_version = Some(version);
  instance.isolation_locked = true;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::ModLoaderType;
  use crate::instance::modpack_update::remote::{RemoteModpack, RuleKind, RuleState};
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
  fn adopted_instances_stay_locked_after_unlinking() {
    let mut instance = Instance::default();
    adopt_instance_identity(
      &mut instance,
      InstanceOrigin {
        platform: OriginPlatform::Campus,
        project_id: "project".into(),
        version_id: "version".into(),
        game_version: "1.21.1".into(),
        loader_type: ModLoaderType::Unknown,
        follow: true,
      },
      "1.0.0".into(),
    );
    assert!(instance.isolation_locked);

    instance.origin = None;
    instance.modpack_version = None;
    let global = crate::launcher_config::models::GameConfig {
      version_isolation: false,
      ..Default::default()
    };
    assert_eq!(
      crate::instance::helpers::misc::resolve_isolation(&instance, &global),
      (
        true,
        crate::instance::helpers::misc::IsolationSource::Locked
      )
    );
  }

  fn picked(path: &str) -> PublishFile {
    PublishFile {
      path: path.into(),
      disabled: false,
      once: false,
    }
  }

  #[tokio::test]
  async fn selection_is_hashed_and_cannot_escape_the_instance() {
    let root = instance_dir(&[("config/a.toml", "hello")]);

    let hashed = hash_selection(&root, &[picked("config/a.toml")], |_| {})
      .await
      .unwrap();
    assert_eq!(hashed[0].key.size, 5);
    assert_eq!(
      hashed[0].key.sha1,
      "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"
    );

    assert!(hash_selection(&root, &[picked("../outside")], |_| {})
      .await
      .is_err());
    assert!(hash_selection(&root, &[picked("missing.txt")], |_| {})
      .await
      .is_err());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn hashing_reports_progress_and_preserves_selection_order() {
    let root = instance_dir(&[("config/a.toml", "a"), ("config/b.toml", "bb")]);
    let selected = vec![picked("config/b.toml"), picked("config/a.toml")];
    let mut progress = Vec::new();

    let hashed = hash_selection(&root, &selected, |done| progress.push(done))
      .await
      .unwrap();

    assert_eq!(progress, [1, 2]);
    let paths: Vec<_> = hashed.into_iter().map(|file| file.path).collect();
    assert_eq!(paths, ["config/b.toml", "config/a.toml"]);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_disabled_mod_is_hashed_from_its_disabled_file_and_both_variants_are_refused() {
    let root = instance_dir(&[("mods/a.jar.disabled", "off"), ("mods/b.jar", "on")]);
    let off = PublishFile {
      path: "mods/a.jar".into(),
      disabled: true,
      once: true,
    };
    let hashed = hash_selection(&root, &[off], |_| {}).await.unwrap();
    assert_eq!(hashed[0].path, "mods/a.jar");
    assert_eq!(hashed[0].key.size, 3);
    assert!(hashed[0].disabled && hashed[0].once);

    // Both variants on disk: neither is picked silently.
    fs::write(root.join("mods/b.jar.disabled"), "x").unwrap();
    assert!(hash_selection(&root, &[picked("mods/b.jar")], |_| {})
      .await
      .is_err());
    // Only mods can ship disabled.
    let config = PublishFile {
      path: "config/a.toml".into(),
      disabled: true,
      once: false,
    };
    assert!(hash_selection(&root, &[config], |_| {}).await.is_err());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn duplicate_entries_are_refused_case_folded() {
    assert!(reject_duplicates(&[picked("mods/a.jar"), picked("mods/b.jar")]).is_ok());
    assert!(reject_duplicates(&[picked("mods/a.jar"), picked("Mods/A.jar")]).is_err());
  }

  #[test]
  fn the_previous_release_keeps_flags_and_rules_as_published() {
    let manifest = serde_json::json!({
      "format": "ustbl.modpack/3",
      "files": [
        { "path": "mods/a.jar", "sha1": "a", "size": 1, "disabled": true },
        { "path": "config/x.toml", "sha1": "b", "size": 1, "once": true },
        { "path": "options.txt", "sha1": "c", "size": 1 }
      ],
      "rules": [
        { "path": "config/cache", "kind": "dir", "state": "ignore" },
        { "path": "mods/gone.jar", "kind": "file", "state": "delete" }
      ]
    });
    let release = previous_release_of(&manifest).unwrap();
    let flags: Vec<_> = release.files.iter().map(|f| (f.disabled, f.once)).collect();
    assert_eq!(flags, [(true, false), (false, true), (false, false)]);
    assert_eq!(release.rules.len(), 2);
    assert_eq!(release.rules[0].path, "config/cache");
    assert_eq!(release.rules[1].kind, RuleKind::File);
    assert_eq!(release.rules[1].state, RuleState::Delete);
    let out = serde_json::to_value(&release).unwrap();
    assert_eq!(out["files"][0]["disabled"], true);
    assert_eq!(out["rules"][0]["state"], "ignore");
  }

  fn published(files: &[(&str, &str, bool, bool)], rules: Vec<RemoteRule>) -> RemoteModpack {
    use crate::instance::modpack_update::remote::{FileSource, RemoteFile};
    use sha1::Digest;
    RemoteModpack {
      version_id: "2".into(),
      label: "2".into(),
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: crate::instance::models::misc::ModLoaderType::Fabric,
        version: "0.16.0".into(),
      },
      components: None,
      component_pack: None,
      files: files
        .iter()
        .map(|(path, content, disabled, once)| RemoteFile {
          path: path.to_string(),
          key: FileKey {
            sha1: hex::encode(sha1::Sha1::digest(content.as_bytes())),
            size: content.len() as u64,
          },
          source: FileSource::ArchiveEntry {
            archive: std::path::PathBuf::new(),
            entry: path.to_string(),
          },
          disabled: *disabled,
          once: *once,
        })
        .collect(),
      rules,
      rules_manifest: true,
      path_index: Default::default(),
    }
  }

  fn first_install() -> ModpackBaseline {
    let origin = followed(OriginPlatform::Campus, "p", "1.21.1");
    ModpackBaseline::build(
      &origin,
      BaselineLoader {
        loader_type: origin.loader_type,
        version: "0.16.0".into(),
      },
      Vec::new(),
    )
  }

  fn rule(path: &str, kind: RuleKind, state: RuleState) -> RemoteRule {
    RemoteRule {
      path: path.into(),
      kind,
      state,
    }
  }

  #[tokio::test]
  async fn a_release_the_disk_matches_is_a_fast_forward_that_records_every_state() {
    let root = instance_dir(&[
      ("mods/a.jar", "a"),
      ("mods/b.jar.disabled", "b"),
      ("config/once.toml", "o"),
      ("config/cache/x", "ignored"),
    ]);
    let remote = published(
      &[
        ("mods/a.jar", "a", false, false),
        ("mods/b.jar", "b", true, false),
        ("config/once.toml", "o", false, true),
      ],
      vec![rule("config/cache", RuleKind::Dir, RuleState::Ignore)],
    );

    let compared = compare(&root, &first_install(), &remote).await.unwrap();

    assert!(is_fast_forward(&compared));
    let entry = |path: &str| {
      compared
        .next_baseline
        .iter()
        .find(|file| file.path == path)
        .unwrap()
    };
    assert!(entry("mods/a.jar").is_managed());
    assert!(entry("mods/b.jar").is_managed() && entry("mods/b.jar").disabled);
    // A once-only file is remembered but not managed.
    assert!(!entry("config/once.toml").is_managed());
    assert!(compared
      .next_baseline
      .iter()
      .all(|f| f.path != "config/cache/x"));
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_red_target_still_on_disk_or_a_changed_file_blocks_the_adoption() {
    let root = instance_dir(&[("mods/a.jar", "a"), ("old/stale.zip", "z")]);
    let delete = published(
      &[("mods/a.jar", "a", false, false)],
      vec![rule("old/stale.zip", RuleKind::File, RuleState::Delete)],
    );
    let compared = compare(&root, &first_install(), &delete).await.unwrap();
    assert!(!is_fast_forward(&compared));

    // The same target gone from disk is fine.
    fs::remove_file(root.join("old/stale.zip")).unwrap();
    let compared = compare(&root, &first_install(), &delete).await.unwrap();
    assert!(is_fast_forward(&compared));

    // A file that no longer matches what was published.
    fs::write(root.join("mods/a.jar"), "edited").unwrap();
    let compared = compare(&root, &first_install(), &delete).await.unwrap();
    assert!(!is_fast_forward(&compared));
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
  fn a_listed_line_carries_its_changelog_when_the_platform_sends_one() {
    let body = serde_json::json!([{
      "id": 1, "slug": "gtnh", "name": "GTNH", "summary": "",
      "lines": [
        { "id": 7, "game_version": "1.7.10", "loader_type": "forge",
          "loader_version": "10.13.4.1614", "version_number": "2.8.0",
          "changelog": "修复崩溃\n更新模组" },
        // An older platform has no changelog on the listing.
        { "id": 8, "game_version": "1.12.2", "loader_type": "forge",
          "loader_version": "14.23.5.2860", "version_number": "1.0.0" }
      ]
    }]);
    let parsed: Vec<CampusProject> = serde_json::from_value(body).unwrap();
    let out = serde_json::to_value(&parsed).unwrap();
    assert_eq!(out[0]["lines"][0]["changelog"], "修复崩溃\n更新模组");
    assert!(out[0]["lines"][1]["changelog"].is_null());
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
