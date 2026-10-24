//! Tauri commands for following a modpack: check, plan, apply, recover.

use super::apply::{
  apply, can_roll_back_last, ensure_unlink_safe, materialize_remote_file, prepare_packs,
  recover_checked, roll_back_last_checked, Progress, Recovery, UpdateEnv, UpdateRequest,
};
use super::diff::{Conflict, ConflictChoice, FileChange};
use super::merge::merge_with_choices;
use super::notice::{read_update_notice, write_update_notice, UpdateNotice};
use super::plan::{
  compare, count_local_changes, finish, invalidate_scan_cache, local_changes, requires_reinstall,
  resolve, LocalChange,
};
use super::plan_cache::{self, PlanCache};
use super::remote::ComponentPack;
use super::restore::restore_component_profile;
use super::sources::{download_param, latest_version, resolve_remote, LatestVersion};
use crate::error::USTBLResult;
use crate::instance::commands::switch_mod_loader;
use crate::instance::helpers::misc::{get_instance_game_config, refresh_and_update_instances};
use crate::instance::helpers::modpack::baseline::{
  BaselineLoader, ModpackBaseline, BASELINE_FILE_NAME,
};
use crate::instance::helpers::modpack::text;
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceOrigin, ModLoader, ModLoaderStatus, ModLoaderType,
  OriginPlatform,
};
use crate::launch::models::LaunchingState;
use crate::resource::models::ModLoaderResourceInfo;
use crate::tasks::download::DownloadParam;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
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
struct ProgressEvent {
  instance_id: String,
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
  Ok(
    state
      .get(instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone(),
  )
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
        origin.version_id = version_id.to_string();
      }
      instance.modpack_version = Some(self.label.clone());
    })
    .await
  }

  fn report(&self, progress: Progress) {
    let _ = self.app.emit_to(
      "main",
      PROGRESS_EVENT,
      ProgressEvent {
        instance_id: self.instance_id.clone(),
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
  if !target.origin.follow {
    return Ok(check_of(CheckStatus::Paused, &target));
  }
  if !get_instance_game_config(&app, &target.instance).version_isolation {
    return Ok(UpdateCheck {
      reason: Some(InstanceError::ModpackUpdateNotIsolated.to_string()),
      ..check_of(CheckStatus::NeedsRepair, &target)
    });
  }
  let baseline = match load_baseline(&target).await {
    Ok(baseline) => baseline,
    Err(err) => {
      return Ok(UpdateCheck {
        reason: Some(err.0),
        ..check_of(CheckStatus::NeedsRepair, &target)
      })
    }
  };
  if target.instance.mod_loader.status != ModLoaderStatus::Installed {
    return Ok(check_of(CheckStatus::WaitingLoader, &target));
  }
  match light_check(&app, &target).await {
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
      ..check_of(CheckStatus::Available, &target)
    }),
    Ok(LightCheck::UpToDate) => {
      let mut check = check_of(CheckStatus::UpToDate, &target);
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
      ..check_of(CheckStatus::Unknown, &target)
    }),
    Ok(LightCheck::ProjectGone) => {
      // The project listing already confirmed the deletion, so the identity
      // is dropped on this same check.
      match unlink_origin(&app, &instance_id, &target).await {
        Ok(()) => {
          // The ledger groups rows by the origin in the instance list, so it
          // has to hear about the lost identity right away.
          refresh_and_update_instances(&app, false).await;
          Ok(UpdateCheck {
            unlinked: true,
            ..check_of(CheckStatus::NotFollowing, &target)
          })
        }
        Err(error) => Ok(UpdateCheck {
          reason: Some(error.0),
          ..check_of(CheckStatus::Unknown, &target)
        }),
      }
    }
    Err(err) => Ok(UpdateCheck {
      reason: Some(err.0),
      ..check_of(CheckStatus::Unknown, &target)
    }),
  }
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
  ensure_not_running(app, instance_id)?;
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
  let loader_change = (remote.loader != baseline.loader).then(|| LoaderChange {
    from: baseline.loader.clone(),
    to: remote.loader.clone(),
  });
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

/// A plan fast-forwards when nothing but the baseline moves. A `Retire`
/// change only rewrites the entry (the file itself stays), so it does not
/// count; everything else, a conflict or a loader change does.
fn fast_forward<'a>(
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
  };
  let preview = target.root.join(super::remote::UPDATE_DIR).join("remote-preview");
  let _ = tokio::fs::remove_dir_all(&preview).await;
  let mut result = BTreeMap::new();
  let candidates: Vec<_> = remote
    .files
    .iter()
    .filter(|file| wanted.contains_key(file.path.as_str()))
    .collect();
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
  };
  let version_id = remote.version_id.clone();
  let mut apply_choices = choices.clone();
  let merged_contents = selected_hunk_contents(&prepared, &mut apply_choices)?;
  // How every conflict was settled. Paths, hunk indexes and sides only.
  log::info!(
    "modpack update apply resolutions: instance={instance_id} plan_hash={plan_hash} resolutions=[{}]",
    describe_conflict_resolutions(&prepared.diff, &apply_choices).join(","),
  );
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
    },
  )
  .await;
  if result.is_err() {
    // A rolled back update restored the files on disk; drop stale memory.
    refresh_and_update_instances(&app, false).await;
  }
  // Applied or not, the preview was consumed.
  plan_cache::clear_plan_cache(&target.root);
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
) -> USTBLResult<UpdateOutcome> {
  // The instance was created moments ago; make sure the app knows it.
  refresh_and_update_instances(&app, false).await;
  let plan = prepare_modpack_update(app.clone(), instance_id.clone()).await?;
  if plan.reinstall_required {
    return Err(InstanceError::ModpackUpdateReinstallRequired.into());
  }
  let choices = plan
    .conflicts
    .iter()
    .map(|conflict| (conflict.path.clone(), ConflictChoice::TakeRemote))
    .collect();
  apply_modpack_update(app, instance_id, plan.plan_hash, choices).await
}

/// Undo or finish an update that was interrupted. Returns whether anything
/// had to be done.
#[tauri::command]
pub async fn recover_modpack_update(app: AppHandle, instance_id: String) -> USTBLResult<bool> {
  let instance = current_instance(&app, &instance_id)?;
  let root = instance.version_path.clone();
  let check_app = app.clone();
  let outcome = tokio::task::spawn_blocking({
    let root = root.clone();
    move || recover_checked(&root, || ensure_not_running(&check_app, &instance_id))
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
