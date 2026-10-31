//! Tauri commands for following a modpack: check, plan, apply, recover.

use super::apply::{
  apply, can_roll_back_last, ensure_unlink_safe, materialize_remote_file, pack_wanted,
  prepare_packs, recover_checked, roll_back_last_checked, wrap_locate, Progress, Recovery, Stage,
  UpdateEnv, UpdateGuard, UpdateRequest,
};
use super::diff::{Conflict, ConflictChoice, Diff, FileChange};
use super::merge::merge_with_choices;
use super::notice::{read_update_notice, write_update_notice, UpdateNotice};
use super::plan::{
  compare, count_local_changes, finish, invalidate_scan_cache, local_changes,
  requires_reinstall, resolve, LocalChange, Prepared,
};
use super::plan_cache::{self, PlanCache};
use super::remote::{ComponentPack, RemoteModpack};
use super::restore::restore_component_profile;
use super::sources::{download_param, latest_version, resolve_remote, LatestVersion, Payload};
use crate::error::USTBLResult;
use crate::instance::commands::switch_mod_loader;
use crate::instance::helpers::client_json::McClientInfo;
use crate::instance::helpers::misc::{
  get_instance_game_config, get_instance_subdir_paths, refresh_and_update_instances,
};
use crate::instance::helpers::modpack::baseline::{
  BaselineLoader, ModpackBaseline, BASELINE_FILE_NAME,
};
use crate::instance::helpers::modpack::text;
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceOrigin, ModLoader, ModLoaderStatus, ModLoaderType,
  OriginPlatform,
};
use crate::launch::models::LaunchingState;
use crate::launch::helpers::file_validator::{get_invalid_assets, get_invalid_library_files};
use crate::resource::helpers::misc::{download_source_candidates, get_source_priority_list};
use crate::resource::models::{ModLoaderResourceInfo, ResourceType};
use crate::tasks::download::DownloadParam;
use crate::tasks::download::DownloadTransferOptions;
use crate::tasks::events::TauriEventSink;
use crate::tasks::monitor::TaskMonitor;
use crate::tasks::streams::desc::PDesc;
use crate::tasks::streams::reporter::Sink;
use crate::tasks::streams::PHandle;
use crate::tasks::upload::upload_handle;
use crate::tasks::PTaskParam;
use crate::utils::fs::validate_sha1;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::reveal_item_in_dir;

const PROGRESS_EVENT: &str = "modpack-update:progress";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
  /// The instance has no followable identity.
  NotFollowing,
  /// The player paused following; nothing is checked.
  Paused,
  /// Version isolation is off or the baseline is unusable, so the instance
  /// cannot be compared safely. `reason` says which.
  NeedsRepair,
  /// The loader of the instance is not installed yet (a first install or an
  /// update is still finishing it), so nothing is compared.
  WaitingLoader,
  /// A persisted campus installation must be resumed or discarded first.
  Installing,
  UpToDate,
  /// Up to date, but managed files were changed here. Only reported when the
  /// caller asked for the local scan.
  LocalChanges,
  Available,
  /// The platform could not be asked; `reason` carries the error code.
  Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
  pub status: CheckStatus,
  /// The player paused following; updates are shown but never forced.
  pub follow: bool,
  pub current_label: Option<String>,
  pub latest_version_id: Option<String>,
  pub latest_label: Option<String>,
  /// Error code explaining `Unknown` or `NeedsRepair`.
  pub reason: Option<String>,
  /// The previous update left a backup that can still be rolled back.
  pub can_roll_back: bool,
  /// How many managed files differ from the install; set by the local scan.
  pub local_changes: Option<usize>,
  /// The platform confirmed the followed project is gone, so this check
  /// detached the instance from it. Only set on the check that unlinked.
  pub unlinked: bool,
}

#[derive(Debug, Clone, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderChange {
  pub from: BaselineLoader,
  pub to: BaselineLoader,
}

fn compute_loader_change(
  baseline: &BaselineLoader,
  remote: &BaselineLoader,
) -> Option<LoaderChange> {
  (remote != baseline).then(|| LoaderChange {
    from: baseline.clone(),
    to: remote.clone(),
  })
}

