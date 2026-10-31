use crate::error::USTBLResult;
use crate::instance::helpers::client_jar::load_game_version_from_jar;
use crate::instance::helpers::client_json::{libraries_to_info, patches_to_info, McClientInfo};
use crate::instance::helpers::loader::forge::download_forge_libraries;
use crate::instance::helpers::loader::neoforge::download_neoforge_libraries;
use crate::instance::helpers::loader::optifine::download_optifine_libraries;
use crate::instance::helpers::loader_lifecycle::{
  claim_download, recover_interrupted, LOADER_LIFECYCLE,
};
use crate::instance::models::misc::{
  Instance, InstanceError, InstanceSubdirType, ModLoader, ModLoaderStatus, ModLoaderType, OptiFine,
};
use crate::launcher_config::helpers::misc::get_global_game_config;
use crate::launcher_config::models::{GameConfig, GameDirectory, LauncherConfig};
use crate::resource::helpers::misc::get_source_priority_list;
use crate::storage::load_json_async;
use sanitize_filename;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use zip::ZipArchive;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IsolationSource {
  Locked,
  Instance,
  Global,
}

impl IsolationSource {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Locked => "locked",
      Self::Instance => "instance",
      Self::Global => "global",
    }
  }
}

/// Whether an instance has pack or loader files that only work in its own directory.
pub fn owns_files(instance: &Instance) -> bool {
  instance.origin.is_some()
    || instance.modpack_version.is_some()
    || instance.mod_loader.loader_type != ModLoaderType::Unknown
    || instance.optifine.is_some()
}

pub fn resolve_isolation(instance: &Instance, global: &GameConfig) -> (bool, IsolationSource) {
  if instance.isolation_locked {
    return (true, IsolationSource::Locked);
  }
  if instance.use_spec_game_config {
    if let Some(spec) = &instance.spec_game_config {
      return (spec.version_isolation, IsolationSource::Instance);
    }
  }
  (global.version_isolation, IsolationSource::Global)
}

pub fn should_lock_on_refresh(instance: &Instance, version_isolated: bool) -> bool {
  !instance.isolation_locked && owns_files(instance) && version_isolated
}

pub fn get_instance_game_config(app: &AppHandle, instance: &Instance) -> GameConfig {
  let global = get_global_game_config(app);
  let mut config = if instance.use_spec_game_config {
    instance
      .spec_game_config
      .clone()
      .unwrap_or_else(|| global.clone())
  } else {
    global.clone()
  };
  config.version_isolation = resolve_isolation(instance, &global).0;
  config
}

pub fn get_instance_subdir_paths(
  app: &AppHandle,
  instance: &Instance,
  directory_types: &[&InstanceSubdirType],
) -> Option<Vec<PathBuf>> {
  let version_path = &instance.version_path;
  let game_dir = version_path.parent()?.parent()?; // safe unwrap to `?`

  let version_isolation = get_instance_game_config(app, instance).version_isolation;
  let path = if version_isolation {
    version_path
  } else {
    game_dir
  };

  let paths = directory_types
    .iter()
    .map(|directory_type| {
      let path_buf = match directory_type {
        InstanceSubdirType::Assets => game_dir.join("assets"),
        InstanceSubdirType::Libraries => game_dir.join("libraries"),
        InstanceSubdirType::Mods => path.join("mods"),
        InstanceSubdirType::ResourcePacks => path.join("resourcepacks"),
        InstanceSubdirType::Root => path.to_path_buf(),
        InstanceSubdirType::Saves => path.join("saves"),
        InstanceSubdirType::Schematics => path.join("schematics"),
        InstanceSubdirType::Screenshots => path.join("screenshots"),
        InstanceSubdirType::ServerResourcePacks => path.join("server-resource-packs"),
        InstanceSubdirType::ShaderPacks => path.join("shaderpacks"),
        // native libraries extracted by USTBL
        InstanceSubdirType::NativeLibraries => version_path.join(format!(
          "natives-{}-{}",
          tauri_plugin_os::platform(),
          tauri_plugin_os::arch()
        )),
      };

      // Create directory if it doesn't exist
      if !path_buf.exists() {
        let _ = fs::create_dir_all(&path_buf);
      }

      path_buf
    })
    .collect();

  Some(paths) // if instance_id not exists, return None
}

