use super::helpers::loader::fabric::remove_fabric_api_mods;
use crate::error::USTBLResult;
use crate::instance::helpers::client_json::{replace_native_libraries, McClientInfo};
use crate::instance::helpers::deletion::{delete_instance_files, InstanceDeletionProgress};
use crate::instance::helpers::game_version::{build_game_version_cmp_fn, compare_game_versions};
use crate::instance::helpers::identity::{new_instance_id, write_display_name};
use crate::instance::helpers::loader::common::{execute_processors, install_mod_loader};
use crate::instance::helpers::loader::forge::download_forge_libraries;
use crate::instance::helpers::loader::forge::InstallProfile;
use crate::instance::helpers::loader::neoforge::download_neoforge_libraries;
use crate::instance::helpers::loader::optifine::download_optifine_libraries;
use crate::instance::helpers::loader::optifine::{
  download_optifine_installer, finish_optifine_install,
};
use crate::instance::helpers::loader_lifecycle::{
  claim_install, complete_install, fail_download, loader_patch_ready, optifine_patch_ready,
  preparation_allowed, LOADER_LIFECYCLE,
};
use crate::instance::helpers::misc::{
  check_folder, existing_folder_names, get_instance_game_config, get_instance_subdir_path_by_id,
  get_instance_subdir_paths, migrate_selected_instance_id, owns_files,
  refresh_and_update_instances, resolve_isolation, InstanceFolderCheck,
};
use crate::instance::helpers::modpack::base_store;
use crate::instance::helpers::modpack::baseline::{hash_file, BaselineLoader, ModpackBaseline};
use crate::instance::helpers::modpack::export::{
  build_component_export, collect_modrinth_files, create_modpack_zip, generate_modrinth_manifest,
  generate_multimc_instance_cfg, generate_multimc_manifest, list_files, validate_export_options,
  ExportFormat, ExportModpackOptions,
};
use crate::instance::helpers::modpack::misc::ModpackMetaInfo;
use crate::instance::helpers::modpack::multimc::snapshot::{
  is_component_instance, ComponentSnapshot,
};
use crate::instance::helpers::mods::common::{
  compress_icon, get_mod_info_from_dir, get_mod_info_from_jar,
};
use crate::instance::helpers::mods::enrich::{cancel_mod_info_refresh, spawn_mod_info_refresh};
use crate::instance::helpers::mods::remote::apply_cached_infos;
use crate::instance::helpers::options_txt::get_zh_hans_lang_tag;
use crate::instance::helpers::resourcepack::{
  load_resourcepack_from_dir, load_resourcepack_from_zip,
};
use crate::instance::helpers::server::{
  get_servers_nbt_path_by_instance_id, load_servers_info_from_nbt, query_servers_online,
  save_servers_to_nbt, GameServerInfo,
};
use crate::instance::helpers::world::{load_level_data_from_nbt, load_world_info_from_dir};
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceOrigin, InstanceSubdirType, InstanceSummary, LocalModInfo,
  ModLoader, ModLoaderStatus, ModLoaderType, ModpackFileList, ModpackOriginInput, OptiFine,
  OriginPlatform, ResourcePackInfo, SchematicInfo, ScreenshotInfo, ShaderPackInfo,
};
use crate::instance::models::world::base::WorldInfo;
use crate::instance::models::world::level::LevelData;
use crate::launch::helpers::file_validator::{get_invalid_assets, get_invalid_library_files};
use crate::launcher_config::helpers::misc::get_global_game_config;
use crate::launcher_config::models::{GameConfig, GameDirectory, LauncherConfig};
use crate::partial::{PartialError, PartialUpdate};
use crate::resource::helpers::misc::{get_download_api, get_source_priority_list};
use crate::resource::models::{
  GameClientResourceInfo, ModLoaderResourceInfo, OptiFineResourceInfo, ResourceType, SourceType,
};
use crate::storage::{load_json_async, save_json_async, Storage};
use crate::tasks::commands::schedule_progressive_task_group;
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
use crate::tasks::PTaskParam;
use crate::utils::fs::{
  copy_whole_dir, create_url_shortcut, generate_unique_filename, get_files_with_regex,
  get_subdirectories, RemoveDirGuard,
};
use crate::utils::image::ImageWrapper;
use crate::utils::web::fetch_json_with_fallbacks;
use lazy_static::lazy_static;
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use tokio;
use tokio::sync::Semaphore;
use url::Url;
use zip::read::ZipArchive;

#[tauri::command]
pub async fn retrieve_instance_list(app: AppHandle) -> USTBLResult<Vec<InstanceSummary>> {
  refresh_and_update_instances(&app, false).await; // firstly refresh and update
  let global_game_config = get_global_game_config(&app);
  let mut summary_list = Vec::new();

  let instance_binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let mut instances = instance_binding.lock().unwrap().clone();
  for (id, instance) in crate::instance::modpack_update::commands::pending_campus_instances(&app) {
    instances.entry(id).or_insert(instance);
  }
  // The refresh covers the main table; this also covers a pending install.
  migrate_selected_instance_id(&app, &instances);
  for (id, instance) in instances.iter() {
    let (is_version_isolated, isolation_source) = resolve_isolation(instance, &global_game_config);

    summary_list.push(
      InstanceSummary::from_instance(
        &app,
        id.clone(),
        instance,
        is_version_isolated,
        isolation_source.as_str(),
      )
      .await,
    );
  }

  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let mut config_state = config_binding.lock()?;
  // sort instances (starred instance will be pinned to top by frontend)
  let version_cmp_fn = build_game_version_cmp_fn(&app);
  match config_state.states.all_instances_page.sort_by.as_str() {
    "versionAsc" => {
      summary_list.sort_by(|a, b| {
        version_cmp_fn(&a.version, &b.version)
          .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
      });
    }
    "versionDesc" => {
      summary_list.sort_by(|a, b| {
        version_cmp_fn(&b.version, &a.version)
          .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
      });
    }
    _ => {
      summary_list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    }
  }

  // ensure an instance is selected if instance list is not empty
  if !summary_list.is_empty()
    && !summary_list
      .iter()
      .any(|instance| instance.id == config_state.states.shared.selected_instance_id)
  {
    config_state.partial_update(
      &app,
      "states.shared.selected_instance_id",
      &serde_json::to_string(&summary_list[0].id).unwrap_or_default(),
    )?;
    config_state.save()?;
  }

  Ok(summary_list)
}

#[tauri::command]
pub async fn update_instance_config(
  app: AppHandle,
  instance_id: String,
  key_path: String,
  value: String,
) -> USTBLResult<()> {
  let instance = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let mut state = binding.lock().unwrap();
    let instance = state
      .get_mut(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?;
    let key_path = {
      let mut snake = String::new();
      for (i, ch) in key_path.char_indices() {
        if i > 0 && ch.is_uppercase() {
          snake.push('_');
        }
        snake.push(ch.to_ascii_lowercase());
      }
      snake
    };
    // PartialUpdate not support Option<T> yet
    if key_path == "description" {
      instance.description = serde_json::from_str::<String>(&value).unwrap_or(value);
    } else if key_path == "icon_src" {
      instance.icon_src = serde_json::from_str::<String>(&value).unwrap_or(value);
    } else if key_path == "starred" {
      instance.starred = value.parse::<bool>()?;
    } else if key_path == "use_spec_game_config" {
      let value = value.parse::<bool>()?;
      instance.use_spec_game_config = value;
      if value && instance.spec_game_config.is_none() {
        instance.spec_game_config = Some(get_global_game_config(&app));
      }
    } else if key_path.starts_with("spec_game_config.") {
      let key = key_path.split_at("spec_game_config.".len()).1;
      if let Some(game_config) = instance.spec_game_config.as_mut() {
        game_config.update(key, &value)?;
      }
    } else {
      return Err(PartialError::NotFound.into());
    }
    instance.clone()
  };
  instance.save_json_cfg().await?;
  Ok(())
}