#[derive(Debug, Clone, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePlan {
  pub from_version_id: String,
  pub to_version_id: String,
  pub to_label: String,
  pub changes: Vec<FileChange>,
  pub conflicts: Vec<Conflict>,
  pub auto_merged: Vec<FileChange>,
  pub loader_change: Option<LoaderChange>,
  /// The new version changes the component chain, so it has to be installed
  /// again. No changes are planned and `apply_modpack_update` refuses.
  pub reinstall_required: bool,
  /// The instance already matches the new version: applying only moves the
  /// recorded version forward, nothing is written to the files.
  pub fast_forward: bool,
  pub plan_hash: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcome {
  pub version_id: String,
  /// Forge-family loaders still need `finish_mod_loader_install` before the
  /// instance can start.
  pub loader_install_pending: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CampusInstallStarted {
  pub group: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStarted {
  pub group: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampusUnfinishedInstall {
  pub instance_id: String,
  pub instance_name: String,
  pub label: String,
  pub phase: String,
  pub error: Option<String>,
  pub resumable: bool,
  pub group: Option<String>,
  pub progress: Option<Progress>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampusInstallFinished {
  instance_id: String,
  group: String,
  error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModpackUpdateFinished {
  instance_id: String,
  instance_name: String,
  group: String,
  plan_hash: String,
  outcome: Option<UpdateOutcome>,
  error: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallTarget {
  pub version_id: String,
  pub label: String,
  pub url: String,
  pub sha1: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CampusInstallRecord {
  instance_id: String,
  instance_name: String,
  label: String,
  phase: String,
  error: Option<String>,
  #[serde(default)]
  group: Option<String>,
  #[serde(default)]
  progress: Option<Progress>,
  #[serde(default)]
  client_group: Option<String>,
  #[serde(default)]
  target: Option<InstallTarget>,
}

pub(crate) fn install_target_from_origin(
  input: Option<&crate::instance::models::misc::ModpackOriginInput>,
) -> Option<InstallTarget> {
  let input = input?;
  matches!(
    input.platform,
    OriginPlatform::Modrinth | OriginPlatform::CurseForge
  )
  .then(|| InstallTarget {
    version_id: input.version_id.clone(),
    label: input.label.clone().unwrap_or_default(),
    url: input.url.clone().unwrap_or_default(),
    sha1: input.expected_sha1.clone(),
  })
}

/// Campus installs and platform first installs use the same resumable record.
fn is_campus_install(origin: &InstanceOrigin, record: Option<&CampusInstallRecord>) -> bool {
  origin.platform == OriginPlatform::Campus
    || matches!(
      origin.platform,
      OriginPlatform::Modrinth | OriginPlatform::CurseForge
    ) && record.is_some_and(|record| record.target.is_some())
}

fn latest_from_install_target(target: InstallTarget) -> LatestVersion {
  LatestVersion {
    version_id: target.version_id,
    label: target.label,
    payload: Some(Payload::Archive {
      url: target.url,
      sha1: target.sha1,
    }),
  }
}

fn rewrite_install_record(
  instance_id: String,
  instance_name: String,
  label: String,
  old_record: Option<CampusInstallRecord>,
) -> CampusInstallRecord {
  CampusInstallRecord {
    instance_id,
    instance_name,
    label,
    phase: "waitingClient".into(),
    error: None,
    group: None,
    progress: None,
    client_group: old_record
      .as_ref()
      .and_then(|record| record.client_group.clone()),
    target: old_record.and_then(|record| record.target),
  }
}

fn campus_install_record(root: &Path) -> PathBuf {
  root
    .join(super::remote::UPDATE_DIR)
    .join("campus-install.json")
}

fn save_campus_install_record(root: &Path, record: &CampusInstallRecord) -> USTBLResult<()> {
  let path = campus_install_record(root);
  fs::create_dir_all(path.parent().unwrap())?;
  let temporary = path.with_extension("json.tmp");
  fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
  fs::rename(temporary, path)?;
  Ok(())
}

fn load_campus_install_record(root: &Path) -> Option<CampusInstallRecord> {
  serde_json::from_slice(&fs::read(campus_install_record(root)).ok()?).ok()
}

pub fn has_campus_install(root: &Path) -> bool {
  load_campus_install_record(root).is_some()
}

pub fn pending_campus_instances(app: &AppHandle) -> HashMap<String, Instance> {
  campus_instances_on_disk(app)
    .into_iter()
    .filter(|(_, instance)| has_campus_install(&instance.version_path))
    .collect()
}

fn remove_campus_install_record(root: &Path) {
  let _ = fs::remove_file(campus_install_record(root));
}

/// Persist the pending install before instance creation reports success. The
/// entry may subsequently replace the instance name with the project label.
pub(crate) fn register_created_campus_instance(
  instance: &Instance,
  client_group: String,
  target: Option<InstallTarget>,
) -> USTBLResult<()> {
  let record = CampusInstallRecord {
    instance_id: instance.id.clone(),
    instance_name: instance.name.clone(),
    label: instance.name.clone(),
    phase: "waitingClient".into(),
    error: None,
    group: None,
    progress: None,
    client_group: Some(client_group),
    target,
  };
  if instance
    .origin
    .as_ref()
    .is_some_and(|origin| is_campus_install(origin, Some(&record)) && is_fresh(origin))
  {
    save_campus_install_record(&instance.version_path, &record)?;
  }
  Ok(())
}

fn campus_install_records(app: &AppHandle) -> Vec<CampusUnfinishedInstall> {
  let instances = campus_instances_on_disk(app);
  instances
    .values()
    .filter_map(unfinished_campus_install)
    .collect()
}

fn unfinished_campus_install(instance: &Instance) -> Option<CampusUnfinishedInstall> {
  let record = load_campus_install_record(&instance.version_path)?;
  if active_plans().lock().ok()?.contains(&instance.version_path) {
    return None;
  }
  let interrupted = record.phase == "installing";
  Some(CampusUnfinishedInstall {
    instance_id: instance.id.clone(),
    instance_name: instance.name.clone(),
    label: record.label,
    resumable: true,
    phase: if interrupted {
      "failed".into()
    } else {
      record.phase
    },
    error: if interrupted {
      Some(InstanceError::ModpackInstallInterrupted.to_string())
    } else {
      record.error
    },
    group: record.group,
    progress: record.progress,
  })
}

fn fail_campus_install_record(root: &Path, error: &str) -> USTBLResult<()> {
  if let Some(mut record) = load_campus_install_record(root) {
    record.phase = "failed".into();
    record.error = Some(error.to_string());
    save_campus_install_record(root, &record)?;
  }
  Ok(())
}

fn campus_install_group_belongs_to(group: &str, instance_id: &str) -> bool {
  let suffix = format!(" [{}]", instance_id.replace(['?', '@', '\n', '\r'], " "));
  group.starts_with("modpack-install?")
    && group
      .rsplit_once('@')
      .is_some_and(|(name, _)| name.ends_with(&suffix))
}

fn remove_campus_install_groups(app: &AppHandle, instance_id: &str, recorded: Option<&str>) {
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  let mut groups: Vec<String> = monitor
    .state_list()
    .into_iter()
    .map(|group| group.task_group)
    .filter(|group| campus_install_group_belongs_to(group, instance_id))
    .collect();
  if let Some(group) = recorded {
    if !groups.iter().any(|name| name == group) {
      groups.push(group.to_string());
    }
  }
  monitor.remove_job_groups(&groups);
  let _ = app.emit_to("main", "modpack-install:groups-removed", groups);
}

#[tauri::command]
pub fn list_campus_modpack_install_jobs(app: AppHandle) -> Vec<CampusUnfinishedInstall> {
  campus_install_records(&app)
}

#[tauri::command]
pub async fn register_campus_modpack_install(
  app: AppHandle,
  instance_id: String,
  label: String,
) -> USTBLResult<()> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let old_record = load_campus_install_record(&target.root);
  if !is_campus_install(&target.origin, old_record.as_ref()) || !is_fresh(&target.origin) {
    return Err(InstanceError::ModpackUpdateUnsupported.into());
  }
  let _guard = PlanGuard::acquire(&target.root)?;
  save_campus_install_record(
    &target.root,
    &rewrite_install_record(instance_id, target.instance.name, label, old_record),
  )
}

#[tauri::command]
pub async fn discard_campus_modpack_install(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<()> {
  let instance = current_instance(&app, &instance_id)?;
  let root = instance.version_path;
  let _guard = PlanGuard::acquire(&root)?;
  let record = load_campus_install_record(&root).ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let recovery = recover_checked(&root, || ensure_not_running(&app, &instance_id))?;
  for name in ["staging", "components", "remote"] {
    match fs::remove_dir_all(root.join(super::remote::UPDATE_DIR).join(name)) {
      Ok(()) => {}
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
  }
  for name in [
    "staging-manifest.json",
    "staging-manifest.json.tmp",
    "campus-install.json.tmp",
    "campus-install.json",
  ] {
    match fs::remove_file(root.join(super::remote::UPDATE_DIR).join(name)) {
      Ok(()) => {}
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
  }
  plan_cache::clear_plan_cache(&root);
  remove_campus_install_groups(&app, &instance_id, record.group.as_deref());
  if recovery != Recovery::Nothing {
    refresh_and_update_instances(&app, false).await;
  }
  Ok(())
}

#[tauri::command]
pub async fn resume_campus_modpack_install(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<CampusInstallStarted> {
  start_campus_modpack_install(app, instance_id, true).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
  instance_id: String,
  group: Option<String>,
  #[serde(flatten)]
  progress: Progress,
}

struct Target {
  instance: Instance,
  origin: InstanceOrigin,
  root: PathBuf,
}

fn current_instance(app: &AppHandle, instance_id: &str) -> USTBLResult<Instance> {
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let state = binding.lock()?;
  if let Some(instance) = state.get(instance_id) {
    return Ok(instance.clone());
  }
  drop(state);
  // A pending client has no jar yet, so the normal instance scanner omits it.
  // Read only configs inside configured game directories, never an ID as a path.
  campus_instances_on_disk(app)
    .remove(instance_id)
    .ok_or_else(|| InstanceError::InstanceNotFoundByID.into())
}

fn campus_instances_on_disk(app: &AppHandle) -> HashMap<String, Instance> {
  let binding = app.state::<Mutex<crate::launcher_config::models::LauncherConfig>>();
  let directories = match binding.lock() {
    Ok(config) => config.local_game_directories.clone(),
    Err(_) => return HashMap::new(),
  };
  scan_campus_instances(&directories)
}

fn scan_campus_instances(
  directories: &[crate::launcher_config::models::GameDirectory],
) -> HashMap<String, Instance> {
  let mut instances = HashMap::new();
  for directory in directories {
    let Ok(entries) = fs::read_dir(directory.dir.join("versions")) else {
      continue;
    };
    for entry in entries.flatten() {
      let root = entry.path();
      let Ok(bytes) = fs::read(root.join(crate::instance::constants::INSTANCE_CFG_FILE_NAME))
      else {
        continue;
      };
      let Ok(mut instance) = serde_json::from_slice::<Instance>(&bytes) else {
        continue;
      };
      if instance
        .origin
        .as_ref()
        .is_none_or(|origin| !is_campus_install(origin, load_campus_install_record(&root).as_ref()))
      {
        continue;
      }
      let Some(name) = entry.file_name().to_str().map(str::to_string) else {
        continue;
      };
      instance.id = format!("{}:{}", directory.name, name);
      instance.name = name;
      instance.version_path = root;
      instances.insert(instance.id.clone(), instance);
    }
  }
  instances
}

fn target(app: &AppHandle, instance_id: &str) -> USTBLResult<Option<Target>> {
  let instance = current_instance(app, instance_id)?;
  let Some(origin) = instance.origin.clone() else {
    return Ok(None);
  };
  let root = instance.version_path.clone();
  Ok(Some(Target {
    instance,
    origin,
    root,
  }))
}

fn ensure_not_running(app: &AppHandle, instance_id: &str) -> USTBLResult<()> {
  let binding = app.state::<Mutex<Vec<LaunchingState>>>();
  let queue = binding.lock()?;
  if queue
    .iter()
    .any(|launching| launching.selected_instance.id == instance_id)
  {
    return Err(InstanceError::ModpackUpdateRunning.into());
  }
  Ok(())
}

/// Instances an install or an update is being planned or applied to, from
/// the command's first step (prepare included) until it answers. The
/// `UpdateGuard` only covers the transaction on disk; this one covers the
/// whole run, so a second request is told busy instead of re-planning.
fn active_plans() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
  static ACTIVE: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
  ACTIVE.get_or_init(Default::default)
}

struct PlanGuard(PathBuf);

impl PlanGuard {
  fn acquire(root: &Path) -> USTBLResult<Self> {
    let mut active = active_plans().lock()?;
    if !active.insert(root.to_path_buf()) {
      return Err(InstanceError::ModpackUpdateBusy.into());
    }
    Ok(Self(root.to_path_buf()))
  }
}

impl Drop for PlanGuard {
  fn drop(&mut self) {
    if let Ok(mut active) = active_plans().lock() {
      active.remove(&self.0);
    }
  }
}

/// The baseline is the record of what the modpack installed. Without a
/// matching one the instance cannot be compared safely.
async fn load_baseline(target: &Target) -> USTBLResult<ModpackBaseline> {
  let baseline = ModpackBaseline::load(&target.root)
    .await
    .map_err(|_| InstanceError::ModpackUpdateBaselineMissing)?;
  if !baseline.is_consistent_with(&target.origin) {
    return Err(InstanceError::ModpackUpdateBaselineMissing.into());
  }
  Ok(baseline)
}

/// Change an instance's saved config (identity, shown version) and keep the
/// in-memory copy in step. Starts from the file on disk, because the loader
/// switch and the importer write the instance file on their own.
pub async fn store_instance(
  app: &AppHandle,
  instance_id: &str,
  change: impl FnOnce(&mut Instance),
) -> USTBLResult<()> {
  let mut instance = current_instance(app, instance_id)?.load_json_cfg().await?;
  change(&mut instance);
  instance.save_json_cfg().await?;
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  binding.lock()?.insert(instance_id.to_string(), instance);
  Ok(())
}

struct AppEnv {
  app: AppHandle,
  instance_id: String,
  label: String,
  /// The loader of the version being installed.
  loader: BaselineLoader,
  loader_install_pending: AtomicBool,
  started: Instant,
  tasks: Option<CampusInstallTasks>,
}

type ModpackTaskHandle<S> = Arc<RwLock<PHandle<S, PTaskParam>>>;

/// Shared stage handles for campus installs and existing-instance updates.
struct CampusInstallTasks<S: Sink = TauriEventSink> {
  group: String,
  is_update: bool,
  waiting_client: ModpackTaskHandle<S>,
  preparing: ModpackTaskHandle<S>,
  locating: ModpackTaskHandle<S>,
  fetching: ModpackTaskHandle<S>,
  applying: ModpackTaskHandle<S>,
}

impl<S: Sink> Clone for CampusInstallTasks<S> {
  fn clone(&self) -> Self {
    Self {
      group: self.group.clone(),
      is_update: self.is_update,
      waiting_client: self.waiting_client.clone(),
      preparing: self.preparing.clone(),
      locating: self.locating.clone(),
      fetching: self.fetching.clone(),
      applying: self.applying.clone(),
    }
  }
}

impl<S: Sink> CampusInstallTasks<S> {
  fn all(&self) -> Vec<&ModpackTaskHandle<S>> {
    let mut handles = vec![
      &self.waiting_client,
      &self.preparing,
      &self.locating,
      &self.fetching,
      &self.applying,
    ];
    if self.is_update {
      handles.remove(0);
    }
    handles
  }

  fn with_handles(
    group: String,
    is_update: bool,
    mut make: impl FnMut(&str, i64) -> ModpackTaskHandle<S>,
  ) -> Self {
    let preparing = make("Preparing", 1);
    // Updates have no waiting-client phase; this unused alias is not registered.
    let waiting_client = if is_update {
      preparing.clone()
    } else {
      make("WaitingClient", 1)
    };
    let locating = make("Locating", 1);
    let fetching = make("Fetching", 0);
    let applying = make("Applying", 1);
    Self {
      group,
      is_update,
      waiting_client,
      preparing,
      locating,
      fetching,
      applying,
    }
  }

  fn finish(&self, error: Option<&str>) {
    if let Some(error) = error {
      self.fail(error);
    } else {
      for handle in self.all() {
        Self::complete(handle);
      }
    }
  }

  fn complete(handle: &ModpackTaskHandle<S>) {
    let mut handle = handle.write().unwrap();
    handle.desc.current = handle.desc.total;
    handle.mark_completed();
  }

  fn start(handle: &ModpackTaskHandle<S>) {
    handle.write().unwrap().mark_started();
  }

  fn fail(&self, error: &str) {
    let handles = self.all();
    let current = handles
      .iter()
      .find(|handle| handle.read().unwrap().status().is_in_progress())
      .or_else(|| {
        handles
          .iter()
          .find(|handle| !handle.read().unwrap().status().is_completed())
      });
    if let Some(handle) = current {
      handle.write().unwrap().mark_failed(error.to_string());
    }
  }
}

impl CampusInstallTasks {
  fn register(app: &AppHandle, instance_id: &str, label: &str) -> Self {
    Self::register_kind(app, instance_id, label, false)
  }

  fn register_kind(app: &AppHandle, instance_id: &str, label: &str, is_update: bool) -> Self {
    let monitor = app.state::<Pin<Box<TaskMonitor>>>();
    let kind = if is_update {
      "modpack-update"
    } else {
      "modpack-install"
    };
    let group = format!(
      "{kind}?{} [{}]@{}",
      label.replace(['?', '@', '\n', '\r'], " "),
      instance_id.replace(['?', '@', '\n', '\r'], " "),
      chrono::Utc::now().timestamp_millis()
    );
    Self::with_handles(group.clone(), is_update, |filename, total| {
      upload_handle(app, monitor.get_new_id(), &group, filename.into(), total)
    })
  }
}

impl AppEnv {
  fn preparing(app: &AppHandle, target: &Target) -> Self {
    let reporter = Self {
      app: app.clone(),
      instance_id: target.instance.id.clone(),
      label: String::new(),
      loader: BaselineLoader {
        loader_type: target.instance.mod_loader.loader_type,
        version: target.instance.mod_loader.version.clone(),
      },
      loader_install_pending: AtomicBool::new(false),
      started: Instant::now(),
      tasks: None,
    };
    reporter.progress(Stage::Preparing);
    reporter
  }

  fn progress(&self, stage: Stage) {
    self.report(Progress {
      stage,
      done: 0,
      total: 1,
      bytes_done: 0,
      bytes_total: 0,
    });
  }
}

#[async_trait]
impl UpdateEnv for AppEnv {
  fn ensure_not_running(&self) -> USTBLResult<()> {
    ensure_not_running(&self.app, &self.instance_id)
  }
  async fn fetch(&self, param: DownloadParam) -> USTBLResult<()> {
    download_param(&self.app, param).await
  }

  async fn switch_loader(&self, loader: &BaselineLoader) -> USTBLResult<()> {
    let instance = current_instance(&self.app, &self.instance_id)?;
    let needs_branch = matches!(
      loader.loader_type,
      ModLoaderType::Forge | ModLoaderType::LegacyForge | ModLoaderType::NeoForge
    );
    let branch = if needs_branch {
      ModLoader {
        loader_type: loader.loader_type,
        version: loader.version.clone(),
        status: ModLoaderStatus::NotDownloaded,
        branch: None,
      }
      .with_branch(&self.app, instance.version.clone())
      .await?
      .branch
    } else {
      None
    };
    switch_mod_loader(
      self.app.clone(),
      self.instance_id.clone(),
      ModLoaderResourceInfo {
        loader_type: loader.loader_type,
        version: loader.version.clone(),
        description: String::new(),
        stable: true,
        branch,
      },
      Some(false),
      true,
    )
    .await?;
    self
      .loader_install_pending
      .store(needs_branch, Ordering::SeqCst);
    Ok(())
  }

  async fn restore_components(&self, pack: &ComponentPack, staged: &Path) -> USTBLResult<()> {
    let instance = current_instance(&self.app, &self.instance_id)?;
    restore_component_profile(&self.app, &instance, &self.loader, pack, staged).await?;
    // The chain carries the loader, so the instance counts as installed.
    let loader = self.loader.clone();
    store_instance(&self.app, &self.instance_id, |instance| {
      instance.mod_loader = ModLoader {
        loader_type: loader.loader_type,
        version: loader.version,
        status: ModLoaderStatus::Installed,
        branch: None,
      };
    })
    .await
  }

  async fn commit_origin(&self, version_id: &str) -> USTBLResult<()> {
    store_instance(&self.app, &self.instance_id, |instance| {
      if let Some(origin) = instance.origin.as_mut() {
        if origin.version_id.is_empty() {
          origin.loader_type = self.loader.loader_type;
        }
        origin.version_id = version_id.to_string();
      }
      instance.modpack_version = Some(self.label.clone());
    })
    .await
  }

  fn report(&self, progress: Progress) {
    if progress.done == 0 {
      log::info!(
        "modpack progress: instance={} stage={:?} elapsed_ms={}",
        self.instance_id, progress.stage, self.started.elapsed().as_millis()
      );
    }
    if let Some(tasks) = &self.tasks {
      if !tasks.is_update {
        if let Ok(instance) = current_instance(&self.app, &self.instance_id) {
          if let Some(mut record) = load_campus_install_record(&instance.version_path) {
            record.progress = Some(progress);
            if let Err(error) = save_campus_install_record(&instance.version_path, &record) {
              log::warn!("cannot save campus install progress: {error:?}");
            }
          }
        }
      }
      let (current, next) = match progress.stage {
        Stage::WaitingClient => (&tasks.waiting_client, None),
        Stage::Preparing => (&tasks.preparing, Some(&tasks.waiting_client)),
        Stage::Locating => (&tasks.locating, Some(&tasks.preparing)),
        Stage::Fetching => (&tasks.fetching, Some(&tasks.locating)),
        Stage::Applying | Stage::Finalizing => (&tasks.applying, Some(&tasks.fetching)),
      };
      if let Some(previous) = next {
        if !(tasks.is_update && progress.stage == Stage::Preparing)
          && !previous.read().unwrap().status().is_completed()
        {
          CampusInstallTasks::complete(previous);
        }
      }
      if progress.stage == Stage::Fetching {
        let mut current = current.write().unwrap();
        current.set_total(progress.bytes_total as i64);
        if current.status().is_waiting() {
          current.mark_started();
        }
        current.set_current(progress.bytes_done as i64);
      } else {
        if progress.stage != Stage::Finalizing && progress.total > 0 {
          current.write().unwrap().set_total(progress.total as i64);
        }
        if current.read().unwrap().status().is_waiting() {
          CampusInstallTasks::start(current);
        }
        if progress.stage != Stage::Finalizing && progress.total > 0 {
          current.write().unwrap().set_current(progress.done as i64);
        }
      }
    }
    let _ = self.app.emit_to(
      "main",
      PROGRESS_EVENT,
      ProgressEvent {
        instance_id: self.instance_id.clone(),
        group: self.tasks.as_ref().map(|tasks| tasks.group.clone()),
        progress,
      },
    );
  }
}

fn check_of(status: CheckStatus, target: &Target) -> UpdateCheck {
  UpdateCheck {
    status,
    follow: target.origin.follow,
    current_label: target.instance.modpack_version.clone(),
    latest_version_id: None,
    latest_label: None,
    reason: None,
    can_roll_back: can_roll_back_last(&target.root),
    local_changes: None,
    unlinked: false,
  }
}

#[tauri::command]
pub async fn check_modpack_update(
  app: AppHandle,
  instance_id: String,
  scan_local: Option<bool>,
) -> USTBLResult<UpdateCheck> {
  let Some(target) = target(&app, &instance_id)? else {
    return Ok(UpdateCheck {
      status: CheckStatus::NotFollowing,
      follow: false,
      current_label: None,
      latest_version_id: None,
      latest_label: None,
      reason: None,
      can_roll_back: false,
      local_changes: None,
      unlinked: false,
    });
  };
  check_unless_installing(&target, check_installed_modpack(&app, &target, scan_local)).await
}

async fn check_installed_modpack(
  app: &AppHandle,
  target: &Target,
  scan_local: Option<bool>,
) -> USTBLResult<UpdateCheck> {
  if !target.origin.follow {
    return Ok(check_of(CheckStatus::Paused, target));
  }
  if !get_instance_game_config(app, &target.instance).version_isolation {
    return Ok(UpdateCheck {
      reason: Some(InstanceError::ModpackUpdateNotIsolated.to_string()),
      ..check_of(CheckStatus::NeedsRepair, target)
    });
  }
  let baseline = match load_baseline(target).await {
    Ok(baseline) => baseline,
    Err(err) => {
      return Ok(UpdateCheck {
        reason: Some(err.0),
        ..check_of(CheckStatus::NeedsRepair, target)
      })
    }
  };
  if target.instance.mod_loader.status != ModLoaderStatus::Installed {
    return Ok(check_of(CheckStatus::WaitingLoader, target));
  }
  match light_check(app, target).await {
    // The status stays `Available`; the count rides along so the local
    // edits can be shown next to the update.
    Ok(LightCheck::Available {
      latest_version_id,
      latest_label,
    }) => Ok(UpdateCheck {
      latest_version_id: Some(latest_version_id),
      latest_label: Some(latest_label),
      local_changes: if scan_local == Some(true) {
        Some(count_local_changes(&target.root, &baseline).await?)
      } else {
        None
      },
      ..check_of(CheckStatus::Available, target)
    }),
    Ok(LightCheck::UpToDate) => {
      let mut check = check_of(CheckStatus::UpToDate, target);
      if scan_local == Some(true) {
        let changed = count_local_changes(&target.root, &baseline).await?;
        check.local_changes = Some(changed);
        if changed > 0 {
          check.status = CheckStatus::LocalChanges;
        }
      }
      Ok(check)
    }
    Ok(LightCheck::LineGone) => Ok(UpdateCheck {
      reason: Some(InstanceError::ModpackUpdateLineGone.to_string()),
      ..check_of(CheckStatus::Unknown, target)
    }),
    Ok(LightCheck::ProjectGone) => {
      // The project listing already confirmed the deletion, so the identity
      // is dropped on this same check.
      match unlink_origin(app, &target.instance.id, target).await {
        Ok(()) => {
          // The ledger groups rows by the origin in the instance list, so it
          // has to hear about the lost identity right away.
          refresh_and_update_instances(app, false).await;
          Ok(UpdateCheck {
            unlinked: true,
            ..check_of(CheckStatus::NotFollowing, target)
          })
        }
        Err(error) => Ok(UpdateCheck {
          reason: Some(error.0),
          ..check_of(CheckStatus::Unknown, target)
        }),
      }
    }
    Err(err) => Ok(UpdateCheck {
      reason: Some(err.0),
      ..check_of(CheckStatus::Unknown, target)
    }),
  }
}

async fn check_unless_installing(
  target: &Target,
  check: impl std::future::Future<Output = USTBLResult<UpdateCheck>>,
) -> USTBLResult<UpdateCheck> {
  if has_campus_install(&target.root) {
    return Ok(check_of(CheckStatus::Installing, target));
  }
  check.await
}

/// What a light check found: whether there is a newer version, or why that
/// cannot be said. It never downloads a manifest.
enum LightCheck {
  UpToDate,
  Available {
    latest_version_id: String,
    latest_label: String,
  },
  LineGone,
  ProjectGone,
}

/// Ask the platform what the newest version is, cheaply. Campus instances are
/// answered from the project listing (`GET /projects/{ref}`), which also
/// carries the deletion proof on its 404; the other platforms query their
/// version lists as before.
async fn light_check(app: &AppHandle, target: &Target) -> USTBLResult<LightCheck> {
  if target.origin.platform != OriginPlatform::Campus {
    return match latest_version(app, &target.origin).await? {
      Some(latest) => Ok(LightCheck::Available {
        latest_version_id: latest.version_id,
        latest_label: latest.label,
      }),
      None => Ok(LightCheck::UpToDate),
    };
  }
  let Some(project) =
    super::sources::campus_project_for_light_check(app, &target.origin.project_id).await?
  else {
    return Ok(LightCheck::ProjectGone);
  };
  Ok(
    match super::sources::campus_line_status(&project.lines, &target.origin) {
      super::sources::CampusLineStatus::Current => LightCheck::UpToDate,
      super::sources::CampusLineStatus::Newer { id, version_number } => LightCheck::Available {
        latest_version_id: id.to_string(),
        latest_label: version_number,
      },
      super::sources::CampusLineStatus::LineGone => LightCheck::LineGone,
    },
  )
}

/// Drop the identity and baseline; the game files and isolation stay.
async fn unlink_origin(app: &AppHandle, instance_id: &str, target: &Target) -> USTBLResult<()> {
  ensure_unlink_safe(&target.root)?;
  fs::remove_file(target.root.join(BASELINE_FILE_NAME)).ok();
  super::ignores::remove(&target.root);
  plan_cache::clear_plan_cache(&target.root);
  store_instance(app, instance_id, |instance| {
    instance.origin = None;
    instance.modpack_version = None;
  })
  .await?;
  invalidate_scan_cache(&target.root);
  Ok(())
}

/// Detach the instance from its modpack: identity and baseline go, the game
/// files stay. Refused while an update transaction is unfinished.
#[tauri::command]
pub async fn unlink_modpack_origin(app: AppHandle, instance_id: String) -> USTBLResult<()> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  unlink_origin(&app, &instance_id, &target).await?;
  refresh_and_update_instances(&app, false).await;
  Ok(())
}

/// Take back the previous update. Returns whether there was one to take back.
#[tauri::command]
pub async fn rollback_modpack_update(app: AppHandle, instance_id: String) -> USTBLResult<bool> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  ensure_not_running(&app, &instance_id)?;
  let root = target.root.clone();
  let check_app = app.clone();
  let done = tokio::task::spawn_blocking({
    let root = root.clone();
    move || roll_back_last_checked(&root, || ensure_not_running(&check_app, &instance_id))
  })
  .await
  .map_err(|e| crate::error::USTBLError(e.to_string()))??;
  if done {
    plan_cache::clear_plan_cache(&root);
    refresh_and_update_instances(&app, false).await;
  }
  Ok(done)
}

/// The preview task of each instance. A new preview aborts the previous one.
fn prepare_handles() -> &'static Mutex<HashMap<String, tokio::task::AbortHandle>> {
  static HANDLES: OnceLock<Mutex<HashMap<String, tokio::task::AbortHandle>>> = OnceLock::new();
  HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cancel_prepare(instance_id: &str) {
  if let Ok(mut handles) = prepare_handles().lock() {
    if let Some(handle) = handles.remove(instance_id) {
      handle.abort();
    }
  }
}

/// Cancel the preview of this instance, if any. The preview answers with
/// `ModpackUpdatePrepareCancelled`, which the dialog ignores.
#[tauri::command]
pub async fn cancel_modpack_update_prepare(instance_id: String) -> USTBLResult<()> {
  cancel_prepare(&instance_id);
  Ok(())
}

/// Pause or resume following. A paused instance is never checked; its identity
/// and baseline stay, so resuming picks up where it left off.
#[tauri::command]
pub async fn set_modpack_follow(
  app: AppHandle,
  instance_id: String,
  follow: bool,
) -> USTBLResult<()> {
  target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  store_instance(&app, &instance_id, |instance| {
    if let Some(origin) = instance.origin.as_mut() {
      origin.follow = follow;
    }
  })
  .await
}

/// The managed files changed here since the modpack installed them, with
/// line diffs for text files. Works offline: only the local record is used.
#[tauri::command]
pub async fn list_modpack_local_changes(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<LocalChange>> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let baseline = load_baseline(&target).await?;
  local_changes(&target.root, &baseline, true).await
}

/// Map the ignore store's refusals onto the update errors: a request that no
/// longer matches the files is a stale one.
fn ignore_error(error: super::ignores::IgnoreError) -> crate::error::USTBLError {
  match error {
    super::ignores::IgnoreError::Mismatch => InstanceError::ModpackUpdateStale.into(),
    super::ignores::IgnoreError::Io(message) => crate::error::USTBLError(message),
  }
}

/// Keep a local change as the player's own: one spot of a text file (`range`,
/// as the local-change list gave it) or the whole file (`None`). Takes the
/// update lock, so it never runs inside an update.
#[tauri::command]
pub async fn ignore_modpack_local_change(
  app: AppHandle,
  instance_id: String,
  path: String,
  baseline_sha1: String,
  range: Option<super::ignores::Region>,
) -> USTBLResult<()> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let _guard = UpdateGuard::acquire(&target.root)?;
  let baseline = load_baseline(&target).await?;
  let root = target.root.clone();
  tokio::task::spawn_blocking(move || {
    super::ignores::add(&root, &baseline, &path, &baseline_sha1, range)
  })
  .await
  .map_err(|e| crate::error::USTBLError(e.to_string()))?
  .map_err(ignore_error)?;
  plan_cache::clear_plan_cache(&target.root);
  Ok(())
}

/// The ignored spots and files of this instance, for the undo list.
#[tauri::command]
pub async fn list_modpack_ignores(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<super::ignores::IgnoredItem>> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let baseline = load_baseline(&target).await?;
  let root = target.root.clone();
  tokio::task::spawn_blocking(move || super::ignores::list(&root, &baseline))
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()).into())
}

/// Take an ignore back: one spot (`range`) or everything recorded for the
/// file (`None`).
#[tauri::command]
pub async fn unignore_modpack_local_change(
  app: AppHandle,
  instance_id: String,
  path: String,
  range: Option<super::ignores::Region>,
) -> USTBLResult<()> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let _guard = UpdateGuard::acquire(&target.root)?;
  let baseline = load_baseline(&target).await?;
  let root = target.root.clone();
  tokio::task::spawn_blocking(move || super::ignores::undo(&root, &baseline, &path, range))
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()))?
    .map_err(ignore_error)?;
  plan_cache::clear_plan_cache(&target.root);
  Ok(())
}

/// Reveal a managed file in the system file manager for external editing.
#[tauri::command]
pub fn reveal_modpack_conflict_file(
  app: AppHandle,
  instance_id: String,
  relative_path: String,
) -> USTBLResult<()> {
  let instance = current_instance(&app, &instance_id)?;
  let relative = super::remote::safe_relative_path(&relative_path)?;
  reveal_item_in_dir(instance.version_path.join(relative)).map_err(crate::error::USTBLError::from)
}

/// Fetch the new version and compare it with the instance. Changes nothing
/// except downloading the modpack archive into the update folder. The work
/// runs as a registered task so the dialog can cancel it; a new preview of
/// the same instance cancels the previous one.
#[tauri::command]
pub async fn prepare_modpack_update(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<UpdatePlan> {
  // A new preview supersedes a previous one of this instance.
  cancel_prepare(&instance_id);
  let app_clone = app.clone();
  let id = instance_id.clone();
  let spawned = tokio::spawn(async move { prepare_inner(&app_clone, &id).await });
  let abort_id = spawned.abort_handle().id();
  if let Ok(mut handles) = prepare_handles().lock() {
    handles.insert(instance_id.clone(), spawned.abort_handle());
  }
  let result = spawned.await;
  // Only remove the handle if it is still this task's: a newer preview may
  // have replaced it while this one was being cancelled or awaited.
  if let Ok(mut handles) = prepare_handles().lock() {
    if handles
      .get(&instance_id)
      .is_some_and(|held| held.id() == abort_id)
    {
      handles.remove(&instance_id);
    }
  }
  match result {
    Ok(result) => result,
    Err(join_error) => Err(if join_error.is_cancelled() {
      InstanceError::ModpackUpdatePrepareCancelled.into()
    } else {
      crate::error::USTBLError(join_error.to_string())
    }),
  }
}

/// Compare the newest version with the instance, hitting the cache when the
/// baseline and the local files are unchanged since the last plan.
async fn prepare_inner(app: &AppHandle, instance_id: &str) -> USTBLResult<UpdatePlan> {
  let target = target(app, instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let _guard = PlanGuard::acquire(&target.root)?;
  if has_campus_install(&target.root) {
    return Err(InstanceError::ModpackInstallInterrupted.into());
  }
  prepare_target(app, instance_id, &target).await
}

async fn prepare_target(
  app: &AppHandle,
  instance_id: &str,
  target: &Target,
) -> USTBLResult<UpdatePlan> {
  ensure_not_running(app, instance_id)?;
  let _reporter = AppEnv::preparing(app, target);
  let baseline = load_baseline(&target).await?;
  if recover_checked(&target.root, || ensure_not_running(app, instance_id))? != Recovery::Nothing {
    // Recovery moved the files, so any cached plan describes another state.
    plan_cache::clear_plan_cache(&target.root);
  }
  let latest = match light_check(app, &target).await? {
    LightCheck::Available {
      latest_version_id,
      latest_label,
    } => LatestVersion {
      version_id: latest_version_id,
      label: latest_label,
      // Only the id and label are carried over; the manifest is fetched
      // below, and the version the line answers with wins.
      payload: None,
    },
    LightCheck::UpToDate => return Err(InstanceError::ModpackUpdateStale.into()),
    LightCheck::LineGone => return Err(InstanceError::ModpackUpdateLineGone.into()),
    LightCheck::ProjectGone => return Err(InstanceError::ModpackUpdateLineGone.into()),
  };

  // Hit: version, install record and on-disk stat fingerprint all match.
  let from_version_id = target.origin.version_id.clone();
  let baseline_sha1 = plan_cache::baseline_sha1(&baseline);
  if let Some(cache) = plan_cache::load_plan_cache(&target.root) {
    if cache.to_version_id == latest.version_id
      && cache.from_version_id == from_version_id
      && cache.baseline_sha1 == baseline_sha1
    {
      let root = target.root.clone();
      let paths = cache.paths.clone();
      let fingerprint =
        tokio::task::spawn_blocking(move || plan_cache::fingerprint_paths(&root, &paths))
          .await
          .map_err(|e| crate::error::USTBLError(e.to_string()))??;
      if fingerprint == cache.fingerprint {
        return Ok(cache.plan);
      }
    }
  }

  // Drop leftovers of an interrupted previous run before hashing anything.
  cleanup_preview_leftovers(&target.root);

  // The line answers the manifest; its version is authoritative for the plan
  // even when it is newer than what the check just saw.
  let latest = resolve_latest_for_plan(app, &target).await?;
  let prepared = plan_prepared(app, &target, &baseline, &latest).await?;
  // The fingerprint is taken over the same paths the plan covers.
  let paths = plan_cache::plan_paths(&baseline, &prepared.remote);
  let plan = plan_of(&target.origin, prepared);
  let fingerprint = {
    let root = target.root.clone();
    let paths = paths.clone();
    tokio::task::spawn_blocking(move || plan_cache::fingerprint_paths(&root, &paths))
      .await
      .map_err(|e| crate::error::USTBLError(e.to_string()))??
  };
  let cache = PlanCache::new(from_version_id, baseline_sha1, paths, fingerprint, plan.clone());
  if let Err(error) = plan_cache::write_plan_cache(&target.root, &cache) {
    log::warn!("cannot write modpack plan cache: {error:?}");
  }
  Ok(plan)
}

/// Run one comparison: hash the local files once, then reuse those hashes for
/// the conflict setup and the merge step.
async fn plan_prepared(
  app: &AppHandle,
  target: &Target,
  baseline: &ModpackBaseline,
  latest: &LatestVersion,
) -> USTBLResult<PlannedRun> {
  let remote = resolve_remote(
    app,
    &target.origin,
    &target.root,
    &target.instance.name,
    latest,
  )
  .await?;
  if requires_reinstall(baseline, &remote, is_fresh(&target.origin)) {
    return Ok(PlannedRun {
      version_id: remote.version_id.clone(),
      label: remote.label.clone(),
      diff: Default::default(),
      plan_hash: String::new(),
      loader_change: None,
      reinstall_required: true,
      remote,
    });
  }
  let loader_change = compute_loader_change(&baseline.loader, &remote.loader);
  let compared = compare(&target.root, baseline, &remote).await?;
  if !compared.both_variants.is_empty() {
    log::info!(
      "modpack update plan refused: instance={} both variants on disk: {}",
      target.instance.id,
      compared.both_variants.join(", "),
    );
    return Err(InstanceError::ModpackUpdateBothVariants.into());
  }
  // Missing text bases are best effort; if any came back, the normalized
  // comparison and the merge blocks change, so compare once more.
  let compared = if recover_base_copies(app, target, baseline, &compared.diff.conflicts).await {
    let again = compare(&target.root, baseline, &remote).await?;
    if !again.both_variants.is_empty() {
      return Err(InstanceError::ModpackUpdateBothVariants.into());
    }
    again
  } else {
    compared
  };
  let remote_contents =
    materialize_text_candidates(app, target, &remote, &compared.diff.conflicts).await?;
  let prepared = finish(compared, &target.root, &remote, &remote_contents)?;
  Ok(PlannedRun {
    version_id: remote.version_id.clone(),
    label: remote.label.clone(),
    diff: prepared.diff,
    plan_hash: prepared.plan_hash,
    loader_change,
    reinstall_required: false,
    remote,
  })
}

/// A full comparison run: the diff and everything applying the plan needs.
struct PlannedRun {
  version_id: String,
  label: String,
  diff: super::diff::Diff,
  plan_hash: String,
  loader_change: Option<LoaderChange>,
  reinstall_required: bool,
  remote: super::remote::RemoteModpack,
}

/// Drop files an interrupted preview could have left behind.
fn cleanup_preview_leftovers(root: &Path) {
  let update = root.join(super::remote::UPDATE_DIR);
  let _ = fs::remove_dir_all(update.join("remote-preview"));
  if let Ok(entries) = fs::read_dir(&update) {
    for entry in entries.flatten() {
      if entry
        .file_name()
        .to_string_lossy()
        .starts_with("base-recovery-")
      {
        let _ = fs::remove_dir_all(entry.path());
      }
    }
  }
}

/// Ask the line for the manifest of the newest version; the plan is computed
/// against whatever the line answers, not against the check's answer.
async fn resolve_latest_for_plan(app: &AppHandle, target: &Target) -> USTBLResult<LatestVersion> {
  latest_version(app, &target.origin)
    .await?
    .ok_or(InstanceError::ModpackUpdateStale.into())
}

/// An instance created for a modpack that has not installed a version yet.
fn is_fresh(origin: &InstanceOrigin) -> bool {
  origin.version_id.is_empty()
}

fn fresh_identity_loader(
  origin: &InstanceOrigin,
  game_version: &str,
  loader_type: ModLoaderType,
) -> USTBLResult<Option<ModLoaderType>> {
  if game_version != origin.game_version {
    return Err(InstanceError::ModpackUpdateLineMismatch.into());
  }
  Ok(
    (loader_type != origin.loader_type || origin.loader_type == ModLoaderType::Unknown)
      .then_some(loader_type),
  )
}

/// A plan fast-forwards when nothing but the baseline moves. A `Retire`
/// change only rewrites the entry (the file itself stays), so it does not
/// count; everything else, a conflict or a loader change does.
pub(crate) fn fast_forward<'a>(
  mut changes: impl Iterator<Item = &'a super::diff::FileChange>,
  conflicts: usize,
  loader_change: bool,
) -> bool {
  !changes.any(|change| change.kind != super::diff::ChangeKind::Retire)
    && conflicts == 0
    && !loader_change
}

/// The plan the dialog shows and the cache stores. A plan that needs a
/// reinstall is never a fast forward.
fn plan_of(origin: &InstanceOrigin, prepared: PlannedRun) -> UpdatePlan {
  UpdatePlan {
    from_version_id: origin.version_id.clone(),
    to_version_id: prepared.version_id.clone(),
    to_label: prepared.label.clone(),
    fast_forward: !prepared.reinstall_required
      && fast_forward(
        prepared
          .diff
          .changes
          .iter()
          .chain(&prepared.diff.auto_merged),
        prepared.diff.conflicts.len(),
        prepared.loader_change.is_some(),
      ),
    changes: prepared.diff.changes,
    conflicts: prepared.diff.conflicts,
    auto_merged: prepared.diff.auto_merged,
    loader_change: prepared.loader_change,
    reinstall_required: prepared.reinstall_required,
    plan_hash: prepared.plan_hash,
  }
}

/// Everything `install_campus_modpack` needs from one fetch of the version
/// detail: the plan it shows in the log, and the plan the apply runs. An
/// install answers every conflict with the remote side, so the plan the
/// dialog would show is never built; both are computed once, from the same
/// remote, and the apply below never asks the platform again.
struct InstallPlan {
  plan: UpdatePlan,
  prepared: Prepared,
  remote: RemoteModpack,
}

/// Compare the newest version against the fresh instance and compute the
/// apply's plan in one pass. The baseline is empty, so every shipped file is
/// an addition and every file in the way is answered with the remote side;
/// the conflict and merge steps of `plan_prepared` can never fire, which is
/// what makes one comparison enough. Locating the shipped files in the packs
/// starts the moment the manifest arrives and runs while the instance is
/// hashed; `apply` reuses what it located.
async fn plan_fresh_install(
  app: &AppHandle,
  target: &Target,
  reporter: &AppEnv,
) -> USTBLResult<InstallPlan> {
  debug_assert!(is_fresh(&target.origin));
  let details_started = Instant::now();
  let install_target = load_campus_install_record(&target.root)
    .and_then(|record| record.target)
    .filter(|_| {
      matches!(
        target.origin.platform,
        OriginPlatform::Modrinth | OriginPlatform::CurseForge
      )
    });
  let latest = if let Some(install_target) = install_target {
    latest_from_install_target(install_target)
  } else {
    // The listing answers first, so a line that disappeared between the
    // install click and here says so before the version detail is fetched.
    match light_check(app, target).await? {
      LightCheck::Available { .. } => {}
      LightCheck::UpToDate => return Err(InstanceError::ModpackUpdateStale.into()),
      LightCheck::LineGone | LightCheck::ProjectGone => {
        return Err(InstanceError::ModpackUpdateLineGone.into());
      }
    }
    resolve_latest_for_plan(app, target).await?
  };
  let baseline = ModpackBaseline::build(
    &target.origin,
    BaselineLoader {
      loader_type: target.instance.mod_loader.loader_type,
      version: target.instance.mod_loader.version.clone(),
    },
    Vec::new(),
  );
  let remote = resolve_remote(
    app,
    &target.origin,
    &target.root,
    &target.instance.name,
    &latest,
  )
  .await?;
  log::info!(
    "modpack install details: instance={} version={} elapsed_ms={}",
    target.instance.id, remote.version_id, details_started.elapsed().as_millis()
  );
  if requires_reinstall(&baseline, &remote, true) {
    let plan = UpdatePlan {
      from_version_id: target.origin.version_id.clone(),
      to_version_id: remote.version_id.clone(),
      to_label: remote.label.clone(),
      changes: Vec::new(),
      conflicts: Vec::new(),
      auto_merged: Vec::new(),
      loader_change: None,
      reinstall_required: true,
      fast_forward: false,
      plan_hash: String::new(),
    };
    return Ok(InstallPlan {
      plan,
      prepared: Prepared {
        diff: Diff::default(),
        plan_hash: String::new(),
        merged_contents: BTreeMap::new(),
        conflict_sources: BTreeMap::new(),
        next_baseline: Vec::new(),
        variants: BTreeMap::new(),
        prune_dirs: BTreeMap::new(),
        ignores: super::ignores::Ignores::default(),
        ignores_on_disk: super::ignores::Ignores::default(),
      },
      remote,
    });
  }
  // The manifest names every file the install writes; locating them can run
  // while the instance is hashed and compared. The fetch reuses what the
  // sessions located here. When the comparison wins, the wait for the locate
  // is the locating stage.
  let wanted = {
    let shipped: Vec<&super::remote::RemoteFile> = remote
      .files
      .iter()
      .chain(
        remote
          .component_pack
          .as_ref()
          .into_iter()
          .flat_map(|pack| pack.files.iter()),
      )
      .collect();
    pack_wanted(&shipped)
  };
  let locating = async move {
    for (session, wanted) in wanted {
      session.prepare(&wanted).await.map_err(wrap_locate)?;
    }
    Ok::<_, crate::error::USTBLError>(())
  };
  let planning = async {
    let started = Instant::now();
    let compared = compare(&target.root, &baseline, &remote).await?;
    if !compared.both_variants.is_empty() {
      log::info!(
        "modpack install refused: instance={} both variants on disk: {}",
        target.instance.id,
        compared.both_variants.join(", "),
      );
      return Err(InstanceError::ModpackUpdateBothVariants.into());
    }
    let prepared = finish(compared, &target.root, &remote, &BTreeMap::new())?;
    log::info!(
      "modpack install compare: instance={} elapsed_ms={}",
      target.instance.id, started.elapsed().as_millis()
    );
    reporter.progress(Stage::Locating);
    Ok::<_, crate::error::USTBLError>(prepared)
  };
  let (prepared, ()) = tokio::try_join!(planning, locating)?;
  let loader_change = compute_loader_change(&baseline.loader, &remote.loader);
  let plan = UpdatePlan {
    from_version_id: target.origin.version_id.clone(),
    to_version_id: remote.version_id.clone(),
    to_label: remote.label.clone(),
    fast_forward: fast_forward(
      prepared
        .diff
        .changes
        .iter()
        .chain(&prepared.diff.auto_merged),
      prepared.diff.conflicts.len(),
      loader_change.is_some(),
    ),
    changes: prepared.diff.changes.clone(),
    conflicts: prepared.diff.conflicts.clone(),
    auto_merged: prepared.diff.auto_merged.clone(),
    loader_change,
    reinstall_required: false,
    plan_hash: prepared.plan_hash.clone(),
  };
  Ok(InstallPlan {
    plan,
    prepared,
    remote,
  })
}

/// Fetch the installed version's copy of conflicting text files whose cached
/// base is missing, so the conflict can be resolved block by block. Campus
/// packs only; best effort, a failure leaves whole-file choices. Returns
/// whether anything was actually recovered.
async fn recover_base_copies(
  app: &AppHandle,
  target: &Target,
  baseline: &ModpackBaseline,
  conflicts: &[Conflict],
) -> bool {
  use crate::instance::helpers::modpack::base_store;
  use crate::instance::models::misc::OriginPlatform;
  if target.origin.platform != OriginPlatform::Campus || is_fresh(&target.origin) {
    return false;
  }
  let wanted: Vec<_> = baseline
    .files
    .iter()
    .filter(|file| {
      text::is_text_path(&file.path, &[])
        && conflicts.iter().any(|c| c.path == file.path)
        && base_store::load(&target.root, &file.sha1)
          .ok()
          .flatten()
          .is_none()
    })
    .collect();
  if wanted.is_empty() {
    return false;
  }
  let attempt = async {
    let remote =
      super::sources::campus_base_remote(app, &target.origin, &target.root, &target.instance.name)
        .await?;
    let env = AppEnv {
      app: app.clone(),
      instance_id: target.instance.id.clone(),
      label: remote.label.clone(),
      loader: remote.loader.clone(),
      loader_install_pending: AtomicBool::new(false),
      started: Instant::now(),
      tasks: None,
    };
    let preview = target
      .root
      .join(super::remote::UPDATE_DIR)
      .join(format!("base-recovery-{}", uuid::Uuid::new_v4()));
    let files: Vec<_> = remote
      .files
      .iter()
      .filter(|f| {
        wanted
          .iter()
          .any(|b| b.path == f.path && b.sha1 == f.key.sha1)
      })
      .collect();
    let recovered = async {
      let mut recovered = false;
      env.progress(Stage::Locating);
      prepare_packs(&files).await?;
      for file in files {
        let dest = preview.join(&file.path);
        materialize_remote_file(&env, dest.clone(), file).await?;
        let bytes = tokio::fs::read(dest).await?;
        if let Some(base) = wanted.iter().find(|b| b.path == file.path) {
          base_store::store(&target.root, base, &bytes)?;
          recovered = true;
        }
      }
      Ok::<_, crate::error::USTBLError>(recovered)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(preview).await;
    recovered
  }
  .await;
  match attempt {
    Ok(recovered) => recovered,
    Err(error) => {
      log::warn!("cannot recover modpack text baseline: {error:?}");
      false
    }
  }
}

/// A merge keeps the local line endings. When nothing of the local side is
/// left, write the published bytes instead: the file then matches the new
/// baseline and the local scan does not report it as changed.
fn remote_if_equivalent(path: &str, merged: Vec<u8>, remote: &[u8]) -> Vec<u8> {
  match (
    text::normalize_text(path, &merged),
    text::normalize_text(path, remote),
  ) {
    (Some(merged_text), Some(remote_text)) if merged_text == remote_text => remote.to_vec(),
    _ => merged,
  }
}

/// Describe how each conflict was answered, for the apply log: the whole
/// file, single blocks (`path#index`), or the default keep-local.
pub(super) fn describe_conflict_resolutions(
  diff: &super::diff::Diff,
  choices: &BTreeMap<String, ConflictChoice>,
) -> Vec<String> {
  let mut described = Vec::new();
  for conflict in &diff.conflicts {
    if conflict.hunks.is_empty() {
      let choice = choices.get(&conflict.path).copied().unwrap_or_default();
      described.push(format!("{}:file={choice:?}", conflict.path));
    } else {
      let selected: Vec<String> = (0..conflict.hunks.len())
        .map(|index| {
          let choice = choices
            .get(&format!("{}#{index}", conflict.path))
            .copied()
            .unwrap_or_default();
          format!("#{index}={choice:?}")
        })
        .collect();
      described.push(format!("{}:hunks[{}]", conflict.path, selected.join(",")));
    }
  }
  described
}

pub(super) fn selected_hunk_contents(
  prepared: &super::plan::Prepared,
  choices: &mut BTreeMap<String, ConflictChoice>,
) -> USTBLResult<BTreeMap<String, Vec<u8>>> {
  let mut result = prepared.merged_contents.clone();
  for (path, (base, local, remote)) in &prepared.conflict_sources {
    let hunk_count = prepared
      .diff
      .conflicts
      .iter()
      .find(|conflict| conflict.path == *path)
      .map(|conflict| conflict.hunks.len())
      .unwrap_or(0);
    let selected: Vec<usize> = (0..)
      .take(hunk_count)
      .filter(|index| choices.get(&format!("{path}#{index}")) == Some(&ConflictChoice::TakeRemote))
      .collect();
    if hunk_count == 0 {
      continue;
    }
    choices.insert(path.clone(), ConflictChoice::TakeRemote);
    let merged = merge_with_choices(path, base, local, remote, &selected)
      .map_err(|_| InstanceError::ModpackUpdateMergeInvalid)?;
    result.insert(path.clone(), remote_if_equivalent(path, merged, remote));
  }
  Ok(result)
}

async fn materialize_text_candidates(
  app: &AppHandle,
  target: &Target,
  remote: &super::remote::RemoteModpack,
  conflicts: &[Conflict],
) -> USTBLResult<BTreeMap<String, Vec<u8>>> {
  let wanted: BTreeMap<_, _> = conflicts
    .iter()
    .filter(|conflict| {
      conflict.kind == super::diff::ConflictKind::ModifiedLocallyChangedRemotely
        && text::is_text_path(&conflict.path, &[])
    })
    .map(|conflict| (conflict.path.as_str(), conflict))
    .collect();
  if wanted.is_empty() {
    return Ok(BTreeMap::new());
  }
  let env = AppEnv {
    app: app.clone(),
    instance_id: target.instance.id.clone(),
    label: remote.label.clone(),
    loader: remote.loader.clone(),
    loader_install_pending: AtomicBool::new(false),
    started: Instant::now(),
    tasks: None,
  };
  let preview = target.root.join(super::remote::UPDATE_DIR).join("remote-preview");
  let _ = tokio::fs::remove_dir_all(&preview).await;
  let mut result = BTreeMap::new();
  let candidates: Vec<_> = remote
    .files
    .iter()
    .filter(|file| wanted.contains_key(file.path.as_str()))
    .collect();
  env.progress(Stage::Locating);
  prepare_packs(&candidates).await?;
  for file in candidates {
    if !wanted.contains_key(file.path.as_str()) {
      continue;
    }
    let dest = preview.join(&file.path);
    materialize_remote_file(&env, dest.clone(), file).await?;
    let bytes = tokio::fs::read(&dest).await?;
    if text::is_text_path(&file.path, &bytes) {
      result.insert(file.path.clone(), bytes);
    }
  }
  let _ = tokio::fs::remove_dir_all(&preview).await;
  Ok(result)
}

/// Apply the plan the player confirmed. `choices` answers conflicts by path;
/// anything unanswered keeps the local file.
#[tauri::command]
pub async fn apply_modpack_update(
  app: AppHandle,
  instance_id: String,
  plan_hash: String,
  choices: BTreeMap<String, ConflictChoice>,
) -> USTBLResult<UpdateStarted> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  let guard = PlanGuard::acquire(&target.root)?;
  if has_campus_install(&target.root) {
    return Err(InstanceError::ModpackInstallInterrupted.into());
  }
  ensure_not_running(&app, &instance_id)?;
  let tasks = CampusInstallTasks::register_kind(&app, &instance_id, &target.instance.name, true);
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  monitor.register_modpack_update_group(
    &instance_id,
    &tasks.group,
    &tasks.all().into_iter().cloned().collect::<Vec<_>>(),
  );
  let group = tasks.group.clone();
  let anchor = tasks.applying.read().unwrap().desc.task_id;
  let app_for_job = app.clone();
  let group_for_job = group.clone();
  let (start, ready) = tokio::sync::oneshot::channel::<()>();
  let job = tauri::async_runtime::spawn(async move {
    let _guard = guard;
    if ready.await.is_err() {
      return;
    }
    let mut result = run_modpack_update_task(apply_modpack_update_inner(
      app_for_job.clone(),
      instance_id.clone(),
      plan_hash.clone(),
      choices,
      tasks.clone(),
    ))
    .await;
    if result.is_err() {
      log::warn!("modpack update failed: {:?}", result.as_ref().err());
      // An unwound worker may have left a journal. Recover it before releasing
      // the plan claim, just as the next opening of the dialog would do.
      if let Err(error) = recover_checked(&target.root, || {
        ensure_not_running(&app_for_job, &instance_id)
      }) {
        log::warn!("modpack update recovery failed: {error:?}");
        result = Err(error);
      }
      refresh_and_update_instances(&app_for_job, false).await;
    }
    plan_cache::clear_plan_cache(&target.root);
    tasks.finish(result.as_ref().err().map(|error| error.0.as_str()));
    app_for_job
      .state::<Pin<Box<TaskMonitor>>>()
      .finish_job_group(&group_for_job, anchor, result.is_ok());
    drop(_guard);
    let (outcome, error) = match result {
      Ok(outcome) => (Some(outcome), None),
      Err(error) => (None, Some(error.0)),
    };
    let _ = app_for_job.emit_to(
      "main",
      "modpack-update:finished",
      ModpackUpdateFinished {
        instance_id,
        instance_name: target.instance.name,
        group: group_for_job,
        plan_hash,
        outcome,
        error,
      },
    );
  });
  monitor.bind_job(anchor, job);
  let _ = start.send(());
  Ok(UpdateStarted { group })
}

async fn run_modpack_update_task(
  task: impl std::future::Future<Output = USTBLResult<UpdateOutcome>> + Send + 'static,
) -> USTBLResult<UpdateOutcome> {
  tokio::spawn(task).await.map_err(|error| {
    log::warn!("modpack update task terminated: {error:?}");
    crate::error::USTBLError::from(InstanceError::ModpackUpdateTaskFailed)
  })?
}

async fn apply_modpack_update_inner(
  app: AppHandle,
  instance_id: String,
  plan_hash: String,
  choices: BTreeMap<String, ConflictChoice>,
  tasks: CampusInstallTasks,
) -> USTBLResult<UpdateOutcome> {
  // What the player answered, before anything is checked: a Stale answer
  // below must not take the record of the choices with it. Paths, hunk
  // indexes and sides only; never file contents.
  {
    let received: Vec<String> = choices
      .iter()
      .map(|(key, choice)| format!("{key}={choice:?}"))
      .collect();
    log::info!(
      "modpack update apply: instance={instance_id} plan_hash={plan_hash} choices=[{}]",
      received.join(","),
    );
  }
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  ensure_not_running(&app, &instance_id)?;
  let mut reporter = AppEnv::preparing(&app, &target);
  reporter.tasks = Some(tasks);
  reporter.progress(Stage::Preparing);
  let baseline = load_baseline(&target).await?;
  recover_checked(&target.root, || ensure_not_running(&app, &instance_id))?;
  let latest = latest_version(&app, &target.origin)
    .await?
    .ok_or(InstanceError::ModpackUpdateStale)?;
  let remote = resolve_remote(
    &app,
    &target.origin,
    &target.root,
    &target.instance.name,
    &latest,
  )
  .await?;
  let fresh = is_fresh(&target.origin);
  if requires_reinstall(&baseline, &remote, fresh) {
    return Err(InstanceError::ModpackUpdateReinstallRequired.into());
  }
  // A fresh instance has no chain yet; the engine restores the version's own.
  let restore_components = fresh && remote.component_pack.is_some();
  let components = if restore_components {
    remote.components.clone()
  } else {
    baseline.component_refs()
  };
  let compared = compare(&target.root, &baseline, &remote).await?;
  if !compared.both_variants.is_empty() {
    log::info!(
      "modpack update apply refused: instance={instance_id} both variants on disk: {}",
      compared.both_variants.join(", "),
    );
    return Err(InstanceError::ModpackUpdateBothVariants.into());
  }
  let compared = if recover_base_copies(&app, &target, &baseline, &compared.diff.conflicts).await {
    let again = compare(&target.root, &baseline, &remote).await?;
    if !again.both_variants.is_empty() {
      return Err(InstanceError::ModpackUpdateBothVariants.into());
    }
    again
  } else {
    compared
  };
  let remote_contents =
    materialize_text_candidates(&app, &target, &remote, &compared.diff.conflicts).await?;
  // The new text bytes fetched for the merge; `apply` caches them so a file
  // that ends up differing from the new version (merged, or kept local) still
  // has a base to diff against. Text files only; `store` re-validates.
  let base_texts = remote_contents.clone();
  let prepared = finish(compared, &target.root, &remote, &remote_contents)?;
  if prepared.plan_hash != plan_hash {
    // The cached plan the player looked at describes another state; drop it
    // so a retry re-plans instead of serving it again.
    plan_cache::clear_plan_cache(&target.root);
    log::info!(
      "modpack update apply stale: instance={instance_id} expected plan_hash={} got plan_hash={plan_hash}",
      prepared.plan_hash,
    );
    return Err(InstanceError::ModpackUpdateStale.into());
  }

  let env = AppEnv {
    app: app.clone(),
    instance_id: instance_id.clone(),
    label: remote.label.clone(),
    loader: remote.loader.clone(),
    loader_install_pending: AtomicBool::new(false),
    started: reporter.started,
    tasks: reporter.tasks,
  };
  let version_id = remote.version_id.clone();
  let mut apply_choices = choices.clone();
  let merged_contents = selected_hunk_contents(&prepared, &mut apply_choices)?;
  // How every conflict was settled. Paths, hunk indexes and sides only.
  log::info!(
    "modpack update apply resolutions: instance={instance_id} plan_hash={plan_hash} resolutions=[{}]",
    describe_conflict_resolutions(&prepared.diff, &apply_choices).join(","),
  );
  let ignores = super::ignores::plan_update(&target.root, &prepared, &base_texts, &apply_choices);
  let result = apply(
    &env,
    UpdateRequest {
      root: target.root.clone(),
      instance_name: target.instance.name.clone(),
      operations: resolve(&prepared, &apply_choices),
      remote,
      origin: target.origin.clone(),
      previous_loader: baseline.loader.clone(),
      components,
      restore_components,
      merged_contents,
      base_texts,
      next_baseline: prepared.next_baseline.clone(),
      ignores,
      plan_hash: plan_hash.clone(),
    },
  )
  .await;
  // The supervisor refreshes memory on every error, including preparation and
  // panic failures, and consumes the preview while it still holds PlanGuard.
  result?;
  Ok(UpdateOutcome {
    version_id,
    loader_install_pending: env.loader_install_pending.load(Ordering::SeqCst),
  })
}

/// The modpack version the launcher already announced for this instance.
/// `None` when no version was announced yet or the record is unreadable.
#[tauri::command]
pub async fn get_modpack_update_notice(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Option<String>> {
  let instance = current_instance(&app, &instance_id)?;
  let notice = read_update_notice(&instance.version_path);
  Ok(notice.map(|n| n.version_id))
}

/// Remember that the launcher announced `version_id` for this instance, so a
/// later startup check does not announce the same version again.
#[tauri::command]
pub async fn set_modpack_update_notice(
  app: AppHandle,
  instance_id: String,
  version_id: String,
) -> USTBLResult<()> {
  let instance = current_instance(&app, &instance_id)?;
  write_update_notice(&instance.version_path, &UpdateNotice { version_id })
}

/// Install the files of a campus modpack into the empty instance created for it.
///
/// Installing is the degenerate update: the baseline is empty, so everything the
/// version lists is an addition. Files the instance already holds (such as an
/// options file written at creation) are replaced by the modpack's own, because
/// a fresh install should reproduce the published content.
#[tauri::command]
pub async fn install_campus_modpack(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<CampusInstallStarted> {
  start_campus_modpack_install(app, instance_id, false).await
}

async fn start_campus_modpack_install(
  app: AppHandle,
  instance_id: String,
  retry_client: bool,
) -> USTBLResult<CampusInstallStarted> {
  let instance = current_instance(&app, &instance_id)?;
  let guard = PlanGuard::acquire(&instance.version_path)?;
  let old_record = load_campus_install_record(&instance.version_path);
  if instance.origin.as_ref().is_none_or(|origin| {
    !is_campus_install(origin, old_record.as_ref()) || (old_record.is_none() && !is_fresh(origin))
  }) {
    return Err(InstanceError::ModpackUpdateUnsupported.into());
  }
  let label = old_record
    .as_ref()
    .map(|record| record.label.clone())
    .unwrap_or_else(|| instance.name.clone());
  let tasks = CampusInstallTasks::register(&app, &instance_id, &label);
  save_campus_install_record(
    &instance.version_path,
    &CampusInstallRecord {
      instance_id: instance_id.clone(),
      instance_name: instance.name.clone(),
      label: label.clone(),
      phase: "installing".into(),
      error: None,
      group: Some(tasks.group.clone()),
      progress: old_record.as_ref().and_then(|record| record.progress),
      client_group: old_record
        .as_ref()
        .and_then(|record| record.client_group.clone()),
      target: old_record.as_ref().and_then(|record| record.target.clone()),
    },
  )?;
  remove_campus_install_groups(
    &app,
    &instance_id,
    old_record
      .as_ref()
      .and_then(|record| record.group.as_deref()),
  );
  app.state::<Pin<Box<TaskMonitor>>>().register_job_group(
    &tasks.group,
    &tasks.all().into_iter().cloned().collect::<Vec<_>>(),
  );
  let task_handles = tasks.clone();
  let group = tasks.group.clone();
  let anchor = tasks.applying.read().unwrap().desc.task_id;
  let app_for_job = app.clone();
  let root_for_job = instance.version_path.clone();
  let instance_for_job = instance_id.clone();
  let instance_for_error = instance_for_job.clone();
  let group_for_job = group.clone();
  let (start, ready) = tokio::sync::oneshot::channel::<()>();
  let job = tauri::async_runtime::spawn(async move {
    let _guard = guard;
    if ready.await.is_err() {
      return;
    }
    // Keep the plan guard in the supervisor: unwinding the inner task must
    // still persist the failure and publish the card's final result.
    let result = run_campus_install_task(install_campus_modpack_inner(
      app_for_job.clone(),
      instance_for_job,
      tasks,
      retry_client,
    ))
    .await;
    if let Err(error) = &result {
      log::warn!("campus install failed: {error:?}");
      task_handles.fail(&error.0);
      if let Err(save_error) = fail_campus_install_record(&root_for_job, &error.0) {
        log::warn!("cannot save campus install failure: {save_error:?}");
      }
      app_for_job
        .state::<Pin<Box<TaskMonitor>>>()
        .finish_job_group(&group_for_job, anchor, false);
    } else {
      for handle in task_handles.all() {
        CampusInstallTasks::complete(handle);
      }
      remove_campus_install_record(&root_for_job);
      app_for_job
        .state::<Pin<Box<TaskMonitor>>>()
        .finish_job_group(&group_for_job, anchor, true);
    }
    drop(_guard);
    let _ = app_for_job.emit_to(
      "main",
      "modpack-install:finished",
      CampusInstallFinished {
        instance_id: instance_for_error,
        group: group_for_job,
        error: result.err().map(|error| error.0),
      },
    );
  });
  app.state::<Pin<Box<TaskMonitor>>>().bind_job(anchor, job);
  let _ = start.send(());
  Ok(CampusInstallStarted { group })
}

async fn run_campus_install_task(
  task: impl std::future::Future<Output = USTBLResult<UpdateOutcome>> + Send + 'static,
) -> USTBLResult<UpdateOutcome> {
  tokio::spawn(task).await.map_err(|error| {
    log::warn!("campus install task terminated: {error:?}");
    crate::error::USTBLError::from(InstanceError::ModpackInstallTaskFailed)
  })?
}

/// History and cancelled groups may no longer be in the monitor's group queue.
/// Select from all descriptors, matching the jar path to isolate game directories.
fn campus_client_tasks(
  descriptors: Vec<PDesc<PTaskParam>>,
  instance: &Instance,
  linked_group: Option<&str>,
) -> Vec<PDesc<PTaskParam>> {
  let jar = instance.version_path.join(format!("{}.jar", instance.name));
  let client_name = format!("game-client?{}", instance.name);
  let group = descriptors
    .iter()
    .filter_map(|task| {
      let group = task.task_group.as_deref()?;
      let raw = group.rsplit_once('@').map_or(group, |(raw, _)| raw);
      let matches_jar = matches!(&task.payload, crate::tasks::PTaskParam::Download(param)
        if param.dest == jar);
      (linked_group == Some(group)
        || (raw.trim_start_matches("retry-") == client_name && matches_jar))
        .then_some(group)
    })
    .max_by_key(|group| {
      group
        .rsplit_once('@')
        .and_then(|(_, time)| time.parse::<i64>().ok())
        .unwrap_or(0)
    })
    .map(str::to_owned);
  descriptors
    .into_iter()
    .filter(|task| group.is_some() && task.task_group == group)
    .collect()
}

async fn install_campus_modpack_inner(
  app: AppHandle,
  instance_id: String,
  tasks: CampusInstallTasks,
  mut retry_client: bool,
) -> USTBLResult<UpdateOutcome> {
  let started = Instant::now();
  log::info!("modpack install start: instance={instance_id}");
  let instance = current_instance(&app, &instance_id)?;
  if recover_checked(&instance.version_path, || {
    ensure_not_running(&app, &instance_id)
  })?
    != Recovery::Nothing
  {
    plan_cache::clear_plan_cache(&instance.version_path);
  }
  let waiting = AppEnv {
    app: app.clone(),
    instance_id: instance_id.clone(),
    label: String::new(),
    loader: BaselineLoader {
      loader_type: instance.mod_loader.loader_type,
      version: instance.mod_loader.version.clone(),
    },
    loader_install_pending: AtomicBool::new(false),
    started,
    tasks: Some(tasks.clone()),
  };
  waiting.progress(Stage::WaitingClient);
  let mut client_group =
    load_campus_install_record(&instance.version_path).and_then(|record| record.client_group);
  let mut client_requeued = false;
  // Pending installs may be resumed while their client download is still
  // active (including restored download tasks after a launcher restart).
  loop {
    let descriptors = campus_client_tasks(
      app.state::<Pin<Box<TaskMonitor>>>().task_descriptors(),
      &instance,
      client_group.as_deref(),
    );
    if descriptors.is_empty() && !client_requeued {
      client_requeued = true;
      if let Some(group) = resume_campus_client(&app, &instance).await? {
        client_group = Some(group);
        if let Some(mut record) = load_campus_install_record(&instance.version_path) {
          record.client_group = client_group.clone();
          save_campus_install_record(&instance.version_path, &record)?;
        }
        continue;
      }
    }
    if let Some(client) = descriptors.first() {
      if client_group != client.task_group {
        client_group = client.task_group.clone();
        if let Some(mut record) = load_campus_install_record(&instance.version_path) {
          record.client_group = client_group.clone();
          save_campus_install_record(&instance.version_path, &record)?;
        }
      }
      if descriptors.iter().any(|task| {
        task.status == crate::tasks::streams::desc::PStatus::Failed || task.status.is_cancelled()
      }) {
        if retry_client {
          if descriptors.iter().any(|task| {
            task.status.is_in_progress() || task.status.is_waiting() || task.status.is_stopped()
          }) {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            continue;
          }
          retry_client = false;
          let params = descriptors
            .into_iter()
            .filter(|task| !task.status.is_completed())
            .map(|task| task.payload)
            .collect::<Vec<_>>();
          if !params.is_empty() {
            let retried = crate::tasks::commands::schedule_progressive_task_group(
              app.clone(),
              format!("retry-game-client?{}", instance.name),
              params,
              true,
            )
            .await?;
            client_group = Some(retried.task_group);
            if let Some(mut record) = load_campus_install_record(&instance.version_path) {
              record.client_group = client_group.clone();
              save_campus_install_record(&instance.version_path, &record)?;
            }
            continue;
          }
        }
        return Err(InstanceError::ModpackInstallInterrupted.into());
      }
      if descriptors.iter().any(|task| !task.status.is_completed()) {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        continue;
      }
    }
    if !instance
      .version_path
      .join(format!("{}.jar", instance.name))
      .is_file()
    {
      return Err(InstanceError::ModpackInstallInterrupted.into());
    }
    break;
  }
  // The instance was created moments ago; make sure the app knows it.
  refresh_and_update_instances(&app, false).await;
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  ensure_not_running(&app, &instance_id)?;
  let reporter = AppEnv {
    app: app.clone(),
    instance_id: instance_id.clone(),
    label: String::new(),
    loader: BaselineLoader {
      loader_type: target.instance.mod_loader.loader_type,
      version: target.instance.mod_loader.version.clone(),
    },
    loader_install_pending: AtomicBool::new(false),
    started,
    tasks: Some(tasks),
  };
  reporter.progress(Stage::Preparing);
  let install_record = load_campus_install_record(&target.root);
  if !is_campus_install(&target.origin, install_record.as_ref()) {
    return Err(InstanceError::ModpackUpdateUnsupported.into());
  }
  if !is_fresh(&target.origin) {
    // Recovery kept a committed transaction; only the final task result was
    // interrupted. Do not apply a second time or report a spurious stale plan.
    return Ok(UpdateOutcome {
      version_id: target.origin.version_id,
      loader_install_pending: target.instance.mod_loader.status != ModLoaderStatus::Installed,
    });
  }
  let planning_started = Instant::now();
  let InstallPlan {
    plan,
    prepared,
    remote,
  } = plan_fresh_install(&app, &target, &reporter).await?;
  if let Some(loader_type) = fresh_identity_loader(
    &target.origin,
    &remote.game_version,
    remote.loader.loader_type,
  )? {
    store_instance(&app, &instance_id, |instance| {
      if let Some(origin) = instance.origin.as_mut() {
        origin.loader_type = loader_type;
      }
    })
    .await?;
  }
  if plan.reinstall_required {
    return Err(InstanceError::ModpackUpdateReinstallRequired.into());
  }
  log::info!(
    "modpack install planned: instance={instance_id} version={} changes={} conflicts={} plan_hash={} {:?} elapsed",
    plan.to_version_id,
    plan.changes.len(),
    plan.conflicts.len(),
    plan.plan_hash,
    planning_started.elapsed(),
  );
  let choices: BTreeMap<String, ConflictChoice> = plan
    .conflicts
    .iter()
    .map(|conflict| (conflict.path.clone(), ConflictChoice::TakeRemote))
    .collect();
  let env = AppEnv {
    app: app.clone(),
    instance_id: instance_id.clone(),
    label: remote.label.clone(),
    loader: remote.loader.clone(),
    loader_install_pending: AtomicBool::new(false),
    started,
    tasks: reporter.tasks,
  };
  let version_id = remote.version_id.clone();
  let mut apply_choices = choices.clone();
  let merged_contents = selected_hunk_contents(&prepared, &mut apply_choices)?;
  log::info!(
    "modpack install resolutions: instance={instance_id} plan_hash={} resolutions=[{}]",
    plan.plan_hash,
    describe_conflict_resolutions(&prepared.diff, &apply_choices).join(","),
  );
  let restore_components = remote.component_pack.is_some();
  let components = if restore_components {
    remote.components.clone()
  } else {
    None
  };
  let ignores =
    super::ignores::plan_update(&target.root, &prepared, &BTreeMap::new(), &apply_choices);
  let result = apply(
    &env,
    UpdateRequest {
      root: target.root.clone(),
      instance_name: target.instance.name.clone(),
      operations: resolve(&prepared, &apply_choices),
      remote: remote.clone(),
      origin: target.origin.clone(),
      previous_loader: BaselineLoader {
        loader_type: target.instance.mod_loader.loader_type,
        version: target.instance.mod_loader.version.clone(),
      },
      components,
      restore_components,
      merged_contents,
      base_texts: BTreeMap::new(),
      next_baseline: prepared.next_baseline.clone(),
      ignores,
      plan_hash: plan.plan_hash.clone(),
    },
  )
  .await;
  if result.is_err() {
    // A rolled back update restored the files on disk; drop stale memory.
    refresh_and_update_instances(&app, false).await;
  }
  plan_cache::clear_plan_cache(&target.root);
  result?;
  remove_campus_install_record(&target.root);
  Ok(UpdateOutcome {
    version_id,
    loader_install_pending: env.loader_install_pending.load(Ordering::SeqCst),
  })
}

async fn resume_campus_client(
  app: &AppHandle,
  instance: &Instance,
) -> USTBLResult<Option<String>> {
  let json_path = instance.version_path.join(format!("{}.json", instance.name));
  let client: McClientInfo = crate::storage::load_json_async(&json_path).await?;
  let priority = {
    let config = app.state::<Mutex<crate::launcher_config::models::LauncherConfig>>();
    let snapshot = config.lock()?.clone();
    get_source_priority_list(&snapshot)
  };
  let dirs = get_instance_subdir_paths(
    app,
    instance,
    &[&crate::instance::models::misc::InstanceSubdirType::Libraries,
      &crate::instance::models::misc::InstanceSubdirType::Assets],
  ).ok_or(InstanceError::InvalidSourcePath)?;
  let [libraries, assets] = dirs.as_slice() else {
    return Err(InstanceError::InvalidSourcePath.into());
  };
  let mut params = get_invalid_library_files(&priority, libraries, &client, true).await?;
  params.extend(get_invalid_assets(app, &client, &priority, assets, true).await?);
  let jar = instance.version_path.join(format!("{}.jar", instance.name));
  let Some(download) = client.downloads.get("client") else {
    return Err(InstanceError::ClientJsonParseError.into());
  };
  let jar_valid = jar.is_file()
    && (download.sha1.is_empty() || validate_sha1(jar.clone(), download.sha1.clone()).is_ok());
  if !jar_valid {
    let sources = download_source_candidates(
      &url::Url::parse(&download.url)?,
      &[ResourceType::Launcher],
      &priority,
    )?;
    let mut sources = sources.into_iter();
    let src = sources.next().ok_or(InstanceError::ClientJsonParseError)?;
    params.push(PTaskParam::Download(DownloadParam {
      src,
      dest: jar,
      filename: None,
      sha1: (!download.sha1.is_empty()).then(|| download.sha1.clone()),
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(sources.collect(), 10),
    }));
  }
  if params.is_empty() {
    return Ok(None);
  }
  let group = crate::tasks::commands::schedule_progressive_task_group(
    app.clone(),
    format!("retry-game-client?{}", instance.name),
    params,
    true,
  )
  .await?;
  Ok(Some(group.task_group))
}

/// Undo or finish an update that was interrupted. Returns whether anything
/// had to be done.
#[tauri::command]
pub async fn recover_modpack_update(app: AppHandle, instance_id: String) -> USTBLResult<bool> {
  let instance = current_instance(&app, &instance_id)?;
  let root = instance.version_path.clone();
  let guard = PlanGuard::acquire(&root)?;
  let check_app = app.clone();
  let outcome = tokio::task::spawn_blocking({
    let root = root.clone();
    move || {
      let _guard = guard;
      recover_checked(&root, || ensure_not_running(&check_app, &instance_id))
    }
  })
  .await
  .map_err(|e| crate::error::USTBLError(e.to_string()))??;
  if outcome == Recovery::RolledBack {
    refresh_and_update_instances(&app, false).await;
  }
  if outcome != Recovery::Nothing {
    plan_cache::clear_plan_cache(&root);
  }
  Ok(outcome != Recovery::Nothing)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::InstanceError;

  #[derive(Clone, Default)]
  struct StageSink(Arc<Mutex<Vec<String>>>);

  fn platform_origin(loader_type: ModLoaderType) -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Modrinth,
      project_id: "project".into(),
      version_id: String::new(),
      game_version: "1.21.1".into(),
      loader_type,
      follow: true,
    }
  }

  #[test]
  fn install_record_supports_old_json_and_serializes_a_target() {
    let old: CampusInstallRecord = serde_json::from_value(serde_json::json!({
      "instanceId": "games:pack",
      "instanceName": "pack",
      "label": "Pack",
      "phase": "waitingClient",
      "error": null
    }))
    .unwrap();
    assert!(old.target.is_none());

    let target = InstallTarget {
      version_id: "v1".into(),
      label: "1.0".into(),
      url: "https://example.test/pack.mrpack".into(),
      sha1: "abc".into(),
    };
    let record = CampusInstallRecord {
      target: Some(target.clone()),
      ..old
    };
    assert_eq!(
      serde_json::to_value(record).unwrap()["target"]["versionId"],
      "v1"
    );
  }

  #[test]
  fn install_support_predicate_covers_campus_and_fixed_platform_targets() {
    let campus = InstanceOrigin {
      platform: OriginPlatform::Campus,
      ..platform_origin(ModLoaderType::Forge)
    };
    let target = CampusInstallRecord {
      target: Some(InstallTarget {
        version_id: "v1".into(),
        label: "Pack".into(),
        url: "https://example.test/pack".into(),
        sha1: "abc".into(),
      }),
      ..serde_json::from_value(serde_json::json!({
        "instanceId": "i",
        "instanceName": "i",
        "label": "i",
        "phase": "waitingClient",
        "error": null
      }))
      .unwrap()
    };
    let no_target = CampusInstallRecord {
      target: None,
      ..target.clone()
    };
    assert!(is_campus_install(&campus, None));
    assert!(is_campus_install(
      &platform_origin(ModLoaderType::Fabric),
      Some(&target)
    ));
    assert!(!is_campus_install(
      &platform_origin(ModLoaderType::Fabric),
      None
    ));
    assert!(!is_campus_install(
      &platform_origin(ModLoaderType::Fabric),
      Some(&no_target)
    ));
  }

  #[test]
  fn fixed_target_builds_an_archive_latest_without_a_platform_check() {
    let latest = latest_from_install_target(InstallTarget {
      version_id: "file-42".into(),
      label: "Release 42".into(),
      url: "https://example.test/pack.mrpack".into(),
      sha1: "abc".into(),
    });
    assert_eq!(latest.version_id, "file-42");
    assert_eq!(latest.label, "Release 42");
    assert_eq!(
      latest.archive_ref(),
      Some(("abc", "https://example.test/pack.mrpack"))
    );
  }

  #[test]
  fn registration_rewrite_preserves_the_fixed_target() {
    let old = CampusInstallRecord {
      instance_id: "i".into(),
      instance_name: "old".into(),
      label: "old".into(),
      phase: "failed".into(),
      error: Some("error".into()),
      group: None,
      progress: None,
      client_group: Some("client".into()),
      target: Some(InstallTarget {
        version_id: "v1".into(),
        label: "Pack 1".into(),
        url: "https://example.test/pack".into(),
        sha1: "abc".into(),
      }),
    };
    let rewritten = rewrite_install_record("i".into(), "new".into(), "New".into(), Some(old));
    assert_eq!(rewritten.target.as_ref().unwrap().version_id, "v1");
    assert_eq!(rewritten.client_group.as_deref(), Some("client"));
  }

  #[test]
  fn fresh_identity_accepts_loader_changes_and_rejects_game_version_changes() {
    let origin = platform_origin(ModLoaderType::Unknown);
    assert_eq!(
      fresh_identity_loader(&origin, "1.21.1", ModLoaderType::Fabric).unwrap(),
      Some(ModLoaderType::Fabric)
    );
    assert_eq!(
      fresh_identity_loader(
        &platform_origin(ModLoaderType::Fabric),
        "1.21.1",
        ModLoaderType::Fabric
      )
      .unwrap(),
      None
    );
    assert_eq!(
      fresh_identity_loader(&origin, "1.20.1", ModLoaderType::Fabric)
        .unwrap_err()
        .0,
      "MODPACK_UPDATE_LINE_MISMATCH"
    );
  }

  #[test]
  fn vanilla_placeholder_plan_reports_loader_change() {
    let baseline = BaselineLoader {
      loader_type: ModLoaderType::Unknown,
      version: String::new(),
    };
    let remote = BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: "0.16.9".into(),
    };
    let change = compute_loader_change(&baseline, &remote).unwrap();
    assert_eq!(change.from, baseline);
    assert_eq!(change.to, remote);
  }

  impl Sink for StageSink {
    fn report_progress(
      &self,
      _: u32,
      _: Option<&str>,
      _: i64,
      _: i64,
      _: u32,
      _: Option<f64>,
      _: f64,
    ) {
    }
    fn report_completion(&self, _: u32, _: Option<&str>) {
      self.0.lock().unwrap().push("completed".into());
    }
    fn report_stopped(&self, _: u32, _: Option<&str>) {}
    fn report_cancelled(&self, _: u32, _: Option<&str>) {}
    fn report_started(&self, _: u32, _: Option<&str>, _: i64) {}
    fn report_failed(&self, _: u32, _: Option<&str>, reason: String) {
      self.0.lock().unwrap().push(reason);
    }
  }

  #[tokio::test]
  async fn update_groups_register_four_stages_and_finish_success_or_readable_failure() {
    use crate::tasks::streams::desc::PStatus;
    use crate::tasks::streams::reporter::Reporter;
    use crate::tasks::upload::UploadParam;
    let root = std::env::temp_dir().join(format!("ustbl-update-card-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    for failure in [None, Some("MODPACK_UPDATE_VERIFY_FAILED")] {
      let sink = StageSink::default();
      let mut id = 0;
      let mut created = Vec::new();
      let tasks = CampusInstallTasks::with_handles(
        "modpack-update?test@1".into(),
        true,
        |name, total| {
          created.push(name.to_owned());
          id += 1;
          Arc::new(RwLock::new(PHandle::new(
            PDesc::new(
              id,
              Some("modpack-update?test@1".into()),
              total,
              PTaskParam::Upload(UploadParam { filename: name.into() }),
              PStatus::Waiting,
            ),
            std::time::Duration::from_secs(1),
            root.join(format!("{id}.json")),
            Reporter::new(total, std::time::Duration::from_secs(1), sink.clone()),
          )))
        },
      );
      assert_eq!(created, ["Preparing", "Locating", "Fetching", "Applying"]);
      assert_eq!(tasks.all().len(), 4);
      CampusInstallTasks::complete(&tasks.preparing);
      CampusInstallTasks::complete(&tasks.locating);
      CampusInstallTasks::start(&tasks.fetching);
      tasks.finish(failure);
      if let Some(reason) = failure {
        assert!(matches!(
          tasks.fetching.read().unwrap().status(),
          PStatus::Failed
        ));
        assert!(sink.0.lock().unwrap().contains(&reason.to_string()));
        assert!(tasks.applying.read().unwrap().status().is_waiting());
      } else {
        assert!(tasks
          .all()
          .iter()
          .all(|task| task.read().unwrap().status().is_completed()));
      }
    }
    fs::remove_dir_all(root).unwrap();
  }

  #[tokio::test]
  async fn update_supervisor_catches_panic_and_holds_instance_claim_through_final_result() {
    let root = std::env::temp_dir().join(format!("ustbl-update-panic-{}", uuid::Uuid::new_v4()));
    let guard = PlanGuard::acquire(&root).unwrap();
    let error = run_modpack_update_task(async { panic!("simulated update panic") })
      .await
      .unwrap_err();
    assert_eq!(error.0, "MODPACK_UPDATE_TASK_FAILED");
    assert!(PlanGuard::acquire(&root).is_err());
    let event = ModpackUpdateFinished {
      instance_id: "test".into(),
      instance_name: "test".into(),
      group: "modpack-update?test@1".into(),
      plan_hash: "hash".into(),
      outcome: None,
      error: Some(error.0),
    };
    assert_eq!(
      serde_json::to_value(event).unwrap()["error"],
      "MODPACK_UPDATE_TASK_FAILED"
    );
    drop(guard);
    assert!(PlanGuard::acquire(&root).is_ok());
  }

  #[tokio::test]
  async fn update_supervisor_preserves_outcome_and_normal_failure() {
    let outcome = run_modpack_update_task(async {
      Ok(UpdateOutcome {
        version_id: "next".into(),
        loader_install_pending: true,
      })
    })
    .await
    .unwrap();
    assert!(outcome.loader_install_pending);
    assert_eq!(outcome.version_id, "next");
    let error = run_modpack_update_task(async { Err(InstanceError::ModpackUpdateStale.into()) })
      .await
      .unwrap_err();
    assert_eq!(error.0, "MODPACK_UPDATE_STALE");
  }

  #[tokio::test]
  async fn persisted_installs_skip_the_entire_update_check_including_the_platform() {
    let root = std::env::temp_dir().join(format!("ustbl-check-install-{}", uuid::Uuid::new_v4()));
    let target = Target {
      instance: Instance {
        version_path: root.clone(),
        ..Default::default()
      },
      origin: InstanceOrigin {
        platform: OriginPlatform::Campus,
        project_id: "test".into(),
        version_id: String::new(),
        game_version: "1.21".into(),
        loader_type: ModLoaderType::Unknown,
        follow: true,
      },
      root: root.clone(),
    };
    for phase in ["pending", "waitingClient", "installing", "failed"] {
      save_campus_install_record(
        &root,
        &CampusInstallRecord {
          instance_id: "test:pending".into(),
          instance_name: "pending".into(),
          label: "test".into(),
          phase: phase.into(),
          error: None,
          group: None,
          progress: None,
          client_group: None,
          target: None,
        },
      )
      .unwrap();
      let called = std::cell::Cell::new(false);
      let check = check_unless_installing(&target, async {
        called.set(true);
        panic!("baseline, platform and comparison must not be called");
      })
      .await
      .unwrap();
      assert_eq!(check.status, CheckStatus::Installing);
      assert_eq!(serde_json::to_value(check).unwrap()["status"], "installing");
      assert!(!called.get());
    }
    remove_campus_install_record(&root);
    let called = std::cell::Cell::new(false);
    let check = check_unless_installing(&target, async {
      called.set(true);
      Ok(check_of(CheckStatus::WaitingLoader, &target))
    })
    .await
    .unwrap();
    assert!(called.get());
    assert_eq!(check.status, CheckStatus::WaitingLoader);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn install_group_cleanup_matches_the_instance_not_the_human_label() {
    assert!(campus_install_group_belongs_to(
      "modpack-install?航空学 [APP_DATA_SUBDIR:a]@1",
      "APP_DATA_SUBDIR:a"
    ));
    assert!(campus_install_group_belongs_to(
      "modpack-install?旧标题 [APP_DATA_SUBDIR:a]@2",
      "APP_DATA_SUBDIR:a"
    ));
    assert!(!campus_install_group_belongs_to(
      "modpack-install?航空学 [APP_DATA_SUBDIR:ab]@1",
      "APP_DATA_SUBDIR:a"
    ));
    assert!(!campus_install_group_belongs_to(
      "modpack-publish?航空学 [APP_DATA_SUBDIR:a]@1",
      "APP_DATA_SUBDIR:a"
    ));
  }

  #[tokio::test]
  async fn an_install_panic_is_a_localized_failure_and_persists_as_failed() {
    let root = std::env::temp_dir().join(format!("ustbl-panic-{}", uuid::Uuid::new_v4()));
    let instance = Instance {
      id: "games:panic".into(),
      name: "panic".into(),
      version_path: root.clone(),
      ..Default::default()
    };
    let progress = Progress {
      stage: Stage::Locating,
      done: 0,
      total: 1,
      bytes_done: 0,
      bytes_total: 0,
    };
    save_campus_install_record(
      &root,
      &CampusInstallRecord {
        instance_id: instance.id.clone(),
        instance_name: instance.name.clone(),
        label: "测试安装".into(),
        phase: "installing".into(),
        error: None,
        group: Some("modpack-install?panic@1".into()),
        progress: Some(progress),
        client_group: None,
        target: None,
      },
    )
    .unwrap();
    let guard = PlanGuard::acquire(&root).unwrap();
    let result = run_campus_install_task(async {
      panic!("simulated plan_groups panic");
    })
    .await;
    let error = result.unwrap_err();
    assert_eq!(error.0, "MODPACK_INSTALL_TASK_FAILED");
    fail_campus_install_record(&root, &error.0).unwrap();
    let record = load_campus_install_record(&root).unwrap();
    assert_eq!(record.phase, "failed");
    assert_eq!(record.error.as_deref(), Some("MODPACK_INSTALL_TASK_FAILED"));
    assert_eq!(record.progress, Some(progress));
    // The supervisor keeps the claim until failure handling has finished.
    assert!(unfinished_campus_install(&instance).is_none());
    drop(guard);
    let listed = unfinished_campus_install(&instance).unwrap();
    assert!(listed.resumable);
    assert_eq!(listed.error, record.error);
    assert!(PlanGuard::acquire(&root).is_ok());
    fs::remove_dir_all(root).unwrap();
  }

  #[tokio::test]
  async fn a_supervised_install_preserves_success_and_normal_errors() {
    let outcome = run_campus_install_task(async {
      Ok(UpdateOutcome {
        version_id: "35".into(),
        loader_install_pending: false,
      })
    })
    .await
    .unwrap();
    assert_eq!(outcome.version_id, "35");
    let error =
      run_campus_install_task(async { Err(InstanceError::ModpackUpdateVerifyFailed.into()) })
        .await
        .unwrap_err();
    assert_eq!(error.0, "MODPACK_UPDATE_VERIFY_FAILED");
  }

  #[test]
  fn an_installing_record_without_a_live_task_is_listed_as_resumable_after_restart() {
    let root = std::env::temp_dir().join(format!("ustbl-restart-{}", uuid::Uuid::new_v4()));
    let instance = Instance {
      id: "games:interrupted".into(),
      name: "interrupted".into(),
      version_path: root.clone(),
      ..Default::default()
    };
    save_campus_install_record(
      &root,
      &CampusInstallRecord {
        instance_id: instance.id.clone(),
        instance_name: instance.name.clone(),
        label: "测试安装".into(),
        phase: "installing".into(),
        error: None,
        group: Some("modpack-install?interrupted@1".into()),
        progress: None,
        client_group: None,
        target: None,
      },
    )
    .unwrap();
    let listed = unfinished_campus_install(&instance).unwrap();
    assert_eq!(listed.phase, "failed");
    assert_eq!(listed.error.as_deref(), Some("MODPACK_INSTALL_INTERRUPTED"));
    assert!(listed.resumable);
    assert!(PlanGuard::acquire(&root).is_ok());
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn client_wait_uses_retained_descriptors_and_never_links_another_game_directory() {
    use crate::tasks::streams::desc::PStatus;
    let instance = Instance {
      name: "same-name".into(),
      version_path: PathBuf::from("games-a/versions/same-name"),
      ..Default::default()
    };
    let task = |group: &str, path: PathBuf, status| {
      PDesc::new(
        0,
        Some(group.into()),
        10,
        PTaskParam::Download(DownloadParam {
          src: "https://example.com/client".parse().unwrap(),
          dest: path,
          filename: None,
          sha1: None,
          custom_headers: None,
          transfer_options: Default::default(),
        }),
        status,
      )
    };
    let original = "game-client?same-name@1";
    let retry = "retry-game-client?same-name@2";
    let other = "game-client?same-name@3";
    let jar = instance.version_path.join("same-name.jar");
    let descriptors = vec![
      task(original, jar.clone(), PStatus::Completed),
      task(
        original,
        instance.version_path.join("library.jar"),
        PStatus::Failed,
      ),
      task(retry, jar, PStatus::Completed),
      task(
        retry,
        instance.version_path.join("library.jar"),
        PStatus::Cancelled,
      ),
      task(
        other,
        PathBuf::from("games-b/versions/same-name/same-name.jar"),
        PStatus::Waiting,
      ),
    ];
    let selected = campus_client_tasks(descriptors, &instance, Some(original));
    assert_eq!(selected.len(), 2);
    assert!(selected
      .iter()
      .all(|task| task.task_group.as_deref() == Some(retry)));
    assert!(selected.iter().any(|task| task.status.is_cancelled()));
    // After restart a completed jar's descriptor may be gone; the persisted
    // association still finds the remaining library download.
    let selected = campus_client_tasks(
      vec![task(
        retry,
        instance.version_path.join("library.jar"),
        PStatus::Waiting,
      )],
      &instance,
      Some(retry),
    );
    assert_eq!(selected.len(), 1);
  }

  #[test]
  fn pending_campus_configs_are_found_without_a_client_jar_after_restart() {
    use crate::launcher_config::models::GameDirectory;
    let root = std::env::temp_dir().join(format!("ustbl-pending-{}", uuid::Uuid::new_v4()));
    let version = root.join("versions/pending");
    fs::create_dir_all(&version).unwrap();
    let instance = Instance {
      name: "stale-name".into(),
      id: "stale-id".into(),
      version_path: "stale-path".into(),
      origin: Some(InstanceOrigin {
        platform: OriginPlatform::Campus,
        project_id: "p".into(),
        version_id: String::new(),
        game_version: "1.21.1".into(),
        loader_type: ModLoaderType::Fabric,
        follow: true,
      }),
      ..Default::default()
    };
    fs::write(
      version.join(crate::instance::constants::INSTANCE_CFG_FILE_NAME),
      serde_json::to_vec(&instance).unwrap(),
    )
    .unwrap();
    register_created_campus_instance(
      &Instance {
        version_path: version.clone(),
        ..instance
      },
      "game-client?pending@1".into(),
      None,
    )
    .unwrap();
    assert_eq!(
      load_campus_install_record(&version)
        .unwrap()
        .client_group
        .as_deref(),
      Some("game-client?pending@1")
    );
    let record = CampusInstallRecord {
      instance_id: "stale-id".into(),
      instance_name: "stale-name".into(),
      label: "测试整合包".into(),
      phase: "waitingClient".into(),
      error: None,
      group: None,
      progress: None,
      client_group: None,
      target: None,
    };
    save_campus_install_record(&version, &record).unwrap();
    save_campus_install_record(&version, &record).unwrap();
    let scanned = scan_campus_instances(&[GameDirectory {
      name: "games".into(),
      dir: root.clone(),
    }]);
    let found = &scanned["games:pending"];
    assert_eq!(found.name, "pending");
    assert_eq!(found.version_path, version);
    assert_eq!(
      load_campus_install_record(&found.version_path)
        .unwrap()
        .phase,
      "waitingClient"
    );
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn scan_finds_platform_instances_only_when_the_install_record_has_a_target() {
    use crate::launcher_config::models::GameDirectory;
    let root = std::env::temp_dir().join(format!("ustbl-platform-scan-{}", uuid::Uuid::new_v4()));
    let versions = root.join("versions");
    let with_target = versions.join("with-target");
    let without_record = versions.join("without-record");
    fs::create_dir_all(&with_target).unwrap();
    fs::create_dir_all(&without_record).unwrap();
    let write_instance = |path: &Path| {
      fs::write(
        path.join(crate::instance::constants::INSTANCE_CFG_FILE_NAME),
        serde_json::to_vec(&Instance {
          origin: Some(platform_origin(ModLoaderType::Fabric)),
          ..Default::default()
        })
        .unwrap(),
      )
      .unwrap();
    };
    write_instance(&with_target);
    write_instance(&without_record);
    let record = CampusInstallRecord {
      instance_id: "games:with-target".into(),
      instance_name: "with-target".into(),
      label: "Pack".into(),
      phase: "waitingClient".into(),
      error: None,
      group: None,
      progress: None,
      client_group: None,
      target: Some(InstallTarget {
        version_id: "v1".into(),
        label: "Pack".into(),
        url: "https://example.test/pack".into(),
        sha1: "abc".into(),
      }),
    };
    save_campus_install_record(&with_target, &record).unwrap();
    let scanned = scan_campus_instances(&[GameDirectory {
      name: "games".into(),
      dir: root.clone(),
    }]);
    assert!(scanned.contains_key("games:with-target"));
    assert!(!scanned.contains_key("games:without-record"));
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_plan_guard_rejects_duplicates_and_moves_to_the_apply_without_relocking() {
    let root = PathBuf::from(format!("campus-plan-{}", uuid::Uuid::new_v4()));
    let held = PlanGuard::acquire(&root).unwrap();
    assert_eq!(PlanGuard::acquire(&root).err().unwrap().0, "MODPACK_UPDATE_BUSY");
    let apply = |guard: PlanGuard| {
      assert!(PlanGuard::acquire(&root).is_err());
      drop(guard);
    };
    apply(held);
    assert!(PlanGuard::acquire(&root).is_ok());
  }

  #[test]
  fn a_cancelled_preview_answers_with_the_cancelled_error() {
    let join_error = tokio::runtime::Runtime::new().unwrap().block_on(async {
      let spawned = tokio::spawn(async {
        std::future::pending::<()>().await;
      });
      spawned.abort();
      spawned.await.unwrap_err()
    });
    assert!(join_error.is_cancelled());
    let mapped: crate::error::USTBLError = InstanceError::ModpackUpdatePrepareCancelled.into();
    assert_eq!(mapped.0, "MODPACK_UPDATE_PREPARE_CANCELLED");
  }

  #[test]
  fn a_merge_that_kept_nothing_local_writes_the_published_bytes() {
    let remote = b"x = 3\ny = 1\n";
    // Only the line endings are local: the published file is written.
    let merged = b"x = 3\r\ny = 1\r\n".to_vec();
    assert_eq!(
      remote_if_equivalent("config/c.toml", merged, remote),
      remote
    );
    // A kept local line stays, with the local line endings.
    let merged = b"x = 2\r\ny = 1\r\n".to_vec();
    assert_eq!(
      remote_if_equivalent("config/c.toml", merged.clone(), remote),
      merged
    );
  }

  #[test]
  fn only_a_plan_with_nothing_to_do_fast_forwards() {
    use super::super::diff::{ChangeKind, FileChange};
    let change = |kind| FileChange {
      path: "mods/a.jar".into(),
      kind,
      remote: None,
      local_size: None,
    };
    let none: &[FileChange] = &[];
    assert!(fast_forward(none.iter(), 0, false));
    assert!(!fast_forward([change(ChangeKind::Update)].iter(), 0, false));
    assert!(!fast_forward(none.iter(), 1, false));
    assert!(!fast_forward(none.iter(), 0, true));
    // A plan that only retires entries has no file action: it fast-forwards,
    // and the retirements still write the baseline when applied.
    assert!(fast_forward([change(ChangeKind::Retire)].iter(), 0, false));
  }

  /// The JSON the update dialog sends for "use the new version on the first
  /// conflict block of config/fadeless.json" must land in the choices map
  /// exactly as `path#index -> TakeRemote`.
  #[test]
  fn the_dialogs_block_choice_deserializes_as_a_hunk_key() {
    let choices: BTreeMap<String, ConflictChoice> = serde_json::from_str(
      r#"{"config/fadeless.json#0":"takeRemote","config/other.json":"keepLocal"}"#,
    )
    .unwrap();
    assert_eq!(
      choices.get("config/fadeless.json#0"),
      Some(&ConflictChoice::TakeRemote)
    );
    assert_eq!(
      choices.get("config/other.json"),
      Some(&ConflictChoice::KeepLocal)
    );
    // A whole-file key never matches a hunk key and vice versa.
    assert!(!choices.contains_key("config/fadeless.json"));
    assert!(!choices.contains_key("config/other.json#0"));
    // Round trip: the keys come back exactly as the dialog built them.
    let bytes = serde_json::to_vec(&choices).unwrap();
    let back: BTreeMap<String, ConflictChoice> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, choices);
  }

  #[test]
  fn conflict_resolutions_describe_every_conflict_without_contents() {
    use crate::instance::modpack_update::diff::{ConflictKind, FileKey};
    let key = |tag: &str| FileKey {
      sha1: tag.into(),
      size: tag.len() as u64,
    };
    let hunk = |start: usize| crate::instance::modpack_update::merge::MergeConflict {
      base_start: start,
      base_end: start + 1,
      base: vec!["b".into()],
      local: vec!["l".into()],
      remote: vec!["r".into()],
    };
    let diff = super::super::diff::Diff {
      conflicts: vec![
        super::super::diff::Conflict {
          path: "config/a.json".into(),
          kind: ConflictKind::ModifiedLocallyChangedRemotely,
          baseline: Some(key("a1")),
          local: Some(key("al")),
          remote: Some(key("a2")),
          remote_disabled: None,
          local_disabled: Some(false),
          hunks: vec![hunk(0), hunk(4)],
          ignored: false,
          ignored_hunks: Vec::new(),
        },
        super::super::diff::Conflict {
          path: "mods/b.jar".into(),
          kind: ConflictKind::ModifiedLocallyChangedRemotely,
          baseline: Some(key("b1")),
          local: Some(key("bl")),
          remote: Some(key("b2")),
          remote_disabled: None,
          local_disabled: Some(false),
          hunks: Vec::new(),
          ignored: false,
          ignored_hunks: Vec::new(),
        },
      ],
      ..Default::default()
    };
    let mut choices = BTreeMap::new();
    choices.insert("config/a.json#0".to_string(), ConflictChoice::TakeRemote);
    choices.insert("mods/b.jar".to_string(), ConflictChoice::TakeRemote);

    assert_eq!(
      describe_conflict_resolutions(&diff, &choices),
      vec![
        "config/a.json:hunks[#0=TakeRemote,#1=KeepLocal]".to_string(),
        "mods/b.jar:file=TakeRemote".to_string(),
      ]
    );
    // Unanswered conflicts are reported as the keep-local default.
    assert_eq!(
      describe_conflict_resolutions(&diff, &BTreeMap::new()),
      vec![
        "config/a.json:hunks[#0=KeepLocal,#1=KeepLocal]".to_string(),
        "mods/b.jar:file=KeepLocal".to_string(),
      ]
    );
  }

  #[test]
  fn a_reinstall_plan_never_fast_forwards() {
    let origin = InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    };
    let run = PlannedRun {
      version_id: "2".into(),
      label: "2.0".into(),
      diff: Default::default(),
      plan_hash: "hh".into(),
      loader_change: None,
      reinstall_required: true,
      remote: crate::instance::modpack_update::remote::RemoteModpack {
        version_id: "2".into(),
        label: "2.0".into(),
        game_version: "1.21.1".into(),
        loader: BaselineLoader {
          loader_type: ModLoaderType::Fabric,
          version: "0.16.0".into(),
        },
        components: None,
        component_pack: None,
        files: Vec::new(),
        rules: Vec::new(),
        rules_manifest: false,
        path_index: std::sync::Arc::new(std::sync::OnceLock::new()),
      },
    };
    let plan = plan_of(&origin, run);
    assert!(plan.reinstall_required);
    assert!(!plan.fast_forward);
    assert!(plan.changes.is_empty());
    assert_eq!(plan.plan_hash, "hh");
  }
}
