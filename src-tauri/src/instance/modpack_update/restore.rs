//! Rebuilding a component profile on an instance that was created empty.
//!
//! An instance installed from a MultiMC archive gets its client json,
//! libraries and launch arguments from the archive's component chain. A campus
//! version publishes that chain as a snapshot instead of an archive, so the
//! same resolution runs here over the fetched snapshot files.

use super::remote::ComponentPack;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::client_json::McClientInfo;
use crate::instance::helpers::misc::get_instance_subdir_paths;
use crate::instance::helpers::modpack::baseline::BaselineLoader;
use crate::instance::helpers::modpack::multimc::profile::{ResolvedMultiMc, StagedFiles};
use crate::instance::helpers::modpack::multimc::MultiMcComponent;
use crate::instance::models::misc::{Instance, InstanceError, InstanceSubdirType, ModLoaderType};
use crate::launch::helpers::file_validator::{get_invalid_assets, get_invalid_library_files};
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::misc::get_source_priority_list;
use crate::storage::save_json_async;
use crate::tasks::commands::schedule_progressive_task_group;
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
use crate::tasks::PTaskParam;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use url::Url;

/// Write the client json, snapshot and component files of `pack` into the
/// instance and start downloading what the profile needs. Returns once the
/// files are written; the downloads run as a task group.
pub async fn restore_component_profile(
  app: &AppHandle,
  instance: &Instance,
  loader: &BaselineLoader,
  pack: &ComponentPack,
  staged: &Path,
) -> USTBLResult<()> {
  let client = app.state::<reqwest::Client>();
  let priority_list = {
    let config = app.state::<Mutex<LauncherConfig>>();
    let config = config.lock()?;
    get_source_priority_list(&config)
  };

  let components: Vec<MultiMcComponent> = serde_json::from_value(
    pack
      .mmc_pack
      .get("components")
      .cloned()
      .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?,
  )?;
  let mut files = StagedFiles {
    files: pack
      .files
      .iter()
      .map(|file| (file.path.clone(), staged.join(&file.path)))
      .collect(),
  };
  let cfg: HashMap<String, String> = pack
    .instance_cfg
    .iter()
    .map(|(key, value)| (key.clone(), value.clone()))
    .collect();
  let mut resolved = ResolvedMultiMc::resolve(
    client.inner(),
    &components,
    pack
      .mmc_pack
      .get("formatVersion")
      .and_then(|value| value.as_u64())
      .unwrap_or(1),
    String::new(),
    cfg,
    pack.mmc_pack.clone(),
    &mut files,
  )
  .await?;
  if resolved.client.client_version.as_deref() != Some(instance.version.as_str()) {
    return Err(InstanceError::ModpackUpdateLineMismatch.into());
  }

  // Same identity the importer gives an instance: named after the instance,
  // with the vanilla part kept as the base patch.
  let name = instance.name.clone();
  let mut info = resolved.client.clone();
  info.id = name.clone();
  info.jar = Some(name.clone());
  let mut vanilla_patch = info.clone();
  vanilla_patch.id = "game".to_string();
  vanilla_patch.version = instance.version.clone().into();
  vanilla_patch.inherits_from = None;
  vanilla_patch.priority = Some(0);
  info.patches.push(vanilla_patch);
  resolved.client = info;

  let subdirs = get_instance_subdir_paths(
    app,
    instance,
    &[&InstanceSubdirType::Libraries, &InstanceSubdirType::Assets],
  )
  .ok_or(InstanceError::InstanceNotFoundByID)?;
  let [libraries_dir, assets_dir] = subdirs.as_slice() else {
    return Err(InstanceError::InstanceNotFoundByID.into());
  };
  resolved.materialize_from(&mut files, libraries_dir, &instance.version_path)?;

  let mut version_info: McClientInfo = resolved.client.clone();
  if loader.loader_type != ModLoaderType::Unknown {
    version_info.patches.push(McClientInfo {
      id: loader.loader_type.to_string().to_lowercase(),
      version: Some(loader.version.clone()),
      ..Default::default()
    });
  }

  let mut task_params = Vec::<PTaskParam>::new();
  let client_download = version_info
    .downloads
    .get("client")
    .ok_or(InstanceError::ClientJsonParseError)?;
  if !client_download.url.is_empty() {
    task_params.push(PTaskParam::Download(DownloadParam {
      src: Url::parse(&client_download.url).map_err(|_| InstanceError::ClientJsonParseError)?,
      dest: instance.version_path.join(format!("{name}.jar")),
      filename: None,
      sha1: (!client_download.sha1.is_empty()).then(|| client_download.sha1.clone()),
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(Vec::new(), 10),
    }));
  }
  task_params
    .extend(get_invalid_library_files(&priority_list, libraries_dir, &version_info, false).await?);
  task_params
    .extend(get_invalid_assets(app, &version_info, &priority_list, assets_dir, false).await?);

  save_json_async(
    &version_info,
    &instance.version_path.join(format!("{name}.json")),
  )
  .await
  .map_err(|e| USTBLError(e.to_string()))?;
  schedule_progressive_task_group(
    app.clone(),
    format!("game-client?{name}"),
    task_params,
    true,
  )
  .await?;
  Ok(())
}