#[tauri::command]
pub fn retrieve_instance_game_config(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<GameConfig> {
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let state = binding.lock().unwrap();
  let instance = state
    .get(&instance_id)
    .ok_or(InstanceError::InstanceNotFoundByID)?;

  Ok(get_instance_game_config(&app, instance))
}

#[tauri::command]
pub async fn restore_instance_game_config(app: AppHandle, instance_id: String) -> USTBLResult<()> {
  let instance = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let mut state = binding.lock().unwrap();
    let instance = state
      .get_mut(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?;
    instance.spec_game_config = Some(get_global_game_config(&app));
    instance.clone()
  };
  instance.save_json_cfg().await?;
  Ok(())
}

#[tauri::command]
pub fn retrieve_instance_subdir_path(
  app: AppHandle,
  instance_id: String,
  dir_type: InstanceSubdirType,
) -> USTBLResult<PathBuf> {
  match get_instance_subdir_path_by_id(&app, &instance_id, &dir_type) {
    Some(path) => Ok(path),
    None => Err(InstanceError::InstanceNotFoundByID.into()),
  }
}

#[tauri::command]
pub async fn delete_instance(
  app: AppHandle,
  instance_id: String,
  on_progress: tauri::ipc::Channel<InstanceDeletionProgress>,
) -> USTBLResult<()> {
  let version_path = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let instances = binding.lock()?;
    instances
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .version_path
      .clone()
  };

  tauri::async_runtime::spawn_blocking(move || -> USTBLResult<()> {
    remove_instance_blocking(&app, &instance_id, &version_path, |progress| {
      let _ = on_progress.send(progress);
    })
  })
  .await?
}

/// Delete an instance's files, forget it and move the selection off it.
/// Blocking; shared by `delete_instance` and discarding a first install.
pub(crate) fn remove_instance_blocking(
  app: &AppHandle,
  instance_id: &str,
  version_path: &Path,
  report: impl FnMut(InstanceDeletionProgress),
) -> USTBLResult<()> {
  cancel_mod_info_refresh(instance_id);
  delete_instance_files(version_path, report)?;
  crate::instance::modpack_update::plan::invalidate_scan_cache(version_path);

  let next_instance_id = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let mut instances = binding.lock()?;
    forget_instance(&mut instances, instance_id)
  };
  let binding = app.state::<Mutex<LauncherConfig>>();
  let mut config = binding.lock()?;
  if config.states.shared.selected_instance_id == instance_id {
    config.partial_update(
      app,
      "states.shared.selected_instance_id",
      &serde_json::to_string(&next_instance_id)?,
    )?;
    config.save()?;
  }
  Ok(())
}

/// Drop the instance from the table; returns the ID to select instead.
fn forget_instance(instances: &mut HashMap<String, Instance>, instance_id: &str) -> String {
  instances.remove(instance_id);
  instances.keys().next().cloned().unwrap_or_default()
}

/// Changes only the display name; the folder, jar and version json stay as they
/// are, so it also works while the game runs. An empty name restores the default
/// (the folder name). Returns the name now shown.
#[tauri::command]
pub async fn rename_instance(
  app: AppHandle,
  instance_id: String,
  new_name: String,
) -> USTBLResult<String> {
  let display_name = new_name.trim().to_string();
  let in_table = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock().unwrap();
    state
      .get(&instance_id)
      .map(|instance| (instance.version_path.clone(), instance.name.clone()))
  };
  let (version_path, folder, pending) = match in_table {
    Some((version_path, folder)) => (version_path, folder, false),
    // A pending install is not in the table yet, but it has a config to write.
    None => {
      let pending = crate::instance::modpack_update::commands::pending_campus_instances(&app);
      let instance = pending
        .get(&instance_id)
        .ok_or(InstanceError::InstanceNotFoundByID)?;
      (instance.version_path.clone(), instance.name.clone(), true)
    }
  };
  if !write_display_name(&version_path, &display_name) {
    return Err(InstanceError::FileCreationFailed.into());
  }
  if !pending {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let mut state = binding.lock().unwrap();
    if let Some(instance) = state.get_mut(&instance_id) {
      instance.display_name = display_name.clone();
    }
  }
  Ok(if display_name.is_empty() {
    folder
  } else {
    display_name
  })
}

/// Whether `folder` can still be created under `versions`, or which name to use
/// instead.
#[tauri::command]
pub fn check_instance_folder(
  directory: GameDirectory,
  folder: String,
) -> USTBLResult<InstanceFolderCheck> {
  let existing = existing_folder_names(&directory.dir.join("versions"));
  Ok(check_folder(&existing, &folder))
}

#[tauri::command]
pub fn copy_resource_to_instances(
  app: AppHandle,
  src_file_path: String,
  tgt_inst_ids: Vec<String>,
  tgt_dir_type: InstanceSubdirType,
  decompress: bool,
) -> USTBLResult<()> {
  let src_path = Path::new(&src_file_path);

  if src_path.is_file() {
    let file_name = src_path
      .file_name()
      .ok_or(InstanceError::InvalidSourcePath)?;

    for tgt_inst_id in tgt_inst_ids {
      let tgt_path = match get_instance_subdir_path_by_id(&app, &tgt_inst_id, &tgt_dir_type) {
        Some(path) => path,
        None => return Err(InstanceError::InstanceNotFoundByID.into()),
      };

      if !tgt_path.exists() {
        fs::create_dir_all(&tgt_path).map_err(|_| InstanceError::FolderCreationFailed)?;
      }

      if decompress {
        let base_name = src_path
          .extension()
          .and_then(|ext| if ext == "zip" { Some(()) } else { None })
          .and_then(|_| Path::new(file_name).file_stem())
          .unwrap_or(file_name);
        let dest_path = generate_unique_filename(&tgt_path, base_name);

        // extract zip
        let file = fs::File::open(src_path).map_err(|_| InstanceError::ZipFileProcessFailed)?;
        let mut archive = ZipArchive::new(file).map_err(|_| InstanceError::ZipFileProcessFailed)?;

        fs::create_dir_all(&dest_path).map_err(|_| InstanceError::FolderCreationFailed)?;

        archive
          .extract(&dest_path)
          .map_err(|_| InstanceError::ZipFileProcessFailed)?;
      } else {
        let dest_path = generate_unique_filename(&tgt_path, file_name);
        fs::copy(&src_file_path, &dest_path).map_err(|_| InstanceError::FileCopyFailed)?;
      }
    }
  } else if src_path.is_dir() {
    for tgt_inst_id in tgt_inst_ids {
      let tgt_path = match get_instance_subdir_path_by_id(&app, &tgt_inst_id, &tgt_dir_type) {
        Some(path) => path,
        None => return Err(InstanceError::InstanceNotFoundByID.into()),
      };

      if !tgt_path.exists() {
        fs::create_dir_all(&tgt_path).map_err(|_| InstanceError::FolderCreationFailed)?;
      }

      let dest_path = generate_unique_filename(&tgt_path, src_path.file_name().unwrap());
      copy_whole_dir(src_path, &dest_path).map_err(|_| InstanceError::FileCopyFailed)?;
    }
  } else {
    return Err(InstanceError::InvalidSourcePath.into());
  }
  Ok(())
}

#[tauri::command]
pub fn move_resource_to_instance(
  app: AppHandle,
  src_file_path: String,
  tgt_inst_id: String,
  tgt_dir_type: InstanceSubdirType,
) -> USTBLResult<()> {
  let tgt_path = match get_instance_subdir_path_by_id(&app, &tgt_inst_id, &tgt_dir_type) {
    Some(path) => path,
    None => return Err(InstanceError::InstanceNotFoundByID.into()),
  };

  let src_path = Path::new(&src_file_path);
  if !src_path.is_dir() && !src_path.is_file() {
    return Err(InstanceError::InvalidSourcePath.into());
  }

  let file_name = src_path
    .file_name()
    .ok_or(InstanceError::InvalidSourcePath)?;

  if !tgt_path.exists() {
    fs::create_dir_all(&tgt_path).map_err(|_| InstanceError::FolderCreationFailed)?;
  }

  let dest_path = generate_unique_filename(&tgt_path, file_name);
  fs::rename(&src_file_path, &dest_path).map_err(|_| InstanceError::FileMoveFailed)?;
  Ok(())
}

