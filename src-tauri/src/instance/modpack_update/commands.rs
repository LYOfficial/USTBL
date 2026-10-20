//! Tauri commands for following a modpack: check, plan, apply, recover.

use super::apply::{
  apply, can_roll_back_last, materialize_remote_file, prepare_packs, recover, roll_back_last,
  Progress, Recovery, UpdateEnv, UpdateRequest,
};
use super::diff::{resolve, Conflict, ConflictChoice, FileChange};
use super::plan::{
  count_local_changes, local_changes, prepare, prepare_with_contents, requires_reinstall,
  LocalChange,
};
use super::merge::merge_with_choices;
use super::remote::ComponentPack;
use super::restore::restore_component_profile;
use super::sources::{download_param, latest_version, resolve_remote, LatestVersion};
use crate::error::USTBLResult;
use crate::instance::commands::switch_mod_loader;
use crate::instance::helpers::misc::{get_instance_game_config, refresh_and_update_instances};
use crate::instance::helpers::modpack::baseline::{BaselineLoader, ModpackBaseline};
use crate::instance::helpers::modpack::text;
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceOrigin, ModLoader, ModLoaderStatus, ModLoaderType,
};
use crate::launch::models::LaunchingState;
use crate::resource::models::ModLoaderResourceInfo;
use crate::tasks::download::DownloadParam;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderChange {
  pub from: BaselineLoader,
  pub to: BaselineLoader,
}

#[derive(Debug, Clone, Serialize)]
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
  match latest_version(&app, &target.origin).await {
    // The status stays `Available`; the count rides along so the local
    // edits can be shown next to the update.
    Ok(Some(latest)) => Ok(UpdateCheck {
      latest_version_id: Some(latest.version_id),
      latest_label: Some(latest.label),
      local_changes: if scan_local == Some(true) {
        Some(count_local_changes(&target.root, &baseline).await?)
      } else {
        None
      },
      ..check_of(CheckStatus::Available, &target)
    }),
    Ok(None) => {
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
    Err(err) => Ok(UpdateCheck {
      reason: Some(err.0),
      ..check_of(CheckStatus::Unknown, &target)
    }),
  }
}

/// Take back the previous update. Returns whether there was one to take back.
#[tauri::command]
pub async fn rollback_modpack_update(app: AppHandle, instance_id: String) -> USTBLResult<bool> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  ensure_not_running(&app, &instance_id)?;
  let root = target.root.clone();
  let done = tokio::task::spawn_blocking(move || roll_back_last(&root))
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()))??;
  if done {
    refresh_and_update_instances(&app, false).await;
  }
  Ok(done)
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
/// except downloading the modpack archive into the update folder.
#[tauri::command]
pub async fn prepare_modpack_update(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<UpdatePlan> {
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  ensure_not_running(&app, &instance_id)?;
  let baseline = load_baseline(&target).await?;
  recover(&target.root)?;
  let latest = latest_version(&app, &target.origin)
    .await?
    .ok_or(InstanceError::ModpackUpdateStale)?;
  plan_for(&app, &target, &baseline, &latest).await
}

/// An instance created for a modpack that has not installed a version yet.
fn is_fresh(origin: &InstanceOrigin) -> bool {
  origin.version_id.is_empty()
}

fn fast_forward(changes: usize, conflicts: usize, loader_change: bool) -> bool {
  changes == 0 && conflicts == 0 && !loader_change
}

async fn plan_for(
  app: &AppHandle,
  target: &Target,
  baseline: &ModpackBaseline,
  latest: &LatestVersion,
) -> USTBLResult<UpdatePlan> {
  let remote = resolve_remote(
    app,
    &target.origin,
    &target.root,
    &target.instance.name,
    latest,
  )
  .await?;
  if requires_reinstall(baseline, &remote, is_fresh(&target.origin)) {
    return Ok(UpdatePlan {
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
    });
  }
  let first = prepare(&target.root, baseline, &remote).await?;
  let remote_contents = materialize_text_candidates(app, target, &remote, &first.diff.conflicts).await?;
  let prepared = prepare_with_contents(&target.root, baseline, &remote, &remote_contents).await?;
  let loader_change = (remote.loader != baseline.loader).then(|| LoaderChange {
    from: baseline.loader.clone(),
    to: remote.loader.clone(),
  });
  Ok(UpdatePlan {
    from_version_id: target.origin.version_id.clone(),
    to_version_id: remote.version_id.clone(),
    to_label: remote.label.clone(),
    fast_forward: fast_forward(
      prepared.diff.changes.len() + prepared.diff.auto_merged.len(),
      prepared.diff.conflicts.len(),
      loader_change.is_some(),
    ),
    changes: prepared.diff.changes,
    conflicts: prepared.diff.conflicts,
    auto_merged: prepared.diff.auto_merged,
    loader_change,
    reinstall_required: false,
    plan_hash: prepared.plan_hash,
  })
}

fn selected_hunk_contents(
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
    result.insert(path.clone(), merged);
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
  let target = target(&app, &instance_id)?.ok_or(InstanceError::ModpackUpdateUnsupported)?;
  ensure_not_running(&app, &instance_id)?;
  let baseline = load_baseline(&target).await?;
  recover(&target.root)?;
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
  let first = prepare(&target.root, &baseline, &remote).await?;
  let remote_contents = materialize_text_candidates(&app, &target, &remote, &first.diff.conflicts).await?;
  let prepared = prepare_with_contents(&target.root, &baseline, &remote, &remote_contents).await?;
  if prepared.plan_hash != plan_hash {
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
  let result = apply(
    &env,
    UpdateRequest {
      root: target.root.clone(),
      instance_name: target.instance.name.clone(),
      operations: resolve(&prepared.diff, &apply_choices),
      remote,
      origin: target.origin.clone(),
      previous_loader: baseline.loader.clone(),
      components,
      restore_components,
      merged_contents,
    },
  )
  .await;
  if result.is_err() {
    // A rolled back update restored the files on disk; drop stale memory.
    refresh_and_update_instances(&app, false).await;
  }
  result?;
  Ok(UpdateOutcome {
    version_id,
    loader_install_pending: env.loader_install_pending.load(Ordering::SeqCst),
  })
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
  let outcome = tokio::task::spawn_blocking(move || recover(&root))
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()))??;
  if outcome == Recovery::RolledBack {
    refresh_and_update_instances(&app, false).await;
  }
  Ok(outcome != Recovery::Nothing)
}

#[cfg(test)]
mod tests {
  use super::fast_forward;

  #[test]
  fn only_a_plan_with_nothing_to_do_fast_forwards() {
    assert!(fast_forward(0, 0, false));
    assert!(!fast_forward(1, 0, false));
    assert!(!fast_forward(0, 1, false));
    assert!(!fast_forward(0, 0, true));
  }
}
