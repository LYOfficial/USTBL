//! Where the next version of a followed modpack comes from.
//!
//! Modrinth and CurseForge publish an archive that is downloaded and read;
//! the campus platform publishes a manifest whose files are either remote or
//! stored in the platform's packs.

use super::diff::{FileKey, DISABLED_SUFFIX};
use super::optional::OptionalFile;
use super::packs::{AppTransport, PackSession};
use super::remote::{ComponentPack, FileSource, RemoteFile, RemoteModpack, UPDATE_DIR};
use crate::error::{USTBLError, USTBLResult};
use crate::file_error::AtPath;
use crate::instance::helpers::modpack::baseline::{
  components_of_mmc_pack, hash_file, normalize_relative_path, BaselineLoader,
};
use crate::instance::helpers::modpack::curseforge::CurseForgeManifest;
use crate::instance::helpers::modpack::misc::{
  list_override_entries, parse_platform_manifest, ManifestBox, ModpackDownload, ModpackManifest,
};
use crate::instance::helpers::modpack::multimc::profile::{safe_relative, PackFiles, ZipFiles};
use crate::instance::helpers::modpack::multimc::MultiMcManifest;
use crate::instance::models::misc::{
  InstanceError, InstanceOrigin, ModLoaderType, OriginPlatform, PackFormat,
};
use crate::resource::commands::fetch_resource_version_packs;
use crate::resource::models::{
  OtherResourceFileInfo, OtherResourceSource, OtherResourceVersionPackQuery,
};
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
use serde::Deserialize;
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use url::Url;

pub const CAMPUS_API: &str = "https://www.ustb.world/api/modpacks";
const ALL_FILTER: &str = "All";

#[derive(Debug, Clone)]
pub enum Payload {
  /// A Modrinth or CurseForge modpack archive.
  Archive { url: String, sha1: String },
  /// A campus version, manifest included.
  Campus(Box<CampusVersion>),
}

#[derive(Debug, Clone)]
pub struct LatestVersion {
  pub version_id: String,
  pub label: String,
  pub payload: Option<Payload>,
}

impl LatestVersion {
  /// The archive sha1 and download url of an Archive payload.
  pub fn archive_ref(&self) -> Option<(&str, &str)> {
    match &self.payload {
      Some(Payload::Archive { url, sha1 }) => Some((sha1, url)),
      _ => None,
    }
  }
}

pub fn loader_slug(loader: ModLoaderType) -> Option<&'static str> {
  match loader {
    ModLoaderType::Unknown => Some("vanilla"),
    ModLoaderType::Fabric => Some("fabric"),
    ModLoaderType::Forge => Some("forge"),
    ModLoaderType::LegacyForge => Some("legacyforge"),
    ModLoaderType::NeoForge => Some("neoforge"),
    ModLoaderType::LiteLoader | ModLoaderType::Quilt => None,
  }
}

fn loader_from_slug(slug: &str) -> Option<ModLoaderType> {
  match slug {
    "vanilla" => Some(ModLoaderType::Unknown),
    "fabric" => Some(ModLoaderType::Fabric),
    "forge" => Some(ModLoaderType::Forge),
    "legacyforge" => Some(ModLoaderType::LegacyForge),
    "neoforge" => Some(ModLoaderType::NeoForge),
    _ => None,
  }
}

/// Platform file listings name the loader loosely; a file without a loader
/// matches everything and is checked against its manifest after download.
fn loader_matches(listed: Option<&str>, wanted: ModLoaderType) -> bool {
  let Some(listed) = listed.map(str::trim).filter(|s| !s.is_empty()) else {
    return true;
  };
  let listed = listed.to_lowercase();
  match wanted {
    ModLoaderType::Forge | ModLoaderType::LegacyForge => listed == "forge",
    other => loader_slug(other) == Some(listed.as_str()),
  }
}

/// The file to update to, if any. The newest listed file wins regardless of its
/// release channel; nothing newer than the current file means nothing to do.
/// Files flagged `update_excluded` (Modrinth additional files, CurseForge server
/// packs) are not candidates.
pub fn select_update(
  items: Vec<OtherResourceFileInfo>,
  loader: ModLoaderType,
  current_id: &str,
) -> Option<OtherResourceFileInfo> {
  let candidates: Vec<OtherResourceFileInfo> = items
    .into_iter()
    .filter(|item| !item.update_excluded && loader_matches(item.loader.as_deref(), loader))
    .collect();
  let best = candidates
    .iter()
    .max_by(|a, b| a.file_date.cmp(&b.file_date))?;
  if best.file_id == current_id {
    return None;
  }
  if let Some(current) = candidates.iter().find(|item| item.file_id == current_id) {
    if current.file_date >= best.file_date {
      return None;
    }
  }
  Some(best.clone())
}

// ---- campus -----------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CampusVersion {
  pub id: i64,
  pub version_number: String,
  pub game_version: String,
  pub loader_type: String,
  pub loader_version: String,
  pub manifest: CampusManifest,
}

/// A line of the project listing: its newest version, without the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CampusLine {
  pub id: i64,
  pub version_number: String,
  pub game_version: String,
  pub loader_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusProject {
  #[serde(default)]
  pub id: i64,
  #[serde(default)]
  pub slug: String,
  pub lines: Vec<CampusLine>,
}

/// The only manifest format the client reads.
pub const MANIFEST_FORMAT: &str = "ustbl.modpack/3";

#[derive(Debug, Clone, Deserialize)]
pub struct CampusManifest {
  pub format: String,
  pub files: Vec<CampusFile>,
  /// Publish rules: the grey (ignore) and red (delete) entries of the tree.
  #[serde(default)]
  pub rules: Vec<CampusRule>,
  /// Everything a component-profile pack needs to be rebuilt.
  #[serde(default)]
  pub components: Option<CampusComponents>,
}

