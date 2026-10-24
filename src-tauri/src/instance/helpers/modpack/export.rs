use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::helpers::modpack::modrinth::{
  ModrinthFile, ModrinthFileEnv, ModrinthFileHashes, ModrinthManifest,
};
use crate::instance::helpers::modpack::multimc::snapshot::{
  is_component_instance, ComponentSnapshot,
};
use crate::instance::helpers::modpack::multimc::{MultiMcComponent, MultiMcManifest};
use crate::instance::models::misc::{
  Instance, InstanceError, ModLoaderType, ModpackFileList, SuggestedEntry,
};
use crate::resource::helpers::modrinth::fetch_remote_resource_by_local_modrinth;
use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use sha1::Digest;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use tauri::AppHandle;
use tokio::sync::Semaphore;
use walkdir::WalkDir;
use zip::write::{ExtendedFileOptions, FileOptions};
use zip::{CompressionMethod, ZipWriter};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum ExportFormat {
  Modrinth,
  MultiMC,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportModpackOptions {
  pub format: ExportFormat,
  pub name: String,
  pub version: String,
  pub author: Option<String>,
  pub description: Option<String>,
  pub pack_with_launcher: Option<bool>, // TODO: unused currently
  pub min_memory: Option<u32>,          // for MultiMC
  pub no_create_remote_files: Option<bool>, // for Modrinth
  pub skip_curseforge_remote_files: Option<bool>, // for Modrinth
}

/// Validate export options before proceeding
pub fn validate_export_options(
  instance: &Instance,
  options: &ExportModpackOptions,
) -> USTBLResult<()> {
  // Check required fields
  if options.name.trim().is_empty() {
    return Err(InstanceError::InvalidNameError.into());
  }

  if options.version.trim().is_empty() {
    return Err(InstanceError::ModpackManifestParseError.into());
  }

  if !instance.version_path.exists() {
    return Err(InstanceError::FileNotFoundError.into());
  }

  // A component profile is a chain of patches and local libraries, which a
  // Modrinth manifest cannot express.
  if matches!(options.format, ExportFormat::Modrinth)
    && is_component_instance(&instance.version_path)
  {
    return Err(InstanceError::ModpackExportComponentInstance.into());
  }

  Ok(())
}

// ============================================================================
// Candidate File Listing
// ============================================================================

static REGEX_BLACKLIST: LazyLock<RegexSet> = LazyLock::new(|| {
  RegexSet::new([
    r".*\.log$",
    r".*-natives$",
    r"natives-.*$",
    r"^\._.*",
    // Backup files
    r".*\.dat_old$",
    r".*\.old$",
    // BakaXL
    r".*\.BakaCoreInfo$",
  ])
  .expect("Invalid regex patterns")
});

static BLACKLIST: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
  HashSet::from([
    ".DS_Store",
    "desktop.ini",
    "Thumbs.db",
    // Minecraft
    "usernamecache.json",
    "usercache.json",
    "jars",
    "logs",
    "versions",
    "assets",
    "libraries",
    "crash-reports",
    // Staging, journal and backups of a modpack update
    ".ustbl-update",
    "NVIDIA",
    "AMD",
    "screenshots",
    "natives",
    "native",
    "$native",
    "$natives",
    "server-resource-packs",
    "command_history.txt",
    // Old Minecraft Launcher
    "launcher_profiles.json",
    "launcher.pack.lzma",
    // New Minecraft Launcher
    "launcher_accounts.json",
    "launcher_cef_log.txt",
    "launcher_log.txt",
    "launcher_msa_credentials.bin",
    "launcher_settings.json",
    "launcher_ui_state.json",
    "realms_persistence.json",
    "webcache2",
    "treatment_tags.json",
    // Plain Craft Launcher
    "clientId.txt",
    "PCL.ini",
    // HMCL
    "backup",
    "pack.json",
    "launcher.jar",
    "cache",
    "modpack.cfg",
    "log4j2.xml",
    "hmclversion.cfg",
    // SJMCL
    "install_profile.json",
    "sjmclcfg.json",
    // Curse
    "manifest.json",
    "minecraftinstance.json",
    ".curseclient",
    // Modrinth
    "modrinth.index.json",
    // 启动器自有元数据由导入流程重新生成。
    "ustblcfg.json",
    "ustbl-multimc-components.json",
    "ustbl-multimc-profile.json",
    "ustbl-modpack-baseline.json",
    // Fabric/OptiFine
    ".fabric",
    ".mixin.out",
    ".optifine",
    // Downloads and Essential
    "downloads",
    "essential",
    // Mods
    "asm",
    "backups",
    "TCNodeTracker",
    "CustomDISkins",
    "data",
    "CustomSkinLoader/caches",
    // Debug files
    "debug",
    // ReplayMod
    ".replay_cache",
    "replay_recordings",
    "replay_videos",
    // Iris
    "irisUpdateInfo.json",
    // ModernFix
    "modernfix",
    // Mod translations
    "modtranslations",
    // Schematics mod
    "schematics",
    // JourneyMap
    "journeymap/data",
    // Sinytra Connector
    "mods/.connector",
  ])
});

