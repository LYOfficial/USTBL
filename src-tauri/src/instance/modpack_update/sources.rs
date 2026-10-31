//! Where the next version of a followed modpack comes from.
//!
//! Modrinth and CurseForge publish an archive that is downloaded and read;
//! the campus platform publishes a manifest whose files are either remote or
//! stored in the platform's packs.

use super::diff::{FileKey, DISABLED_SUFFIX};
use super::packs::{AppTransport, PackSession};
use super::remote::{ComponentPack, FileSource, RemoteFile, RemoteModpack, UPDATE_DIR};
use crate::error::{USTBLError, USTBLResult};
use crate::file_error::AtPath;
use crate::instance::helpers::modpack::baseline::{
  components_of_mmc_pack, hash_file, normalize_relative_path, BaselineLoader,
};
use crate::instance::helpers::modpack::misc::{list_override_entries, parse_platform_manifest};
use crate::instance::models::misc::{InstanceError, InstanceOrigin, ModLoaderType, OriginPlatform};
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
use std::sync::Arc;
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
pub fn select_update(
  items: Vec<OtherResourceFileInfo>,
  loader: ModLoaderType,
  current_id: &str,
) -> Option<OtherResourceFileInfo> {
  let candidates: Vec<OtherResourceFileInfo> = items
    .into_iter()
    .filter(|item| loader_matches(item.loader.as_deref(), loader))
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

fn plain_download(url: &str) -> USTBLResult<Box<DownloadParam>> {
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
    files,
    rules,
    rules_manifest: true,
    disabled_authoritative: true,
    path_index: Arc::new(std::sync::OnceLock::new()),
  })
}

// ---- archives ---------------------------------------------------------------

/// A version's parsed remote, cached under the update folder. The archive of
/// an Archive version is also kept next to it; a hit skips re-downloading,
/// re-hashing and re-parsing it.
const REMOTE_CACHE_SCHEMA: u32 = 1;

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
fn archive_path(root: &Path, sha1: &str) -> PathBuf {
  remote_dir(root).join(format!("{sha1}.zip"))
}

pub fn load_remote_cache(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  version: &LatestVersion,
) -> Option<RemoteModpack> {
  let bytes = fs::read(remote_cache_path(root, &version.version_id)).ok()?;
  let cache: RemoteCache = serde_json::from_slice(&bytes).ok()?;
  if cache.matches(origin, root, version) {
    Some(RemoteModpack::from_stored(cache.remote, |version_id| {
      PackSession::new(AppTransport::new(app.clone()), version_id)
    }))
  } else {
    None
  }
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

/// SHA-1 and size of one archive entry, streamed.
pub fn hash_archive_entry(archive: &Path, entry: &str) -> USTBLResult<FileKey> {
  let mut zip = zip::ZipArchive::new(File::open(archive).at(archive)?)?;
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
pub fn merge_files(downloads: Vec<RemoteFile>, overrides: Vec<RemoteFile>) -> Vec<RemoteFile> {
  let mut by_path = BTreeMap::new();
  for file in downloads.into_iter().chain(overrides) {
    by_path.insert(file.path.clone(), file);
  }
  by_path.into_values().collect()
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
  let game_version = manifest.get_client_version()?;
  let (loader_type, loader_version) = manifest.get_mod_loader_type_version()?;
  if game_version != origin.game_version
    || (loader_type != origin.loader_type && !origin.version_id.is_empty())
  {
    return Err(InstanceError::ModpackUpdateLineMismatch.into());
  }

  let mut downloads = Vec::new();
  for download in manifest.get_download_params(app, root).await? {
    // Without a hash and size a file cannot be compared; a fresh install does
    // not track it either.
    let (Some(sha1), Some(size), Ok(relative)) = (
      download.param.sha1.clone(),
      download.size,
      download.param.dest.strip_prefix(root),
    ) else {
      continue;
    };
    downloads.push(RemoteFile {
      path: normalize_relative_path(&relative.to_string_lossy()),
      key: FileKey {
        sha1: sha1.to_lowercase(),
        size,
      },
      source: FileSource::Download(Box::new(download.param)),
      disabled: false,
      once: false,
    });
  }

  let overrides_paths = manifest.get_overrides_paths();
  let (archive_path, name) = (archive.to_path_buf(), instance_name.to_string());
  let overrides = tokio::task::spawn_blocking(move || -> USTBLResult<Vec<RemoteFile>> {
    let file = File::open(&archive_path)?;
    let entries = list_override_entries(&file, &overrides_paths, &name)?;
    entries
      .into_iter()
      .map(|(path, entry)| {
        Ok(RemoteFile {
          key: hash_archive_entry(&archive_path, &entry)?,
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

  let (downloads, overrides) = read_disabled_mods(downloads, overrides);

  Ok(RemoteModpack {
    version_id: latest.version_id.clone(),
    label: latest.label.clone(),
    game_version,
    loader: BaselineLoader {
      loader_type,
      version: loader_version,
    },
    components: None,
    component_pack: None,
    files: merge_files(downloads, overrides),
    rules: Vec::new(),
    rules_manifest: false,
    disabled_authoritative: true,
    path_index: Arc::new(std::sync::OnceLock::new()),
  })
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

fn remote_dir(root: &Path) -> PathBuf {
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

/// Fetch the project listing for the light check. A 404 whose detail is the
/// deletion proof returns `None`; any other 404 is the line being gone.
pub async fn campus_project_for_light_check(
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
    platform => {
      let source = match platform {
        OriginPlatform::Modrinth => OtherResourceSource::Modrinth,
        _ => OtherResourceSource::CurseForge,
      };
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
  }
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
      Payload::Archive { url, sha1 } => {
        let path = archive_path(root, sha1);
        let cached = tokio::task::spawn_blocking({
          let path = path.clone();
          let sha1 = sha1.clone();
          move || hash_file(&path).is_ok_and(|(actual, _)| actual == sha1)
        })
        .await
        .map_err(|e| USTBLError(e.to_string()))?;
        if !cached {
          download_url(app, url, &path, Some(sha1)).await?;
        }
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

  fn origin_with(platform: OriginPlatform, version_id: &str) -> InstanceOrigin {
    InstanceOrigin {
      platform,
      project_id: "p".into(),
      version_id: version_id.into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
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
    let key = hash_archive_entry(&path, "overrides/a.txt").unwrap();
    let _ = fs::remove_file(&path);
    assert_eq!(key.size, 5);
    assert_eq!(key.sha1, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
  }
}