pub fn get_instance_subdir_path_by_id(
  app: &AppHandle,
  instance_id: &String,
  directory_type: &InstanceSubdirType,
) -> Option<PathBuf> {
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let state = binding.lock().unwrap();
  let instance = state.get(instance_id)?;

  get_instance_subdir_paths(app, instance, &[directory_type]).and_then(|mut paths| paths.pop())
}

pub fn unify_instance_name(src_version_path: &PathBuf, tgt_name: &String) -> USTBLResult<PathBuf> {
  if !sanitize_filename::is_sanitized(tgt_name) {
    return Err(InstanceError::InvalidNameError.into());
  }
  let src_name = src_version_path
    .file_name()
    .ok_or(InstanceError::InvalidSourcePath)?
    .to_string_lossy()
    .to_string();
  let version_root = src_version_path.parent().unwrap();
  // rename version directory (if already exists, return conflict error)
  let dst_dir = version_root.join(tgt_name);
  if dst_dir.exists() {
    let mut entries = fs::read_dir(&dst_dir)?;
    if entries.next().is_some() {
      return Err(InstanceError::ConflictNameError.into());
    }
  }
  fs::rename(src_version_path, &dst_dir).map_err(|_| InstanceError::FileMoveFailed)?;

  // rename client jar
  let old_jar = dst_dir.join(format!("{}.jar", src_name));
  let new_jar = dst_dir.join(format!("{}.jar", tgt_name));
  fs::rename(old_jar, new_jar).map_err(|_| InstanceError::FileMoveFailed)?;

  // rewrite client json, update "id" field and filename
  let old_json = dst_dir.join(format!("{}.json", src_name));
  let new_json = dst_dir.join(format!("{}.json", tgt_name));
  let mut json_value: Value = serde_json::from_reader(fs::File::open(&old_json)?)?;
  if let Some(obj) = json_value.as_object_mut() {
    obj.insert("id".to_string(), Value::String(tgt_name.clone()));
  }
  fs::write(&new_json, json_value.to_string())?;
  fs::remove_file(old_json)?;

  Ok(dst_dir)
}