#[tauri::command]
pub async fn retrieve_world_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<WorldInfo>> {
  let game_version = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    let instance = state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?;
    instance.version.clone()
  };

  // difficulty setting was introduced in game version 14w02a
  let has_difficulty_support = compare_game_versions(&app, &game_version, "14w02a", false)
    .await
    .is_ge();

  let mut world_list: Vec<WorldInfo> = Vec::new();

  let worlds_dir =
    match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::Saves) {
      Some(path) => path,
      None => return Ok(Vec::new()),
    };
  if let Ok(world_paths) = get_subdirectories(worlds_dir) {
    for path in world_paths {
      if let Ok(info) = load_world_info_from_dir(&path, has_difficulty_support).await {
        world_list.push(info);
      }
    }
  }

  Ok(world_list)
}

#[tauri::command]
pub async fn retrieve_game_server_list(
  app: AppHandle,
  instance_id: String,
  query_online: bool,
) -> USTBLResult<Vec<GameServerInfo>> {
  // query_online is false, return local data from nbt (servers.dat)
  let nbt_path = match get_servers_nbt_path_by_instance_id(&app, &instance_id) {
    Some(path) => path,
    None => return Ok(Vec::new()),
  };
  let mut game_servers = match load_servers_info_from_nbt(&nbt_path).await {
    Ok(servers) => servers,
    Err(_) => return Err(InstanceError::ServerNbtReadError.into()),
  };

  // skip hidden servers
  game_servers.retain(|server| !server.hidden);

  // query_online is true, amend query and return player count and online status
  if query_online {
    game_servers = query_servers_online(game_servers).await?;
  }

  Ok(game_servers)
}

#[tauri::command]
pub async fn delete_game_server(
  app: AppHandle,
  instance_id: String,
  server_addr: String,
) -> USTBLResult<()> {
  let nbt_path = match get_servers_nbt_path_by_instance_id(&app, &instance_id) {
    Some(path) => path,
    None => return Err(InstanceError::InstanceNotFoundByID.into()),
  };
  let mut existing_servers = load_servers_info_from_nbt(&nbt_path).await?;

  existing_servers.retain(|server| server.ip != server_addr);
  save_servers_to_nbt(&nbt_path, &existing_servers)
    .await
    .map_err(|_| InstanceError::FileOperationError)?;

  Ok(())
}

#[tauri::command]
pub async fn add_game_server(
  app: AppHandle,
  instance_id: String,
  server_addr: String,
  server_name: String,
) -> USTBLResult<()> {
  let nbt_path = match get_servers_nbt_path_by_instance_id(&app, &instance_id) {
    Some(path) => path,
    None => return Err(InstanceError::InstanceNotFoundByID.into()),
  };
  let mut existing_servers = load_servers_info_from_nbt(&nbt_path).await?;

  if existing_servers
    .iter()
    .any(|server| server.ip == server_addr)
  {
    return Err(InstanceError::DuplicateServer.into());
  }

  existing_servers.push(GameServerInfo {
    ip: server_addr,
    name: server_name,
    ..Default::default()
  });
  save_servers_to_nbt(&nbt_path, &existing_servers)
    .await
    .map_err(|_| InstanceError::FileOperationError)?;

  Ok(())
}

#[tauri::command]
pub async fn retrieve_local_mod_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<LocalModInfo>> {
  let expected_loader_type = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock().unwrap();
    let instance = state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?;

    if instance.mod_loader.loader_type != ModLoaderType::Unknown {
      Some(instance.mod_loader.loader_type)
    } else {
      None
    }
  };

  let mods_dir = match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::Mods)
  {
    Some(path) => path,
    None => return Ok(Vec::new()),
  };

  let valid_extensions = RegexBuilder::new(r"\.(jar|zip)(\.disabled)*$")
    .case_insensitive(true)
    .build()
    .unwrap();

  let mod_paths = get_files_with_regex(&mods_dir, &valid_extensions).unwrap_or_default();
  let mut tasks = Vec::new();
  let semaphore = Arc::new(Semaphore::new(
    std::thread::available_parallelism().unwrap().into(),
  ));
  for path in mod_paths {
    let permit = semaphore
      .clone()
      .acquire_owned()
      .await
      .map_err(|_| InstanceError::SemaphoreAcquireFailed)?;
    let task = tokio::spawn(async move {
      log::debug!("Load mod info from jar: {}", path.display());
      let info = get_mod_info_from_jar(&path, expected_loader_type)
        .await
        .ok();
      drop(permit);
      info
    });
    tasks.push(task);
  }
  #[cfg(debug_assertions)]
  {
    // mod information detection from folders is only used for debugging.
    let mod_paths = get_subdirectories(&mods_dir).unwrap_or_default();
    for path in mod_paths {
      let permit = semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| InstanceError::SemaphoreAcquireFailed)?;
      let task = tokio::spawn(async move {
        log::debug!("Load mod info from dir: {}", path.display());
        let info = get_mod_info_from_dir(&path, expected_loader_type)
          .await
          .ok();
        drop(permit);
        info
      });
      tasks.push(task);
    }
  }
  let mut mod_infos = Vec::new();
  for task in tasks {
    if let Ok(Some(mod_info)) = task.await {
      mod_infos.push(mod_info);
    }
  }

  // check potential incompatibility
  mod_infos.iter_mut().for_each(|mod_info| {
    if let Some(loader_type) = &expected_loader_type {
      mod_info.potential_incompatibility = mod_info.loader_type != *loader_type;
    } else {
      mod_info.potential_incompatibility = false;
    }
  });

  // sort by name (and version)
  mod_infos.sort();

  // Names and descriptions that need the network are completed in the
  // background; the frontend is told by an event (see `mods::enrich`).
  let pending = apply_cached_infos(&app, &mut mod_infos);
  spawn_mod_info_refresh(&app, &instance_id, pending);

  Ok(mod_infos)
}

#[tauri::command]
pub async fn retrieve_resource_pack_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<ResourcePackInfo>> {
  // Get the resource packs list based on the instance
  let resource_packs_dir =
    match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::ResourcePacks) {
      Some(path) => path,
      None => return Ok(Vec::new()),
    };
  let mut info_list: Vec<ResourcePackInfo> = Vec::new();

  let valid_extensions = RegexBuilder::new(r"\.zip$")
    .case_insensitive(true)
    .build()
    .unwrap();

  for path in get_files_with_regex(&resource_packs_dir, &valid_extensions).unwrap_or(vec![]) {
    if let Ok((description, icon_src)) = load_resourcepack_from_zip(&path) {
      let name = match path.file_stem() {
        Some(stem) => stem.to_string_lossy().to_string(),
        None => String::new(),
      };
      info_list.push(ResourcePackInfo {
        name,
        description,
        icon_src: icon_src.map(ImageWrapper::from).map(compress_icon),
        file_path: path.clone(),
      });
    }
  }

  for path in get_subdirectories(&resource_packs_dir).unwrap_or(vec![]) {
    if let Ok((description, icon_src)) = load_resourcepack_from_dir(&path).await {
      let name = match path.file_stem() {
        Some(stem) => stem.to_string_lossy().to_string(),
        None => String::new(),
      };
      info_list.push(ResourcePackInfo {
        name,
        description,
        icon_src: icon_src.map(ImageWrapper::from).map(compress_icon),
        file_path: path.clone(),
      });
    }
  }
  Ok(info_list)
}