// ugly, consider GlobSet if this grows too large
static SUGGESTED_BLACKLIST: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
  HashSet::from([
    // BetterFonts
    "fonts",
    // Minecraft
    "saves",
    "servers.dat",
    "options.txt",
    // BuildCraft
    "blueprints",
    // OptiFine
    "optionsof.txt",
    // JourneyMap
    "journeymap",
    "optionsshaders.txt",
    // VoxelMods
    "mods/VoxelMods",
  ])
});

static SUGGESTED_REGEX_BLACKLIST: LazyLock<RegexSet> = LazyLock::new(|| {
  RegexSet::new([
    r"^fonts/.*$",
    r"^saves/.*$",
    r"^blueprints/.*$",
    r"^journeymap/.*$",
    r"^mods/VoxelMods/.*$",
  ])
  .expect("Invalid regex")
});

/// List all files that can be offered to the user in the modpack export tree UI.
pub fn list_files(instance: &Instance) -> USTBLResult<ModpackFileList> {
  let root = &instance.version_path;
  if !root.exists() {
    return Err(InstanceError::FileNotFoundError.into());
  }

  let name = &instance.name;
  let mut files = Vec::new();
  let mut unchecked_files = Vec::new();
  let mut suggested = BTreeSet::new();

  let walker = WalkDir::new(root).into_iter();
  let iter = walker.filter_entry(|entry| {
    let path = entry.path();

    let rel_path = match path.strip_prefix(root) {
      Ok(rel) => rel,
      Err(_) => return false,
    };
    let rel_str = rel_path.to_string_lossy().replace('\\', "/");

    if BLACKLIST.contains(rel_str.as_str()) || is_launcher_metadata_path(rel_str.as_str(), name) {
      return false;
    }
    if REGEX_BLACKLIST.is_match(rel_str.as_str()) {
      return false;
    }
    true
  });

  for entry in iter {
    let entry = match entry {
      Ok(e) => e,
      Err(_) => continue,
    };

    if entry.file_type().is_file() {
      let path = entry.path();
      // Ignore name.jar / name.json
      if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        if stem == name {
          continue;
        }
      }

      if let Ok(rel_path) = path.strip_prefix(root) {
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");

        if SUGGESTED_BLACKLIST.contains(rel_str.as_str())
          || SUGGESTED_REGEX_BLACKLIST.is_match(&rel_str)
        {
          unchecked_files.push(rel_str.clone());
          if let Some(entry) = suggested_entry(&rel_str) {
            suggested.insert(entry);
          }
        }

        files.push(rel_str);
      }
    }
  }

  files.sort_unstable();
  unchecked_files.sort_unstable();
  Ok(ModpackFileList {
    all: files,
    unchecked: unchecked_files,
    suggested: suggested
      .into_iter()
      .map(|(path, kind)| SuggestedEntry {
        path,
        kind: kind.into(),
      })
      .collect(),
  })
}

/// The suggested-blacklist entry that covers a listed file: the file itself,
/// or the folder the blacklist names above it.
fn suggested_entry(path: &str) -> Option<(String, &'static str)> {
  SUGGESTED_BLACKLIST.iter().find_map(|entry| {
    if path == *entry {
      Some((entry.to_string(), "file"))
    } else if path.starts_with(&format!("{entry}/")) {
      Some((entry.to_string(), "dir"))
    } else {
      None
    }
  })
}

static FORGE_VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
  // Forge: "1.16.5-forge-36.2.39" (Including NeoForge 1.20.1)
  Regex::new(r"([\d.]+)-forge-([\d.]+)").expect("Invalid regex")
});

