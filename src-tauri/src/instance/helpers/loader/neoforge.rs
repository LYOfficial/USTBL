use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use tauri::AppHandle;
use url::Url;
use zip::ZipArchive;

use crate::error::USTBLResult;
use crate::instance::helpers::client_json::{LaunchArgumentTemplate, McClientInfo};
use crate::instance::helpers::loader::common::add_library_entry;
use crate::instance::helpers::loader::forge::InstallProfile;
use crate::instance::helpers::misc::get_instance_subdir_paths;
use crate::instance::models::misc::{Instance, InstanceError, InstanceSubdirType, ModLoader};
use crate::launch::helpers::file_validator::convert_library_name_to_path;
use crate::resource::helpers::misc::{convert_url_to_target_source, get_download_api};
use crate::resource::models::{ResourceType, SourceType};
use crate::tasks::commands::schedule_progressive_task_group;
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
use crate::tasks::PTaskParam;

const NEOFORGE_DOWNLOAD_ATTEMPTS: usize = 10;

fn split_sources(mut sources: Vec<Url>) -> (Url, Vec<Url>) {
  let primary = sources.remove(0);
  (primary, sources)
}

fn ordered_library_sources(
  original: &Url,
  resource_types: &[ResourceType],
  priority: &[SourceType],
) -> USTBLResult<(Url, Vec<Url>)> {
  let mut sources = Vec::with_capacity(priority.len() + 1);
  for source in priority {
    let candidate = convert_url_to_target_source(original, resource_types, source)?;
    if !sources.contains(&candidate) {
      sources.push(candidate);
    }
  }
  if !sources.contains(original) {
    sources.push(original.clone());
  }
  Ok(split_sources(sources))
}

fn installer_sources(priority: &[SourceType], version: &str) -> USTBLResult<(Url, Vec<Url>)> {
  let artifact = if version.starts_with("1.20.1-") {
    format!("net/neoforged/forge/{version}/forge-{version}-installer.jar")
  } else {
    format!("net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar")
  };
  let official =
    get_download_api(SourceType::Official, ResourceType::NeoforgeInstall)?.join(&artifact)?;
  let mirror = if version.starts_with("1.20.1-") {
    get_download_api(SourceType::BMCLAPIMirror, ResourceType::NeoforgeMaven)?.join(&artifact)?
  } else {
    get_download_api(SourceType::BMCLAPIMirror, ResourceType::NeoforgeInstall)?
      .join(&format!("{version}/download/installer"))?
  };

  let mut sources = Vec::with_capacity(2);
  if version.starts_with("1.20.1-") {
    sources.extend([official.clone(), mirror.clone()]);
  } else {
    for source in priority {
      let candidate = match source {
        SourceType::Official => &official,
        SourceType::BMCLAPIMirror => &mirror,
      };
      if !sources.contains(candidate) {
        sources.push(candidate.clone());
      }
    }
    for candidate in [official, mirror] {
      if !sources.contains(&candidate) {
        sources.push(candidate);
      }
    }
  }

  Ok(split_sources(sources))
}

pub async fn install_neoforge_loader(
  priority: &[SourceType],
  loader: &ModLoader,
  lib_dir: PathBuf,
  task_params: &mut Vec<PTaskParam>,
) -> USTBLResult<()> {
  let loader_ver = &loader.version;

  let installer_coord = if loader_ver.starts_with("1.20.1-") {
    format!("net.neoforged:forge:{}-installer", loader.version)
  } else {
    format!("net.neoforged:neoforge:{}-installer", loader.version)
  };
  let (installer_url, fallback_sources) = installer_sources(priority, loader_ver)?;

  let installer_rel = convert_library_name_to_path(&installer_coord, None)?;
  let installer_path = lib_dir.join(&installer_rel);

  task_params.push(PTaskParam::Download(DownloadParam {
    src: installer_url,
    dest: installer_path.clone(),
    filename: None,
    sha1: None,
    custom_headers: None,
    transfer_options: DownloadTransferOptions::resumable(
      fallback_sources,
      NEOFORGE_DOWNLOAD_ATTEMPTS,
    ),
  }));

  Ok(())
}