#[tauri::command]
pub async fn retrieve_server_resource_pack_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<ResourcePackInfo>> {
  let resource_packs_dir = match get_instance_subdir_path_by_id(
    &app,
    &instance_id,
    &InstanceSubdirType::ServerResourcePacks,
  ) {
    Some(path) => path,
    None => return Ok(Vec::new()),
  };
  let mut info_list: Vec<ResourcePackInfo> = Vec::new();

  let valid_extensions = RegexBuilder::new(r".*")
    .case_insensitive(true)
    .build()
    .unwrap();

  for path in get_files_with_regex(&resource_packs_dir, &valid_extensions).unwrap_or(vec![]) {
    if let Ok((description, icon_src)) = load_resourcepack_from_zip(&path) {
      let name = match path.file_stem() {
        Some(stem) => stem.to_string_lossy().to_string(),
        None => String::new(),
      };
      info_list.push(ResourcePackInfo {
        name,
        description,
        icon_src: icon_src.map(ImageWrapper::from).map(compress_icon),
        file_path: path.clone(),
      });
    }
  }

  for path in get_subdirectories(&resource_packs_dir).unwrap_or(vec![]) {
    if let Ok((description, icon_src)) = load_resourcepack_from_dir(&path).await {
      let name = match path.file_stem() {
        Some(stem) => stem.to_string_lossy().to_string(),
        None => String::new(),
      };

      info_list.push(ResourcePackInfo {
        name,
        description,
        icon_src: icon_src.map(ImageWrapper::from).map(compress_icon),
        file_path: path.clone(),
      });
    }
  }
  Ok(info_list)
}

#[tauri::command]
pub fn retrieve_schematic_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<SchematicInfo>> {
  let schematics_dir =
    match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::Schematics) {
      Some(path) => path,
      None => return Ok(Vec::new()),
    };

  if !schematics_dir.exists() {
    return Ok(Vec::new());
  }
  let valid_extensions = RegexBuilder::new(r"\.(litematic|schematic)$")
    .case_insensitive(true)
    .build()
    .unwrap();
  let mut schematic_list = Vec::new();
  for schematic_path in get_files_with_regex(schematics_dir.as_path(), &valid_extensions)? {
    schematic_list.push(SchematicInfo {
      name: schematic_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string(),
      file_path: schematic_path,
    });
  }

  Ok(schematic_list)
}

#[tauri::command]
pub fn retrieve_shader_pack_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<ShaderPackInfo>> {
  // Get the shaderpacks directory based on the instance
  let shaderpacks_dir =
    match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::ShaderPacks) {
      Some(path) => path,
      None => return Ok(Vec::new()),
    };

  if !shaderpacks_dir.exists() {
    return Ok(Vec::new());
  }

  let valid_extensions = RegexBuilder::new(r"\.zip$")
    .case_insensitive(true)
    .build()
    .unwrap();
  let mut shaderpack_list = Vec::new();
  for path in get_files_with_regex(shaderpacks_dir, &valid_extensions)? {
    shaderpack_list.push(ShaderPackInfo {
      file_name: path.file_stem().unwrap().to_string_lossy().to_string(),
      file_path: path,
    });
  }

  Ok(shaderpack_list)
}

#[tauri::command]
pub fn retrieve_screenshot_list(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<Vec<ScreenshotInfo>> {
  let screenshots_dir =
    match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::Screenshots) {
      Some(path) => path,
      None => return Ok(Vec::new()),
    };

  if !screenshots_dir.exists() {
    return Ok(Vec::new());
  }

  // The default screenshot format in Minecraft is PNG. For broader compatibility, JPG and JPEG formats are also included here.
  let valid_extensions = RegexBuilder::new(r"\.(jpg|jpeg|png)$")
    .case_insensitive(true)
    .build()
    .unwrap();
  let mut screenshot_list = Vec::new();
  for path in get_files_with_regex(screenshots_dir, &valid_extensions)? {
    let metadata = path.metadata().unwrap();
    let modified_time = metadata.modified().unwrap();
    let timestamp = modified_time
      .duration_since(SystemTime::UNIX_EPOCH)
      .unwrap()
      .as_secs();
    screenshot_list.push(ScreenshotInfo {
      file_name: path.file_stem().unwrap().to_string_lossy().to_string(),
      file_path: path,
      time: timestamp,
    });
  }

  Ok(screenshot_list)
}

lazy_static! {
  static ref RENAME_LOCK: Mutex<()> = Mutex::new(());
  static ref RENAME_REGEX: Regex = RegexBuilder::new(r"^(.*?)(\.disabled)*$")
    .case_insensitive(true)
    .build()
    .unwrap();
}

#[tauri::command]
pub fn toggle_mod_by_extension(file_path: PathBuf, enable: bool) -> USTBLResult<()> {
  let _lock = RENAME_LOCK.lock().expect("Failed to acquire lock");
  if !file_path.is_file() {
    return Err(InstanceError::FileNotFoundError.into());
  }

  let file_name = file_path
    .file_name()
    .unwrap_or_default()
    .to_str()
    .unwrap_or_default();

  let new_name = if enable {
    if let Some(captures) = RENAME_REGEX.captures(file_name) {
      captures
        .get(1)
        .map(|m| m.as_str())
        .unwrap_or(file_name)
        .to_string()
    } else {
      file_name.to_string()
    }
  } else if RENAME_REGEX.is_match(file_name) {
    format!("{}.disabled", file_name)
  } else {
    file_name.to_string()
  };
  let new_path = file_path.with_file_name(new_name);

  if new_path != file_path {
    fs::rename(&file_path, &new_path)?;
  }

  Ok(())
}

#[tauri::command]
pub async fn retrieve_world_details(
  app: AppHandle,
  instance_id: String,
  world_name: String,
) -> USTBLResult<LevelData> {
  let worlds_dir =
    match get_instance_subdir_path_by_id(&app, &instance_id, &InstanceSubdirType::Saves) {
      Some(path) => path,
      None => return Err(InstanceError::WorldNotExistError.into()),
    };
  let level_path = worlds_dir.join(world_name).join("level.dat");
  if tokio::fs::metadata(&level_path).await.is_err() {
    return Err(InstanceError::LevelNotExistError.into());
  }
  if let Ok(level_data) = load_level_data_from_nbt(&level_path).await {
    Ok(level_data)
  } else {
    Err(InstanceError::LevelParseError.into())
  }
}

#[tauri::command]
pub fn create_launch_desktop_shortcut(
  app: AppHandle,
  instance_id: String,
  icon_src: String,
) -> USTBLResult<()> {
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let state = binding
    .lock()
    .map_err(|_| InstanceError::InstanceNotFoundByID)?;
  let instance = state
    .get(&instance_id)
    .ok_or(InstanceError::InstanceNotFoundByID)?;

  // The display name is free text, so make it safe as a file name.
  let name = match sanitize_filename::sanitize(instance.shown_name()) {
    name if name.trim().is_empty() => instance.name.clone(),
    name => name,
  };
  let encoded_id = url::form_urlencoded::Serializer::new(String::new())
    .append_pair("id", &instance.id)
    .finish()
    .replace("+", "%20");
  let url = format!("ustbl://launch?{}", encoded_id);

  #[cfg(any(target_os = "windows", target_os = "linux"))]
  let icon_path = {
    use crate::instance::helpers::misc::create_instance_shortcut_icon;
    create_instance_shortcut_icon(&app, instance, &icon_src).ok()
  };
  #[cfg(target_os = "macos")]
  let icon_path = {
    let _ = icon_src; // explicitly consume to avoid warning
    None
  };

  create_url_shortcut(&app, name, url, icon_path)
    .map_err(|_| InstanceError::ShortcutCreationFailed)?;

  Ok(())
}

/// The origin an instance is created with, for a modpack that installs after
/// the instance exists. A campus or platform install has no archive yet; a
/// local import has one, copied in by `create_instance`. Either way the origin
/// starts at the placeholder version `""` and the update engine installs the
/// real version into the instance like any other update.
fn creation_origin(
  input: Option<ModpackOriginInput>,
  game_version: &str,
  loader_type: ModLoaderType,
) -> Option<InstanceOrigin> {
  let input = input?;
  let (loader_type, follow) = match input.platform {
    OriginPlatform::Campus if input.expected_sha1.is_empty() => (loader_type, true),
    OriginPlatform::Modrinth | OriginPlatform::CurseForge
      if !input.version_id.is_empty() && !input.expected_sha1.is_empty() =>
    {
      (input.loader_type, true)
    }
    // Nothing is followed: the pack was picked from disk, not published.
    OriginPlatform::Local if input.local_path.is_some() => (input.loader_type, false),
    _ => return None,
  };
  Some(InstanceOrigin {
    platform: input.platform,
    project_id: input.project_id,
    version_id: String::new(),
    game_version: game_version.to_string(),
    loader_type,
    follow,
    pack_format: None,
  })
}