static NEOFORGE_VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
  // NeoForge: "neoforge-21.4.121" (Not processed for "21.10.0-beta" or "0.25w14craftmine.3-beta")
  Regex::new(r"(neoforge-)?([a-zA-Z0-9.-]+)(-beta)?").expect("Invalid regex")
});

fn normalize_mod_loader_version(raw: &str) -> String {
  let trimmed = raw.trim();
  if trimmed.is_empty() {
    return raw.to_string();
  }

  if let Some(caps) = FORGE_VERSION_REGEX.captures(trimmed) {
    if let Some(matched) = caps.get(2) {
      return matched.as_str().to_string();
    }
  }

  if let Some(caps) = NEOFORGE_VERSION_REGEX.captures(trimmed) {
    if let Some(matched) = caps.get(2) {
      return matched.as_str().to_string();
    }
  }

  raw.to_string()
}

// ============================================================================
// Export Manifest Generators
// ============================================================================

/// Generate a Modrinth format manifest for the instance
pub fn generate_modrinth_manifest(
  instance: &Instance,
  options: &ExportModpackOptions,
) -> USTBLResult<ModrinthManifest> {
  let mut dependencies = HashMap::new();

  // Add Minecraft version
  dependencies.insert("minecraft".to_string(), instance.version.clone());

  // Add mod loader if present
  if instance.mod_loader.loader_type != ModLoaderType::Unknown {
    let version = normalize_mod_loader_version(&instance.mod_loader.version);
    let loader_key = match instance.mod_loader.loader_type {
      ModLoaderType::Forge | ModLoaderType::LegacyForge => "forge",
      ModLoaderType::Fabric => "fabric-loader",
      ModLoaderType::NeoForge => "neoforge",
      ModLoaderType::Quilt => "quilt-loader",
      _ => "",
    };

    if !loader_key.is_empty() {
      dependencies.insert(loader_key.to_string(), version);
    }
  }

  Ok(ModrinthManifest {
    format_version: 1,
    game: "minecraft".to_string(),
    version_id: options.version.clone(),
    name: options.name.clone(),
    summary: options.description.clone(),
    files: Vec::new(),
    dependencies,
    ..Default::default()
  })
}

/// Generate a MultiMC format manifest for the instance
pub fn generate_multimc_manifest(
  instance: &Instance,
  _options: &ExportModpackOptions,
) -> USTBLResult<MultiMcManifest> {
  let mut components = Vec::new();

  // Add Minecraft component (always important)
  components.push(MultiMcComponent {
    uid: "net.minecraft".to_string(),
    version: Some(instance.version.clone()),
    important: Some(true),
    dependency_only: Some(false),
    cached_name: None,
    cached_requires: None,
    cached_version: None,
    cached_volatile: None,
  });

  // Add mod loader component if present
  if instance.mod_loader.loader_type != ModLoaderType::Unknown {
    let version = normalize_mod_loader_version(&instance.mod_loader.version);
    let uid = match instance.mod_loader.loader_type {
      ModLoaderType::Forge | ModLoaderType::LegacyForge => "net.minecraftforge",
      ModLoaderType::NeoForge => "net.neoforged",
      ModLoaderType::Fabric => "net.fabricmc.fabric-loader",
      ModLoaderType::Quilt => "org.quiltmc.quilt-loader",
      _ => "",
    };

    if !uid.is_empty() {
      components.push(MultiMcComponent {
        uid: uid.to_string(),
        version: Some(version),
        important: Some(false),
        dependency_only: Some(false),
        cached_name: None,
        cached_requires: None,
        cached_version: None,
        cached_volatile: None,
      });
    }
  }

  Ok(MultiMcManifest {
    format_version: 1,
    components,
    cfg: HashMap::new(),
    base_path: String::new(),
    game_dir: String::new(),
  })
}

/// Generate instance.cfg file content for MultiMC
pub fn generate_multimc_instance_cfg(
  _instance: &Instance,
  options: &ExportModpackOptions,
) -> String {
  let mut content = format!(
    "# Auto generated by SJMC Launcher\nInstanceType=OneSix\nname={}\n",
    options.name
  );
  if let Some(min_memory) = options.min_memory {
    content.push_str("OverrideMemory=true\n");
    content.push_str(&format!("MinMemAlloc={}\n", min_memory));
  }
  content
}