pub async fn download_neoforge_libraries(
  app: &AppHandle,
  priority: &[SourceType],
  instance: &Instance,
  client_info: &mut McClientInfo,
) -> USTBLResult<()> {
  let subdirs = get_instance_subdir_paths(
    app,
    instance,
    &[&InstanceSubdirType::Root, &InstanceSubdirType::Libraries],
  )
  .ok_or(InstanceError::InvalidSourcePath)?;
  let [root_dir, lib_dir] = subdirs.as_slice() else {
    return Err(InstanceError::InvalidSourcePath.into());
  };
  let mut task_params = vec![];

  let name = if instance.mod_loader.version.starts_with("1.20.1-") {
    "forge"
  } else {
    "neoforge"
  };

  let installer_coord = format!(
    "net.neoforged:{name}:{}-installer",
    instance.mod_loader.version
  );
  let installer_rel = convert_library_name_to_path(&installer_coord, None)?;
  let installer_path = lib_dir.join(&installer_rel);
  let bin_patch = lib_dir.join(convert_library_name_to_path(
    &format!(
      "net.neoforged:{name}:{}:clientdata@lzma",
      instance.mod_loader.version
    ),
    None,
  )?);
  if !installer_path.exists() {
    return Err(InstanceError::LoaderInstallerNotFound.into());
  }
  let (content, version) = {
    let file = File::open(&installer_path)?;
    let mut archive = ZipArchive::new(file)?;

    // Extract maven folder contents to lib_dir
    for i in 0..archive.len() {
      let mut file = archive.by_index(i)?;
      let path = file.mangled_name();
      let outpath = if path.starts_with("maven/") {
        // Remove "maven/" prefix and join with lib_dir
        let relative_path = path.strip_prefix("maven/").unwrap();
        lib_dir.join(relative_path)
      } else if path == *"data/client.lzma" {
        bin_patch.clone()
      } else {
        continue;
      };

      if file.name().ends_with('/') {
        // Create directory
        fs::create_dir_all(&outpath)?;
      } else {
        // Create parent directories if they don't exist
        if let Some(p) = outpath.parent() {
          if !p.exists() {
            fs::create_dir_all(p)?;
          }
        }

        // Extract file
        let mut outfile = File::create(&outpath)?;
        std::io::copy(&mut file, &mut outfile)?;
      }
    }

    let mut s = String::new();
    {
      let mut install_profile = archive.by_name("install_profile.json")?;
      install_profile.read_to_string(&mut s)?;
    }

    let mut t = String::new();
    {
      let mut version_file = archive.by_name("version.json")?;
      version_file.read_to_string(&mut t)?;
    }

    (s, t)
  };

  let mut profile: InstallProfile = serde_json::from_str(&content)?;

  let mut args_map = HashMap::<String, String>::new();
  args_map.insert(
    "{MINECRAFT_JAR}".into(),
    instance
      .version_path
      .join(format!("{}.jar", instance.name))
      .to_string_lossy()
      .to_string(),
  );
  args_map.insert("{BINPATCH}".into(), bin_patch.to_string_lossy().to_string());
  args_map.insert(
    "{INSTALLER}".into(),
    installer_path.to_string_lossy().to_string(),
  );
  args_map.insert("{SIDE}".into(), "client".to_string());
  args_map.insert("{ROOT}".into(), root_dir.to_string_lossy().to_string());
  for (key, value) in profile.data.iter() {
    if args_map.contains_key(&format!("{{{key}}}")) {
      continue;
    }
    let mut value_client = value.client.clone();
    if value_client.starts_with('[') && value_client.ends_with(']') {
      value_client = value_client
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
      value_client = lib_dir
        .join(convert_library_name_to_path(&value_client, None)?)
        .to_string_lossy()
        .to_string();
    }
    args_map.insert(format!("{{{key}}}"), value_client);
  }

  for processor in profile.processors.iter_mut() {
    if processor.args.contains(&"DOWNLOAD_MOJMAPS".to_string()) {
      if let Some(mojmaps) = args_map.get("{MOJMAPS}") {
        if let Some(client_mappings) = client_info.downloads.get("client_mappings") {
          let original = Url::parse(&client_mappings.url)?;
          let (src, fallback_sources) =
            ordered_library_sources(&original, &[ResourceType::Libraries], priority)?;
          task_params.push(PTaskParam::Download(DownloadParam {
            src,
            dest: lib_dir.join(mojmaps),
            filename: None,
            sha1: Some(client_mappings.sha1.clone()),
            custom_headers: None,
            transfer_options: DownloadTransferOptions::resumable(
              fallback_sources,
              NEOFORGE_DOWNLOAD_ATTEMPTS,
            ),
          }));
        }
      }
      processor.args.clear();
      continue;
    }

    processor.jar = lib_dir
      .join(convert_library_name_to_path(&processor.jar, None)?)
      .to_string_lossy()
      .to_string();

    for class in processor.classpath.iter_mut() {
      *class = lib_dir
        .join(convert_library_name_to_path(class, None)?)
        .to_string_lossy()
        .to_string();
    }

    for arg in processor.args.iter_mut() {
      if arg.starts_with('[') && arg.ends_with(']') {
        *arg = arg
          .trim_start_matches('[')
          .trim_end_matches(']')
          .to_string();
        *arg = lib_dir
          .join(convert_library_name_to_path(arg, None)?)
          .to_string_lossy()
          .to_string();
      }
      for (key, value) in &args_map {
        *arg = arg.replace(key, value);
      }
    }
  }

  profile.processors.retain(|processor| {
    if let Some(sides) = &processor.sides {
      sides.contains(&"client".to_string())
    } else {
      !processor.args.is_empty()
    }
  });

  fs::write(
    instance.version_path.join("install_profile.json"),
    &serde_json::to_vec_pretty(&profile)?,
  )?;

  let neoforge_info: McClientInfo = serde_json::from_str(&version)?;
  client_info.main_class = neoforge_info.main_class.clone();

  for lib in neoforge_info.libraries.iter() {
    let name = &lib.name;
    add_library_entry(&mut client_info.libraries, name, Some(lib.clone()))?;

    let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) else {
      continue;
    };
    if artifact.url.is_empty() {
      continue;
    }

    let original = Url::parse(&artifact.url)?;
    let (src, fallback_sources) = ordered_library_sources(
      &original,
      &[ResourceType::NeoforgeMaven, ResourceType::Libraries],
      priority,
    )?;
    task_params.push(PTaskParam::Download(DownloadParam {
      src,
      dest: lib_dir.join(&convert_library_name_to_path(name, None)?),
      filename: None,
      sha1: (!artifact.sha1.is_empty()).then(|| artifact.sha1.clone()),
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(
        fallback_sources,
        NEOFORGE_DOWNLOAD_ATTEMPTS,
      ),
    }));
  }

  let nf_args = neoforge_info
    .arguments
    .ok_or(InstanceError::ModLoaderVersionParseError)?;
  let v_args = client_info
    .arguments
    .clone()
    .ok_or(InstanceError::ClientJsonParseError)?;
  let new_args = LaunchArgumentTemplate {
    game: [v_args.game, nf_args.game].concat(),
    jvm: [v_args.jvm, nf_args.jvm].concat(),
  };
  client_info.arguments = Some(new_args.clone());
  client_info.patches.push(McClientInfo {
    id: "neoforge".to_string(),
    version: Some(neoforge_info.id.clone()),
    priority: Some(30000),
    inherits_from: neoforge_info.inherits_from.clone(),
    main_class: neoforge_info.main_class.clone(),
    arguments: Some(new_args.clone()),
    ..Default::default()
  });

  for lib in profile.libraries.iter() {
    let name = &lib.name;
    let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) else {
      continue;
    };
    if artifact.url.is_empty() {
      continue;
    }

    let rel = convert_library_name_to_path(&name.to_string(), None)?;
    let original = Url::parse(&artifact.url)?;
    let (src, fallback_sources) = ordered_library_sources(
      &original,
      &[ResourceType::NeoforgeMaven, ResourceType::Libraries],
      priority,
    )?;
    task_params.push(PTaskParam::Download(DownloadParam {
      src,
      dest: lib_dir.join(&rel),
      filename: None,
      sha1: (!artifact.sha1.is_empty()).then(|| artifact.sha1.clone()),
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(
        fallback_sources,
        NEOFORGE_DOWNLOAD_ATTEMPTS,
      ),
    }));
  }

  let mut seen = std::collections::HashSet::new();
  task_params.retain(|param| match param {
    PTaskParam::Download(dp) => seen.insert(dp.dest.clone()),
  });

  schedule_progressive_task_group(
    app.clone(),
    format!("neoforge-libraries?{}", instance.id),
    task_params,
    true,
  )
  .await?;

  Ok(())
}