/// Copy the picked archive into the instance's update folder under its SHA-1
/// and describe it as the version to install. The original may be moved or
/// deleted before the install runs, or while it is retried.
async fn stage_local_archive(
  source: &str,
  version_path: &Path,
  label: String,
) -> USTBLResult<crate::instance::modpack_update::commands::InstallTarget> {
  let (source, root) = (PathBuf::from(source), version_path.to_path_buf());
  tokio::task::spawn_blocking(move || {
    let (sha1, _) = hash_file(&source)?;
    let target = crate::instance::modpack_update::sources::archive_path(&root, &sha1);
    let parent = target
      .parent()
      .ok_or(InstanceError::FileCreationFailed)?
      .to_path_buf();
    fs::create_dir_all(&parent)?;
    let temporary = target.with_extension("zip.part");
    fs::copy(&source, &temporary)?;
    fs::rename(&temporary, &target)?;
    Ok::<_, crate::error::USTBLError>(crate::instance::modpack_update::commands::InstallTarget {
      version_id: sha1.clone(),
      label,
      url: String::new(),
      sha1,
    })
  })
  .await
  .map_err(|e| crate::error::USTBLError(e.to_string()))?
}

fn apply_creation_isolation(instance: &mut Instance) {
  if owns_files(instance) {
    instance.isolation_locked = true;
  }
}

#[tauri::command]
pub async fn create_instance(
  app: AppHandle,
  directory: GameDirectory,
  name: String,
  description: String,
  icon_src: String,
  game: GameClientResourceInfo,
  mod_loader: ModLoaderResourceInfo,
  optifine: Option<OptiFineResourceInfo>,
  mut is_install_fabric_api: Option<bool>,
  modpack_version: Option<String>,
  origin: Option<ModpackOriginInput>,
) -> USTBLResult<String> {
  let client = app.state::<reqwest::Client>();
  let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
  // Get priority list
  let priority_list = {
    let launcher_config = launcher_config_state.lock()?;
    get_source_priority_list(&launcher_config)
  };

  // The frontend checks the folder first; this is the check that counts. Creating
  // the directory claims the name, so two creations cannot both pass.
  let versions_dir = directory.dir.join("versions");
  let version_path = versions_dir.join(&name);
  if !check_folder(&existing_folder_names(&versions_dir), &name).available {
    return Err(InstanceError::ConflictNameError.into());
  }
  fs::create_dir_all(&versions_dir)?;
  match fs::create_dir(&version_path) {
    Ok(()) => {}
    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
      return Err(InstanceError::ConflictNameError.into());
    }
    Err(e) => return Err(e.into()),
  }

  // Guard removes version_path on any early return (errors)
  let dir_guard = RemoveDirGuard::new(version_path.clone());

  let origin_input = origin.clone();
  let origin = creation_origin(origin, &game.id, mod_loader.loader_type);

  let optifine_info = optifine.as_ref().map(|info| OptiFine {
    filename: info.filename.clone(),
    version: format!("{}_{}", info.r#type, info.patch),
    status: ModLoaderStatus::NotDownloaded,
  });

  // Create instance config
  let mut instance = Instance {
    id: new_instance_id(),
    name: name.clone(),
    display_name: String::new(),
    game_dir: directory.name.clone(),
    version: game.id.clone(),
    version_path: version_path.clone(),
    mod_loader: ModLoader {
      loader_type: mod_loader.loader_type,
      status: if matches!(
        mod_loader.loader_type,
        ModLoaderType::Unknown | ModLoaderType::Fabric
      ) {
        ModLoaderStatus::Installed
      } else {
        ModLoaderStatus::NotDownloaded
      },
      version: mod_loader.version.clone(),
      branch: mod_loader.branch.clone(),
    },
    optifine: optifine_info,
    description,
    icon_src,
    starred: false,
    play_time: 0,
    use_spec_game_config: false,
    spec_game_config: None,
    modpack_version,
    origin,
    isolation_locked: false,
  };

  apply_creation_isolation(&mut instance);

  // Download version info, retrying through both official and mirror routes.
  let official_version_url =
    Url::parse(&game.url).map_err(|_| InstanceError::ClientJsonParseError)?;
  let mut version_sources = Vec::new();
  for source in &priority_list {
    let candidate = match source {
      SourceType::Official => official_version_url.clone(),
      SourceType::BMCLAPIMirror => {
        get_download_api(SourceType::BMCLAPIMirror, ResourceType::Launcher)?
          .join(&format!("version/{}/json", game.id))?
      }
    };
    if !version_sources.contains(&candidate) {
      version_sources.push(candidate);
    }
  }
  if version_sources.is_empty() {
    version_sources.push(official_version_url);
  }
  let mut version_info: McClientInfo =
    fetch_json_with_fallbacks(client.inner(), &version_sources, 10).await?;

  version_info.id = name.clone();
  version_info.jar = Some(name.clone());

  // convert vanilla version info to vanilla patch
  let mut vanilla_patch = version_info.clone();
  vanilla_patch.id = "game".to_string();
  vanilla_patch.version = Some(game.id.clone());
  vanilla_patch.inherits_from = None;
  vanilla_patch.priority = Some(0);
  version_info.patches.push(vanilla_patch);

  let mut task_params = Vec::<PTaskParam>::new();

  // Download client (use task)
  let client_download_info = version_info
    .downloads
    .get("client")
    .ok_or(InstanceError::ClientJsonParseError)?;

  if !client_download_info.url.is_empty() {
    let original_client_url = Url::parse(&client_download_info.url.clone())
      .map_err(|_| InstanceError::ClientJsonParseError)?;
    let official_client_url = Url::parse(&format!(
      "https://piston-data.mojang.com/v1/objects/{}/client.jar",
      client_download_info.sha1
    ))
    .map_err(|_| InstanceError::ClientJsonParseError)?;
    let mut client_sources = Vec::new();
    for source in &priority_list {
      let candidate = match source {
        SourceType::Official => official_client_url.clone(),
        SourceType::BMCLAPIMirror => {
          get_download_api(SourceType::BMCLAPIMirror, ResourceType::Launcher)?
            .join(&format!("version/{}/client", game.id))?
        }
      };
      if !client_sources.contains(&candidate) {
        client_sources.push(candidate);
      }
    }
    if client_sources.is_empty() {
      client_sources.push(original_client_url.clone());
    }
    if !client_sources.contains(&original_client_url) {
      client_sources.push(original_client_url);
    }
    let client_src = client_sources.remove(0);

    task_params.push(PTaskParam::Download(DownloadParam {
      src: client_src,
      dest: instance.version_path.join(format!("{}.jar", name)),
      filename: None,
      sha1: (!client_download_info.sha1.is_empty()).then(|| client_download_info.sha1.clone()),
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(client_sources, 10),
    }));
  }
  let subdirs = get_instance_subdir_paths(
    &app,
    &instance,
    &[
      &InstanceSubdirType::Libraries,
      &InstanceSubdirType::Assets,
      &InstanceSubdirType::Mods,
    ],
  )
  .ok_or(InstanceError::InstanceNotFoundByID)?;
  let [libraries_dir, assets_dir, mods_dir] = subdirs.as_slice() else {
    return Err(InstanceError::InstanceNotFoundByID.into());
  };

  replace_native_libraries(&app, &mut version_info, &instance)
    .await
    .map_err(|_| InstanceError::ClientJsonParseError)?;

  // We only download libraries if they are invalid (not already downloaded)
  task_params
    .extend(get_invalid_library_files(&priority_list, libraries_dir, &version_info, false).await?);

  // We only download assets if they are invalid (not already downloaded)
  task_params
    .extend(get_invalid_assets(&app, &version_info, &priority_list, assets_dir, false).await?);

  // A local import installs its own loader and mods afterwards; Fabric API
  // would duplicate the one the pack names.
  if origin_input
    .as_ref()
    .is_some_and(|input| input.platform == OriginPlatform::Local)
  {
    is_install_fabric_api = Some(false);
  }

  // download loader (installer)
  if instance.mod_loader.loader_type != ModLoaderType::Unknown {
    install_mod_loader(
      app.clone(),
      &priority_list,
      &instance.version,
      &instance.mod_loader,
      libraries_dir.to_path_buf(),
      mods_dir.to_path_buf(),
      &mut version_info,
      &mut task_params,
      is_install_fabric_api,
    )
    .await?;
  }

  if let Some(info) = optifine.as_ref() {
    download_optifine_installer(
      &instance.version,
      info,
      libraries_dir.to_path_buf(),
      &mut task_params,
    )
    .await?;
  }

  // Optionally skip first-screen options by adding options.txt (available for zh-Hans only)
  let (language, skip_first_screen_options) = {
    let launcher_config = launcher_config_state.lock()?;
    (
      launcher_config.general.general.language.clone(),
      launcher_config
        .general
        .functionality
        .skip_first_screen_options,
    )
  };
  if language == "zh-Hans" && skip_first_screen_options {
    if let Some(lang_code) = get_zh_hans_lang_tag(&instance.version, &app).await {
      let options_path = get_instance_subdir_paths(&app, &instance, &[&InstanceSubdirType::Root])
        .ok_or(InstanceError::InstanceNotFoundByID)?[0]
        .join("options.txt");
      if !options_path.exists() {
        fs::write(options_path, format!("lang:{}\n", lang_code))
          .map_err(|_| InstanceError::FileCreationFailed)?;
      }
    }
  }

  // Save the edited client json
  save_json_async(&version_info, &version_path.join(format!("{}.json", name))).await?;
  // Save the USTBL instance config json
  instance
    .save_json_cfg()
    .await
    .map_err(|_| InstanceError::FileCreationFailed)?;
  // A local import is copied in before the install is scheduled, so a failure
  // here still rolls the whole instance back.
  let local_target = match origin_input.as_ref() {
    Some(input) if input.platform == OriginPlatform::Local => {
      let source = input
        .local_path
        .as_deref()
        .ok_or(InstanceError::ModpackManifestParseError)?;
      let label = input.label.clone().unwrap_or_default();
      Some(stage_local_archive(source, &version_path, label).await?)
    }
    _ => None,
  };
  if let Some(origin) = &instance.origin {
    let baseline = ModpackBaseline::build(
      origin,
      BaselineLoader {
        loader_type: instance.mod_loader.loader_type,
        version: instance.mod_loader.version.clone(),
      },
      Vec::new(),
    );
    baseline
      .save(&version_path)
      .await
      .map_err(|_| InstanceError::FileCreationFailed)?;
    crate::instance::modpack_update::release::write(
      &version_path,
      &crate::instance::modpack_update::release::Release::from_baseline(
        &baseline,
        origin.version_id.clone(),
      ),
    )
    .await
    .map_err(|_| InstanceError::FileCreationFailed)?;
    let files = baseline.files.clone();
    tokio::task::spawn_blocking({
      let version_path = version_path.clone();
      move || base_store::sync(&version_path, &files)
    })
    .await
    .map_err(|_| InstanceError::FileCreationFailed)?
    .map_err(|_| InstanceError::FileCreationFailed)?;
  }

  // A cached client can finish immediately. Publish all metadata first so its
  // completion refresh cannot read an unfinished instance (all import paths).
  let client_group = schedule_progressive_task_group(
    app.clone(),
    format!("game-client?{}", name),
    task_params,
    true,
  )
  .await?;
  crate::instance::modpack_update::commands::register_created_campus_instance(
    &instance,
    client_group.task_group,
    local_target.or_else(|| {
      crate::instance::modpack_update::commands::install_target_from_origin(origin_input.as_ref())
    }),
  )?;
  dir_guard.commit(); // disarm the cleanup guard on success
  Ok(instance.id)
}

