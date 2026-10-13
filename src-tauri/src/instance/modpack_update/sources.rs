//! Where the next version of a followed modpack comes from.
//!
//! Modrinth and CurseForge publish an archive that is downloaded and read;
//! the campus platform publishes a manifest plus an optional bundle.

use super::diff::FileKey;
use super::remote::{FileSource, RemoteFile, RemoteModpack, UPDATE_DIR};
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::{
  canonical_components, hash_file, normalize_relative_path, BaselineLoader, ComponentRef,
};
use crate::instance::helpers::modpack::misc::{list_override_entries, parse_platform_manifest};
use crate::instance::models::misc::{InstanceError, InstanceOrigin, ModLoaderType, OriginPlatform};
use crate::resource::commands::fetch_resource_version_packs;
use crate::resource::models::{
  OtherResourceFileInfo, OtherResourceSource, OtherResourceVersionPackQuery,
};
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
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
  pub payload: Payload,
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

#[derive(Debug, Clone, Deserialize)]
pub struct CampusManifest {
  pub files: Vec<CampusFile>,
  /// Component chain of a component-profile pack.
  #[serde(default)]
  pub components: Option<Vec<CampusComponent>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusComponent {
  pub uid: String,
  #[serde(default)]
  pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CampusFile {
  pub path: String,
  pub size: u64,
  pub sha1: String,
  #[serde(default)]
  pub env: CampusEnv,
  pub source: CampusSource,
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

impl CampusVersion {
  pub fn needs_bundle(&self) -> bool {
    self
      .manifest
      .files
      .iter()
      .any(|file| file.source.kind == "bundle" && file.env.client != "unsupported")
  }
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

/// Turn a campus version into the platform independent description. Files the
/// client does not use are left out; bundle files are read from `bundle`.
pub fn campus_remote(
  version: &CampusVersion,
  bundle: Option<&Path>,
  api_base: &str,
) -> USTBLResult<RemoteModpack> {
  let loader_type =
    loader_from_slug(&version.loader_type).ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
  let mut files = Vec::new();
  for file in &version.manifest.files {
    if file.env.client == "unsupported" {
      continue;
    }
    let source = match (file.source.kind.as_str(), &file.source.url) {
      ("remote", Some(url)) => FileSource::Download(plain_download(url)?),
      ("blob", _) => FileSource::Download(plain_download(&format!(
        "{api_base}/blobs/{}",
        file.sha1.to_lowercase()
      ))?),
      ("bundle", _) => FileSource::ArchiveEntry {
        archive: bundle
          .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?
          .to_path_buf(),
        entry: file.path.clone(),
      },
      _ => return Err(InstanceError::ModpackUpdateRemoteInvalid.into()),
    };
    files.push(RemoteFile {
      path: normalize_relative_path(&file.path),
      key: FileKey {
        sha1: file.sha1.to_lowercase(),
        size: file.size,
      },
      source,
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
    components: version.manifest.components.as_ref().map(|chain| {
      canonical_components(chain.iter().map(|component| ComponentRef {
        uid: component.uid.clone(),
        version: component.version.clone(),
      }))
    }),
    files,
  })
}

// ---- archives ---------------------------------------------------------------

/// SHA-1 and size of one archive entry, streamed.
pub fn hash_archive_entry(archive: &Path, entry: &str) -> USTBLResult<FileKey> {
  let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
  let mut source = zip.by_name(entry)?;
  let mut hasher = Sha1::new();
  let mut buffer = vec![0u8; 64 * 1024];
  let mut size = 0u64;
  loop {
    let read = source.read(&mut buffer)?;
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
  if game_version != origin.game_version || loader_type != origin.loader_type {
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
        })
      })
      .collect()
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))??;

  Ok(RemoteModpack {
    version_id: latest.version_id.clone(),
    label: latest.label.clone(),
    game_version,
    loader: BaselineLoader {
      loader_type,
      version: loader_version,
    },
    components: None,
    files: merge_files(downloads, overrides),
  })
}

// ---- network ----------------------------------------------------------------

fn remote_dir(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join("remote")
}

/// Run one download to completion. Used for archives and staged files, which
/// are needed right away rather than queued as a background group.
pub async fn download_param(app: &AppHandle, mut param: DownloadParam) -> USTBLResult<()> {
  use crate::tasks::{download::DownloadTask, monitor::TaskMonitor};
  use std::{pin::Pin, time::Duration};

  if param.filename.is_none() {
    param.filename = param
      .dest
      .file_name()
      .map(|name| name.to_string_lossy().into_owned());
  }
  if let Some(parent) = param.dest.parent() {
    fs::create_dir_all(parent)?;
  }
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
  future.await
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
  let client = app.state::<reqwest::Client>();
  let response = client.get(format!("{CAMPUS_API}{path}")).send().await?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    return Ok(None);
  }
  Ok(Some(response.error_for_status()?.json().await?))
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
      let Some(version) = campus_get::<CampusVersion>(app, &path).await? else {
        return Ok(None);
      };
      if version.id.to_string() == origin.version_id {
        return Ok(None);
      }
      Ok(Some(LatestVersion {
        version_id: version.id.to_string(),
        label: version.version_number.clone(),
        payload: Payload::Campus(Box::new(version)),
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
        payload: Payload::Archive {
          url: item.download_url,
          sha1: item.sha1.to_lowercase(),
        },
      }))
    }
  }
}