// ============================================================================
// Component Profile Export
// ============================================================================

/// `instance.cfg` keys that describe the machine the pack was imported on.
const MACHINE_CFG_KEYS: [&str; 9] = [
  "JavaPath",
  "OverrideJavaLocation",
  "JavaSignature",
  "JavaArchitecture",
  "JavaRealArchitecture",
  "JavaVendor",
  "JavaVersion",
  "lastLaunchTime",
  "totalTimePlayed",
];

/// Everything a MultiMC export of a component profile needs besides the game
/// directory.
#[derive(Debug, Default)]
pub struct ComponentExport {
  /// Text entries at the archive root: `mmc-pack.json`, `instance.cfg`, patches.
  pub entries: Vec<(String, String)>,
  /// Library files to store as `libraries/<name>`, with their path on disk.
  pub libraries: Vec<(String, PathBuf)>,
}

fn is_plain_name(name: &str) -> bool {
  !name.is_empty()
    && name != "."
    && name != ".."
    && name
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

/// `instance.cfg` of the exported pack: what the pack declared, minus machine
/// specific state, with the export name and memory option applied.
fn component_instance_cfg(snapshot: &ComponentSnapshot, options: &ExportModpackOptions) -> String {
  let mut cfg = snapshot.instance_cfg.clone();
  for key in MACHINE_CFG_KEYS {
    cfg.remove(key);
  }
  cfg
    .entry("InstanceType".to_string())
    .or_insert_with(|| "OneSix".to_string());
  cfg.insert("name".to_string(), options.name.clone());
  if let Some(min_memory) = options.min_memory {
    cfg.insert("OverrideMemory".to_string(), "true".to_string());
    cfg.insert("MinMemAlloc".to_string(), min_memory.to_string());
  }

  let mut content = String::from("# Auto generated by SJMC Launcher\n");
  for (key, value) in cfg {
    // Values were read from a line based file; keep the output line based.
    let value = value.replace(['\r', '\n'], " ");
    content.push_str(&format!("{key}={value}\n"));
  }
  content
}

/// Rebuild the MultiMC pack of an imported component profile from its snapshot.
/// The component list and the shipped patches are written back as they were;
/// components that were resolved from public metadata stay plain references.
pub fn build_component_export(
  snapshot: &ComponentSnapshot,
  options: &ExportModpackOptions,
  libraries_dir: &Path,
) -> USTBLResult<ComponentExport> {
  let mut export = ComponentExport::default();
  export.entries.push((
    "mmc-pack.json".to_string(),
    serde_json::to_string_pretty(&snapshot.mmc_pack)
      .map_err(|_| InstanceError::ModpackManifestParseError)?,
  ));
  export.entries.push((
    "instance.cfg".to_string(),
    component_instance_cfg(snapshot, options),
  ));
  export
    .entries
    .push((".packignore".to_string(), String::new()));

  for (uid, patch) in &snapshot.local_patches {
    if !is_plain_name(uid) {
      return Err(InstanceError::ModpackExportProfileMissing.into());
    }
    export.entries.push((
      format!("patches/{uid}.json"),
      serde_json::to_string_pretty(patch).map_err(|_| InstanceError::ModpackManifestParseError)?,
    ));
  }

  for library in &snapshot.local_libraries {
    let relative = super::multimc::profile::safe_relative(&library.path)?;
    if !is_plain_name(&library.filename) {
      return Err(InstanceError::ModpackExportProfileMissing.into());
    }
    let full = libraries_dir.join(relative);
    if !full.is_file() {
      return Err(USTBLError(format!(
        "Local library {} is missing from the libraries directory",
        library.filename
      )));
    }
    export
      .libraries
      .push((format!("libraries/{}", library.filename), full));
  }
  Ok(export)
}
// ============================================================================
// Remote File Collectors
// ============================================================================

async fn build_modrinth_remote_file(
  app: &AppHandle,
  rel: &str,
  full: &Path,
  skip_curseforge: bool,
) -> USTBLResult<Option<ModrinthFile>> {
  // If disabled, mark it as optional in env
  let is_disabled = rel.ends_with(".disabled");
  let manifest_path = if is_disabled {
    rel.trim_end_matches(".disabled").to_string()
  } else {
    rel.to_string()
  };

  let mut downloads = Vec::new();

  if let Ok(remote) =
    fetch_remote_resource_by_local_modrinth(app, full.to_string_lossy().as_ref()).await
  {
    downloads.push(remote.download_url);
  }

  let _ = skip_curseforge;

  if downloads.is_empty() {
    return Ok(None);
  }

  let file_content = tokio::fs::read(full).await?;
  let mut sha1_hasher = sha1::Sha1::new();
  let mut sha512_hasher = sha2::Sha512::new();
  sha1_hasher.update(&file_content);
  sha512_hasher.update(&file_content);
  let sha1 = hex::encode(sha1_hasher.finalize());
  let sha512 = hex::encode(sha512_hasher.finalize());
  let file_size = file_content.len() as u64;
  let env = if is_disabled {
    Some(ModrinthFileEnv {
      client: "optional".to_string(),
      server: "unsupported".to_string(),
    })
  } else {
    None
  };

  Ok(Some(ModrinthFile {
    path: manifest_path,
    hashes: ModrinthFileHashes { sha1, sha512 },
    env,
    downloads,
    file_size,
  }))
}

pub async fn collect_modrinth_files(
  app: &AppHandle,
  selected_files: &[(String, PathBuf)],
  no_create_remote_files: bool,
  skip_curseforge: bool,
) -> USTBLResult<(Vec<ModrinthFile>, Vec<(String, PathBuf)>)> {
  let is_remote_candidate = |rel: &str| {
    rel.starts_with("mods/") || rel.starts_with("resourcepacks/") || rel.starts_with("shaderpacks/")
  };

  let mut tasks = Vec::new();
  let semaphore = Arc::new(Semaphore::new(
    std::thread::available_parallelism().unwrap().into(),
  ));

  for (rel, full) in selected_files {
    let rel = rel.clone();
    let full = full.clone();
    let app = app.clone();
    let permit = semaphore
      .clone()
      .acquire_owned()
      .await
      .map_err(|_| InstanceError::SemaphoreAcquireFailed)?;

    let task = tokio::spawn(async move {
      let result = if is_remote_candidate(&rel) && !no_create_remote_files {
        build_modrinth_remote_file(&app, &rel, &full, skip_curseforge)
          .await
          .ok()
          .flatten()
      } else {
        None
      };

      drop(permit);
      (rel, full, result)
    });

    tasks.push(task);
  }

  let mut modrinth_files = Vec::new();
  let mut override_files = Vec::new();

  for task in tasks {
    if let Ok(res) = task.await {
      match res {
        (_, _, Some(modrinth_file)) => {
          modrinth_files.push(modrinth_file);
        }
        (rel, full, None) => {
          override_files.push((rel.to_string(), full));
        }
      }
    }
  }

  Ok((modrinth_files, override_files))
}

pub async fn create_modpack_zip(
  save_path: &str,
  overrides_prefix: &str,
  overrides_files: Vec<(String, PathBuf)>,
  extra_files: Vec<(String, String)>,
  // Files stored under an exact entry name, without the overrides prefix.
  raw_files: Vec<(String, PathBuf)>,
) -> USTBLResult<()> {
  let save_path = save_path.to_string();
  let overrides_prefix = overrides_prefix.to_string();
  tokio::task::spawn_blocking(move || {
    let output = std::fs::File::create(&save_path)
      .map_err(|error| USTBLError(format!("Failed to create modpack archive: {error}")))?;
    let mut writer = ZipWriter::new(io::BufWriter::new(output));
    let options = FileOptions::<ExtendedFileOptions>::default()
      .compression_method(CompressionMethod::Deflated)
      .compression_level(Some(1));
    for (name, content) in extra_files {
      writer
        .start_file(name, options.clone())
        .map_err(|error| USTBLError(format!("Failed to create archive entry: {error}")))?;
      writer
        .write_all(content.as_bytes())
        .map_err(|error| USTBLError(format!("Failed to write archive entry: {error}")))?;
    }
    let entries = raw_files.into_iter().chain(
      overrides_files
        .into_iter()
        .map(|(relative, full)| (format!("{overrides_prefix}/{relative}"), full)),
    );
    for (entry_name, full) in entries {
      writer
        .start_file(entry_name, options.clone())
        .map_err(|error| USTBLError(format!("Failed to create archive entry: {error}")))?;
      let mut file = std::fs::File::open(&full)
        .map_err(|error| USTBLError(format!("Failed to open {}: {error}", full.display())))?;
      io::copy(&mut file, &mut writer)
        .map_err(|error| USTBLError(format!("Failed to write {}: {error}", full.display())))?;
    }
    writer
      .finish()
      .map_err(|error| USTBLError(format!("Failed to finalize modpack archive: {error}")))?;
    Ok(())
  })
  .await
  .map_err(|error| USTBLError(format!("Modpack archive task failed: {error}")))?
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::misc::extract_overrides;
  use crate::instance::helpers::modpack::multimc::profile::ResolvedMultiMc;
  use serde_json::{json, Value};
  use std::fs::{self, File};

  struct Temp(PathBuf);
  impl Temp {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-export-test-{}", uuid::Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }
  impl Drop for Temp {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn options(name: &str) -> ExportModpackOptions {
    ExportModpackOptions {
      format: ExportFormat::MultiMC,
      name: name.to_string(),
      version: "1.0.0".to_string(),
      author: None,
      description: None,
      pack_with_launcher: None,
      min_memory: None,
      no_create_remote_files: None,
      skip_curseforge_remote_files: None,
    }
  }

  /// A pack shaped like a heavyweight component pack: one wrapper directory,
  /// component flags, shipped patches, shipped libraries and a full
  /// `instance.cfg`. Synthetic content only.
  fn write_pack(path: &Path) {
    let wrapper = "GT New Horizons 2.9.0-beta-3/";
    let entries: Vec<(String, Vec<u8>)> = vec![
      (
        "mmc-pack.json".into(),
        json!({
          "formatVersion": 1,
          "components": [
            {"cachedName": "Minecraft", "cachedVersion": "1.7.10", "important": true,
             "uid": "net.minecraft", "version": "1.7.10"},
            {"cachedName": "Bootstrap", "cachedRequires": [{"equals": "1.7.10", "uid": "net.minecraft"}],
             "cachedVersion": "1", "uid": "example.bootstrap", "version": "1"},
            {"cachedName": "Graphics", "cachedVersion": "1", "dependencyOnly": true,
             "uid": "org.lwjgl3", "version": "1"}
          ]
        })
        .to_string()
        .into_bytes(),
      ),
      (
        "instance.cfg".into(),
        b"InstanceType=OneSix\nname=GT New Horizons\niconKey=gtnh\nOverrideMemory=true\nMaxMemAlloc=6144\nOverrideJavaArgs=false\nOverrideJavaLocation=true\nJavaPath=C:/Java/bin/javaw.exe\nlastLaunchTime=123\n".to_vec(),
      ),
      (
        "patches/net.minecraft.json".into(),
        json!({"uid": "net.minecraft", "version": "1.7.10", "formatVersion": 1, "order": -2,
          "name": "Minecraft", "compatibleJavaMajors": [17, 21], "compatibleJavaName": "java-runtime-test",
          "mainClass": "original.Main",
          "mainJar": {"name": "test:client:1", "MMC-hint": "local"},
          "assetIndex": {"id": "test", "url": "https://example.invalid/assets.json"},
          "minecraftArguments": "--username ${auth_player_name}",
          "libraries": [{"name": "test:dependency:1.0", "url": "https://example.invalid/maven/"}],
          "futureField": {"kept": true}})
        .to_string()
        .into_bytes(),
      ),
      (
        "patches/example.bootstrap.json".into(),
        json!({"uid": "example.bootstrap", "version": "1", "formatVersion": 1,
          "mainClass": "example.bootstrap.Main", "+jvmArgs": ["-Dexample=value"],
          "requires": [{"uid": "net.minecraft", "equals": "1.7.10"}, {"uid": "org.lwjgl3"}],
          "libraries": [
            {"name": "test:bootstrap:1", "MMC-hint": "local"},
            {"name": "test:dependency:2.0", "url": "https://example.invalid/maven/"}
          ]})
        .to_string()
        .into_bytes(),
      ),
      (
        "patches/org.lwjgl3.json".into(),
        json!({"uid": "org.lwjgl3", "version": "1", "formatVersion": 1}).to_string().into_bytes(),
      ),
      ("libraries/bootstrap-1.jar".into(), b"synthetic early classpath".to_vec()),
      ("libraries/client-1.jar".into(), b"synthetic client".to_vec()),
      (".minecraft/config/example.cfg".into(), b"example=true".to_vec()),
      (".minecraft/mods/example-mod.jar".into(), vec![0, 1, 2, 3, 255]),
      (".minecraft/options.txt".into(), b"lang:en_us".to_vec()),
    ];
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    for (name, bytes) in entries {
      zip
        .start_file(
          format!("{wrapper}{name}"),
          zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
      zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap();
  }

  struct Imported {
    instance: PathBuf,
    libraries: PathBuf,
    client: Value,
    files: Vec<String>,
  }

  async fn import(root: &Path, archive: &Path, label: &str) -> Imported {
    let file = File::open(archive).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut resolved = ResolvedMultiMc::from_archive(&client, &file)
      .await
      .unwrap()
      .expect("component pack");
    resolved.client.id = "imported".into();
    let libraries = root.join(label).join("libraries");
    let instance = root.join(label).join("instance");
    resolved.materialize(&file, &libraries, &instance).unwrap();
    let files = extract_overrides(&file, &instance).unwrap();
    Imported {
      instance,
      libraries,
      client: serde_json::to_value(&resolved.client).unwrap(),
      files,
    }
  }

  /// Import, export, import again, and require that nothing the pack declared
  /// changed on the way.
  async fn round_trip(first: &Path, temp: &Temp) -> usize {
    let a = import(&temp.0, first, "a").await;
    let missing: Vec<&String> = a
      .files
      .iter()
      .filter(|relative| !a.instance.join(relative).is_file())
      .collect();
    assert!(
      missing.is_empty(),
      "extracted files are missing: {missing:?}"
    );
    let snapshot_a = ComponentSnapshot::load(&a.instance).await.unwrap().unwrap();

    let export = build_component_export(&snapshot_a, &options("Round Trip"), &a.libraries).unwrap();
    let selected: Vec<(String, PathBuf)> = a
      .files
      .iter()
      .map(|relative| (relative.clone(), a.instance.join(relative)))
      .collect();
    let exported = temp.0.join("exported.zip");
    create_modpack_zip(
      exported.to_str().unwrap(),
      ".minecraft",
      selected,
      export.entries,
      export.libraries,
    )
    .await
    .unwrap();

    let b = import(&temp.0, &exported, "b").await;
    let snapshot_b = ComponentSnapshot::load(&b.instance).await.unwrap().unwrap();

    assert_eq!(snapshot_b.mmc_pack, snapshot_a.mmc_pack, "component list");
    assert_eq!(
      snapshot_b.local_patches, snapshot_a.local_patches,
      "patch files"
    );
    let mut cfg_a = snapshot_a.instance_cfg.clone();
    for key in MACHINE_CFG_KEYS {
      cfg_a.remove(key);
    }
    cfg_a.insert("name".into(), "Round Trip".into());
    assert_eq!(snapshot_b.instance_cfg, cfg_a, "instance.cfg");
    assert_eq!(b.client, a.client, "resolved launch profile");
    assert_eq!(b.files, a.files, "game files");
    for relative in &a.files {
      assert_eq!(
        fs::read(b.instance.join(relative)).unwrap(),
        fs::read(a.instance.join(relative)).unwrap(),
        "{relative}"
      );
    }
    let libraries = |snapshot: &ComponentSnapshot, dir: &Path| {
      let mut list: Vec<(String, Vec<u8>)> = snapshot
        .local_libraries
        .iter()
        .map(|l| (l.filename.clone(), fs::read(dir.join(&l.path)).unwrap()))
        .collect();
      list.sort();
      list
    };
    assert_eq!(
      libraries(&snapshot_b, &b.libraries),
      libraries(&snapshot_a, &a.libraries),
      "shipped libraries"
    );
    snapshot_a.local_libraries.len()
  }

  #[tokio::test]
  async fn component_pack_survives_export_and_reimport() {
    let temp = Temp::new();
    let pack = temp.0.join("pack.zip");
    write_pack(&pack);
    assert_eq!(round_trip(&pack, &temp).await, 2, "shipped libraries");
  }

  /// Acceptance run against a real pack, e.g. the GT New Horizons archive:
  /// `USTBL_ROUNDTRIP_PACK=<zip> cargo test --lib -- --ignored real_pack`.
  /// Needs network access when the pack refers to public component metadata.
  #[tokio::test]
  #[ignore]
  async fn real_pack_survives_export_and_reimport() {
    let pack = std::env::var("USTBL_ROUNDTRIP_PACK").expect("USTBL_ROUNDTRIP_PACK");
    let temp = Temp::new();
    let libraries = round_trip(Path::new(&pack), &temp).await;
    eprintln!("round trip ok, {libraries} shipped librar(ies)");
  }

  #[test]
  fn update_folder_is_never_offered_for_export() {
    let root = std::env::temp_dir().join(format!("ustbl-export-list-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join(".ustbl-update/backup/files/mods")).unwrap();
    fs::write(root.join(".ustbl-update/backup/files/mods/a.jar"), "old").unwrap();
    fs::create_dir_all(root.join("mods")).unwrap();
    fs::write(root.join("mods/b.jar"), "kept").unwrap();
    let instance = Instance {
      name: "pack".into(),
      version_path: root.clone(),
      ..Default::default()
    };

    let listed = list_files(&instance).unwrap();

    assert_eq!(listed.all, vec!["mods/b.jar".to_string()]);
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn suggested_entries_are_named_at_the_blacklists_own_level() {
    assert_eq!(
      suggested_entry("saves/world/level.dat"),
      Some(("saves".into(), "dir"))
    );
    assert_eq!(
      suggested_entry("options.txt"),
      Some(("options.txt".into(), "file"))
    );
    assert_eq!(
      suggested_entry("mods/VoxelMods/x.json"),
      Some(("mods/VoxelMods".into(), "dir"))
    );
    assert_eq!(suggested_entry("mods/a.jar"), None);
  }

  #[test]
  fn exported_cfg_drops_machine_state_and_applies_options() {
    let snapshot = ComponentSnapshot {
      schema: 2,
      mmc_pack: json!({}),
      instance_cfg: [
        ("JavaPath", "C:/Java/javaw.exe"),
        ("OverrideJavaLocation", "true"),
        ("iconKey", "gtnh"),
        ("name", "old"),
      ]
      .into_iter()
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .collect(),
      local_patches: Default::default(),
      local_libraries: Vec::new(),
    };
    let mut opts = options("New Name");
    opts.min_memory = Some(4096);
    let cfg = component_instance_cfg(&snapshot, &opts);
    assert!(!cfg.contains("JavaPath") && !cfg.contains("OverrideJavaLocation"));
    assert!(cfg.contains("iconKey=gtnh\n"));
    assert!(cfg.contains("name=New Name\n") && !cfg.contains("name=old"));
    assert!(cfg.contains("OverrideMemory=true\n") && cfg.contains("MinMemAlloc=4096\n"));
    assert!(cfg.contains("InstanceType=OneSix\n"));
  }

  #[test]
  fn export_refuses_unsafe_snapshot_entries() {
    let temp = Temp::new();
    let mut snapshot = ComponentSnapshot {
      schema: 2,
      mmc_pack: json!({}),
      instance_cfg: Default::default(),
      local_patches: [("../evil".to_string(), json!({}))].into_iter().collect(),
      local_libraries: Vec::new(),
    };
    assert!(build_component_export(&snapshot, &options("x"), &temp.0).is_err());
    snapshot.local_patches.clear();
    snapshot.local_libraries.push(
      crate::instance::helpers::modpack::multimc::snapshot::LocalLibrary {
        filename: "a.jar".into(),
        path: "../outside.jar".into(),
      },
    );
    assert!(build_component_export(&snapshot, &options("x"), &temp.0).is_err());
  }

  #[test]
  fn component_instances_cannot_export_modrinth() {
    let temp = Temp::new();
    let instance = Instance {
      name: "pack".into(),
      version_path: temp.0.clone(),
      ..Default::default()
    };
    let mut opts = options("pack");
    opts.format = ExportFormat::Modrinth;
    assert!(validate_export_options(&instance, &opts).is_ok());

    fs::write(temp.0.join("ustbl-multimc-components.json"), "[]").unwrap();
    assert!(validate_export_options(&instance, &opts).is_err());
    opts.format = ExportFormat::MultiMC;
    assert!(validate_export_options(&instance, &opts).is_ok());
  }
}