#[tauri::command]
pub async fn finish_mod_loader_install(app: AppHandle, instance_id: String) -> USTBLResult<bool> {
  finish_mod_loader_install_inner(&app, &instance_id).await
}

#[tauri::command]
pub async fn prepare_mod_loader_install(
  app: AppHandle,
  instance_id: String,
  component: String,
) -> USTBLResult<bool> {
  LOADER_LIFECYCLE
    .run(async {
      let instance = {
        let state = app.state::<Mutex<HashMap<String, Instance>>>();
        let instance = state
          .lock()?
          .get(&instance_id)
          .cloned()
          .ok_or(InstanceError::InstanceNotFoundByID)?;
        instance
      };
      let optifine = component == "optifine";
      let status = if optifine {
        instance
          .optifine
          .as_ref()
          .ok_or(InstanceError::ClientJsonParseError)?
          .status
          .clone()
      } else {
        instance.mod_loader.status.clone()
      };
      if !preparation_allowed(&status) {
        return Ok(false);
      }
      let result = async {
        let priority = {
          let state = app.state::<Mutex<LauncherConfig>>();
          let config = state.lock()?;
          get_source_priority_list(&config)
        };
        let mut client = load_json_async::<McClientInfo>(
          &instance
            .version_path
            .join(format!("{}.json", instance.name)),
        )
        .await?;
        if optifine {
          download_optifine_libraries(&app, &priority, &instance, &mut client).await
        } else {
          match instance.mod_loader.loader_type {
            ModLoaderType::Forge | ModLoaderType::LegacyForge => {
              download_forge_libraries(&app, &priority, &instance, &mut client).await
            }
            ModLoaderType::NeoForge => {
              download_neoforge_libraries(&app, &priority, &instance, &mut client).await
            }
            _ => Ok(()),
          }
        }
      }
      .await;
      if let Err(error) = result {
        // Only the status changes; `instance` is a memory snapshot and must not
        // be written back as a whole.
        if let Err(save_error) = fail_download(&instance, optifine).await {
          log::warn!("Could not save the failed download state: {save_error:?}");
        }
        let state = app.state::<Mutex<HashMap<String, Instance>>>();
        if let Some(current) = state.lock()?.get_mut(&instance_id) {
          if optifine {
            if let Some(value) = &mut current.optifine {
              value.status = ModLoaderStatus::DownloadFailed;
            }
          } else {
            current.mod_loader.status = ModLoaderStatus::DownloadFailed;
          }
        }
        return Err(error);
      }
      Ok(true)
    })
    .await
}

async fn finish_mod_loader_install_inner(app: &AppHandle, instance_id: &str) -> USTBLResult<bool> {
  // Keep only the claim under the lifecycle lock. The processors may run for
  // minutes and must not block instance-list refreshes.
  let claimed = LOADER_LIFECYCLE
    .run(async {
      let snapshot = {
        let binding = app.state::<Mutex<HashMap<String, Instance>>>();
        let state = binding.lock()?;
        state
          .get(instance_id)
          .ok_or(InstanceError::InstanceNotFoundByID)?
          .clone()
      };
      let mut instance = snapshot.load_json_cfg().await?;
      {
        use crate::tasks::events::GEventStatus;
        use crate::tasks::monitor::TaskMonitor;
        use std::pin::Pin;
        let groups = app.state::<Pin<Box<TaskMonitor>>>().state_list();
        if groups.iter().any(|group| {
          let key = group
            .task_group
            .rsplit_once('@')
            .map_or(group.task_group.as_str(), |(key, _)| key);
          key.split_once('?').is_some_and(|(name, id)| {
            id == instance_id
              && matches!(
                name,
                "forge-libraries" | "neoforge-libraries" | "optifine-libraries"
              )
              && matches!(group.status, GEventStatus::Started | GEventStatus::Stopped)
          })
        }) {
          return Ok::<
            Option<(
              Instance,
              crate::instance::helpers::loader_lifecycle::InstallSteps,
            )>,
            crate::error::USTBLError,
          >(None);
        }
      }
      Ok(
        claim_install(&mut instance)
          .await?
          .map(|steps| (instance, steps)),
      )
    })
    .await?;
  let Some((instance, steps)) = claimed else {
    return Ok(false);
  };

  // The marker is already durable; run the expensive work without the global
  // lock. A concurrent refresh sees Installing and leaves it untouched.
  let process_result = async {
    let client_info_dir = instance
      .version_path
      .join(format!("{}.json", instance.name));
    let client_info = load_json_async::<McClientInfo>(&client_info_dir).await?;
    let install_profile_dir = instance.version_path.join("install_profile.json");
    if steps.loader {
      if !loader_patch_ready(&client_info, instance.mod_loader.loader_type) {
        return Err(InstanceError::ProcessorExecutionFailed.into());
      }
      if install_profile_dir.exists() {
        let install_profile = load_json_async::<InstallProfile>(&install_profile_dir).await?;
        execute_processors(app, &instance, &client_info, &install_profile).await?;
      }
    }
    if steps.optifine {
      if !optifine_patch_ready(&client_info) {
        return Err(InstanceError::ProcessorExecutionFailed.into());
      }
      finish_optifine_install(app, &instance, &client_info).await?;
    }
    Ok(())
  }
  .await;

  // Only the final status write and publication to the in-memory map are
  // serialized. This also publishes DownloadFailed when processors fail.
  LOADER_LIFECYCLE
    .run(async {
      // The claim snapshot only located the config and fed the processors; the
      // saved config (re-read inside the lock) is what gets published.
      let (saved, result) = complete_install(&instance, steps, process_result).await;
      if let Some(saved) = saved {
        let binding = app.state::<Mutex<HashMap<String, Instance>>>();
        let mut state = binding.lock()?;
        let current = state
          .get_mut(instance_id)
          .ok_or(InstanceError::InstanceNotFoundByID)?;
        current.mod_loader = saved.mod_loader;
        current.optifine = saved.optifine;
      }
      result
    })
    .await
    .map(|()| true)
}