async fn refresh_instance(
  app: &AppHandle,
  mut version_path: PathBuf,
  name: String,
  jar_path: PathBuf,
  json_path: PathBuf,
  is_first_run: bool,
) -> USTBLResult<Option<Instance>> {
  let mut client_data = match load_json_async::<McClientInfo>(&json_path).await {
    Ok(v) => v,
    Err(e) => {
      log::warn!("Failed to load client info for {}: {}", name, e);
      return Ok(None);
    }
  };
  if client_data.id != name {
    if let Ok(dst_dir) = unify_instance_name(&version_path, &client_data.id) {
      version_path = dst_dir;
    } else {
      log::warn!("Failed to unify instance name for {}", name);
      return Ok(None);
    }
  }
  let name = client_data.id.clone();
  let mut cfg_read = Instance {
    version_path: version_path.clone(),
    ..Default::default()
  }
  .load_json_cfg()
  .await
  .unwrap_or_default();

  if cfg_read.mod_loader.status != ModLoaderStatus::Installed {
    let priority_list = {
      let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
      let launcher_config = launcher_config_state.lock()?;
      get_source_priority_list(&launcher_config)
    };
    if let Err(e) = async {
      match cfg_read.mod_loader.status {
        ModLoaderStatus::NotDownloaded | ModLoaderStatus::DownloadFailed => {
          match cfg_read.mod_loader.loader_type {
            ModLoaderType::Forge => {
              if claim_download(&mut cfg_read, false).await? {
                download_forge_libraries(app, &priority_list, &cfg_read, &mut client_data).await?;
              }
            }
            ModLoaderType::NeoForge => {
              if claim_download(&mut cfg_read, false).await? {
                download_neoforge_libraries(app, &priority_list, &cfg_read, &mut client_data)
                  .await?;
              }
            }
            _ => {}
          }
          Ok::<(), crate::error::USTBLError>(())
        }
        ModLoaderStatus::Downloading | ModLoaderStatus::Installing => {
          recover_interrupted(&mut cfg_read.mod_loader.status, is_first_run);
          Ok::<(), crate::error::USTBLError>(())
        }
        ModLoaderStatus::Installed => Ok(()),
      }
    }
    .await
    {
      log::warn!("Failed to install mod loader for {}: {:?}", name, e);
      cfg_read.mod_loader.status = ModLoaderStatus::DownloadFailed;
      cfg_read.save_json_cfg().await?;
    }
  }

  if cfg_read
    .optifine
    .as_ref()
    .is_some_and(|o| o.status != ModLoaderStatus::Installed)
  {
    let priority_list = {
      let launcher_config_state = app.state::<Mutex<LauncherConfig>>();
      let launcher_config = launcher_config_state.lock()?;
      get_source_priority_list(&launcher_config)
    };
    if let Err(e) = async {
      match cfg_read.optifine.as_ref().unwrap().status {
        ModLoaderStatus::Downloading | ModLoaderStatus::Installing => {
          recover_interrupted(
            &mut cfg_read.optifine.as_mut().unwrap().status,
            is_first_run,
          );
          Ok::<(), crate::error::USTBLError>(())
        }
        ModLoaderStatus::NotDownloaded | ModLoaderStatus::DownloadFailed => {
          if claim_download(&mut cfg_read, true).await? {
            download_optifine_libraries(app, &priority_list, &cfg_read, &mut client_data).await?;
          }
          Ok(())
        }
        ModLoaderStatus::Installed => Ok(()),
      }
    }
    .await
    {
      log::warn!("Failed to install OptiFine for {}: {:?}", name, e);
      if let Some(optifine_cfg) = &mut cfg_read.optifine {
        optifine_cfg.status = ModLoaderStatus::DownloadFailed;
      }
      cfg_read.save_json_cfg().await?;
    }
  }

  let (mut game_version, loader_version, loader_type, optifine_info) =
    if !client_data.patches.is_empty() {
      patches_to_info(&client_data.patches)
    } else {
      libraries_to_info(&client_data).await
    };
  // TODO: patches related logic
  if game_version.is_none() {
    let file = Cursor::new(tokio::fs::read(jar_path).await?);
    if let Ok(mut jar) = ZipArchive::new(file) {
      game_version = load_game_version_from_jar(&mut jar);
    }
  }

  if cfg_read.icon_src.is_empty() {
    cfg_read.icon_src = loader_type.to_icon_path().to_string();
  }

  let mod_loader_installed = cfg_read.mod_loader.status == ModLoaderStatus::Installed;
  let optifine_installed = cfg_read
    .optifine
    .as_ref()
    .is_some_and(|o| o.status == ModLoaderStatus::Installed);
  let optifine_filename = optifine_info.as_ref().map(|info| info.filename.clone());
  let optifine_version = optifine_info.map(|info| info.r#type.to_string());
  let mut instance = Instance {
    name,
    version: game_version.unwrap_or_default(),
    version_path,
    mod_loader: if !mod_loader_installed {
      // pass mod loader check if download is not ready
      cfg_read.mod_loader
    } else {
      ModLoader {
        loader_type,
        version: loader_version.unwrap_or_default(),
        status: ModLoaderStatus::Installed,
        branch: None,
      }
    },
    optifine: if !optifine_installed {
      cfg_read.optifine.clone()
    } else {
      Some(OptiFine {
        filename: optifine_filename.unwrap_or_default(),
        version: optifine_version.unwrap_or_default(),
        status: ModLoaderStatus::Installed,
      })
    },
    ..cfg_read
  };
  if should_lock_on_refresh(
    &instance,
    get_instance_game_config(app, &instance).version_isolation,
  ) {
    instance.isolation_locked = true;
  }
  // ignore error here, for now
  instance.save_json_cfg().await?;

  Ok(Some(instance))
}

pub async fn refresh_instances(
  app: &AppHandle,
  game_directory: &GameDirectory,
  is_first_run: bool,
) -> USTBLResult<Vec<Instance>> {
  LOADER_LIFECYCLE
    .run(refresh_instances_inner(app, game_directory, is_first_run))
    .await
}

async fn refresh_instances_inner(
  app: &AppHandle,
  game_directory: &GameDirectory,
  is_first_run: bool,
) -> USTBLResult<Vec<Instance>> {
  let mut instances = vec![];
  // traverse the "versions" directory
  let versions_dir = game_directory.dir.join("versions");
  for entry in fs::read_dir(versions_dir)? {
    let entry = match entry {
      Ok(v) => v,
      Err(_) => continue,
    };
    let version_path = entry.path();
    if !version_path.is_dir() {
      continue;
    }

    let name = match entry.file_name().into_string() {
      Ok(v) => v,
      Err(_) => {
        log::warn!("Skipped instance with non-UTF-8 name: {:?}", version_path);
        continue;
      }
    };
    // A modpack update that died half way is undone before the instance is read.
    if is_first_run && version_path.join(".ustbl-update/journal.json").is_file() {
      match crate::instance::modpack_update::apply::recover(&version_path) {
        Ok(outcome) => log::warn!("Recovered interrupted modpack update of {name}: {outcome:?}"),
        Err(error) => log::error!("Could not recover modpack update of {name}: {error:?}"),
      }
    }

    // if there exists name.jar and name.json, then it's a valid instance
    let jar_path = version_path.join(format!("{}.jar", name));
    let json_path = version_path.join(format!("{}.json", name));
    if !jar_path.exists() || !json_path.exists() {
      continue; // not a valid instance
    }

    match refresh_instance(
      app,
      version_path.clone(),
      name.clone(),
      jar_path,
      json_path,
      is_first_run,
    )
    .await
    {
      Ok(Some(instance)) => instances.push(instance),
      Ok(None) => {}
      Err(e) => {
        log::warn!(
          "Skipped invalid instance {} at {:?}: {:?}",
          name,
          version_path,
          e
        );
      }
    }
  }

  Ok(instances)
}

async fn refresh_all_instances(
  app: &AppHandle,
  game_directories: &[GameDirectory],
  is_first_run: bool,
) -> HashMap<String, Instance> {
  let mut instance_map = HashMap::new();

  for game_directory in game_directories {
    let dir_name = game_directory.name.clone();
    match refresh_instances_inner(app, game_directory, is_first_run).await {
      Ok(vs) => {
        for mut instance in vs {
          let composed_id = format!("{}:{}", dir_name, instance.name);
          instance.id = composed_id.clone();
          instance_map.insert(composed_id, instance);
        }
      }
      Err(_) => continue,
    }
  }

  instance_map
}

pub async fn refresh_and_update_instances(app: &AppHandle, is_first_run: bool) {
  LOADER_LIFECYCLE
    .run(refresh_and_update_instances_inner(app, is_first_run))
    .await;
}

async fn refresh_and_update_instances_inner(app: &AppHandle, is_first_run: bool) {
  // get launcher config -> local game directories
  let local_game_directories = {
    let binding = app.state::<Mutex<LauncherConfig>>();
    let state = binding.lock().unwrap();
    state.local_game_directories.clone()
  };
  let instances = refresh_all_instances(app, &local_game_directories, is_first_run).await;
  // update the instances in the app state
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let mut state = binding.lock().unwrap();
  *state = instances;
}

#[cfg(test)]
mod tests {
  use super::*;

  fn instance_with(
    locked: bool,
    use_spec: bool,
    spec_isolated: Option<bool>,
    owns_files: bool,
  ) -> Instance {
    let mut instance = Instance {
      isolation_locked: locked,
      use_spec_game_config: use_spec,
      ..Default::default()
    };
    if let Some(version_isolation) = spec_isolated {
      instance.spec_game_config = Some(GameConfig {
        version_isolation,
        ..Default::default()
      });
    }
    if owns_files {
      instance.modpack_version = Some("1.0.0".into());
    }
    instance
  }

  #[test]
  fn resolve_isolation_uses_locked_before_instance_or_global() {
    for spec_isolated in [false, true] {
      for global_isolated in [false, true] {
        let instance = instance_with(true, true, Some(spec_isolated), true);
        let global = GameConfig {
          version_isolation: global_isolated,
          ..Default::default()
        };
        assert_eq!(
          resolve_isolation(&instance, &global),
          (true, IsolationSource::Locked)
        );
      }
    }
  }

  #[test]
  fn resolve_isolation_uses_instance_setting_when_enabled() {
    for spec_isolated in [false, true] {
      for global_isolated in [false, true] {
        let instance = instance_with(false, true, Some(spec_isolated), true);
        let global = GameConfig {
          version_isolation: global_isolated,
          ..Default::default()
        };
        assert_eq!(
          resolve_isolation(&instance, &global),
          (spec_isolated, IsolationSource::Instance)
        );
      }
    }
  }

  #[test]
  fn resolve_isolation_uses_global_when_instance_setting_is_disabled() {
    for spec_isolated in [false, true] {
      for global_isolated in [false, true] {
        let instance = instance_with(false, false, Some(spec_isolated), true);
        let global = GameConfig {
          version_isolation: global_isolated,
          ..Default::default()
        };
        assert_eq!(
          resolve_isolation(&instance, &global),
          (global_isolated, IsolationSource::Global)
        );
      }
    }
  }

  #[test]
  fn refresh_lock_requires_owned_files_and_isolation() {
    let owned = instance_with(false, false, None, true);
    assert!(should_lock_on_refresh(&owned, true));
    assert!(!should_lock_on_refresh(&owned, false));
    assert!(!should_lock_on_refresh(
      &instance_with(false, false, None, false),
      true
    ));
    assert!(!should_lock_on_refresh(
      &instance_with(true, false, None, true),
      true
    ));
  }

  #[test]
  fn owns_files_detects_each_source() {
    assert!(!owns_files(&Instance::default()));

    let mut instance = Instance::default();
    instance.origin = Some(crate::instance::models::misc::InstanceOrigin {
      platform: crate::instance::models::misc::OriginPlatform::Campus,
      project_id: "project".into(),
      version_id: "version".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Unknown,
      follow: true,
    });
    assert!(owns_files(&instance));

    instance.origin = None;
    instance.modpack_version = Some("1.0.0".into());
    assert!(owns_files(&instance));

    instance.modpack_version = None;
    instance.mod_loader.loader_type = ModLoaderType::Fabric;
    assert!(owns_files(&instance));

    instance.mod_loader.loader_type = ModLoaderType::Unknown;
    instance.optifine = Some(OptiFine::default());
    assert!(owns_files(&instance));
  }

  #[test]
  fn isolation_source_serializes_to_lowercase() {
    assert_eq!(
      serde_json::to_string(&IsolationSource::Locked).unwrap(),
      "\"locked\""
    );
    assert_eq!(
      serde_json::to_string(&IsolationSource::Instance).unwrap(),
      "\"instance\""
    );
    assert_eq!(
      serde_json::to_string(&IsolationSource::Global).unwrap(),
      "\"global\""
    );
  }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn create_instance_shortcut_icon(
  app: &AppHandle,
  instance: &Instance,
  icon_src: &str,
) -> USTBLResult<PathBuf> {
  use crate::utils::fs::get_app_resource_filepath;

  let mut img = if icon_src == "custom" {
    image::ImageReader::open(instance.version_path.join("icon"))?
      .with_guessed_format()?
      .decode()?
  } else {
    let asset = app
      .asset_resolver()
      .get(icon_src.to_string())
      .ok_or_else(|| crate::error::USTBLError(format!("Icon asset not found: {}", icon_src)))?;
    image::load_from_memory(asset.bytes())?
  };

  // combine instance icon with square overlay of the launcher icon's variant. (see #448)
  img = img.resize_exact(128, 128, image::imageops::FilterType::Nearest);
  let overlay = get_app_resource_filepath(app, "assets/icons/variants/square.png")?;
  let square = image::open(overlay)?.resize_exact(42, 42, image::imageops::FilterType::Lanczos3);
  image::imageops::overlay(&mut img, &square, 80, 80);

  #[cfg(target_os = "windows")]
  let icon_name = "icon.ico";
  #[cfg(target_os = "linux")]
  let icon_name = "icon.png";

  let icon_path = instance.version_path.join(icon_name);
  img.save(&icon_path)?;
  Ok(icon_path)
}