/// Fetch whatever the new version needs to be described file by file. Archives
/// are cached under the update folder so applying right after planning does
/// not download them twice.
pub async fn resolve_remote(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  instance_name: &str,
  latest: &LatestVersion,
) -> USTBLResult<RemoteModpack> {
  let dir = remote_dir(root);
  fs::create_dir_all(&dir)?;
  let remote = match &latest.payload {
    Payload::Archive { url, sha1 } => {
      let path = dir.join(format!("{sha1}.zip"));
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
      remote_from_archive(app, &path, root, instance_name, origin, latest).await?
    }
    Payload::Campus(version) => {
      let bundle = if version.needs_bundle() {
        let path = dir.join(format!("bundle-{}.zip", version.id));
        download_url(
          app,
          &format!("{CAMPUS_API}/versions/{}/bundle", version.id),
          &path,
          None,
        )
        .await?;
        Some(path)
      } else {
        None
      };
      campus_remote(version, bundle.as_deref(), CAMPUS_API)?
    }
  };
  if remote.game_version != origin.game_version || remote.loader.loader_type != origin.loader_type {
    return Err(InstanceError::ModpackUpdateLineMismatch.into());
  }
  remote.validate(instance_name)?;
  Ok(remote)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

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
      "manifest": { "format": "ustbl.modpack/1", "files": files },
    }))
    .unwrap()
  }

  #[test]
  fn campus_versions_map_every_source_kind() {
    let sha = "a".repeat(40);
    let version = campus(serde_json::json!([
      {"path": "mods/r.jar", "size": 1, "sha1": sha, "source": {"kind": "remote", "url": "https://cdn.example/r.jar"}},
      {"path": "mods/b.jar", "size": 2, "sha1": sha.to_uppercase(), "source": {"kind": "blob"}},
      {"path": "config/c.toml", "size": 3, "sha1": sha, "source": {"kind": "bundle"}},
      {"path": "mods/server.jar", "size": 4, "sha1": sha, "env": {"client": "unsupported", "server": "required"}, "source": {"kind": "blob"}},
    ]));
    assert!(version.needs_bundle());

    let remote = campus_remote(
      &version,
      Some(Path::new("bundle.zip")),
      "https://api.example",
    )
    .unwrap();

    assert_eq!(remote.version_id, "7");
    assert_eq!(remote.label, "1.2.0");
    assert_eq!(remote.loader.loader_type, ModLoaderType::NeoForge);
    assert_eq!(remote.loader.version, "21.1.5");
    let paths: Vec<_> = remote.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["mods/r.jar", "mods/b.jar", "config/c.toml"]);
    assert_eq!(remote.files[1].key.sha1, sha);
    match &remote.files[0].source {
      FileSource::Download(param) => assert_eq!(param.src.as_str(), "https://cdn.example/r.jar"),
      other => panic!("{other:?}"),
    }
    match &remote.files[1].source {
      FileSource::Download(param) => {
        assert_eq!(
          param.src.as_str(),
          &format!("https://api.example/blobs/{sha}")
        )
      }
      other => panic!("{other:?}"),
    }
    match &remote.files[2].source {
      FileSource::ArchiveEntry { archive, entry } => {
        assert_eq!(archive, Path::new("bundle.zip"));
        assert_eq!(entry, "config/c.toml");
      }
      other => panic!("{other:?}"),
    }
  }

  #[test]
  fn campus_component_chain_is_read_in_canonical_order() {
    let mut version = campus(serde_json::json!([]));
    assert!(campus_remote(&version, None, "https://api.example")
      .unwrap()
      .components
      .is_none());

    version.manifest.components = serde_json::from_value(serde_json::json!([
      {"uid": "net.minecraftforge", "version": "10.13.4.1614", "cachedName": "Forge"},
      {"uid": "net.minecraft", "version": "1.7.10"},
    ]))
    .unwrap();
    let chain = campus_remote(&version, None, "https://api.example")
      .unwrap()
      .components
      .unwrap();
    let uids: Vec<_> = chain.iter().map(|c| c.uid.as_str()).collect();
    assert_eq!(uids, ["net.minecraft", "net.minecraftforge"]);
    assert_eq!(chain[1].version, "10.13.4.1614");
  }

  #[test]
  fn campus_bundle_files_without_a_bundle_and_unknown_loaders_are_invalid() {
    let sha = "a".repeat(40);
    let bundled = campus(serde_json::json!([
      {"path": "a", "size": 1, "sha1": sha, "source": {"kind": "bundle"}},
    ]));
    assert!(campus_remote(&bundled, None, "https://api.example").is_err());

    let mut odd = campus(serde_json::json!([]));
    odd.loader_type = "quilt".into();
    assert!(campus_remote(&odd, None, "https://api.example").is_err());
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