#[tauri::command]
pub async fn check_change_mod_loader_availablity(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<bool> {
  let instance = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let launcher_config_state = binding.lock()?;
    launcher_config_state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone()
  };

  let json_path = instance
    .version_path
    .join(format!("{}.json", instance.name));
  if !json_path.exists() {
    return Err(InstanceError::NotSupportChangeModLoader.into());
  }

  let current_info: McClientInfo = load_json_async(&json_path)
    .await
    .map_err(|_| InstanceError::NotSupportChangeModLoader)?;

  if current_info.patches.is_empty() || current_info.component_profile.is_some() {
    return Err(InstanceError::NotSupportChangeModLoader.into());
  }

  Ok(true)
}

#[tauri::command]
pub async fn change_mod_loader(
  app: AppHandle,
  instance_id: String,
  new_mod_loader: ModLoaderResourceInfo,
  is_install_fabric_api: Option<bool>,
) -> USTBLResult<()> {
  switch_mod_loader(
    app,
    instance_id,
    new_mod_loader,
    is_install_fabric_api,
    false,
  )
  .await
}

/// keep_fabric_api leaves the Fabric API mods alone when leaving Fabric; a
/// modpack update manages those files itself.
pub(crate) async fn switch_mod_loader(
  app: AppHandle,
  instance_id: String,
  new_mod_loader: ModLoaderResourceInfo,
  is_install_fabric_api: Option<bool>,
  keep_fabric_api: bool,
) -> USTBLResult<()> {
  let mut instance = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone()
  };
  let version_isolation = get_instance_game_config(&app, &instance).version_isolation;
  // Get priority list
  let priority_list = {
    let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
    let launcher_config = launcher_config_state.lock()?;
    get_source_priority_list(&launcher_config)
  };

  // load current version info
  let json_path = instance
    .version_path
    .join(format!("{}.json", instance.name));
  let current_info: McClientInfo = load_json_async(&json_path).await?;
  if current_info.component_profile.is_some() {
    return Err(InstanceError::NotSupportChangeModLoader.into());
  }
  let vanilla_info = current_info
    .patches
    .first()
    .cloned()
    .ok_or(InstanceError::NotSupportChangeModLoader)?;

  let mod_loader = ModLoader {
    loader_type: new_mod_loader.loader_type,
    version: new_mod_loader.version.clone(),
    status: if matches!(
      new_mod_loader.loader_type,
      ModLoaderType::Unknown | ModLoaderType::Fabric
    ) {
      ModLoaderStatus::Installed
    } else {
      ModLoaderStatus::NotDownloaded
    },
    branch: new_mod_loader.branch.clone(),
  };
  let game_version = instance.version.clone();
  let subdirs = get_instance_subdir_paths(
    &app,
    &instance,
    &[&InstanceSubdirType::Libraries, &InstanceSubdirType::Mods],
  )
  .ok_or(InstanceError::InstanceNotFoundByID)?;
  let [libraries_dir, mods_dir] = subdirs.as_slice() else {
    return Err(InstanceError::InstanceNotFoundByID.into());
  };
  // Remove Fabric API mods if switching from Fabric modloader
  if instance.mod_loader.loader_type == ModLoaderType::Fabric
    && version_isolation
    && !keep_fabric_api
  {
    remove_fabric_api_mods(mods_dir).await?;
  }
  // construct new version info
  instance.mod_loader = mod_loader.clone();
  let mut version_info: McClientInfo = vanilla_info.clone();
  version_info.id = current_info.id.clone();
  version_info.jar = Some(instance.name.clone());
  version_info.java_version = current_info.java_version.clone();
  version_info.client_version = Some(instance.version.clone());
  version_info.patches = vec![vanilla_info];

  // install new mod loader
  let mut task_params: Vec<PTaskParam> = Vec::new();
  install_mod_loader(
    app.clone(),
    &priority_list,
    &game_version,
    &mod_loader,
    libraries_dir.to_path_buf(),
    mods_dir.to_path_buf(),
    &mut version_info,
    &mut task_params,
    is_install_fabric_api,
  )
  .await?;

  save_json_async(&version_info, &json_path).await?;
  instance
    .save_json_cfg()
    .await
    .map_err(|_| InstanceError::FileCreationFailed)?;

  schedule_progressive_task_group(
    app.clone(),
    format!(
      "change-mod-loader?{} {}",
      mod_loader.loader_type, mod_loader.version
    ),
    task_params,
    true,
  )
  .await?;

  Ok(())
}

#[tauri::command]
pub async fn retrieve_modpack_meta_info(
  app: AppHandle,
  path: String,
) -> USTBLResult<ModpackMetaInfo> {
  let path = PathBuf::from(path);
  let file = fs::File::open(&path).map_err(|_| InstanceError::FileNotFoundError)?;
  crate::instance::modpack_update::sources::check_local_archive(&path).await?;
  ModpackMetaInfo::from_archive(&app, &file).await
}

#[tauri::command]
pub fn add_custom_instance_icon(
  app: AppHandle,
  instance_id: String,
  source_src: String,
) -> USTBLResult<()> {
  let version_path = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    let instance = state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?;
    instance.version_path.clone()
  };

  let source_path = Path::new(&source_src);
  if !source_path.exists() || !source_path.is_file() {
    return Err(InstanceError::FileNotFoundError.into());
  }

  let dest_path = Path::new(&version_path).join("icon");
  fs::copy(source_path, &dest_path)?;

  Ok(())
}

#[tauri::command]
pub async fn list_modpack_files(
  app: AppHandle,
  instance_id: String,
) -> USTBLResult<ModpackFileList> {
  let instance = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone()
  };
  tokio::task::spawn_blocking(move || list_files(&instance)).await?
}