/// One publish rule. `kind` is `dir` or `file`, `state` `ignore` or `delete`.
#[derive(Debug, Clone, Deserialize)]
pub struct CampusRule {
  pub path: String,
  pub kind: String,
  pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusComponents {
  /// `mmc-pack.json` as published, component flags included.
  #[serde(rename = "mmcPack")]
  pub mmc_pack: serde_json::Value,
  #[serde(rename = "instanceCfg", default)]
  pub instance_cfg: BTreeMap<String, String>,
  #[serde(default)]
  pub patches: Vec<CampusComponentPatch>,
  #[serde(default)]
  pub libraries: Vec<CampusComponentLibrary>,
}

/// A patch file the pack ships itself, stored in the packs (`blob` is its sha1).
#[derive(Debug, Clone, Deserialize)]
pub struct CampusComponentPatch {
  pub uid: String,
  pub blob: String,
  pub size: u64,
}

/// A library the pack ships itself (`libraries/<name>`), stored in the packs.
#[derive(Debug, Clone, Deserialize)]
pub struct CampusComponentLibrary {
  pub path: String,
  pub blob: String,
  pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusFile {
  pub path: String,
  pub size: u64,
  pub sha1: String,
  #[serde(default)]
  pub env: CampusEnv,
  pub source: CampusSource,
  #[serde(default)]
  pub disabled: bool,
  #[serde(default)]
  pub once: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusEnv {
  #[serde(default = "required")]
  pub client: String,
}

impl Default for CampusEnv {
  fn default() -> Self {
    Self { client: required() }
  }
}

fn required() -> String {
  "required".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusSource {
  pub kind: String,
  pub url: Option<String>,
}

pub(super) fn plain_download(url: &str) -> USTBLResult<Box<DownloadParam>> {
  Ok(Box::new(DownloadParam {
    src: Url::parse(url).map_err(|_| InstanceError::ModpackUpdateRemoteInvalid)?,
    dest: PathBuf::new(),
    filename: None,
    sha1: None,
    custom_headers: None,
    transfer_options: DownloadTransferOptions::resumable(Vec::new(), 5),
  }))
}

fn is_sha1(value: &str) -> bool {
  value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A patch or library of a component profile. Like every other file it is an
/// object in the platform's packs.
fn pack_file(
  name: String,
  blob: &str,
  size: u64,
  packs: &Arc<PackSession>,
) -> USTBLResult<RemoteFile> {
  if !is_sha1(blob) {
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  Ok(RemoteFile {
    path: name,
    source: FileSource::Pack(packs.clone()),
    key: FileKey {
      sha1: blob.to_lowercase(),
      size,
    },
    disabled: false,
    once: false,
  })
}

fn component_pack(
  components: &CampusComponents,
  packs: &Arc<PackSession>,
) -> USTBLResult<ComponentPack> {
  let mut files = Vec::new();
  for patch in &components.patches {
    files.push(pack_file(
      format!("patches/{}.json", patch.uid),
      &patch.blob,
      patch.size,
      packs,
    )?);
  }
  for library in &components.libraries {
    files.push(pack_file(
      library.path.clone(),
      &library.blob,
      library.size,
      packs,
    )?);
  }
  Ok(ComponentPack {
    mmc_pack: components.mmc_pack.clone(),
    instance_cfg: components.instance_cfg.clone(),
    files,
  })
}

/// Turn a campus version into the platform independent description. Files the
/// client does not use are left out; files in packs are read through `packs`.
/// Only `ustbl.modpack/3` manifests are read at all: anything older has no
/// rules and no file attributes the client could trust.
pub fn campus_remote(
  version: &CampusVersion,
  packs: &Arc<PackSession>,
) -> USTBLResult<RemoteModpack> {
  if version.manifest.format != MANIFEST_FORMAT {
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  let loader_type =
    loader_from_slug(&version.loader_type).ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
  let mut files = Vec::new();
  for file in &version.manifest.files {
    if file.env.client == "unsupported" {
      continue;
    }
    let source = match (file.source.kind.as_str(), &file.source.url) {
      ("remote", Some(url)) => FileSource::Download(plain_download(url)?),
      ("pack", _) if is_sha1(&file.sha1) => FileSource::Pack(packs.clone()),
      _ => return Err(InstanceError::ModpackUpdateRemoteInvalid.into()),
    };
    files.push(RemoteFile {
      path: normalize_relative_path(&file.path),
      key: FileKey {
        sha1: file.sha1.to_lowercase(),
        size: file.size,
      },
      source,
      disabled: file.disabled,
      once: file.once,
    });
  }
  let mut rules = Vec::new();
  for rule in &version.manifest.rules {
    let kind = match rule.kind.as_str() {
      "dir" => super::remote::RuleKind::Dir,
      "file" => super::remote::RuleKind::File,
      _ => return Err(InstanceError::ModpackUpdateRemoteInvalid.into()),
    };
    let state = match rule.state.as_str() {
      "ignore" => super::remote::RuleState::Ignore,
      "delete" => super::remote::RuleState::Delete,
      _ => return Err(InstanceError::ModpackUpdateRemoteInvalid.into()),
    };
    rules.push(super::remote::RemoteRule {
      path: normalize_relative_path(&rule.path),
      kind,
      state,
    });
  }
  Ok(RemoteModpack {
    version_id: version.id.to_string(),
    label: version.version_number.clone(),
    game_version: version.game_version.clone(),
    loader: BaselineLoader {
      loader_type,
      version: version.loader_version.clone(),
    },
    components: version
      .manifest
      .components
      .as_ref()
      .map(|components| components_of_mmc_pack(&components.mmc_pack)),
    component_pack: version
      .manifest
      .components
      .as_ref()
      .map(|components| component_pack(components, packs))
      .transpose()?,
    recommended_memory_mb: None,
    files,
    rules,
    rules_manifest: true,
    disabled_authoritative: true,
    optional: Vec::new(),
    path_index: Arc::new(std::sync::OnceLock::new()),
  })
}

// ---- archives ---------------------------------------------------------------

/// A version's parsed remote, cached under the update folder. The archive of
/// an Archive version is also kept next to it; a hit skips re-downloading,
/// re-hashing and re-parsing it.
const REMOTE_CACHE_SCHEMA: u32 = 2;

fn remote_cache_path(root: &Path, version_id: &str) -> PathBuf {
  remote_dir(root).join(format!("{version_id}.remote.json"))
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCache {
  pub schema: u32,
  pub platform: OriginPlatform,
  pub version_id: String,
  /// Sha1 and size of the archive the remote was parsed from, Archive only.
  pub archive_sha1: Option<String>,
  pub archive_size: Option<u64>,
  /// The instance root the `DownloadParam.dest` paths were built against.
  pub root: PathBuf,
  pub remote: super::remote::StoredRemoteModpack,
}

impl RemoteCache {
  pub fn new(
    origin: &InstanceOrigin,
    root: &Path,
    remote: &RemoteModpack,
    archive: Option<(&str, u64)>,
  ) -> Self {
    Self {
      schema: REMOTE_CACHE_SCHEMA,
      platform: origin.platform,
      version_id: remote.version_id.clone(),
      archive_sha1: archive.map(|(sha1, _)| sha1.to_string()),
      archive_size: archive.map(|(_, size)| size),
      root: root.to_path_buf(),
      remote: super::remote::StoredRemoteModpack::from(remote),
    }
  }

  /// Whether this cache describes `version` fetched into `root` again. An
  /// Archive cache additionally needs its archive, still there with the same
  /// size (overrides are read from it).
  pub fn matches(&self, origin: &InstanceOrigin, root: &Path, version: &LatestVersion) -> bool {
    if self.schema != REMOTE_CACHE_SCHEMA
      || self.platform != origin.platform
      || self.version_id != version.version_id
      || self.root != root
    {
      return false;
    }
    match (
      version.archive_ref(),
      &self.archive_sha1,
      &self.archive_size,
    ) {
      (Some((sha1, _)), Some(cached_sha1), Some(cached_size)) => {
        if cached_sha1 != sha1 {
          return false;
        }
        let path = archive_path(root, sha1);
        fs::metadata(&path)
          .map(|meta| meta.len() as u64 == *cached_size)
          .unwrap_or(false)
      }
      (None, None, None) => true,
      _ => false,
    }
  }
}

/// The archive of an Archive payload: its sha1 and the path it is kept at.
pub(crate) fn archive_path(root: &Path, sha1: &str) -> PathBuf {
  remote_dir(root).join(format!("{sha1}.zip"))
}

/// The archive of an Archive payload, when it is on disk. The update has just
/// downloaded and verified it by the time it asks.
pub fn downloaded_archive(root: &Path, latest: &LatestVersion) -> Option<PathBuf> {
  let (sha1, _) = latest.archive_ref()?;
  let path = archive_path(root, sha1);
  path.is_file().then_some(path)
}

/// The cache of `version`, when one exists that describes it fetched into `root`.
pub fn matching_remote_cache(
  origin: &InstanceOrigin,
  root: &Path,
  version: &LatestVersion,
) -> Option<RemoteCache> {
  let bytes = fs::read(remote_cache_path(root, &version.version_id)).ok()?;
  let cache: RemoteCache = serde_json::from_slice(&bytes).ok()?;
  (cache.matches(origin, root, version)
    && !cache.remote.has_listed_files()
    && cache.remote.cached_files_present())
  .then_some(cache)
}

pub fn load_remote_cache(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  version: &LatestVersion,
) -> Option<RemoteModpack> {
  let cache = matching_remote_cache(origin, root, version)?;
  Some(RemoteModpack::from_stored(cache.remote, |version_id| {
    PackSession::new(AppTransport::new(app.clone()), version_id)
  }))
}

pub fn write_remote_cache(root: &Path, cache: &RemoteCache) -> std::io::Result<()> {
  let path = remote_cache_path(root, &cache.version_id);
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let temp = path.with_extension("json.tmp");
  fs::write(&temp, serde_json::to_vec_pretty(cache)?)?;
  fs::rename(temp, path)
}

/// SHA-1 and size of one entry of an open archive, streamed.
fn hash_zip_entry(
  zip: &mut zip::ZipArchive<File>,
  archive: &Path,
  entry: &str,
) -> USTBLResult<FileKey> {
  let mut source = zip.by_name(entry)?;
  let mut hasher = Sha1::new();
  let mut buffer = vec![0u8; 64 * 1024];
  let mut size = 0u64;
  loop {
    let read = source.read(&mut buffer).at(archive)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    size += read as u64;
  }
  Ok(FileKey {
    sha1: hex::encode(hasher.finalize()),
    size,
  })
}

/// Downloaded files first, then overrides: an override wins over a download of
/// the same path, matching what a fresh install does.
///
/// Paths that differ only in case land on the same file on Windows. When they
/// carry the same content (GTNH 2.8.4 ships four such pairs) one of them is
/// kept; when the content differs both stay and `validate` refuses the pack.
pub fn merge_files(downloads: Vec<RemoteFile>, overrides: Vec<RemoteFile>) -> Vec<RemoteFile> {
  let mut by_path = BTreeMap::new();
  for file in downloads.into_iter().chain(overrides) {
    by_path.insert(file.path.clone(), file);
  }
  let mut kept: BTreeMap<String, usize> = BTreeMap::new();
  let mut files: Vec<RemoteFile> = Vec::with_capacity(by_path.len());
  for file in by_path.into_values() {
    let lowered = file.path.to_lowercase();
    if let Some(&index) = kept.get(&lowered) {
      let first = &files[index];
      if first.key == file.key && first.disabled == file.disabled {
        log::warn!(
          "modpack ships {} and {} with the same content; keeping the first",
          first.path,
          file.path
        );
        continue;
      }
    } else {
      kept.insert(lowered, files.len());
    }
    files.push(file);
  }
  files
}

/// Read the `.disabled` files of a platform archive. A mod under `mods/` named
/// `x.jar.disabled` is the disabled mod `mods/x.jar`; where the pack carries
/// both spellings the enabled one stays. A `.disabled` file anywhere else is
/// dropped: the engine owns that suffix only for mods.
fn read_disabled_mods(
  downloads: Vec<RemoteFile>,
  overrides: Vec<RemoteFile>,
) -> (Vec<RemoteFile>, Vec<RemoteFile>) {
  fn fold(files: Vec<RemoteFile>) -> Vec<RemoteFile> {
    files
      .into_iter()
      .filter_map(|mut file| {
        let lowered = file.path.to_lowercase();
        if !lowered.ends_with(DISABLED_SUFFIX) {
          return Some(file);
        }
        if lowered.starts_with("mods/") && lowered.len() > "mods/".len() + DISABLED_SUFFIX.len() {
          file.path.truncate(file.path.len() - DISABLED_SUFFIX.len());
          file.disabled = true;
          Some(file)
        } else {
          log::warn!(
            "skipping modpack file outside mods/ with a .disabled suffix: {}",
            file.path
          );
          None
        }
      })
      .collect()
  }
  let (downloads, overrides) = (fold(downloads), fold(overrides));
  let enabled: std::collections::HashSet<String> = downloads
    .iter()
    .chain(&overrides)
    .filter(|file| !file.disabled)
    .map(|file| file.path.to_lowercase())
    .collect();
  let keep_enabled = |files: Vec<RemoteFile>| -> Vec<RemoteFile> {
    files
      .into_iter()
      .filter(|file| {
        let shadowed = file.disabled && enabled.contains(&file.path.to_lowercase());
        if shadowed {
          log::warn!(
            "modpack carries both {0} and {0}{DISABLED_SUFFIX}; keeping the enabled one",
            file.path
          );
        }
        !shadowed
      })
      .collect()
  };
  (keep_enabled(downloads), keep_enabled(overrides))
}

/// Read a downloaded Modrinth or CurseForge archive.
pub async fn remote_from_archive(
  app: &AppHandle,
  archive: &Path,
  root: &Path,
  instance_name: &str,
  origin: &InstanceOrigin,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack> {
  let file = File::open(archive)?;
  let manifest = parse_platform_manifest(&file)?;
  remote_from_manifest(app, &manifest, archive, root, instance_name, origin, latest).await
}

async fn remote_from_manifest(
  app: &AppHandle,
  manifest: &ManifestBox,
  archive: &Path,
  root: &Path,
  instance_name: &str,
  origin: &InstanceOrigin,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack> {
  let line = manifest_line(manifest, origin)?;
  let downloads = manifest.get_download_params(app, root).await?;
  remote_from_downloads(
    manifest,
    line,
    downloads,
    |param| download_param(app, param),
    archive,
    root,
    instance_name,
    latest,
  )
  .await
}

#[allow(clippy::too_many_arguments)]
async fn remote_from_downloads<F, Fut>(
  manifest: &ManifestBox,
  line: (String, BaselineLoader),
  downloads: Vec<ModpackDownload>,
  fetch: F,
  archive: &Path,
  root: &Path,
  instance_name: &str,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack>
where
  F: Fn(DownloadParam) -> Fut,
  Fut: std::future::Future<Output = USTBLResult<()>>,
{
  let facts = facts_of(manifest, line, downloads, fetch, root).await?;
  let overrides = hash_overrides(archive, &facts, instance_name).await?;
  Ok(assemble_remote(latest, facts, overrides))
}

/// The files the archive's override directories contribute, hashed in place.
async fn hash_overrides(
  archive: &Path,
  facts: &ArchiveFacts,
  instance_name: &str,
) -> USTBLResult<Vec<RemoteFile>> {
  let hashing_started = std::time::Instant::now();
  let overrides_paths = facts.overrides_paths.clone();
  let (archive_path, name) = (archive.to_path_buf(), instance_name.to_string());
  let overrides = tokio::task::spawn_blocking(move || -> USTBLResult<Vec<RemoteFile>> {
    let file = File::open(&archive_path)?;
    let entries = list_override_entries(&file, &overrides_paths, &name)?;
    // One open archive for every entry, not one per entry.
    let mut zip = zip::ZipArchive::new(file)?;
    entries
      .into_iter()
      .map(|(path, entry)| {
        Ok(RemoteFile {
          key: hash_zip_entry(&mut zip, &archive_path, &entry)?,
          path,
          source: FileSource::ArchiveEntry {
            archive: archive_path.clone(),
            entry,
          },
          disabled: false,
          once: false,
        })
      })
      .collect()
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))??;
  log::info!(
    "modpack archive overrides: {} files hashed elapsed_ms={}",
    overrides.len(),
    hashing_started.elapsed().as_millis()
  );
  Ok(overrides)
}

/// What a picked archive turned out to be.
enum LocalPack {
  /// Modrinth or CurseForge: the manifest lists downloads, the overrides ship the rest.
  Platform(ManifestBox),
  /// MultiMC: a component chain plus the files of its game folder.
  MultiMc(ArchiveFacts, ComponentPack),
}

impl LocalPack {
  fn open(archive: &Path) -> USTBLResult<Self> {
    let file = File::open(archive)?;
    if let Ok(manifest) = parse_platform_manifest(&file) {
      // The formats name these loaders, the engine installs neither.
      if matches!(
        declared_loader(manifest.as_ref())?.0,
        ModLoaderType::Quilt | ModLoaderType::LiteLoader
      ) {
        return Err(InstanceError::UnsupportedModLoader.into());
      }
      return Ok(Self::Platform(manifest));
    }
    drop(file);
    let (facts, pack) = multimc_pack(archive)?;
    Ok(Self::MultiMc(facts, pack))
  }
}

/// Whether the engine can install the archive, by the same reading the install
/// starts with. Lets the import dialog refuse a pack before an instance exists.
pub async fn check_local_archive(archive: &Path) -> USTBLResult<()> {
  let archive = archive.to_path_buf();
  tokio::task::spawn_blocking(move || LocalPack::open(&archive).map(|_| ()))
    .await
    .map_err(|e| USTBLError(e.to_string()))?
}

/// What updating a local import with a picked archive needs to know about it.
pub struct LocalPackFacts {
  pub format: PackFormat,
  pub game_version: String,
}

/// Read a picked archive the way the install does and say which format it is
/// and which Minecraft version it names.
pub async fn local_pack_facts(archive: &Path) -> USTBLResult<LocalPackFacts> {
  let archive = archive.to_path_buf();
  tokio::task::spawn_blocking(move || {
    Ok(match LocalPack::open(&archive)? {
      LocalPack::Platform(manifest) => LocalPackFacts {
        format: platform_format(&archive),
        game_version: manifest.get_client_version()?,
      },
      LocalPack::MultiMc(facts, _) => LocalPackFacts {
        format: PackFormat::MultiMc,
        game_version: facts.game_version,
      },
    })
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))?
}

/// The format of an archive the install can read, without reading its files.
/// `None` when it is none of the three.
pub fn detect_pack_format(archive: &Path) -> Option<PackFormat> {
  let file = File::open(archive).ok()?;
  if parse_platform_manifest(&file).is_ok() {
    return Some(platform_format(archive));
  }
  MultiMcManifest::from_archive(&file)
    .ok()
    .map(|_| PackFormat::MultiMc)
}

/// Which platform's manifest `parse_platform_manifest` reads from the archive;
/// it tries CurseForge first.
fn platform_format(archive: &Path) -> PackFormat {
  let curseforge =
    File::open(archive).is_ok_and(|file| CurseForgeManifest::from_archive(&file).is_ok());
  if curseforge {
    PackFormat::CurseForge
  } else {
    PackFormat::Modrinth
  }
}

/// Read a modpack archive the user picked from disk. A Modrinth or CurseForge
/// pack is described like a downloaded one; a MultiMC pack becomes a component
/// pack plus the files of its game folder.
pub async fn local_remote(
  app: &AppHandle,
  archive: &Path,
  root: &Path,
  instance_name: &str,
  origin: &InstanceOrigin,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack> {
  let pack = tokio::task::spawn_blocking({
    let archive = archive.to_path_buf();
    move || LocalPack::open(&archive)
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))??;
  match pack {
    LocalPack::Platform(manifest) => {
      remote_from_manifest(app, &manifest, archive, root, instance_name, origin, latest).await
    }
    LocalPack::MultiMc(facts, pack) => {
      multimc_remote(facts, pack, archive, instance_name, latest).await
    }
  }
}

async fn multimc_remote(
  facts: ArchiveFacts,
  pack: ComponentPack,
  archive: &Path,
  instance_name: &str,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack> {
  let overrides = hash_overrides(archive, &facts, instance_name).await?;
  let mut remote = assemble_remote(latest, facts, overrides);
  remote.components = Some(components_of_mmc_pack(&pack.mmc_pack));
  remote.component_pack = Some(pack);
  Ok(remote)
}
/// What a MultiMC archive says: the game and loader its component chain
/// names, and the chain itself with the patches and libraries it ships.
/// Nothing is resolved here; the chain is resolved when it is restored.
fn multimc_pack(archive: &Path) -> USTBLResult<(ArchiveFacts, ComponentPack)> {
  let file = File::open(archive)?;
  let manifest = MultiMcManifest::from_archive(&file)?;
  let game_version = manifest.get_client_version()?;
  let (loader_type, loader_version) = declared_loader(&manifest)?;
  let mmc_pack: Value = serde_json::from_slice(
    &ZipFiles::new(&file, &manifest.base_path)?.read("mmc-pack.json", 4 << 20)?,
  )?;

  let mut zip = zip::ZipArchive::new(&file)?;
  let mut pack_files = Vec::new();
  for index in 0..zip.len() {
    let (name, symlink) = {
      let entry = zip.by_index(index)?;
      if !entry.is_file() {
        continue;
      }
      (
        entry.name()?.into_owned(),
        entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000),
      )
    };
    let Some(relative) = name.strip_prefix(&manifest.base_path) else {
      continue;
    };
    if !relative.starts_with("patches/") && !relative.starts_with("libraries/") {
      continue;
    }
    if symlink || safe_relative(relative).is_err() {
      log::warn!("modpack archive: skipped unusable component file {name}");
      continue;
    }
    let mut source = zip.by_name(&name)?;
    let mut hasher = Sha1::new();
    let size = std::io::copy(&mut source, &mut hasher).at(archive)?;
    pack_files.push(RemoteFile {
      path: relative.to_string(),
      key: FileKey {
        sha1: hex::encode(hasher.finalize()),
        size,
      },
      source: FileSource::ArchiveEntry {
        archive: archive.to_path_buf(),
        entry: name,
      },
      disabled: false,
      once: false,
    });
  }

  Ok((
    ArchiveFacts {
      game_version,
      loader: BaselineLoader {
        loader_type,
        version: loader_version,
      },
      overrides_paths: manifest.get_overrides_paths(),
      downloads: Vec::new(),
      // A MultiMC pack's recommendation is in its `instance.cfg`.
      recommended_memory_mb: None,
      optional: Vec::new(),
    },
    ComponentPack {
      mmc_pack,
      instance_cfg: manifest.cfg.into_iter().collect(),
      files: pack_files,
    },
  ))
}

/// What a platform archive's manifest says, independent of how its overrides
/// are read: the same for a downloaded archive and for one read through byte
/// ranges.
pub struct ArchiveFacts {
  pub game_version: String,
  pub loader: BaselineLoader,
  /// Archive directories holding overrides, in layering order.
  pub overrides_paths: Vec<String>,
  /// Files the manifest asks to download, with the platform's hash and size.
  pub downloads: Vec<RemoteFile>,
  /// Memory in MiB the manifest recommends, if it says.
  pub recommended_memory_mb: Option<u32>,
  /// The downloads the pack author marked optional.
  pub optional: Vec<OptionalFile>,
}

/// How many hash-less files are fetched at once while the remote is built.
const HASHLESS_CONCURRENCY: usize = 8;

/// Where a file the manifest gave no hash or size for is kept once fetched.
fn hashless_cache_path(root: &Path, url: &str, relative: &str) -> PathBuf {
  let name = hex::encode(Sha1::digest(format!("{url}\n{relative}").as_bytes()));
  remote_dir(root).join("files").join(name)
}

/// Turn the downloads a manifest asks for into remote files. A download with a
/// hash and size is described as is. One without is fetched now, into the
/// remote cache, and hashed, so that it compares and installs like any other
/// file: its key is only known afterwards. A file already fetched by an
/// earlier call is reused, which keeps a preview and the apply on one key.
async fn download_files<F, Fut>(
  root: &Path,
  downloads: Vec<ModpackDownload>,
  fetch: F,
) -> USTBLResult<Vec<RemoteFile>>
where
  F: Fn(DownloadParam) -> Fut,
  Fut: std::future::Future<Output = USTBLResult<()>>,
{
  use futures::stream::{StreamExt, TryStreamExt};

  let mut files = Vec::new();
  let mut hashless = Vec::new();
  for download in downloads {
    let Ok(relative) = download.param.dest.strip_prefix(root) else {
      continue;
    };
    let path = normalize_relative_path(&relative.to_string_lossy());
    match (download.param.sha1.clone(), download.size) {
      (Some(sha1), Some(size)) => files.push(RemoteFile {
        path,
        key: FileKey {
          sha1: sha1.to_lowercase(),
          size,
        },
        source: FileSource::Download(Box::new(download.param)),
        disabled: false,
        once: false,
      }),
      _ => hashless.push((path, download.param)),
    }
  }
  if hashless.is_empty() {
    return Ok(files);
  }
  log::info!(
    "modpack archive: fetching {} files that carry no hash or size",
    hashless.len()
  );
  let fetch = &fetch;
  let fetched: Vec<RemoteFile> = futures::stream::iter(hashless)
    .map(|(path, param)| async move {
      let cached = hashless_cache_path(root, param.src.as_str(), &path);
      fetch_hashless(root, &cached, param, fetch)
        .await
        .map(|key| RemoteFile {
          path,
          key,
          source: FileSource::Cached { path: cached },
          disabled: false,
          once: false,
        })
    })
    .buffer_unordered(HASHLESS_CONCURRENCY)
    .try_collect()
    .await?;
  files.extend(fetched);
  Ok(files)
}

/// Make sure `cached` holds the file of `param` and return its key. The
/// download is written under another name and moved into place whole, like an
/// archive. The manifest's hash, when it has one, is still checked.
async fn fetch_hashless<F, Fut>(
  root: &Path,
  cached: &Path,
  mut param: DownloadParam,
  fetch: &F,
) -> USTBLResult<FileKey>
where
  F: Fn(DownloadParam) -> Fut,
  Fut: std::future::Future<Output = USTBLResult<()>>,
{
  let expected = param.sha1.as_ref().map(|sha1| sha1.to_lowercase());
  let hash = |path: PathBuf| async move {
    tokio::task::spawn_blocking(move || hash_file(&path))
      .await
      .map_err(|e| USTBLError(e.to_string()))
  };
  if let Ok(Ok((sha1, size))) = hash(cached.to_path_buf()).await {
    if expected.as_ref().is_none_or(|expected| *expected == sha1) {
      return Ok(FileKey { sha1, size });
    }
  }
  remove_parts_of(cached);
  let part = part_path(cached);
  param.dest = part.clone();
  if let Err(error) = fetch(param).await {
    let _ = fs::remove_file(&part);
    return Err(error);
  }
  let key = match hash(part.clone()).await? {
    Ok((sha1, size)) => FileKey { sha1, size },
    Err(error) => {
      let _ = fs::remove_file(&part);
      return Err(USTBLError(error.to_string()));
    }
  };
  fs::rename(&part, cached).at_in(&part, root)?;
  Ok(key)
}

/// The loader a manifest declares. A pack that declares none is vanilla; a
/// loader the format names but the launcher cannot install is an error.
fn declared_loader(manifest: &dyn ModpackManifest) -> USTBLResult<(ModLoaderType, String)> {
  manifest.get_mod_loader_type_version().or_else(|error| {
    if error.0 == InstanceError::ModpackManifestParseError.to_string() {
      Ok((ModLoaderType::Unknown, String::new()))
    } else {
      Err(error)
    }
  })
}

/// The game and loader a manifest declares, when they are the followed line's.
/// Checked before anything is looked up or fetched for the pack.
fn manifest_line(
  manifest: &ManifestBox,
  origin: &InstanceOrigin,
) -> USTBLResult<(String, BaselineLoader)> {
  let game_version = manifest.get_client_version()?;
  let (loader_type, version) = declared_loader(manifest.as_ref())?;
  if game_version != origin.game_version
    || (loader_type != origin.loader_type && !origin.version_id.is_empty())
  {
    return Err(InstanceError::ModpackUpdateLineMismatch.into());
  }
  Ok((
    game_version,
    BaselineLoader {
      loader_type,
      version,
    },
  ))
}

/// Check the manifest against the followed line and look up its downloads.
pub async fn archive_facts(
  app: &AppHandle,
  manifest: &ManifestBox,
  root: &Path,
  origin: &InstanceOrigin,
) -> USTBLResult<ArchiveFacts> {
  let line = manifest_line(manifest, origin)?;
  let params_started = std::time::Instant::now();
  let downloads = manifest.get_download_params(app, root).await?;
  let facts = facts_of(
    manifest,
    line,
    downloads,
    |param| download_param(app, param),
    root,
  )
  .await?;
  log::info!(
    "modpack archive files: {} downloads, platform lookup elapsed_ms={}",
    facts.downloads.len(),
    params_started.elapsed().as_millis()
  );
  Ok(facts)
}

async fn facts_of<F, Fut>(
  manifest: &ManifestBox,
  (game_version, loader): (String, BaselineLoader),
  downloads: Vec<ModpackDownload>,
  fetch: F,
  root: &Path,
) -> USTBLResult<ArchiveFacts>
where
  F: Fn(DownloadParam) -> Fut,
  Fut: std::future::Future<Output = USTBLResult<()>>,
{
  // The same list the install dialog offers, so the picks made there apply.
  let optional = super::optional_list::optional_of_downloads(&downloads, root);
  Ok(ArchiveFacts {
    game_version,
    loader,
    overrides_paths: manifest.get_overrides_paths(),
    downloads: download_files(root, downloads, fetch).await?,
    recommended_memory_mb: manifest.recommended_memory_mb(),
    optional,
  })
}
/// The remote of a platform archive from its manifest facts and the files its
/// overrides contribute.
pub fn assemble_remote(
  latest: &LatestVersion,
  facts: ArchiveFacts,
  overrides: Vec<RemoteFile>,
) -> RemoteModpack {
  let (downloads, overrides) = read_disabled_mods(facts.downloads, overrides);
  // A download the pack ships as `x.jar.disabled` is listed as `x.jar` now.
  let optional = facts
    .optional
    .into_iter()
    .map(|mut file| {
      file.path = super::optional::without_disabled_suffix(file.path);
      file
    })
    .filter(|file| {
      downloads
        .iter()
        .any(|download| download.path.eq_ignore_ascii_case(&file.path))
    })
    .collect();
  RemoteModpack {
    version_id: latest.version_id.clone(),
    label: latest.label.clone(),
    game_version: facts.game_version,
    loader: facts.loader,
    components: None,
    component_pack: None,
    recommended_memory_mb: facts.recommended_memory_mb,
    files: merge_files(downloads, overrides),
    rules: Vec::new(),
    rules_manifest: false,
    disabled_authoritative: true,
    optional,
    path_index: Arc::new(std::sync::OnceLock::new()),
  }
}

// ---- light check --------------------------------------------------------------

/// How the line a followed campus instance sits on compares with the newest
/// publication, as seen from the project listing (no manifest is fetched).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampusLineStatus {
  /// The line's newest version is the one the instance follows.
  Current,
  /// The line has a newer version: its id and label.
  Newer { id: i64, version_number: String },
  /// The project exists but has no line for this game version and loader.
  LineGone,
}

/// Match the followed line in a project listing, comparing the loader slug in
/// lower case like the platform does.
pub fn campus_line_status(lines: &[CampusLine], origin: &InstanceOrigin) -> CampusLineStatus {
  let Some(loader) = loader_slug(origin.loader_type) else {
    return CampusLineStatus::LineGone;
  };
  let Some(line) = lines.iter().find(|line| {
    line.game_version == origin.game_version && line.loader_type.to_lowercase() == loader
  }) else {
    return CampusLineStatus::LineGone;
  };
  if line.id.to_string() == origin.version_id {
    CampusLineStatus::Current
  } else {
    CampusLineStatus::Newer {
      id: line.id,
      version_number: line.version_number.clone(),
    }
  }
}

// ---- network ----------------------------------------------------------------

/// The client campus JSON endpoints are read through. Unlike the shared
/// client it accepts a gzipped response: the version detail is several
/// megabytes of JSON and the platform compresses it. Downloads and pack
/// Range reads keep using the shared client, which never asks for gzip.
pub fn campus_client(app: &AppHandle) -> reqwest::Client {
  app
    .try_state::<crate::utils::web::CampusClient>()
    .map(|client| client.0.clone())
    .unwrap_or_else(|| crate::utils::web::build_campus_client(app))
}

pub(super) fn remote_dir(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join("remote")
}

#[derive(Deserialize)]
struct CampusLocation {
  url: String,
  method: String,
  #[serde(default)]
  headers: HashMap<String, String>,
}

/// Blobs and bundles are not served by the platform itself. It answers with the
/// cloud address plus signed request headers that stop working after a few
/// minutes. Returns the endpoint that hands those out for such a URL.
fn campus_location_url(src: &str) -> Option<String> {
  let rest = src.strip_prefix(CAMPUS_API)?.strip_prefix('/')?;
  let is_object =
    rest.starts_with("blobs/") || (rest.starts_with("versions/") && rest.ends_with("/bundle"));
  is_object.then(|| format!("{src}/location"))
}

/// Swap a campus object URL for the cloud address just before downloading, so
/// the signature is fresh however long the queue ahead of it took.
async fn resolve_campus_location(app: &AppHandle, param: &mut DownloadParam) -> USTBLResult<()> {
  let Some(endpoint) = campus_location_url(param.src.as_str()) else {
    return Ok(());
  };
  let client = campus_client(app);
  let location: CampusLocation = client
    .get(endpoint)
    .send()
    .await?
    .error_for_status()?
    .json()
    .await?;
  if !location.method.eq_ignore_ascii_case("GET") || !location.url.starts_with("https://") {
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  param.src = Url::parse(&location.url).map_err(|_| InstanceError::ModpackUpdateRemoteInvalid)?;
  param.custom_headers = Some(location.headers);
  Ok(())
}

/// Run one download to completion. Used for archives and staged files, which
/// are needed right away rather than queued as a background group.
pub async fn download_param(app: &AppHandle, mut param: DownloadParam) -> USTBLResult<()> {
  use crate::tasks::{download::DownloadTask, monitor::TaskMonitor};
  use std::{pin::Pin, time::Duration};

  resolve_campus_location(app, &mut param).await?;
  if param.filename.is_none() {
    param.filename = param
      .dest
      .file_name()
      .map(|name| name.to_string_lossy().into_owned());
  }
  if let Some(parent) = param.dest.parent() {
    fs::create_dir_all(parent).at(parent)?;
  }
  let dest = param.dest.clone();
  let monitor = app.state::<Pin<Box<TaskMonitor>>>();
  let task = DownloadTask::new(
    app.clone(),
    monitor.get_new_id(),
    None,
    param,
    Duration::from_secs(1),
  );
  let (future, _handle) = task
    .future(app.clone(), monitor.download_rate_limiter.clone())
    .await?;
  // The task reports a failed write as text; name the file for the known
  // file problems (a scanner removing it, no space) instead of the bare code.
  future
    .await
    .map_err(|error| USTBLError(crate::file_error::from_os_text(&error.0, &dest, None)))
}

async fn download_url(
  app: &AppHandle,
  url: &str,
  dest: &Path,
  sha1: Option<&str>,
) -> USTBLResult<()> {
  let mut param = *plain_download(url)?;
  param.dest = dest.to_path_buf();
  param.sha1 = sha1.map(str::to_lowercase);
  download_param(app, param).await
}

/// Marks a download in progress next to the archive it will become.
const PART_SUFFIX: &str = ".part";

/// The name a download of the archive at `path` is written under: one of its
/// own, so a cancelled earlier download whose writer is still winding down
/// cannot write into it.
fn part_path(path: &Path) -> PathBuf {
  let mut name = path.as_os_str().to_os_string();
  name.push(format!(".{}{PART_SUFFIX}", uuid::Uuid::new_v4().simple()));
  PathBuf::from(name)
}

/// Remove what earlier downloads of the archive at `path` left behind.
fn remove_parts_of(path: &Path) {
  let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
    return;
  };
  let prefix = format!("{}.", name.to_string_lossy());
  let Ok(entries) = fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let entry_name = entry.file_name().to_string_lossy().into_owned();
    if entry_name.starts_with(&prefix) && entry_name.ends_with(PART_SUFFIX) {
      let _ = fs::remove_file(entry.path());
    }
  }
}

/// Download a modpack archive to `path`. It is written under another name and
/// moved into place once its SHA-1 has been verified, so `path` is either
/// missing or a whole archive: a cancelled download leaves nothing there for
/// the next preview to mistake for the archive.
pub(super) async fn download_archive(
  app: &AppHandle,
  url: &str,
  path: &Path,
  sha1: &str,
  root: &Path,
) -> USTBLResult<()> {
  remove_parts_of(path);
  let part = part_path(path);
  if let Err(error) = download_url(app, url, &part, Some(sha1)).await {
    let _ = fs::remove_file(&part);
    return Err(error);
  }
  fs::rename(&part, path).at_in(&part, root)?;
  Ok(())
}

async fn campus_get<T: serde::de::DeserializeOwned>(
  app: &AppHandle,
  path: &str,
) -> USTBLResult<Option<T>> {
  let client = campus_client(app);
  let response = client.get(format!("{CAMPUS_API}{path}")).send().await?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Ok(None);
  }
  Ok(Some(response.error_for_status()?.json().await?))
}

/// The `detail` of the platform's answer to a project that no longer exists.
/// Only a 404 with exactly this text counts as deleted: the same status from a
/// reverse proxy, or any other 404 the platform itself returns, must not.
pub const PROJECT_GONE_DETAIL: &str = "整合包不存在";

/// Whether the answer to `GET /projects/{ref}` says the project was deleted.
/// The body is only examined on a 404.
pub fn is_project_gone_detail(status: u16, detail: Option<&Value>) -> bool {
  status == 404 && detail == Some(&Value::String(PROJECT_GONE_DETAIL.into()))
}

/// The `detail` field of an error body, used to classify a 404.
pub fn error_detail(body: Option<&Value>) -> Option<&Value> {
  body.and_then(|body| body.get("detail"))
}

/// Whether a project reference is worth asking about. Both the install flow
/// and publish adoption store the slug; the platform's `get_project` accepts a
/// slug or a numeric id alike, so any non-empty ref may be queried. The
/// strictness of the deletion check comes from the exact 404 detail, not from
/// the ref's shape.
pub fn campus_project_queryable(project_ref: &str) -> bool {
  !project_ref.is_empty()
}

/// Ask the platform whether a campus project was deleted. `None` when the
/// answer is anything but a confirmed deletion, including every error.
pub async fn campus_project_gone(app: &AppHandle, project_ref: &str) -> Option<bool> {
  if !campus_project_queryable(project_ref) {
    return None;
  }
  let client = campus_client(app);
  let response = client
    .get(format!("{CAMPUS_API}/projects/{project_ref}"))
    .send()
    .await
    .ok()?;
  let status = response.status().as_u16();
  if status != 404 {
    return if response.status().is_success() {
      Some(false)
    } else {
      None
    };
  }
  let detail = response.json::<Value>().await.ok()?.get("detail").cloned();
  Some(is_project_gone_detail(status, detail.as_ref()))
}

/// Fetch the project listing for the light check, from the primed listing of
/// the startup check when it holds the project. A 404 whose detail is the
/// deletion proof returns `None`; any other 404 is the line being gone.
pub async fn campus_project_for_light_check(
  app: &AppHandle,
  project_ref: &str,
) -> USTBLResult<Option<CampusProject>> {
  resolve_light_check_project(campus_listing(), project_ref, || {
    campus_project_single(app, project_ref)
  })
  .await
}

async fn campus_project_single(
  app: &AppHandle,
  project_ref: &str,
) -> USTBLResult<Option<CampusProject>> {
  let client = campus_client(app);
  let response = client
    .get(format!("{CAMPUS_API}/projects/{project_ref}"))
    .send()
    .await?;
  let status = response.status().as_u16();
  if status == 404 {
    let body = response.json::<Value>().await.ok();
    if is_project_gone_detail(status, error_detail(body.as_ref())) {
      return Ok(None);
    }
    return Err(InstanceError::ModpackUpdateLineGone.into());
  }
  Ok(Some(response.error_for_status()?.json().await?))
}

// ---- batched light check -------------------------------------------------------

/// How long a primed listing may answer light checks. The startup check of
/// all followed instances finishes well within this; a stale listing never
/// outlives it even if nobody clears it.
const LISTING_TTL: Duration = Duration::from_secs(60);

/// The project listing (`GET /projects`) fetched once for a batch of light
/// checks, so each followed campus instance does not ask the platform again.
#[derive(Default)]
pub struct CampusListing {
  slot: Mutex<Option<(Instant, Arc<Vec<CampusProject>>)>>,
}

impl CampusListing {
  pub fn store(&self, projects: Vec<CampusProject>) {
    if let Ok(mut slot) = self.slot.lock() {
      *slot = Some((Instant::now(), Arc::new(projects)));
    }
  }

  pub fn clear(&self) {
    if let Ok(mut slot) = self.slot.lock() {
      *slot = None;
    }
  }

  fn current(&self) -> Option<Arc<Vec<CampusProject>>> {
    let slot = self.slot.lock().ok()?;
    let (stored, projects) = slot.as_ref()?;
    (stored.elapsed() < LISTING_TTL).then(|| projects.clone())
  }
}

/// The listing shared by the startup check.
pub fn campus_listing() -> &'static CampusListing {
  static LISTING: OnceLock<CampusListing> = OnceLock::new();
  LISTING.get_or_init(CampusListing::default)
}

/// Find a project by the ref an instance stores: the slug, or the numeric id
/// like the platform's own lookup.
pub fn find_listed_project<'a>(
  projects: &'a [CampusProject],
  project_ref: &str,
) -> Option<&'a CampusProject> {
  if project_ref.is_empty() {
    return None;
  }
  let by_id = project_ref
    .bytes()
    .all(|byte| byte.is_ascii_digit())
    .then(|| project_ref.parse::<i64>().ok())
    .flatten();
  projects.iter().find(|project| match by_id {
    Some(id) => project.id == id,
    None => project.slug == project_ref,
  })
}

/// Answer a light check from the primed listing when it holds the project.
/// The listing hides projects without a published line, so a project missing
/// from it is not known to be deleted: it goes to `single`, the per-project
/// request that carries the deletion proof.
pub async fn resolve_light_check_project<F, Fut>(
  listing: &CampusListing,
  project_ref: &str,
  single: F,
) -> USTBLResult<Option<CampusProject>>
where
  F: FnOnce() -> Fut,
  Fut: std::future::Future<Output = USTBLResult<Option<CampusProject>>>,
{
  if let Some(projects) = listing.current() {
    if let Some(project) = find_listed_project(&projects, project_ref) {
      return Ok(Some(project.clone()));
    }
  }
  single().await
}

/// Fetch the listing with `fetch` and keep it for the coming light checks.
/// A failure only costs the saving: the checks fall back to one request per
/// project. Returns whether a listing is now held.
pub async fn prime_campus_listing_with<F, Fut>(listing: &CampusListing, fetch: F) -> bool
where
  F: FnOnce() -> Fut,
  Fut: std::future::Future<Output = USTBLResult<Vec<CampusProject>>>,
{
  let started = Instant::now();
  match fetch().await {
    Ok(projects) => {
      log::info!(
        "campus project listing for the startup check: {} projects in {} ms",
        projects.len(),
        started.elapsed().as_millis()
      );
      listing.store(projects);
      true
    }
    Err(error) => {
      log::warn!("campus project listing failed, checking per project: {error:?}");
      listing.clear();
      false
    }
  }
}

async fn fetch_campus_listing(app: &AppHandle) -> USTBLResult<Vec<CampusProject>> {
  let response = campus_client(app)
    .get(format!("{CAMPUS_API}/projects"))
    .send()
    .await?;
  Ok(response.error_for_status()?.json().await?)
}

/// Fetch the listing for the startup check. Never fails.
pub async fn prime_campus_listing(app: &AppHandle) -> bool {
  prime_campus_listing_with(campus_listing(), || fetch_campus_listing(app)).await
}

#[cfg(test)]
mod listing_tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  fn project(id: i64, slug: &str, version: i64) -> CampusProject {
    CampusProject {
      id,
      slug: slug.into(),
      lines: vec![CampusLine {
        id: version,
        version_number: format!("v{version}"),
        game_version: "1.21.1".into(),
        loader_type: "fabric".into(),
      }],
    }
  }

  fn primed(projects: Vec<CampusProject>) -> CampusListing {
    let listing = CampusListing::default();
    listing.store(projects);
    listing
  }

  /// A `single` request that counts its calls and answers `answer`.
  fn counted(
    calls: &AtomicUsize,
    answer: Option<CampusProject>,
  ) -> impl FnOnce() -> std::future::Ready<USTBLResult<Option<CampusProject>>> + '_ {
    move || {
      calls.fetch_add(1, Ordering::SeqCst);
      std::future::ready(Ok(answer))
    }
  }

  #[test]
  fn listed_project_is_found_by_slug_or_numeric_id() {
    let projects = vec![project(3, "gtnh", 30), project(24, "pixel", 40)];
    assert_eq!(find_listed_project(&projects, "gtnh").unwrap().id, 3);
    assert_eq!(find_listed_project(&projects, "24").unwrap().slug, "pixel");
    assert!(find_listed_project(&projects, "missing").is_none());
    assert!(find_listed_project(&projects, "25").is_none());
    assert!(find_listed_project(&projects, "").is_none());
    // Like the platform's `isdigit` + `int(ref)`, leading zeros are the same id.
    assert_eq!(find_listed_project(&projects, "03").map(|p| p.id), Some(3));
  }

  #[tokio::test]
  async fn every_listed_project_is_answered_without_a_request() {
    let listing = primed(vec![project(3, "gtnh", 30), project(24, "pixel", 40)]);
    let calls = AtomicUsize::new(0);
    for project_ref in ["gtnh", "pixel", "3", "24"] {
      let found = resolve_light_check_project(&listing, project_ref, counted(&calls, None))
        .await
        .unwrap();
      assert!(found.is_some(), "{project_ref}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
  }

  #[tokio::test]
  async fn a_project_missing_from_the_listing_asks_on_its_own() {
    let listing = primed(vec![project(3, "gtnh", 30)]);
    let calls = AtomicUsize::new(0);
    // Hidden from the listing (no published line yet) but still there.
    let hidden = resolve_light_check_project(
      &listing,
      "draft",
      counted(&calls, Some(project(9, "draft", 90))),
    )
    .await
    .unwrap();
    assert_eq!(hidden.unwrap().id, 9);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The listed one still costs nothing.
    resolve_light_check_project(&listing, "gtnh", counted(&calls, None))
      .await
      .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn a_single_404_still_means_deleted() {
    let listing = primed(vec![project(3, "gtnh", 30)]);
    let calls = AtomicUsize::new(0);
    // `None` is what the per-project request returns for the deletion proof.
    let gone = resolve_light_check_project(&listing, "removed", counted(&calls, None))
      .await
      .unwrap();
    assert!(gone.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The single request's errors pass through unchanged.
    let failed = resolve_light_check_project(&listing, "removed", || {
      std::future::ready(Err(USTBLError("boom".into())))
    })
    .await;
    assert_eq!(failed.unwrap_err().0, "boom");
  }

  #[tokio::test]
  async fn a_failed_listing_request_falls_back_to_each_project() {
    let listing = CampusListing::default();
    let primed_ok = prime_campus_listing_with(&listing, || {
      std::future::ready(Err(USTBLError("network".into())))
    })
    .await;
    assert!(!primed_ok);
    let calls = AtomicUsize::new(0);
    for project_ref in ["gtnh", "pixel"] {
      resolve_light_check_project(
        &listing,
        project_ref,
        counted(&calls, Some(project(1, project_ref, 1))),
      )
      .await
      .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
  }

  #[tokio::test]
  async fn a_failed_refresh_drops_the_older_listing() {
    let listing = primed(vec![project(3, "gtnh", 30)]);
    let refreshed = prime_campus_listing_with(&listing, || {
      std::future::ready(Err(USTBLError("network".into())))
    })
    .await;
    assert!(!refreshed);
    let calls = AtomicUsize::new(0);
    resolve_light_check_project(
      &listing,
      "gtnh",
      counted(&calls, Some(project(3, "gtnh", 31))),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn priming_stores_the_listing_and_clearing_removes_it() {
    let listing = CampusListing::default();
    let ok = prime_campus_listing_with(&listing, || {
      std::future::ready(Ok(vec![project(3, "gtnh", 30)]))
    })
    .await;
    assert!(ok);
    let calls = AtomicUsize::new(0);
    resolve_light_check_project(&listing, "gtnh", counted(&calls, None))
      .await
      .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    listing.clear();
    resolve_light_check_project(&listing, "gtnh", counted(&calls, None))
      .await
      .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn an_expired_listing_is_not_used() {
    let listing = CampusListing::default();
    *listing.slot.lock().unwrap() = Some((
      Instant::now() - LISTING_TTL - Duration::from_secs(1),
      Arc::new(vec![project(3, "gtnh", 30)]),
    ));
    let calls = AtomicUsize::new(0);
    resolve_light_check_project(&listing, "gtnh", counted(&calls, None))
      .await
      .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn listing_json_parses_with_extra_fields() {
    let body = serde_json::json!([{
      "id": 3, "slug": "gtnh", "name": "GTNH", "summary": "s",
      "lines": [{
        "id": 30, "project_id": 3, "game_version": "1.7.10", "loader_type": "forge",
        "loader_version": "10.13", "version_number": "2.7.0", "published_at": null
      }]
    }]);
    let parsed: Vec<CampusProject> = serde_json::from_value(body).unwrap();
    assert_eq!(parsed[0].slug, "gtnh");
    assert_eq!(parsed[0].lines[0].id, 30);
  }
}

#[cfg(test)]
mod project_gone_tests {
  use super::*;
  use crate::instance::models::misc::{ModLoaderType, OriginPlatform};

  fn origin_at(version_id: &str) -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: version_id.into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
      pack_format: None,
    }
  }

  fn line(id: i64, version_number: &str, game_version: &str, loader_type: &str) -> CampusLine {
    CampusLine {
      id,
      version_number: version_number.into(),
      game_version: game_version.into(),
      loader_type: loader_type.into(),
    }
  }

  #[test]
  fn line_status_tells_current_newer_and_gone_apart() {
    let lines = vec![
      line(7, "1.2.0", "1.21.1", "fabric"),
      line(9, "3.0.0", "1.20.1", "forge"),
    ];
    // The followed version is the line's newest: up to date.
    assert_eq!(
      campus_line_status(&lines, &origin_at("7")),
      CampusLineStatus::Current
    );
    // Any other version of the line is an update.
    assert_eq!(
      campus_line_status(&lines, &origin_at("5")),
      CampusLineStatus::Newer {
        id: 7,
        version_number: "1.2.0".into(),
      }
    );
    // No line matches this game version / loader pair.
    let mut other = origin_at("7");
    other.game_version = "1.19.4".into();
    assert_eq!(
      campus_line_status(&lines, &other),
      CampusLineStatus::LineGone
    );
    // The loader slug matches case-insensitively (the platform stores lower).
    let lines = vec![line(7, "1.2.0", "1.21.1", "Fabric")];
    assert_eq!(
      campus_line_status(&lines, &origin_at("7")),
      CampusLineStatus::Current
    );
    // A loader without a slug (LiteLoader, Quilt) never has a line.
    let mut quilt = origin_at("7");
    quilt.loader_type = ModLoaderType::Quilt;
    assert_eq!(
      campus_line_status(&lines, &quilt),
      CampusLineStatus::LineGone
    );
  }

  #[test]
  fn only_the_exact_404_detail_confirms_deletion() {
    let gone = Value::String(PROJECT_GONE_DETAIL.into());
    let line_gone = Value::String("该版本线尚未发布".into());
    let not_found = Value::String("Not Found".into());
    assert!(is_project_gone_detail(404, Some(&gone)));
    // Any other 404 (reverse proxy, "该版本线尚未发布") is not a deletion.
    assert!(!is_project_gone_detail(404, Some(&line_gone)));
    assert!(!is_project_gone_detail(404, Some(&not_found)));
    assert!(!is_project_gone_detail(404, None));
    assert!(!is_project_gone_detail(404, Some(&Value::Null)));
    // No other status counts, not even with the exact detail.
    assert!(!is_project_gone_detail(500, Some(&gone)));
    assert!(!is_project_gone_detail(200, Some(&gone)));
  }

  #[test]
  fn any_non_empty_project_ref_is_queried() {
    // Install and publish both store the slug; the platform also accepts ids.
    assert!(campus_project_queryable("gtnh"));
    assert!(campus_project_queryable("24"));
    assert!(!campus_project_queryable(""));
  }

  #[test]
  fn a_404_with_another_detail_is_not_a_deletion() {
    // The exact detail is the deletion proof; anything else keeps the line.
    let body = serde_json::json!({"detail": "Not Found"});
    assert!(!is_project_gone_detail(404, error_detail(Some(&body))));
    let gone = serde_json::json!({"detail": PROJECT_GONE_DETAIL});
    assert!(is_project_gone_detail(404, error_detail(Some(&gone))));
  }
}

pub async fn campus_base_remote(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  name: &str,
) -> USTBLResult<RemoteModpack> {
  let id: i64 = origin
    .version_id
    .parse()
    .map_err(|_| InstanceError::ModpackUpdateRemoteInvalid)?;
  let version = campus_get::<CampusVersion>(app, &format!("/versions/{id}"))
    .await?
    .ok_or(InstanceError::ModpackUpdatePackUnavailable)?;
  let latest = LatestVersion {
    version_id: origin.version_id.clone(),
    label: version.version_number.clone(),
    payload: Some(Payload::Campus(Box::new(version))),
  };
  resolve_remote(app, origin, root, name, &latest).await
}

/// The newest version of the followed line, or `None` when there is nothing
/// newer than the current one. Cheap: no archive is downloaded.
pub async fn latest_version(
  app: &AppHandle,
  origin: &InstanceOrigin,
) -> USTBLResult<Option<LatestVersion>> {
  match origin.platform {
    OriginPlatform::Campus => {
      let slug = loader_slug(origin.loader_type).ok_or(InstanceError::ModpackUpdateUnsupported)?;
      let path = format!(
        "/projects/{}/lines/{}/{slug}",
        origin.project_id, origin.game_version
      );
      // A line the platform no longer lists is not "up to date": say so.
      let version = campus_get::<CampusVersion>(app, &path)
        .await?
        .ok_or(InstanceError::ModpackUpdateLineGone)?;
      if version.id.to_string() == origin.version_id {
        return Ok(None);
      }
      Ok(Some(LatestVersion {
        version_id: version.id.to_string(),
        label: version.version_number.clone(),
        payload: Some(Payload::Campus(Box::new(version))),
      }))
    }
    // A local import has no remote: nothing is asked and nothing is newer.
    OriginPlatform::Local => Ok(None),
    OriginPlatform::Modrinth => platform_latest(app, origin, OtherResourceSource::Modrinth).await,
    OriginPlatform::CurseForge => {
      platform_latest(app, origin, OtherResourceSource::CurseForge).await
    }
  }
}

async fn platform_latest(
  app: &AppHandle,
  origin: &InstanceOrigin,
  source: OtherResourceSource,
) -> USTBLResult<Option<LatestVersion>> {
  let packs = fetch_resource_version_packs(
    app.clone(),
    source,
    OtherResourceVersionPackQuery {
      resource_id: origin.project_id.clone(),
      mod_loader: ALL_FILTER.into(),
      game_versions: vec![origin.game_version.clone()],
    },
  )
  .await?;
  let items = packs.into_iter().flat_map(|pack| pack.items).collect();
  let Some(item) = select_update(items, origin.loader_type, &origin.version_id) else {
    return Ok(None);
  };
  if item.download_url.is_empty() || item.sha1.is_empty() {
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  Ok(Some(LatestVersion {
    version_id: item.file_id,
    label: item.name,
    payload: Some(Payload::Archive {
      url: item.download_url,
      sha1: item.sha1.to_lowercase(),
    }),
  }))
}

/// Fetch whatever the new version needs to be described file by file. The
/// parsed result is cached per version under the update folder, so a plan and
/// the apply right after it resolve the remote only once.
pub async fn resolve_remote(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  instance_name: &str,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack> {
  let dir = remote_dir(root);
  fs::create_dir_all(&dir).at_in(&dir, root)?;
  let remote = if let Some(remote) = load_remote_cache(app, origin, root, latest) {
    remote
  } else {
    let payload = latest
      .payload
      .as_ref()
      .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
    let (remote, archive) = match payload {
      Payload::Archive { sha1, .. } if origin.platform == OriginPlatform::Local => {
        // The import copied the archive here and nothing else has it: when it
        // is gone or changed there is nothing to fetch it from.
        let path = archive_path(root, sha1);
        let (actual, size) = tokio::task::spawn_blocking({
          let path = path.clone();
          move || hash_file(&path)
        })
        .await
        .map_err(|e| USTBLError(e.to_string()))?
        .map_err(|_| InstanceError::ModpackUpdateRemoteInvalid)?;
        if actual != *sha1 {
          return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
        }
        (
          local_remote(app, &path, root, instance_name, origin, latest).await?,
          Some((sha1.as_str(), size)),
        )
      }
      Payload::Archive { url, sha1 } => {
        let path = archive_path(root, sha1);
        let cached = tokio::task::spawn_blocking({
          let path = path.clone();
          let sha1 = sha1.clone();
          move || hash_file(&path).is_ok_and(|(actual, _)| actual == sha1)
        })
        .await
        .map_err(|e| USTBLError(e.to_string()))?;
        let download_started = std::time::Instant::now();
        if !cached {
          download_archive(app, url, &path, sha1, root).await?;
        }
        log::info!(
          "modpack archive ready: cached={cached} elapsed_ms={}",
          download_started.elapsed().as_millis()
        );
        let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        (
          remote_from_archive(app, &path, root, instance_name, origin, latest).await?,
          Some((sha1.as_str(), size)),
        )
      }
      Payload::Campus(version) => {
        let packs = PackSession::new(AppTransport::new(app.clone()), version.id);
        (campus_remote(version, &packs)?, None)
      }
    };
    let archive = archive.map(|(sha1, size)| (sha1.to_string(), size));
    let archive_ref = archive.as_ref().map(|(sha1, size)| (sha1.as_str(), *size));
    let cache = RemoteCache::new(origin, root, &remote, archive_ref);
    if let Err(error) = write_remote_cache(root, &cache) {
      log::warn!("cannot write modpack remote cache: {error:?}");
    }
    remote
  };
  if remote.game_version != origin.game_version
    || (remote.loader.loader_type != origin.loader_type && !origin.version_id.is_empty())
  {
    return Err(InstanceError::ModpackUpdateLineMismatch.into());
  }
  remote.validate(instance_name)?;
  Ok(remote)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  #[test]
  fn only_campus_blobs_and_bundles_are_located() {
    assert_eq!(
      campus_location_url(&format!("{CAMPUS_API}/blobs/abc")).as_deref(),
      Some("https://www.ustb.world/api/modpacks/blobs/abc/location")
    );
    assert_eq!(
      campus_location_url(&format!("{CAMPUS_API}/versions/7/bundle")).as_deref(),
      Some("https://www.ustb.world/api/modpacks/versions/7/bundle/location")
    );
    assert!(campus_location_url(&format!("{CAMPUS_API}/projects")).is_none());
    assert!(campus_location_url(&format!("{CAMPUS_API}/versions/7")).is_none());
    assert!(campus_location_url("https://cdn.example/blobs/abc").is_none());
    assert!(campus_location_url("https://www.ustb.world/api/modpacksx/blobs/a").is_none());
  }

  fn item(id: &str, date: &str, loader: Option<&str>) -> OtherResourceFileInfo {
    OtherResourceFileInfo {
      file_id: id.into(),
      name: format!("v{id}"),
      file_date: date.into(),
      loader: loader.map(Into::into),
      ..Default::default()
    }
  }

  #[test]
  fn newest_file_wins_and_the_current_one_is_not_an_update() {
    let items = vec![
      item("a", "2024-01-01T00:00:00Z", Some("fabric")),
      item("c", "2024-03-01T00:00:00Z", Some("fabric")),
      item("b", "2024-02-01T00:00:00Z", Some("fabric")),
    ];
    let update = select_update(items.clone(), ModLoaderType::Fabric, "a").unwrap();
    assert_eq!(update.file_id, "c");
    assert!(select_update(items.clone(), ModLoaderType::Fabric, "c").is_none());
    // An unlisted current file (deleted upstream) still moves to the newest.
    assert_eq!(
      select_update(items, ModLoaderType::Fabric, "gone")
        .unwrap()
        .file_id,
      "c"
    );
  }

  #[test]
  fn an_older_listing_than_the_current_file_is_not_a_downgrade() {
    let items = vec![
      item("old", "2024-01-01T00:00:00Z", None),
      item("cur", "2024-02-01T00:00:00Z", None),
    ];
    assert!(select_update(items, ModLoaderType::Fabric, "cur").is_none());
  }

  #[test]
  fn other_loaders_are_ignored_but_unlabelled_files_are_kept() {
    let items = vec![
      item("fabric", "2024-01-01T00:00:00Z", Some("Fabric")),
      item("forge", "2024-05-01T00:00:00Z", Some("forge")),
      item("blank", "2024-02-01T00:00:00Z", Some("")),
    ];
    assert_eq!(
      select_update(items.clone(), ModLoaderType::Fabric, "x")
        .unwrap()
        .file_id,
      "blank"
    );
    assert_eq!(
      select_update(items.clone(), ModLoaderType::LegacyForge, "x")
        .unwrap()
        .file_id,
      "forge"
    );
    assert!(select_update(vec![], ModLoaderType::Fabric, "x").is_none());
  }

  fn excluded(id: &str, date: &str) -> OtherResourceFileInfo {
    OtherResourceFileInfo {
      update_excluded: true,
      ..item(id, date, None)
    }
  }

  #[test]
  fn excluded_files_are_never_selected_as_the_update() {
    // The newest file is a server pack: the older client pack wins.
    let items = vec![
      item("client", "2024-01-01T00:00:00Z", None),
      excluded("server", "2024-02-01T00:00:00Z"),
    ];
    assert_eq!(
      select_update(items.clone(), ModLoaderType::Fabric, "old")
        .unwrap()
        .file_id,
      "client"
    );
    // Current is the client pack and only a newer excluded file exists.
    assert!(select_update(items, ModLoaderType::Fabric, "client").is_none());
  }

  #[test]
  fn a_newer_additional_file_does_not_hide_or_replace_the_primary() {
    let items = vec![
      item("v2", "2024-03-01T00:00:00Z", None),
      excluded("v2", "2024-03-01T00:00:00Z"),
      item("v1", "2024-01-01T00:00:00Z", None),
      excluded("v1", "2024-01-01T00:00:00Z"),
    ];
    assert!(select_update(items.clone(), ModLoaderType::Fabric, "v2").is_none());
    let update = select_update(items, ModLoaderType::Fabric, "v1").unwrap();
    assert_eq!(update.file_id, "v2");
    assert!(!update.update_excluded);
  }

  #[test]
  fn nothing_is_selected_when_every_file_is_excluded() {
    let items = vec![
      excluded("a", "2024-01-01T00:00:00Z"),
      excluded("b", "2024-02-01T00:00:00Z"),
    ];
    assert!(select_update(items, ModLoaderType::Fabric, "x").is_none());
  }

  #[test]
  fn same_date_selection_ignores_excluded_files_in_any_order() {
    let date = "2024-01-01T00:00:00Z";
    for items in [
      vec![item("main", date, None), excluded("extra", date)],
      vec![excluded("extra", date), item("main", date, None)],
    ] {
      assert_eq!(
        select_update(items, ModLoaderType::Fabric, "x")
          .unwrap()
          .file_id,
        "main"
      );
    }
  }

  fn campus(files: serde_json::Value) -> CampusVersion {
    serde_json::from_value(serde_json::json!({
      "id": 7,
      "version_number": "1.2.0",
      "game_version": "1.21.1",
      "loader_type": "neoforge",
      "loader_version": "21.1.5",
      "manifest": { "format": "ustbl.modpack/3", "files": files },
    }))
    .unwrap()
  }

  #[test]
  fn disabled_once_and_rules_ride_along() {
    let sha = "a".repeat(40);
    let mut version = campus(serde_json::json!([
      {"path": "mods/a.jar", "size": 1, "sha1": sha, "source": {"kind": "pack"}, "disabled": true},
      {"path": "config/first.toml", "size": 1, "sha1": sha, "source": {"kind": "pack"}, "once": true},
    ]));
    version.manifest.rules = serde_json::from_value(serde_json::json!([
      {"path": "config/yes_steve_model/cache", "kind": "dir", "state": "ignore"},
      {"path": "resourcepacks/old.zip", "kind": "file", "state": "delete"},
    ]))
    .unwrap();

    let remote = campus_remote(&version, &session()).unwrap();

    assert!(remote.rules_manifest && remote.disabled_authoritative);
    assert_eq!(remote.files[0].path, "mods/a.jar");
    assert!(remote.files[0].disabled && !remote.files[0].once);
    assert_eq!(remote.files[1].path, "config/first.toml");
    assert!(remote.files[1].once && !remote.files[1].disabled);
    assert_eq!(
      remote.rules,
      vec![
        super::super::remote::RemoteRule {
          path: "config/yes_steve_model/cache".into(),
          kind: super::super::remote::RuleKind::Dir,
          state: super::super::remote::RuleState::Ignore,
        },
        super::super::remote::RemoteRule {
          path: "resourcepacks/old.zip".into(),
          kind: super::super::remote::RuleKind::File,
          state: super::super::remote::RuleState::Delete,
        },
      ]
    );
    // Unknown kinds or states are not a manifest.
    version.manifest.rules = serde_json::from_value(serde_json::json!([
      {"path": "config", "kind": "tree", "state": "ignore"},
    ]))
    .unwrap();
    assert!(campus_remote(&version, &session()).is_err());
    version.manifest.rules = serde_json::from_value(serde_json::json!([
      {"path": "config", "kind": "dir", "state": "ship"},
    ]))
    .unwrap();
    assert!(campus_remote(&version, &session()).is_err());
  }

  /// Packs are never touched by these tests; they only check the mapping.
  struct NoTransport;

  #[async_trait::async_trait]
  impl crate::instance::modpack_update::packs::PackTransport for NoTransport {
    async fn locate(
      &self,
      _: i64,
      _: &[String],
    ) -> Result<
      crate::instance::modpack_update::packs::Located,
      crate::instance::modpack_update::packs::TransportError,
    > {
      unreachable!()
    }
    async fn read_range(
      &self,
      _: &crate::instance::modpack_update::packs::PackLocation,
      _: u64,
      _: u64,
      _: &Path,
    ) -> Result<(), crate::instance::modpack_update::packs::TransportError> {
      unreachable!()
    }
  }

  fn session() -> Arc<PackSession> {
    PackSession::new(Arc::new(NoTransport), 7)
  }

  #[test]
  fn campus_versions_map_every_source_kind() {
    let sha = "a".repeat(40);
    let version = campus(serde_json::json!([
      {"path": "mods/r.jar", "size": 1, "sha1": sha, "source": {"kind": "remote", "url": "https://cdn.example/r.jar"}},
      {"path": "mods/b.jar", "size": 2, "sha1": sha.to_uppercase(), "source": {"kind": "pack"}},
      {"path": "mods/server.jar", "size": 4, "sha1": sha, "env": {"client": "unsupported", "server": "required"}, "source": {"kind": "pack"}},
    ]));
    let packs = session();

    let remote = campus_remote(&version, &packs).unwrap();

    assert_eq!(remote.version_id, "7");
    assert_eq!(remote.label, "1.2.0");
    assert_eq!(remote.loader.loader_type, ModLoaderType::NeoForge);
    assert_eq!(remote.loader.version, "21.1.5");
    let paths: Vec<_> = remote.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["mods/r.jar", "mods/b.jar"]);
    assert_eq!(remote.files[1].key.sha1, sha);
    match &remote.files[0].source {
      FileSource::Download(param) => assert_eq!(param.src.as_str(), "https://cdn.example/r.jar"),
      other => panic!("{other:?}"),
    }
    match &remote.files[1].source {
      FileSource::Pack(found) => assert!(Arc::ptr_eq(found, &packs)),
      other => panic!("{other:?}"),
    }
  }

  #[test]
  fn campus_component_chain_is_read_in_canonical_order() {
    let mut version = campus(serde_json::json!([]));
    assert!(campus_remote(&version, &session())
      .unwrap()
      .components
      .is_none());

    version.manifest.components = serde_json::from_value(serde_json::json!({
      "mmcPack": {
        "formatVersion": 1,
        "components": [
          {"uid": "net.minecraftforge", "version": "10.13.4.1614", "cachedName": "Forge"},
          {"uid": "net.minecraft", "version": "1.7.10", "important": true},
        ],
      },
      "instanceCfg": {"name": "GTNH"},
      "patches": [{"uid": "com.example.loader", "blob": "a".repeat(40), "size": 5}],
      "libraries": [{"path": "libraries/boot-1.jar", "blob": "b".repeat(40), "size": 3}],
    }))
    .unwrap();
    let published = version.manifest.components.as_ref().unwrap();
    assert_eq!(published.patches[0].uid, "com.example.loader");
    assert_eq!(published.libraries[0].path, "libraries/boot-1.jar");
    assert_eq!(published.instance_cfg["name"], "GTNH");
    let chain = campus_remote(&version, &session())
      .unwrap()
      .components
      .unwrap();
    let uids: Vec<_> = chain.iter().map(|c| c.uid.as_str()).collect();
    assert_eq!(uids, ["net.minecraft", "net.minecraftforge"]);
    assert_eq!(chain[1].version, "10.13.4.1614");
  }

  #[test]
  fn retired_source_kinds_malformed_hashes_and_unknown_loaders_are_invalid() {
    let sha = "a".repeat(40);
    for source in [
      serde_json::json!({"kind": "bundle"}),
      serde_json::json!({"kind": "blob"}),
      serde_json::json!({"kind": "remote"}),
    ] {
      let version = campus(serde_json::json!([
        {"path": "a", "size": 1, "sha1": sha, "source": source},
      ]));
      assert!(campus_remote(&version, &session()).is_err());
    }
    // Anything but /3 is refused before any file is looked at.
    let mut old = campus(serde_json::json!([]));
    old.manifest.format = "ustbl.modpack/2".into();
    assert!(campus_remote(&old, &session()).is_err());
    let short = campus(serde_json::json!([
      {"path": "a", "size": 1, "sha1": "abc", "source": {"kind": "pack"}},
    ]));
    assert!(campus_remote(&short, &session()).is_err());

    let mut odd = campus(serde_json::json!([]));
    odd.loader_type = "quilt".into();
    assert!(campus_remote(&odd, &session()).is_err());
  }

  #[test]
  fn overrides_replace_downloads_of_the_same_path() {
    let file = |path: &str, sha1: &str, kind: bool| RemoteFile {
      path: path.into(),
      key: FileKey {
        sha1: sha1.into(),
        size: 1,
      },
      source: if kind {
        FileSource::ArchiveEntry {
          archive: PathBuf::new(),
          entry: path.into(),
        }
      } else {
        FileSource::Download(plain_download("https://example.com/x").unwrap())
      },
      disabled: false,
      once: false,
    };
    let merged = merge_files(
      vec![file("a", "1", false), file("b", "2", false)],
      vec![file("a", "3", true)],
    );
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].key.sha1, "3");
    assert!(matches!(merged[0].source, FileSource::ArchiveEntry { .. }));
  }

  #[test]
  fn paths_differing_only_in_case_are_folded_when_their_content_is_the_same() {
    let file = |path: &str, sha1: &str| RemoteFile {
      path: path.into(),
      key: FileKey {
        sha1: sha1.into(),
        size: 1,
      },
      source: FileSource::ArchiveEntry {
        archive: PathBuf::new(),
        entry: path.into(),
      },
      disabled: false,
      once: false,
    };
    let same = merge_files(
      Vec::new(),
      vec![
        file("config/Pack[ABC]/lang/zh_CN.lang", "1"),
        file("config/Pack[abc]/lang/zh_CN.lang", "1"),
        file("config/other.cfg", "2"),
      ],
    );
    let paths: Vec<&str> = same.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(
      paths,
      ["config/Pack[ABC]/lang/zh_CN.lang", "config/other.cfg"]
    );

    // Different content is left for `validate` to refuse.
    let different = merge_files(
      Vec::new(),
      vec![file("config/A.cfg", "1"), file("config/a.cfg", "2")],
    );
    assert_eq!(different.len(), 2);
  }

  fn origin_with(platform: OriginPlatform, version_id: &str) -> InstanceOrigin {
    InstanceOrigin {
      platform,
      project_id: "p".into(),
      version_id: version_id.into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
      pack_format: None,
    }
  }

  fn archive_version(version_id: &str, sha1: &str) -> LatestVersion {
    LatestVersion {
      version_id: version_id.into(),
      label: version_id.into(),
      payload: Some(Payload::Archive {
        url: "https://cdn.example/pack.zip".into(),
        sha1: sha1.into(),
      }),
    }
  }

  fn remote_for_test() -> RemoteModpack {
    RemoteModpack {
      version_id: "7".into(),
      label: "1.2.0".into(),
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16.0".into(),
      },
      components: None,
      component_pack: None,
      recommended_memory_mb: None,
      files: vec![
        RemoteFile {
          path: "mods/a.jar".into(),
          key: FileKey {
            sha1: "aa".repeat(20),
            size: 3,
          },
          source: FileSource::Download(plain_download("https://cdn.example/a.jar").unwrap()),
          disabled: false,
          once: false,
        },
        RemoteFile {
          path: "config/b.txt".into(),
          key: FileKey {
            sha1: "bb".repeat(20),
            size: 5,
          },
          source: FileSource::ArchiveEntry {
            archive: PathBuf::from("/tmp/pack.zip"),
            entry: "overrides/config/b.txt".into(),
          },
          disabled: false,
          once: false,
        },
      ],
      rules: Vec::new(),
      rules_manifest: false,
      disabled_authoritative: false,
      optional: Vec::new(),
      path_index: Arc::new(std::sync::OnceLock::new()),
    }
  }

  fn archive_file(path: &str, content: &str, from_download: bool) -> RemoteFile {
    RemoteFile {
      path: path.into(),
      key: FileKey {
        sha1: hex::encode(Sha1::digest(content.as_bytes())),
        size: content.len() as u64,
      },
      source: if from_download {
        FileSource::Download(plain_download(&format!("https://cdn.example/{content}")).unwrap())
      } else {
        FileSource::ArchiveEntry {
          archive: PathBuf::from("/tmp/pack.zip"),
          entry: format!("overrides/{path}"),
        }
      },
      disabled: false,
      once: false,
    }
  }

  fn platform_remote(downloads: Vec<RemoteFile>, overrides: Vec<RemoteFile>) -> RemoteModpack {
    let (downloads, overrides) = read_disabled_mods(downloads, overrides);
    RemoteModpack {
      files: merge_files(downloads, overrides),
      rules_manifest: false,
      disabled_authoritative: true,
      ..remote_for_test()
    }
  }

  #[test]
  fn disabled_mods_of_an_archive_are_read_as_disabled_mods() {
    let remote = platform_remote(
      vec![archive_file("mods/dl.jar.disabled", "dl", true)],
      vec![
        archive_file("mods/ov.JAR.DISABLED", "ov", false),
        archive_file("mods/on.jar", "on", false),
      ],
    );

    remote.validate("pack").unwrap();
    let state = |path: &str| {
      let file = remote.file_of(path).unwrap();
      (file.path.clone(), remote.ship_of(path).unwrap().disabled)
    };
    assert_eq!(state("mods/dl.jar"), ("mods/dl.jar".into(), Some(true)));
    assert_eq!(state("mods/ov.jar"), ("mods/ov.JAR".into(), Some(true)));
    assert_eq!(state("mods/on.jar"), ("mods/on.jar".into(), Some(false)));
    assert_eq!(remote.files.len(), 3);
    // A download is still fetched, an override still read out of the archive.
    assert!(matches!(
      remote.file_of("mods/dl.jar").unwrap().source,
      FileSource::Download(_)
    ));
    assert!(matches!(
      remote.file_of("mods/ov.jar").unwrap().source,
      FileSource::ArchiveEntry { .. }
    ));
  }

  #[test]
  fn an_archive_carrying_both_spellings_keeps_the_enabled_one() {
    for (disabled_is_download, enabled_is_download) in
      [(true, true), (true, false), (false, true), (false, false)]
    {
      let disabled = archive_file("mods/a.jar.disabled", "off", disabled_is_download);
      let enabled = archive_file("mods/a.jar", "on", enabled_is_download);
      let (downloads, overrides): (Vec<_>, Vec<_>) = [disabled, enabled]
        .into_iter()
        .partition(|file| matches!(file.source, FileSource::Download(_)));

      let remote = platform_remote(downloads, overrides);

      remote.validate("pack").unwrap();
      assert_eq!(remote.files.len(), 1);
      assert_eq!(remote.files[0].path, "mods/a.jar");
      assert!(!remote.files[0].disabled);
      assert_eq!(remote.files[0].key, archive_file("x", "on", true).key);
    }
  }

  #[test]
  fn a_disabled_suffix_outside_mods_is_skipped() {
    let remote = platform_remote(
      vec![archive_file("resourcepacks/p.zip.disabled", "p", true)],
      vec![
        archive_file("config/a.toml.disabled", "c", false),
        archive_file("mods/.disabled", "e", false),
        archive_file("modsx/b.jar.disabled", "m", false),
        archive_file("mods/keep.jar", "k", false),
      ],
    );

    remote.validate("pack").unwrap();
    let paths: Vec<_> = remote.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["mods/keep.jar"]);
  }

  #[test]
  fn remote_cache_round_trips_and_matches_only_the_same_version_and_root() {
    let root = std::env::temp_dir().join(format!("ustbl-remote-cache-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let origin = origin_with(OriginPlatform::Modrinth, "7");
    let version = archive_version("7", "cc".repeat(20).as_str());
    let sha1 = "cc".repeat(20);
    fs::create_dir_all(remote_dir(&root)).unwrap();
    fs::write(archive_path(&root, &sha1), b"zip-bytes").unwrap();

    let cache = RemoteCache::new(&origin, &root, &remote_for_test(), Some((&sha1, 9)));
    assert!(cache.matches(&origin, &root, &version));

    // Round trip through JSON.
    let bytes = serde_json::to_vec(&cache).unwrap();
    let back: RemoteCache = serde_json::from_slice(&bytes).unwrap();
    assert!(back.matches(&origin, &root, &version));

    // Another version id, platform or root does not match.
    assert!(!back.matches(&origin, &root, &archive_version("8", &sha1)));
    let other_platform = origin_with(OriginPlatform::CurseForge, "7");
    assert!(!back.matches(&other_platform, &root, &version));
    let other_root = root.join("moved");
    assert!(!back.matches(&origin, &other_root, &version));

    // The archive must still be there with the same size.
    fs::write(archive_path(&root, &sha1), b"changed").unwrap();
    assert!(!back.matches(&origin, &root, &version));
    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn a_download_gets_a_part_name_of_its_own_and_old_parts_are_swept() {
    let root = std::env::temp_dir().join(format!("ustbl-archive-part-{}", uuid::Uuid::new_v4()));
    let sha1 = "dd".repeat(20);
    fs::create_dir_all(remote_dir(&root)).unwrap();
    let archive = archive_path(&root, &sha1);

    let (first, second) = (part_path(&archive), part_path(&archive));
    assert_ne!(first, second);
    assert_eq!(first.parent(), archive.parent());
    assert!(first
      .file_name()
      .unwrap()
      .to_string_lossy()
      .starts_with(&format!("{sha1}.zip.")));
    assert!(first.to_string_lossy().ends_with(PART_SUFFIX));

    // The finished archive, another version's parts and the cache are kept.
    let other = archive_path(&root, &"ee".repeat(20));
    let other_part = part_path(&other);
    let cache = remote_cache_path(&root, "v1");
    for path in [&first, &second, &archive, &other, &other_part, &cache] {
      fs::write(path, b"x").unwrap();
    }
    remove_parts_of(&archive);
    assert!(!first.exists() && !second.exists());
    for path in [&archive, &other, &other_part, &cache] {
      assert!(path.exists(), "{path:?} must stay");
    }
    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn a_campus_remote_cache_needs_no_archive() {
    let root = std::env::temp_dir().join(format!("ustbl-remote-cache-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let origin = origin_with(OriginPlatform::Campus, "7");
    let version = LatestVersion {
      version_id: "7".into(),
      label: "1.2.0".into(),
      payload: None,
    };
    let cache = RemoteCache::new(&origin, &root, &remote_for_test(), None);
    assert!(cache.matches(&origin, &root, &version));
    assert!(!cache.matches(
      &origin,
      &root,
      &archive_version("7", "cc".repeat(20).as_str())
    ));
    let _ = fs::remove_dir_all(&root);
  }

  #[test]
  fn archive_entries_are_hashed_without_extracting() {
    let path = std::env::temp_dir().join(format!("ustbl-src-test-{}.zip", uuid::Uuid::new_v4()));
    {
      let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
      zip
        .start_file("overrides/a.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
      zip.write_all(b"hello").unwrap();
      zip.finish().unwrap();
    }
    let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
    let key = hash_zip_entry(&mut zip, &path, "overrides/a.txt").unwrap();
    drop(zip);
    let _ = fs::remove_file(&path);
    assert_eq!(key.size, 5);
    assert_eq!(key.sha1, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
  }

  fn manifest_download(
    root: &Path,
    relative: &str,
    sha1: Option<&str>,
    size: Option<u64>,
  ) -> ModpackDownload {
    let mut param = *plain_download(&format!("https://cdn.example/{relative}")).unwrap();
    param.dest = root.join(relative);
    param.sha1 = sha1.map(str::to_string);
    ModpackDownload {
      param,
      size,
      optional: false,
      select_key: String::new(),
    }
  }

  fn hashless_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-hashless-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
  }

  const HELLO_SHA1: &str = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";

  #[tokio::test]
  async fn downloads_missing_a_hash_or_size_are_fetched_and_keyed() {
    let root = hashless_root();
    let calls = Mutex::new(Vec::new());
    let fetch = |param: DownloadParam| {
      calls.lock().unwrap().push(param.src.to_string());
      async move {
        fs::create_dir_all(param.dest.parent().unwrap()).unwrap();
        fs::write(&param.dest, b"hello").unwrap();
        Ok(())
      }
    };
    let files = download_files(
      &root,
      vec![
        manifest_download(&root, "mods/full.jar", Some(&"AB".repeat(20)), Some(7)),
        manifest_download(&root, "mods/no-sha.jar", None, Some(5)),
        manifest_download(&root, "mods/no-size.jar", Some(HELLO_SHA1), None),
        manifest_download(&root, "mods/neither.jar", None, None),
      ],
      fetch,
    )
    .await
    .unwrap();

    // The complete one is described as is and never fetched.
    let by_path: HashMap<_, _> = files.iter().map(|f| (f.path.as_str(), f)).collect();
    assert_eq!(files.len(), 4);
    let full = by_path["mods/full.jar"];
    assert!(matches!(full.source, FileSource::Download(_)));
    assert_eq!(full.key.sha1, "ab".repeat(20));
    assert_eq!(full.key.size, 7);
    for name in ["mods/no-sha.jar", "mods/no-size.jar", "mods/neither.jar"] {
      let file = by_path[name];
      assert_eq!(file.key.sha1, HELLO_SHA1, "{name}");
      assert_eq!(file.key.size, 5, "{name}");
      let FileSource::Cached { path } = &file.source else {
        panic!("{name} is not cached");
      };
      assert_eq!(fs::read(path).unwrap(), b"hello");
    }
    let mut fetched = calls.lock().unwrap().clone();
    fetched.sort();
    assert_eq!(fetched.len(), 3);
    assert!(!fetched.iter().any(|url| url.ends_with("full.jar")));
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_file_fetched_before_is_reused_and_a_stale_one_is_fetched_again() {
    let root = hashless_root();
    let count = Mutex::new(0usize);
    let content = Mutex::new(b"hello".to_vec());
    let fetch = |param: DownloadParam| {
      *count.lock().unwrap() += 1;
      let bytes = content.lock().unwrap().clone();
      async move {
        fs::create_dir_all(param.dest.parent().unwrap()).unwrap();
        fs::write(&param.dest, bytes).unwrap();
        Ok(())
      }
    };
    let ask = |sha1: Option<&str>| vec![manifest_download(&root, "mods/a.jar", sha1, None)];

    let first = download_files(&root, ask(None), &fetch).await.unwrap();
    let again = download_files(&root, ask(None), &fetch).await.unwrap();
    assert_eq!(*count.lock().unwrap(), 1);
    assert_eq!(first[0].key, again[0].key);

    // The manifest's own hash disagrees with the kept file: fetch anew.
    *content.lock().unwrap() = b"new".to_vec();
    let wanted = hex::encode(Sha1::digest(b"new"));
    let renewed = download_files(&root, ask(Some(&wanted)), &fetch)
      .await
      .unwrap();
    assert_eq!(*count.lock().unwrap(), 2);
    assert_eq!(renewed[0].key.sha1, wanted);
    assert_eq!(renewed[0].key.size, 3);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_failed_download_of_a_hashless_file_is_an_error_and_leaves_no_file() {
    let root = hashless_root();
    let fetch = |param: DownloadParam| async move {
      fs::create_dir_all(param.dest.parent().unwrap()).unwrap();
      fs::write(&param.dest, b"partial").unwrap();
      Err::<(), _>(USTBLError("boom".into()))
    };
    let result = download_files(
      &root,
      vec![manifest_download(&root, "mods/a.jar", None, None)],
      fetch,
    )
    .await;
    assert!(result.is_err());
    let files_dir = remote_dir(&root).join("files");
    assert!(fs::read_dir(&files_dir).unwrap().next().is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_remote_cache_with_a_missing_kept_file_is_not_a_hit() {
    let root = hashless_root();
    let kept = root.join("kept.bin");
    fs::write(&kept, b"hello").unwrap();
    let mut remote = remote_for_test();
    remote.files.push(RemoteFile {
      path: "mods/a.jar".into(),
      key: FileKey {
        sha1: HELLO_SHA1.into(),
        size: 5,
      },
      source: FileSource::Cached { path: kept.clone() },
      disabled: false,
      once: false,
    });
    let stored = super::super::remote::StoredRemoteModpack::from(&remote);
    assert!(stored.cached_files_present());
    fs::write(&kept, b"hi").unwrap();
    assert!(!stored.cached_files_present());
    fs::remove_file(&kept).unwrap();
    assert!(!stored.cached_files_present());
    let _ = fs::remove_dir_all(root);
  }

  // ---- local import ----

  fn local_zip(entries: &[(&str, &[u8])]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ustbl-local-{}.zip", uuid::Uuid::new_v4()));
    let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
    for (name, content) in entries {
      zip
        .start_file(*name, zip::write::SimpleFileOptions::default())
        .unwrap();
      zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
    path
  }

  fn local_origin() -> InstanceOrigin {
    InstanceOrigin {
      game_version: "1.20.1".into(),
      loader_type: ModLoaderType::Unknown,
      follow: false,
      ..origin_with(OriginPlatform::Local, "")
    }
  }

  fn local_version() -> LatestVersion {
    LatestVersion {
      version_id: "ab".repeat(20),
      label: "1.0".into(),
      payload: Some(Payload::Archive {
        url: String::new(),
        sha1: "ab".repeat(20),
      }),
    }
  }

  /// `local_remote` without the platform lookups: the downloads are given.
  async fn offline_remote(
    archive: &Path,
    root: &Path,
    downloads: Vec<ModpackDownload>,
  ) -> USTBLResult<RemoteModpack> {
    let (origin, latest) = (local_origin(), local_version());
    match LocalPack::open(archive)? {
      LocalPack::Platform(manifest) => {
        let line = manifest_line(&manifest, &origin)?;
        remote_from_downloads(
          &manifest,
          line,
          downloads,
          |_| async { Ok(()) },
          archive,
          root,
          "inst",
          &latest,
        )
        .await
      }
      LocalPack::MultiMc(facts, pack) => {
        multimc_remote(facts, pack, archive, "inst", &latest).await
      }
    }
  }

  fn mrpack_index(dependencies: &str) -> String {
    format!(r#"{{"versionId":"1","name":"test","files":[],"dependencies":{{{dependencies}}}}}"#)
  }

  fn curseforge_manifest(loaders: &str) -> String {
    format!(
      r#"{{"name":"cf","version":"2","author":"a","overrides":"overrides","minecraft":{{"version":"1.20.1","modLoaders":[{loaders}]}},"files":[]}}"#
    )
  }

  fn mmc_pack_json(loader: &str) -> String {
    format!(
      r#"{{"formatVersion":1,"components":[{{"uid":"net.minecraft","version":"1.20.1","important":true}}{loader}]}}"#
    )
  }

  fn file_of<'a>(remote: &'a RemoteModpack, path: &str) -> &'a RemoteFile {
    remote
      .files
      .iter()
      .find(|file| file.path == path)
      .unwrap_or_else(|| panic!("{path} missing from {:?}", remote.files))
  }

  #[tokio::test]
  async fn a_local_mrpack_is_described_by_its_manifest_and_overrides() {
    let root = hashless_root();
    let index = mrpack_index(r#""minecraft":"1.20.1","fabric-loader":"0.15.0""#);
    let archive = local_zip(&[
      ("modrinth.index.json", index.as_bytes()),
      ("overrides/config/a.cfg", b"hello"),
      ("overrides/mods/own.jar", b"jar"),
      ("server-overrides/config/server.cfg", b"no"),
    ]);
    let listed = manifest_download(&root, "mods/listed.jar", Some(HELLO_SHA1), Some(5));
    let shadowed = manifest_download(&root, "config/a.cfg", Some(&"cd".repeat(20)), Some(9));
    let remote = offline_remote(&archive, &root, vec![listed, shadowed])
      .await
      .unwrap();

    assert_eq!(remote.game_version, "1.20.1");
    assert_eq!(
      (remote.loader.loader_type, remote.loader.version.as_str()),
      (ModLoaderType::Fabric, "0.15.0")
    );
    assert_eq!(remote.version_id, "ab".repeat(20));
    assert!(remote.component_pack.is_none() && remote.components.is_none());
    assert!(matches!(
      file_of(&remote, "mods/listed.jar").source,
      FileSource::Download(_)
    ));
    // An override wins over a download of the same path.
    let shadow = file_of(&remote, "config/a.cfg");
    assert!(matches!(shadow.source, FileSource::ArchiveEntry { .. }));
    assert_eq!(shadow.key.sha1, HELLO_SHA1);
    assert!(remote.files.iter().all(|f| f.path != "config/server.cfg"));
    assert_eq!(remote.files.len(), 3);
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_local_curseforge_pack_declares_its_loader_and_a_loaderless_pack_is_vanilla() {
    let root = hashless_root();
    let manifest = curseforge_manifest(r#"{"id":"forge-47.1.0","primary":true}"#);
    let archive = local_zip(&[
      ("manifest.json", manifest.as_bytes()),
      ("overrides/options.txt", b"x"),
    ]);
    let remote = offline_remote(&archive, &root, Vec::new()).await.unwrap();
    assert_eq!(
      (remote.loader.loader_type, remote.loader.version.as_str()),
      (ModLoaderType::Forge, "47.1.0")
    );
    assert_eq!(remote.files.len(), 1);
    let _ = fs::remove_file(archive);

    let manifest = curseforge_manifest("");
    let archive = local_zip(&[("manifest.json", manifest.as_bytes())]);
    let vanilla = offline_remote(&archive, &root, Vec::new()).await.unwrap();
    assert_eq!(vanilla.loader.loader_type, ModLoaderType::Unknown);
    assert!(vanilla.loader.version.is_empty());
    let _ = fs::remove_file(archive);

    let index = mrpack_index(r#""minecraft":"1.20.1""#);
    let archive = local_zip(&[("modrinth.index.json", index.as_bytes())]);
    let vanilla = offline_remote(&archive, &root, Vec::new()).await.unwrap();
    assert_eq!(vanilla.loader.loader_type, ModLoaderType::Unknown);
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn quilt_is_refused_for_the_platform_formats_but_kept_for_multimc() {
    let root = hashless_root();
    let index = mrpack_index(r#""minecraft":"1.20.1","quilt-loader":"0.20.0""#);
    let mrpack = local_zip(&[("modrinth.index.json", index.as_bytes())]);
    let manifest = curseforge_manifest(r#"{"id":"quilt-0.20.0","primary":true}"#);
    let curseforge = local_zip(&[("manifest.json", manifest.as_bytes())]);
    let liteloader = curseforge_manifest(r#"{"id":"liteloader-1.0","primary":true}"#);
    let lite = local_zip(&[("manifest.json", liteloader.as_bytes())]);
    for archive in [&mrpack, &curseforge, &lite] {
      let error = offline_remote(archive, &root, Vec::new())
        .await
        .unwrap_err();
      assert_eq!(error.0, InstanceError::UnsupportedModLoader.to_string());
      let _ = fs::remove_file(archive);
    }

    let pack = mmc_pack_json(r#",{"uid":"org.quiltmc.quilt-loader","version":"0.20.0"}"#);
    let archive = local_zip(&[
      ("mmc-pack.json", pack.as_bytes()),
      ("instance.cfg", b"name=Quilty\n"),
    ]);
    let remote = offline_remote(&archive, &root, Vec::new()).await.unwrap();
    assert_eq!(remote.loader.loader_type, ModLoaderType::Quilt);
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_picked_archive_tells_its_format_and_game_version() {
    let index = mrpack_index(r#""minecraft":"1.20.1","fabric-loader":"0.15.0""#);
    let mrpack = local_zip(&[("modrinth.index.json", index.as_bytes())]);
    let manifest = curseforge_manifest(r#"{"id":"forge-47.1.0","primary":true}"#);
    let curseforge = local_zip(&[("manifest.json", manifest.as_bytes())]);
    let pack = mmc_pack_json(r#",{"uid":"net.fabricmc.fabric-loader","version":"0.15.0"}"#);
    let multimc = local_zip(&[
      ("mmc-pack.json", pack.as_bytes()),
      ("instance.cfg", b"name=Pack\n"),
    ]);
    let other = local_zip(&[("readme.txt", b"no pack")]);
    for (archive, format) in [
      (&mrpack, PackFormat::Modrinth),
      (&curseforge, PackFormat::CurseForge),
      (&multimc, PackFormat::MultiMc),
    ] {
      let facts = local_pack_facts(archive).await.unwrap();
      assert_eq!(facts.format, format);
      assert_eq!(facts.game_version, "1.20.1");
      assert_eq!(detect_pack_format(archive), Some(format));
    }
    assert!(local_pack_facts(&other).await.is_err());
    assert_eq!(detect_pack_format(&other), None);
    assert_eq!(detect_pack_format(Path::new("missing.zip")), None);
    for archive in [mrpack, curseforge, multimc, other] {
      let _ = fs::remove_file(archive);
    }
  }

  #[tokio::test]
  async fn a_multimc_pack_becomes_a_component_pack_with_its_shipped_files() {
    let root = hashless_root();
    let pack = mmc_pack_json(r#",{"uid":"net.fabricmc.fabric-loader","version":"0.15.0"}"#);
    for base in ["", "Pack/"] {
      let name = |rest: &str| format!("{base}{rest}");
      let entries = [
        (name("mmc-pack.json"), pack.as_bytes().to_vec()),
        (
          name("instance.cfg"),
          b"name=Pack\nJvmArgs=-Xmx2G\n".to_vec(),
        ),
        (
          name("patches/net.fabricmc.fabric-loader.json"),
          b"{\"uid\":1}".to_vec(),
        ),
        (name("libraries/custom.jar"), b"hello".to_vec()),
        (name("patches/../../evil.json"), b"x".to_vec()),
        (name(".minecraft/mods/m.jar"), b"jar".to_vec()),
        (name(".minecraft/config/c.cfg"), b"cfg".to_vec()),
      ];
      let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_slice()))
        .collect();
      let archive = local_zip(&refs);
      let remote = offline_remote(&archive, &root, Vec::new()).await.unwrap();

      assert_eq!(remote.game_version, "1.20.1");
      assert_eq!(
        (remote.loader.loader_type, remote.loader.version.as_str()),
        (ModLoaderType::Fabric, "0.15.0")
      );
      let mut paths: Vec<&str> = remote.files.iter().map(|f| f.path.as_str()).collect();
      paths.sort();
      assert_eq!(paths, ["config/c.cfg", "mods/m.jar"], "base {base:?}");
      let chain = remote.components.as_ref().unwrap();
      assert!(chain.iter().any(|c| c.uid == "net.fabricmc.fabric-loader"));
      let component_pack = remote.component_pack.as_ref().unwrap();
      let mut shipped: Vec<&str> = component_pack
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
      shipped.sort();
      assert_eq!(
        shipped,
        [
          "libraries/custom.jar",
          "patches/net.fabricmc.fabric-loader.json"
        ]
      );
      let library = component_pack
        .files
        .iter()
        .find(|f| f.path == "libraries/custom.jar")
        .unwrap();
      assert_eq!(
        (library.key.sha1.as_str(), library.key.size),
        (HELLO_SHA1, 5)
      );
      assert_eq!(component_pack.instance_cfg["name"], "Pack");
      assert_eq!(component_pack.instance_cfg["JvmArgs"], "-Xmx2G");
      // The pack is kept as shipped, flags the typed manifest does not model included.
      assert_eq!(component_pack.mmc_pack["components"][0]["important"], true);

      // What the update engine stores between planning and applying.
      let origin = local_origin();
      let cache = RemoteCache::new(&origin, &root, &remote, Some((&"ab".repeat(20), 1)));
      let bytes = serde_json::to_vec(&cache).unwrap();
      let cache: RemoteCache = serde_json::from_slice(&bytes).unwrap();
      let restored = RemoteModpack::from_stored(cache.remote, |_| unreachable!());
      assert_eq!(restored.component_pack.unwrap().files.len(), 2);
      assert_eq!(restored.files.len(), 2);
      let _ = fs::remove_file(archive);
    }
    let _ = fs::remove_dir_all(root);
  }

  /// A picked archive is untrusted. A file it ships or asks for that would land
  /// in the update folder, or outside the instance, must not get through: the
  /// install validates the remote before it writes anything.
  #[tokio::test]
  async fn a_local_pack_cannot_reach_the_update_folder_or_leave_the_instance() {
    let root = hashless_root();
    let index = mrpack_index(r#""minecraft":"1.20.1""#);
    for planted in [
      ".ustbl-update/journal.json",
      ".USTBL-UPDATE/backup/files/x.bat",
    ] {
      let entry = format!("overrides/{planted}");
      let archive = local_zip(&[
        ("modrinth.index.json", index.as_bytes()),
        (entry.as_str(), br#"{"state":"committing"}"#),
      ]);
      let checked = offline_remote(&archive, &root, Vec::new())
        .await
        .and_then(|remote| remote.validate("inst"));
      assert!(checked.is_err(), "{planted} was accepted");
      let _ = fs::remove_file(archive);
    }

    let archive = local_zip(&[("modrinth.index.json", index.as_bytes())]);
    let escaping = manifest_download(&root, "../escape.jar", Some(HELLO_SHA1), Some(5));
    let checked = offline_remote(&archive, &root, vec![escaping])
      .await
      .and_then(|remote| remote.validate("inst"));
    assert!(checked.is_err(), "a path leaving the instance was accepted");

    // An absolute path replaces the instance root when joined; such a file is
    // not one of the instance's and is left out.
    let outside = std::env::temp_dir().join(format!("ustbl-outside-{}.jar", uuid::Uuid::new_v4()));
    let mut absolute = manifest_download(&root, "mods/a.jar", Some(HELLO_SHA1), Some(5));
    absolute.param.dest = outside.clone();
    let remote = offline_remote(&archive, &root, vec![absolute])
      .await
      .unwrap();
    assert!(remote.files.is_empty(), "{:?}", remote.files);
    assert!(!outside.exists());
    let _ = fs::remove_file(archive);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn an_archive_that_is_none_of_the_formats_is_refused() {
    let root = hashless_root();
    let archive = local_zip(&[("readme.txt", b"hi")]);
    assert!(offline_remote(&archive, &root, Vec::new()).await.is_err());
    // A broken manifest of a known format is not mistaken for a MultiMC pack.
    let broken = local_zip(&[("modrinth.index.json", b"{")]);
    assert!(offline_remote(&broken, &root, Vec::new()).await.is_err());
    // A MultiMC pack without a game version is refused as well.
    let no_game = local_zip(&[
      ("mmc-pack.json", br#"{"formatVersion":1,"components":[]}"#),
      ("instance.cfg", b"name=x\n"),
    ]);
    assert!(offline_remote(&no_game, &root, Vec::new()).await.is_err());
    for path in [archive, broken, no_game] {
      let _ = fs::remove_file(path);
    }
    let _ = fs::remove_dir_all(root);
  }
}
