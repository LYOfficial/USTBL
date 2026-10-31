use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::future::Future;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use zip::ZipArchive;

use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::curseforge_cache::{
  cache_path, is_cacheable, FileMetaCache, MAX_ENTRIES,
};
use crate::instance::helpers::modpack::misc::{ModpackDownload, ModpackManifest, ModpackMetaInfo};
use crate::instance::models::misc::{InstanceError, ModLoader, ModLoaderType};
use crate::launcher_config::models::LauncherConfig;
use crate::resource::helpers::curseforge::misc::has_official_api_key;
use crate::resource::helpers::mcim::{
  acquire_mcim_api_permit_for, curseforge_api_base, get_content_source_priority_list, ContentSource,
};
use crate::resource::models::OtherResourceSource;
use crate::tasks::download::DownloadParam;
use crate::utils::web::ApiClient;
use std::sync::Mutex;

lazy_static! {
  static ref CURSEFORGE_API_KEY: String = env::var("USTBL_CURSEFORGE_API_KEY").unwrap_or_default();
}

/// Source order for CurseForge metadata lookups, following the resource download
/// strategy.
///
/// Without an API key the official host only answers `403`, so the MCIM mirror is
/// the source that actually keeps CurseForge modpacks installable.
fn curseforge_content_sources(app: &AppHandle) -> Vec<ContentSource> {
  app
    .state::<Mutex<LauncherConfig>>()
    .lock()
    .map(|config| get_content_source_priority_list(&config))
    .unwrap_or_else(|_| vec![ContentSource::Mcim, ContentSource::Official])
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeModLoader {
  pub id: String,
  pub primary: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFiles {
  #[serde(rename = "projectID")]
  pub project_id: u64,
  #[serde(rename = "fileID")]
  pub file_id: u64,
  pub required: bool,
}

structstruck::strike! {
#[strikethrough[derive(Deserialize, Serialize, Debug, Clone)]]
#[strikethrough[serde(rename_all = "camelCase")]]
  pub struct CurseForgeManifest {
    pub name: String,
    pub version: Option<String>,
    pub author: String,
    pub overrides: String,
    pub minecraft: struct {
      pub version: String,
      pub mod_loaders: Vec<CurseForgeModLoader>,
    },
    pub files: Vec<CurseForgeFiles>,
  }
}

/// One file of `POST /mods/files`, or of `GET /mods/{id}/files/{fileId}`.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFileData {
  #[serde(default)]
  pub id: u64,
  #[serde(default)]
  pub mod_id: u64,
  pub download_url: Option<String>,
  pub file_name: String,
  pub file_length: Option<u64>,
  pub hashes: Option<Vec<CurseForgeFileHash>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CurseForgeFileHash {
  pub value: String,
  pub algo: u64,
}

#[derive(Deserialize, Debug, Clone)]
struct CurseForgeDataRes<T> {
  data: T,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeProject {
  pub id: u64,
  pub class_id: Option<i32>,
}

/// The part of a file's metadata that a modpack install needs.
///
/// `download_url` is kept as the API returned it (`None` when the author
/// disabled third-party distribution), so the value does not depend on which
/// source answered.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeFileMeta {
  pub project_id: u64,
  pub class_id: Option<i32>,
  pub download_url: Option<String>,
  pub file_name: String,
  pub file_length: Option<u64>,
  pub sha1: Option<String>,
}

/// Maximum number of ids sent in one `POST /mods` or `POST /mods/files`.
const BATCH_SIZE: usize = 50;
/// Concurrent single-id requests when a batch left some ids out.
const SINGLE_FALLBACK_CONCURRENCY: usize = 4;
/// Pause before asking a batch again. The mirror answers a project it has not
/// cached yet with a miss and fetches it in the background.
#[cfg(not(test))]
const BATCH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
#[cfg(test)]
const BATCH_RETRY_DELAY: std::time::Duration = std::time::Duration::ZERO;

/// Metadata lookups against the CurseForge API, split out so the batching and
/// fallback logic can be tested without a network.
#[async_trait]
trait MetadataClient: Send + Sync {
  /// `POST /mods`. Ids the server does not know are silently left out.
  async fn mods(&self, ids: &[u64]) -> USTBLResult<Vec<CurseForgeProject>>;
  /// `POST /mods/files`. Ids the server does not know are silently left out.
  async fn files(&self, ids: &[u64]) -> USTBLResult<Vec<CurseForgeFileData>>;
  /// `GET /mods/{id}`.
  async fn mod_by_id(&self, id: u64) -> USTBLResult<CurseForgeProject>;
  /// `GET /mods/{id}/files/{fileId}`.
  async fn file_by_id(&self, project_id: u64, file_id: u64) -> USTBLResult<CurseForgeFileData>;
}

/// Sends metadata requests to the configured sources in order.
struct HttpMetadataClient {
  client: reqwest::Client,
  priority: Vec<ContentSource>,
  requests: AtomicUsize,
}

impl HttpMetadataClient {
  fn new(client: reqwest::Client, priority: Vec<ContentSource>) -> Self {
    Self {
      client,
      priority,
      requests: AtomicUsize::new(0),
    }
  }

  /// Number of HTTP requests sent so far, failed attempts included.
  fn request_count(&self) -> usize {
    self.requests.load(Ordering::Relaxed)
  }

  /// Fetch a CurseForge API document, trying the configured sources in order.
  /// A JSON `body` makes it a `POST`.
  async fn fetch<T>(&self, endpoint: &str, body: Option<&serde_json::Value>) -> USTBLResult<T>
  where
    T: serde::de::DeserializeOwned,
  {
    let mut last_error: Option<USTBLError> = None;

    for source in &self.priority {
      // The key is only meaningful on the official host, and is never sent to the
      // mirror.
      if *source == ContentSource::Official && !has_official_api_key() {
        continue;
      }

      let url = format!("{}{}", curseforge_api_base(*source), endpoint);
      let request = match body {
        Some(body) => self.client.post(&url).json(body),
        None => self.client.get(&url),
      }
      .header("accept", "application/json");
      let request = match source {
        ContentSource::Official => request.header("x-api-key", CURSEFORGE_API_KEY.as_str()),
        ContentSource::Mcim => request,
      };

      // Mirror metadata shares the global budget so that installing a modpack does
      // not flood the mirror with parallel file lookups.
      let _permit = acquire_mcim_api_permit_for(&url).await;

      self.requests.fetch_add(1, Ordering::Relaxed);
      match request.send().await {
        Ok(response) if response.status().is_success() => match response.json::<T>().await {
          Ok(value) => return Ok(value),
          Err(error) => {
            eprintln!("{:?}", error);
            last_error = Some(USTBLError(format!("{:?}", error)));
          }
        },
        Ok(response) => {
          log::warn!(
            "CurseForge request to {} failed with status {}",
            url,
            response.status()
          );
          last_error = Some(InstanceError::NetworkError.into());
        }
        Err(error) => {
          log::warn!("CurseForge request to {} failed: {:?}", url, error);
          last_error = Some(InstanceError::NetworkError.into());
        }
      }
    }

    Err(last_error.unwrap_or_else(|| InstanceError::NetworkError.into()))
  }
}

#[async_trait]
impl MetadataClient for HttpMetadataClient {
  async fn mods(&self, ids: &[u64]) -> USTBLResult<Vec<CurseForgeProject>> {
    let body = serde_json::json!({ "modIds": ids, "filterPcOnly": false });
    let res: CurseForgeDataRes<Vec<CurseForgeProject>> = self.fetch("/mods", Some(&body)).await?;
    Ok(res.data)
  }

  async fn files(&self, ids: &[u64]) -> USTBLResult<Vec<CurseForgeFileData>> {
    let body = serde_json::json!({ "fileIds": ids });
    let res: CurseForgeDataRes<Vec<CurseForgeFileData>> =
      self.fetch("/mods/files", Some(&body)).await?;
    Ok(res.data)
  }

  async fn mod_by_id(&self, id: u64) -> USTBLResult<CurseForgeProject> {
    let res: CurseForgeDataRes<CurseForgeProject> =
      self.fetch(&format!("/mods/{id}"), None).await?;
    Ok(res.data)
  }

  async fn file_by_id(&self, project_id: u64, file_id: u64) -> USTBLResult<CurseForgeFileData> {
    let res: CurseForgeDataRes<CurseForgeFileData> = self
      .fetch(&format!("/mods/{project_id}/files/{file_id}"), None)
      .await?;
    Ok(res.data)
  }
}

/// Look up `ids` in batches of [`BATCH_SIZE`].
///
/// The batch endpoints leave unknown ids out instead of failing, so whatever is
/// still missing is asked for in a second batch, then one by one. A batch that
/// fails outright counts as all of its ids missing. The one-by-one step stops
/// at the first error, which keeps an unreachable server from costing one
/// failed request per id.
async fn lookup_in_batches<T, B, BF, S, SF>(
  ids: &[u64],
  id_of: fn(&T) -> u64,
  batch: B,
  single: S,
) -> USTBLResult<HashMap<u64, T>>
where
  B: Fn(Vec<u64>) -> BF,
  BF: Future<Output = USTBLResult<Vec<T>>>,
  S: Fn(u64) -> SF,
  SF: Future<Output = USTBLResult<T>>,
{
  let mut found: HashMap<u64, T> = HashMap::new();
  let mut pending: Vec<u64> = Vec::new();
  for id in ids {
    if !pending.contains(id) {
      pending.push(*id);
    }
  }

  for attempt in 0..2 {
    if pending.is_empty() {
      break;
    }
    if attempt > 0 {
      tokio::time::sleep(BATCH_RETRY_DELAY).await;
    }

    let results = futures::future::join_all(
      pending
        .chunks(BATCH_SIZE)
        .map(|chunk| batch(chunk.to_vec())),
    )
    .await;
    for result in results {
      match result {
        Ok(items) => {
          for item in items {
            let id = id_of(&item);
            if pending.contains(&id) {
              found.insert(id, item);
            }
          }
        }
        Err(error) => log::warn!("CurseForge batch lookup failed: {:?}", error),
      }
    }
    pending.retain(|id| !found.contains_key(id));
  }

  if !pending.is_empty() {
    log::info!(
      "CurseForge batch lookup left {} ids out, asking one by one",
      pending.len()
    );
    let singles: Vec<(u64, T)> = futures::stream::iter(pending)
      .map(|id| {
        let lookup = single(id);
        async move { lookup.await.map(|item| (id, item)) }
      })
      .buffer_unordered(SINGLE_FALLBACK_CONCURRENCY)
      .try_collect()
      .await?;
    found.extend(singles);
  }

  Ok(found)
}

/// Fetch the metadata of every file in `files` with batched requests, keyed by
/// file id.
async fn fetch_file_metas(
  client: &dyn MetadataClient,
  files: &[CurseForgeFiles],
) -> USTBLResult<HashMap<u64, CurseForgeFileMeta>> {
  if files.is_empty() {
    return Ok(HashMap::new());
  }

  let project_of: HashMap<u64, u64> = files.iter().map(|f| (f.file_id, f.project_id)).collect();
  let file_ids: Vec<u64> = files.iter().map(|f| f.file_id).collect();
  let project_ids: Vec<u64> = files.iter().map(|f| f.project_id).collect();

  let (file_data, projects) = tokio::try_join!(
    lookup_in_batches(
      &file_ids,
      |file: &CurseForgeFileData| file.id,
      |ids| async move { client.files(&ids).await },
      |id| {
        let project_id = project_of[&id];
        async move {
          let mut file = client.file_by_id(project_id, id).await?;
          file.id = id;
          file.mod_id = project_id;
          Ok(file)
        }
      },
    ),
    lookup_in_batches(
      &project_ids,
      |project: &CurseForgeProject| project.id,
      |ids| async move { client.mods(&ids).await },
      |id| async move { client.mod_by_id(id).await },
    ),
  )?;

  let mut metas = HashMap::with_capacity(file_data.len());
  for (file_id, file) in file_data {
    let project_id = project_of[&file_id];
    let sha1 = file
      .hashes
      .as_ref()
      .and_then(|hashes| hashes.iter().find(|hash| hash.algo == 1))
      .map(|hash| hash.value.clone());
    metas.insert(
      file_id,
      CurseForgeFileMeta {
        project_id,
        class_id: projects
          .get(&project_id)
          .and_then(|project| project.class_id),
        download_url: file.download_url.filter(|url| !url.is_empty()),
        file_name: file.file_name,
        file_length: file.file_length,
        sha1,
      },
    );
  }
  Ok(metas)
}

/// Serializes cache writes, so two installs finishing together cannot drop each
/// other's entries.
static CACHE_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Metadata for a modpack's files, plus how many came from the cache.
struct ResolvedMetas {
  metas: HashMap<u64, CurseForgeFileMeta>,
  cache_hits: usize,
}

/// Like [`fetch_file_metas`], but files already in the persistent cache at
/// `cache_path` are not requested again, and complete answers for the others
/// are stored. Cache problems never fail the lookup.
async fn fetch_file_metas_cached(
  client: &dyn MetadataClient,
  files: &[CurseForgeFiles],
  cache_path: Option<&Path>,
) -> USTBLResult<ResolvedMetas> {
  let Some(cache_path) = cache_path else {
    return Ok(ResolvedMetas {
      metas: fetch_file_metas(client, files).await?,
      cache_hits: 0,
    });
  };

  let cache = {
    let path = cache_path.to_path_buf();
    tokio::task::spawn_blocking(move || FileMetaCache::load(&path))
      .await
      .unwrap_or_default()
  };

  let mut metas: HashMap<u64, CurseForgeFileMeta> = HashMap::new();
  let mut misses: Vec<CurseForgeFiles> = Vec::new();
  for file in files {
    if metas.contains_key(&file.file_id) {
      continue;
    }
    match cache.get(file.file_id, file.project_id) {
      Some(meta) => {
        metas.insert(file.file_id, meta);
      }
      None => misses.push(file.clone()),
    }
  }
  let hit_ids: Vec<u64> = metas.keys().copied().collect();
  let cache_hits = hit_ids.len();

  if !misses.is_empty() {
    let fetched = fetch_file_metas(client, &misses).await?;
    let storable: Vec<(u64, CurseForgeFileMeta)> = fetched
      .iter()
      .filter(|(_, meta)| is_cacheable(meta))
      .map(|(id, meta)| (*id, meta.clone()))
      .collect();
    metas.extend(fetched);

    if !storable.is_empty() {
      let path = cache_path.to_path_buf();
      let _guard = CACHE_WRITE_LOCK.lock().await;
      let saved = tokio::task::spawn_blocking(move || {
        // Reload under the lock to keep what other installs stored meanwhile.
        let mut cache = FileMetaCache::load(&path);
        for (id, meta) in storable {
          cache.insert(id, meta);
        }
        for id in hit_ids {
          cache.touch(id);
        }
        cache.save(&path, MAX_ENTRIES)
      })
      .await;
      match saved {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log::warn!("Failed to store CurseForge metadata cache: {error}"),
        Err(error) => log::warn!("Failed to store CurseForge metadata cache: {error}"),
      }
    }
  }

  Ok(ResolvedMetas { metas, cache_hits })
}
/// Where a file of the given CurseForge class belongs in the instance.
fn target_folder(class_id: Option<i32>) -> &'static str {
  match class_id {
    Some(12) | Some(6945) => "resourcepacks",
    Some(6552) => "shaderpacks",
    _ => "mods",
  }
}

fn build_download(
  instance_path: &Path,
  file_id: u64,
  meta: &CurseForgeFileMeta,
) -> USTBLResult<ModpackDownload> {
  let download_url = meta.download_url.clone().unwrap_or_else(|| {
    format!(
      "https://edge.forgecdn.net/files/{}/{}/{}",
      file_id / 1000,
      file_id % 1000,
      urlencoding::encode(&meta.file_name)
    )
  });

  Ok(ModpackDownload {
    param: DownloadParam {
      src: url::Url::parse(&download_url).map_err(|_| InstanceError::InvalidSourcePath)?,
      sha1: meta.sha1.clone(),
      dest: instance_path
        .join(target_folder(meta.class_id))
        .join(&meta.file_name),
      filename: Some(meta.file_name.clone()),
      custom_headers: None,
      transfer_options: Default::default(),
    },
    size: meta.file_length,
  })
}

#[async_trait]
impl ModpackManifest for CurseForgeManifest {
  fn from_archive(file: &File) -> USTBLResult<Self> {
    let mut archive = ZipArchive::new(file)?;
    let mut manifest_file = archive.by_name("manifest.json")?;
    let mut manifest_content = String::new();
    manifest_file.read_to_string(&mut manifest_content)?;
    let manifest: Self = serde_json::from_str(&manifest_content).inspect_err(|e| {
      eprintln!("{:?}", e);
    })?;

    Ok(manifest)
  }

  async fn get_meta_info(&self, app: &AppHandle) -> USTBLResult<ModpackMetaInfo> {
    let client_version = self.get_client_version()?;
    let mod_loader = if let Ok((loader_type, version)) = self.get_mod_loader_type_version() {
      Some(
        ModLoader {
          loader_type,
          version,
          ..Default::default()
        }
        .with_branch(app, client_version.clone())
        .await?,
      )
    } else {
      None
    };
    Ok(ModpackMetaInfo {
      name: self.name.clone(),
      version: self.version.clone(),
      description: None,
      author: Some(self.author.clone()),
      modpack_source: OtherResourceSource::CurseForge,
      client_version,
      mod_loader,
    })
  }

  fn get_client_version(&self) -> USTBLResult<String> {
    Ok(self.minecraft.version.clone())
  }

  fn get_mod_loader_type_version(&self) -> USTBLResult<(ModLoaderType, String)> {
    let loader = self
      .minecraft
      .mod_loaders
      .iter()
      .find(|l| l.primary)
      .ok_or(InstanceError::ModLoaderVersionParseError)?;

    let Some((loader_type, version)) = loader.id.split_once('-') else {
      return Err(InstanceError::ModLoaderVersionParseError.into());
    };
    Ok((
      ModLoaderType::from_str(loader_type)
        .ok()
        .ok_or(InstanceError::ModLoaderVersionParseError)?,
      version.to_string(),
    ))
  }

  async fn get_download_params(
    &self,
    app: &AppHandle,
    instance_path: &Path,
  ) -> USTBLResult<Vec<ModpackDownload>> {
    let started = Instant::now();
    let client = HttpMetadataClient::new(
      app.state::<ApiClient>().inner().0.clone(),
      curseforge_content_sources(app),
    );

    let store_path = crate::APP_DATA_DIR.get().map(|dir| cache_path(dir));
    let ResolvedMetas { metas, cache_hits } =
      fetch_file_metas_cached(&client, &self.files, store_path.as_deref()).await?;

    let mut downloads = Vec::with_capacity(self.files.len());
    for file in &self.files {
      let meta = metas
        .get(&file.file_id)
        .ok_or(InstanceError::NetworkError)?;
      downloads.push(build_download(instance_path, file.file_id, meta)?);
    }

    log::info!(
      "CurseForge modpack metadata: {} files, {} from cache, {} requests, {} ms",
      self.files.len(),
      cache_hits,
      client.request_count(),
      started.elapsed().as_millis()
    );
    Ok(downloads)
  }
  fn get_overrides_paths(&self) -> Vec<String> {
    vec![self.overrides.clone()]
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex as StdMutex;

  /// A fake CurseForge that knows a fixed set of files, and can be told to
  /// leave ids out of the first N batch answers or to fail.
  #[derive(Default)]
  struct FakeClient {
    /// file id -> (project id, class id)
    files: HashMap<u64, (u64, Option<i32>)>,
    /// Ids that `files`/`mods` batches omit until this many batch calls were made.
    hidden_until_call: HashMap<u64, usize>,
    /// Ids the single endpoints do not know either.
    unknown: Vec<u64>,
    fail_everything: bool,
    batch_sizes: StdMutex<Vec<usize>>,
    batch_calls: AtomicUsize,
    single_calls: AtomicUsize,
  }

  impl FakeClient {
    fn with_files(entries: &[(u64, u64, Option<i32>)]) -> Self {
      Self {
        files: entries
          .iter()
          .map(|(file, project, class)| (*file, (*project, *class)))
          .collect(),
        ..Default::default()
      }
    }

    fn file_data(&self, file_id: u64) -> CurseForgeFileData {
      let (project, _) = self.files[&file_id];
      CurseForgeFileData {
        id: file_id,
        mod_id: project,
        download_url: Some(format!("https://edge.forgecdn.net/files/{file_id}/a.jar")),
        file_name: format!("{file_id}.jar"),
        file_length: Some(file_id * 10),
        hashes: Some(vec![
          CurseForgeFileHash {
            value: "md5".into(),
            algo: 2,
          },
          CurseForgeFileHash {
            value: format!("sha1-{file_id}"),
            algo: 1,
          },
        ]),
      }
    }

    fn hidden(&self, id: u64, call: usize) -> bool {
      self
        .hidden_until_call
        .get(&id)
        .is_some_and(|until| call < *until)
    }
  }

  #[async_trait]
  impl MetadataClient for FakeClient {
    async fn mods(&self, ids: &[u64]) -> USTBLResult<Vec<CurseForgeProject>> {
      let call = self.batch_calls.fetch_add(1, Ordering::SeqCst);
      self.batch_sizes.lock().unwrap().push(ids.len());
      if self.fail_everything {
        return Err(InstanceError::NetworkError.into());
      }
      Ok(
        ids
          .iter()
          .filter(|id| !self.hidden(**id, call))
          .filter_map(|id| {
            self
              .files
              .values()
              .find(|(project, _)| project == id)
              .map(|(project, class)| CurseForgeProject {
                id: *project,
                class_id: *class,
              })
          })
          .collect(),
      )
    }

    async fn files(&self, ids: &[u64]) -> USTBLResult<Vec<CurseForgeFileData>> {
      let call = self.batch_calls.fetch_add(1, Ordering::SeqCst);
      self.batch_sizes.lock().unwrap().push(ids.len());
      if self.fail_everything {
        return Err(InstanceError::NetworkError.into());
      }
      Ok(
        ids
          .iter()
          .filter(|id| self.files.contains_key(id) && !self.hidden(**id, call))
          .map(|id| self.file_data(*id))
          .collect(),
      )
    }

    async fn mod_by_id(&self, id: u64) -> USTBLResult<CurseForgeProject> {
      self.single_calls.fetch_add(1, Ordering::SeqCst);
      if self.fail_everything || self.unknown.contains(&id) {
        return Err(InstanceError::NetworkError.into());
      }
      self
        .files
        .values()
        .find(|(project, _)| *project == id)
        .map(|(project, class)| CurseForgeProject {
          id: *project,
          class_id: *class,
        })
        .ok_or_else(|| InstanceError::NetworkError.into())
    }

    async fn file_by_id(&self, _project_id: u64, file_id: u64) -> USTBLResult<CurseForgeFileData> {
      self.single_calls.fetch_add(1, Ordering::SeqCst);
      if self.fail_everything
        || self.unknown.contains(&file_id)
        || !self.files.contains_key(&file_id)
      {
        return Err(InstanceError::NetworkError.into());
      }
      let mut data = self.file_data(file_id);
      // The single endpoint's answer is not relied on to carry the ids.
      data.id = 0;
      data.mod_id = 0;
      Ok(data)
    }
  }

  fn manifest_files(entries: &[(u64, u64)]) -> Vec<CurseForgeFiles> {
    entries
      .iter()
      .map(|(project_id, file_id)| CurseForgeFiles {
        project_id: *project_id,
        file_id: *file_id,
        required: true,
      })
      .collect()
  }

  #[test]
  fn parses_batch_responses() {
    let files: CurseForgeDataRes<Vec<CurseForgeFileData>> = serde_json::from_str(
      r#"{"data":[{"id":5000000,"modId":951084,"fileName":"a.zip","fileLength":1466420,
      "downloadUrl":"https://edge.forgecdn.net/files/5000/0/a.zip","displayName":"x",
      "hashes":[{"value":"4f6c","algo":1},{"value":"c78c","algo":2}]},
      {"id":7,"modId":8,"fileName":"b.jar","downloadUrl":null,"hashes":[]}]}"#,
    )
    .unwrap();
    assert_eq!(files.data.len(), 2);
    assert_eq!(files.data[0].id, 5000000);
    assert_eq!(files.data[0].mod_id, 951084);
    assert_eq!(files.data[0].hashes.as_ref().unwrap()[0].value, "4f6c");
    assert_eq!(files.data[1].download_url, None);
    assert_eq!(files.data[1].file_length, None);

    let mods: CurseForgeDataRes<Vec<CurseForgeProject>> =
      serde_json::from_str(r#"{"data":[{"id":238222,"classId":6,"slug":"jei"},{"id":1}]}"#)
        .unwrap();
    assert_eq!(mods.data[0].class_id, Some(6));
    assert_eq!(mods.data[1].class_id, None);
  }

  #[tokio::test]
  async fn resolves_files_in_batches_of_50() {
    let entries: Vec<(u64, u64, Option<i32>)> =
      (1..=430).map(|i| (i, i + 10_000, Some(6))).collect();
    let client = FakeClient::with_files(&entries);
    let manifest: Vec<(u64, u64)> = (1..=430).map(|i| (i + 10_000, i)).collect();

    let metas = fetch_file_metas(&client, &manifest_files(&manifest))
      .await
      .unwrap();

    assert_eq!(metas.len(), 430);
    assert_eq!(metas[&7].project_id, 10_007);
    assert_eq!(metas[&7].sha1.as_deref(), Some("sha1-7"));
    assert_eq!(metas[&7].class_id, Some(6));
    assert_eq!(metas[&7].file_length, Some(70));
    assert_eq!(client.single_calls.load(Ordering::SeqCst), 0);
    let mut sizes = client.batch_sizes.lock().unwrap().clone();
    sizes.sort();
    // 430 files and 430 projects, each as 8 x 50 + 30.
    let mut expected = vec![30, 30];
    expected.extend([50; 16]);
    assert_eq!(sizes, expected);
  }

  #[tokio::test]
  async fn an_id_the_first_batch_omits_is_found_by_the_second() {
    let mut client = FakeClient::with_files(&[(1, 100, Some(6)), (2, 200, Some(12))]);
    client.hidden_until_call.insert(2, 2);

    let metas = fetch_file_metas(&client, &manifest_files(&[(100, 1), (200, 2)]))
      .await
      .unwrap();

    assert_eq!(metas.len(), 2);
    assert_eq!(client.single_calls.load(Ordering::SeqCst), 0);
  }

  #[tokio::test]
  async fn an_id_both_batches_omit_falls_back_to_a_single_request() {
    let mut client = FakeClient::with_files(&[(1, 100, Some(6)), (2, 200, Some(12))]);
    client.hidden_until_call.insert(2, usize::MAX);
    client.hidden_until_call.insert(200, usize::MAX);

    let metas = fetch_file_metas(&client, &manifest_files(&[(100, 1), (200, 2)]))
      .await
      .unwrap();

    // One single request for the file and one for its project.
    assert_eq!(client.single_calls.load(Ordering::SeqCst), 2);
    assert_eq!(metas[&2].file_name, "2.jar");
    assert_eq!(metas[&2].project_id, 200);
    assert_eq!(metas[&2].class_id, Some(12));
  }

  #[tokio::test]
  async fn an_id_nobody_knows_is_an_error() {
    let mut client = FakeClient::with_files(&[(1, 100, Some(6)), (2, 200, Some(6))]);
    client.hidden_until_call.insert(2, usize::MAX);
    client.unknown.push(2);

    let result = fetch_file_metas(&client, &manifest_files(&[(100, 1), (200, 2)])).await;

    assert!(result.is_err());
  }

  #[tokio::test]
  async fn total_failure_is_an_error_without_one_request_per_file() {
    let entries: Vec<(u64, u64, Option<i32>)> = (1..=300).map(|i| (i, i + 10_000, None)).collect();
    let mut client = FakeClient::with_files(&entries);
    client.fail_everything = true;
    let manifest: Vec<(u64, u64)> = (1..=300).map(|i| (i + 10_000, i)).collect();

    let result = fetch_file_metas(&client, &manifest_files(&manifest)).await;

    assert!(result.is_err());
    let singles = client.single_calls.load(Ordering::SeqCst);
    assert!(
      singles <= SINGLE_FALLBACK_CONCURRENCY,
      "{singles} single requests"
    );
  }

  #[tokio::test]
  async fn no_files_means_no_requests() {
    let client = FakeClient::default();
    let metas = fetch_file_metas(&client, &[]).await.unwrap();
    assert!(metas.is_empty());
    assert_eq!(client.batch_calls.load(Ordering::SeqCst), 0);
  }

  fn meta(class_id: Option<i32>, download_url: Option<&str>) -> CurseForgeFileMeta {
    CurseForgeFileMeta {
      project_id: 1,
      class_id,
      download_url: download_url.map(String::from),
      file_name: "a b.jar".into(),
      file_length: Some(5),
      sha1: Some("abc".into()),
    }
  }

  #[test]
  fn downloads_go_to_the_folder_of_their_class() {
    let root = Path::new("inst");
    for (class_id, folder) in [
      (Some(6), "mods"),
      (None, "mods"),
      (Some(12), "resourcepacks"),
      (Some(6945), "resourcepacks"),
      (Some(6552), "shaderpacks"),
    ] {
      let download =
        build_download(root, 9, &meta(class_id, Some("https://x.test/a.jar"))).unwrap();
      assert_eq!(download.param.dest, root.join(folder).join("a b.jar"));
      assert_eq!(download.param.sha1.as_deref(), Some("abc"));
      assert_eq!(download.size, Some(5));
    }
  }

  #[test]
  fn a_missing_download_url_falls_back_to_the_forge_cdn() {
    let download = build_download(Path::new("inst"), 5_123_456, &meta(None, None)).unwrap();
    assert_eq!(
      download.param.src.as_str(),
      "https://edge.forgecdn.net/files/5123/456/a%20b.jar"
    );
  }

  fn cache_test_path() -> std::path::PathBuf {
    std::env::temp_dir()
      .join(format!("ustbl-cf-meta-{}", uuid::Uuid::new_v4()))
      .join("curseforge_file_meta.json")
  }

  #[tokio::test]
  async fn a_second_lookup_is_served_from_the_cache_without_requests() {
    let path = cache_test_path();
    let client = FakeClient::with_files(&[(1, 100, Some(6)), (2, 200, Some(12))]);
    let files = manifest_files(&[(100, 1), (200, 2)]);

    let first = fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .unwrap();
    assert_eq!(first.cache_hits, 0);
    let calls = client.batch_calls.load(Ordering::SeqCst);
    assert!(calls > 0);

    let second = fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .unwrap();
    assert_eq!(second.cache_hits, 2);
    assert_eq!(client.batch_calls.load(Ordering::SeqCst), calls);
    assert_eq!(second.metas, first.metas);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
  }

  #[tokio::test]
  async fn only_files_missing_from_the_cache_are_requested() {
    let path = cache_test_path();
    let client =
      FakeClient::with_files(&[(1, 100, Some(6)), (2, 200, Some(12)), (3, 300, Some(6))]);

    fetch_file_metas_cached(&client, &manifest_files(&[(100, 1), (200, 2)]), Some(&path))
      .await
      .unwrap();
    client.batch_sizes.lock().unwrap().clear();

    let all = manifest_files(&[(100, 1), (200, 2), (300, 3)]);
    let result = fetch_file_metas_cached(&client, &all, Some(&path))
      .await
      .unwrap();

    assert_eq!(result.cache_hits, 2);
    assert_eq!(result.metas.len(), 3);
    // One file batch and one project batch, one id each.
    assert_eq!(*client.batch_sizes.lock().unwrap(), vec![1, 1]);

    let third = fetch_file_metas_cached(&client, &all, Some(&path))
      .await
      .unwrap();
    assert_eq!(third.cache_hits, 3);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
  }

  #[tokio::test]
  async fn a_corrupt_cache_is_ignored_and_rewritten() {
    let path = cache_test_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"\0\0 definitely not json").unwrap();
    let client = FakeClient::with_files(&[(1, 100, Some(6))]);
    let files = manifest_files(&[(100, 1)]);

    let first = fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .unwrap();
    assert_eq!(first.cache_hits, 0);
    assert_eq!(first.metas.len(), 1);

    let second = fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .unwrap();
    assert_eq!(second.cache_hits, 1);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
  }

  #[tokio::test]
  async fn answers_without_a_class_are_not_cached() {
    let path = cache_test_path();
    let client = FakeClient::with_files(&[(1, 100, None)]);
    let files = manifest_files(&[(100, 1)]);

    fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .unwrap();
    let second = fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .unwrap();

    assert_eq!(second.cache_hits, 0);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
  }

  #[tokio::test]
  async fn a_failed_lookup_stores_nothing() {
    let path = cache_test_path();
    let mut client = FakeClient::with_files(&[(1, 100, Some(6))]);
    client.fail_everything = true;
    let files = manifest_files(&[(100, 1)]);

    assert!(fetch_file_metas_cached(&client, &files, Some(&path))
      .await
      .is_err());
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn without_a_cache_path_everything_is_requested() {
    let client = FakeClient::with_files(&[(1, 100, Some(6))]);
    let files = manifest_files(&[(100, 1)]);

    let result = fetch_file_metas_cached(&client, &files, None)
      .await
      .unwrap();

    assert_eq!(result.cache_hits, 0);
    assert_eq!(result.metas.len(), 1);
  }
}