#[tauri::command]
pub async fn export_modpack(
  app: AppHandle,
  instance_id: String,
  save_path: String,
  options: ExportModpackOptions,
  files: Vec<String>,
) -> USTBLResult<()> {
  let instance = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone()
  };
  validate_export_options(&instance, &options)?;
  let mut selected_files = Vec::new();
  for relative in files {
    let full = instance.version_path.join(&relative);
    if tokio::fs::try_exists(&full).await.unwrap_or(false) {
      selected_files.push((relative, full));
    }
  }
  selected_files.retain(|(relative, _)| {
    !crate::instance::helpers::modpack::misc::is_launcher_metadata_path(relative, &instance.name)
  });
  if selected_files.is_empty() {
    return Err(InstanceError::ModpackManifestParseError.into());
  }
  let mut raw_files = Vec::new();
  let (prefix, overrides, extras) = match options.format {
    ExportFormat::Modrinth => {
      let mut manifest = generate_modrinth_manifest(&instance, &options)?;
      let (remote_files, local_files) = collect_modrinth_files(
        &app,
        &selected_files,
        options.no_create_remote_files.unwrap_or(false),
        options.skip_curseforge_remote_files.unwrap_or(false),
      )
      .await?;
      manifest.files = remote_files;
      let json = serde_json::to_string_pretty(&manifest)
        .map_err(|_| InstanceError::ModpackManifestParseError)?;
      (
        "overrides".to_string(),
        local_files,
        vec![("modrinth.index.json".to_string(), json)],
      )
    }
    ExportFormat::MultiMC if is_component_instance(&instance.version_path) => {
      // An imported component profile is written back from what it declared.
      let snapshot = ComponentSnapshot::load(&instance.version_path)
        .await
        .map_err(|_| InstanceError::ModpackExportProfileMissing)?
        .ok_or(InstanceError::ModpackExportProfileMissing)?;
      let libraries_dir =
        get_instance_subdir_paths(&app, &instance, &[&InstanceSubdirType::Libraries])
          .ok_or(InstanceError::InstanceNotFoundByID)?
          .remove(0);
      let export = build_component_export(&snapshot, &options, &libraries_dir)?;
      raw_files = export.libraries;
      (".minecraft".to_string(), selected_files, export.entries)
    }
    ExportFormat::MultiMC => {
      let manifest = generate_multimc_manifest(&instance, &options)?;
      let json = serde_json::to_string_pretty(&manifest)
        .map_err(|_| InstanceError::ModpackManifestParseError)?;
      (
        ".minecraft".to_string(),
        selected_files,
        vec![
          ("mmc-pack.json".to_string(), json),
          (
            "instance.cfg".to_string(),
            generate_multimc_instance_cfg(&instance, &options),
          ),
          (".packignore".to_string(), String::new()),
        ],
      )
    }
  };
  create_modpack_zip(&save_path, &prefix, overrides, extras, raw_files).await?;
  Ok(())
}

#[cfg(test)]
mod origin_tests {
  use super::*;

  fn input(platform: OriginPlatform, version_id: &str, sha1: &str) -> ModpackOriginInput {
    ModpackOriginInput {
      platform,
      project_id: "p".into(),
      version_id: version_id.into(),
      expected_sha1: sha1.into(),
      url: None,
      label: None,
      loader_type: ModLoaderType::Fabric,
      local_path: None,
    }
  }

  #[test]
  fn a_campus_install_starts_from_the_placeholder_version() {
    let origin = creation_origin(
      Some(input(OriginPlatform::Campus, "42", "")),
      "1.7.10",
      ModLoaderType::Forge,
    )
    .unwrap();
    assert_eq!(origin.version_id, "");
    assert_eq!(origin.loader_type, ModLoaderType::Forge);
    assert!(origin.follow);
    // A hash on a campus input is not a campus install.
    assert!(creation_origin(
      Some(input(OriginPlatform::Campus, "42", "abc")),
      "1.7.10",
      ModLoaderType::Forge
    )
    .is_none());
    assert!(creation_origin(None, "1.7.10", ModLoaderType::Forge).is_none());
  }

  #[test]
  fn a_platform_install_uses_the_input_loader_and_needs_a_version_and_hash() {
    let platform = input(OriginPlatform::Modrinth, "version", "abc");
    let origin = creation_origin(Some(platform.clone()), "1.21.1", ModLoaderType::Unknown).unwrap();
    assert_eq!(origin.version_id, "");
    assert_eq!(origin.loader_type, ModLoaderType::Fabric);
    assert!(origin.follow);
    for broken in [
      ModpackOriginInput {
        version_id: String::new(),
        ..platform.clone()
      },
      ModpackOriginInput {
        expected_sha1: String::new(),
        ..platform
      },
    ] {
      assert!(creation_origin(Some(broken), "1.21.1", ModLoaderType::Unknown).is_none());
    }
  }

  #[test]
  fn a_local_import_needs_its_file_and_follows_nothing() {
    let local = ModpackOriginInput {
      local_path: Some("pack.zip".into()),
      ..input(OriginPlatform::Local, "", "")
    };
    let origin = creation_origin(Some(local.clone()), "1.20.1", ModLoaderType::Unknown).unwrap();
    assert_eq!(
      (
        origin.platform,
        origin.version_id.as_str(),
        origin.loader_type
      ),
      (OriginPlatform::Local, "", ModLoaderType::Fabric)
    );
    assert!(!origin.follow);
    assert!(creation_origin(
      Some(ModpackOriginInput {
        local_path: None,
        ..local
      }),
      "1.20.1",
      ModLoaderType::Unknown
    )
    .is_none());
  }

  #[tokio::test]
  async fn the_staged_archive_is_kept_under_its_sha1_and_survives_the_original() {
    let dir = std::env::temp_dir().join(format!("ustbl-stage-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("pack.zip");
    fs::write(&source, b"abc").unwrap();
    let root = dir.join("versions").join("pack");
    let target = stage_local_archive(source.to_str().unwrap(), &root, "1.0".into())
      .await
      .unwrap();
    // SHA-1 of "abc"
    assert_eq!(target.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(target.version_id, target.sha1);
    assert_eq!((target.label.as_str(), target.url.as_str()), ("1.0", ""));
    fs::remove_file(&source).unwrap();
    let staged = crate::instance::modpack_update::sources::archive_path(&root, &target.sha1);
    assert_eq!(fs::read(&staged).unwrap(), b"abc");
    assert!(!staged.with_extension("zip.part").exists());
    let _ = fs::remove_dir_all(dir);
  }

  #[tokio::test]
  async fn a_missing_local_file_is_an_error_and_stages_nothing() {
    let dir = std::env::temp_dir().join(format!("ustbl-stage-{}", uuid::Uuid::new_v4()));
    let missing = dir.join("gone.zip");
    let root = dir.join("versions").join("pack");
    assert!(
      stage_local_archive(missing.to_str().unwrap(), &root, String::new())
        .await
        .is_err()
    );
    assert!(!root.exists());
  }
}
#[cfg(test)]
mod removal_tests {
  use super::*;

  #[test]
  fn forgetting_an_instance_removes_only_its_entry_and_picks_another() {
    let entry = |id: &str| {
      (
        id.to_string(),
        Instance {
          id: id.into(),
          ..Default::default()
        },
      )
    };
    let mut instances: HashMap<String, Instance> = [entry("a"), entry("b")].into();
    assert_eq!(forget_instance(&mut instances, "a"), "b");
    assert!(!instances.contains_key("a") && instances.contains_key("b"));
    assert_eq!(forget_instance(&mut instances, "b"), "");
    assert!(instances.is_empty());
    // An instance that was never in the table (no jar yet) is a no-op.
    assert_eq!(forget_instance(&mut instances, "pending"), "");
  }
}

#[cfg(test)]
mod isolation_tests {
  use super::*;

  #[test]
  fn create_lock_detects_origin_pack_loader_and_optifine() {
    let mut instance = Instance::default();
    apply_creation_isolation(&mut instance);
    assert!(!instance.isolation_locked);

    instance.origin = Some(InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "project".into(),
      version_id: "version".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Unknown,
      follow: true,
      pack_format: None,
    });
    apply_creation_isolation(&mut instance);
    assert!(instance.isolation_locked);

    for change in [
      |i: &mut Instance| i.modpack_version = Some("1.0.0".into()),
      |i: &mut Instance| i.mod_loader.loader_type = ModLoaderType::Fabric,
      |i: &mut Instance| i.optifine = Some(OptiFine::default()),
    ] {
      let mut instance = Instance::default();
      change(&mut instance);
      apply_creation_isolation(&mut instance);
      assert!(instance.isolation_locked);
    }
  }

  #[test]
  fn creation_isolation_does_not_enable_specific_game_settings() {
    let mut instance = Instance {
      modpack_version: Some("1.0.0".into()),
      ..Default::default()
    };
    apply_creation_isolation(&mut instance);
    assert!(instance.isolation_locked);
    assert!(!instance.use_spec_game_config);
    assert!(instance.spec_game_config.is_none());
  }
}
