//! Publishing an instance to the campus modpack platform.

use super::manifest::{plan_files, LocalFile, PublishPlan};
use crate::account::helpers::vustb;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::hash_file;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::models::misc::{Instance, InstanceError};
use crate::instance::modpack_update::diff::FileKey;
use crate::instance::modpack_update::sources::{loader_slug, CAMPUS_API};
use crate::instance::shared::ensure_minecraft_manager as ensure_manager;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncReadExt;

const PROGRESS_EVENT: &str = "modpack-publish:progress";
const MISSING_BATCH: usize = 5000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct CampusLine {
  pub id: i64,
  pub game_version: String,
  pub loader_type: String,
  pub loader_version: String,
  pub version_number: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct CampusProject {
  pub id: i64,
  pub slug: String,
  pub name: String,
  pub summary: String,
  pub lines: Vec<CampusLine>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishOutcome {
  pub version_id: i64,
  /// Paths left out on purpose (disabled mods, launcher files).
  pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum Stage {
  Hashing,
  Uploading,
  Bundling,
  Publishing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
  instance_id: String,
  stage: Stage,
  done: usize,
  total: usize,
}

fn report(app: &AppHandle, instance_id: &str, stage: Stage, done: usize, total: usize) {
  let _ = app.emit_to(
    "main",
    PROGRESS_EVENT,
    Progress {
      instance_id: instance_id.to_string(),
      stage,
      done,
      total,
    },
  );
}

/// The server explains refusals in `detail`; pass that on instead of a bare
/// status code.
async fn failure(response: reqwest::Response) -> USTBLError {
  let status = response.status();
  let detail = response
    .json::<Value>()
    .await
    .ok()
    .and_then(|body| match body.get("detail") {
      Some(Value::String(text)) => Some(text.clone()),
      Some(other) => other
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string),
      None => None,
    });
  USTBLError(detail.unwrap_or_else(|| format!("HTTP {status}")))
}

async fn authed_json(
  app: &AppHandle,
  method: reqwest::Method,
  path: &str,
  body: Option<Value>,
) -> USTBLResult<reqwest::Response> {
  let url = format!("{CAMPUS_API}{path}");
  vustb::send_authenticated(app, |client, token| {
    let request = client.request(method.clone(), &url).bearer_auth(token);
    match &body {
      Some(body) => request.json(body),
      None => request,
    }
  })
  .await
}

fn file_body(path: &Path) -> io::Result<reqwest::Body> {
  let file = tokio::fs::File::from_std(fs::File::open(path)?);
  let stream = futures::stream::unfold(file, |mut file| async move {
    let mut buffer = vec![0u8; 256 * 1024];
    match file.read(&mut buffer).await {
      Ok(0) => None,
      Ok(read) => {
        buffer.truncate(read);
        Some((Ok::<_, io::Error>(bytes_from(buffer)), file))
      }
      Err(error) => Some((Err(error), file)),
    }
  });
  Ok(reqwest::Body::wrap_stream(stream))
}

fn bytes_from(buffer: Vec<u8>) -> tokio_util::bytes::Bytes {
  tokio_util::bytes::Bytes::from(buffer)
}

async fn put_file(app: &AppHandle, path: &str, file: &Path) -> USTBLResult<reqwest::Response> {
  let url = format!("{CAMPUS_API}{path}");
  let size = tokio::fs::metadata(file).await?.len();
  vustb::send_authenticated(app, |client, token| {
    let request = client
      .put(&url)
      .bearer_auth(token)
      .header("Content-Type", "application/octet-stream")
      .header("Content-Length", size);
    match file_body(file) {
      Ok(body) => request.body(body),
      // An unreadable file surfaces as an empty body the server rejects.
      Err(_) => request,
    }
  })
  .await
}

fn instance_of(app: &AppHandle, instance_id: &str) -> USTBLResult<Instance> {
  let binding = app.state::<Mutex<HashMap<String, Instance>>>();
  let state = binding.lock()?;
  Ok(
    state
      .get(instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .clone(),
  )
}

#[tauri::command]
pub async fn list_campus_modpack_projects(app: AppHandle) -> USTBLResult<Vec<CampusProject>> {
  let client = app.state::<reqwest::Client>();
  let response = client.get(format!("{CAMPUS_API}/projects")).send().await?;
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  Ok(response.json().await?)
}

#[tauri::command]
pub async fn create_campus_modpack_project(
  app: AppHandle,
  slug: String,
  name: String,
  summary: String,
) -> USTBLResult<CampusProject> {
  ensure_manager(&app)?;
  let response = authed_json(
    &app,
    reqwest::Method::POST,
    "/projects",
    Some(json!({ "slug": slug, "name": name, "summary": summary })),
  )
  .await?;
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  Ok(response.json().await?)
}

fn hash_selection(root: &Path, selected: &[String]) -> USTBLResult<Vec<LocalFile>> {
  selected
    .iter()
    .map(|path| {
      safe_relative(path)?;
      let (sha1, size) = hash_file(&root.join(path))?;
      Ok(LocalFile {
        path: path.clone(),
        key: FileKey { sha1, size },
      })
    })
    .collect()
}

/// Zip the files that ride along with the version.
fn write_bundle(root: &Path, files: &[&LocalFile], dest: &Path) -> USTBLResult<()> {
  let mut zip = zip::ZipWriter::new(fs::File::create(dest)?);
  let options =
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
  for file in files {
    zip.start_file(file.path.as_str(), options)?;
    let mut source = fs::File::open(root.join(&file.path))?;
    io::copy(&mut source, &mut zip)?;
  }
  zip.finish()?.flush()?;
  Ok(())
}

struct TempFile(PathBuf);

impl Drop for TempFile {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}

async fn upload_missing_blobs(
  app: &AppHandle,
  instance_id: &str,
  root: &Path,
  plan: &PublishPlan,
) -> USTBLResult<()> {
  let blobs = plan.blobs();
  let hashes: Vec<&String> = blobs.keys().collect();
  let mut missing = Vec::new();
  for batch in hashes.chunks(MISSING_BATCH) {
    let response = authed_json(
      app,
      reqwest::Method::POST,
      "/blobs/missing",
      Some(json!({ "sha1s": batch })),
    )
    .await?;
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    let body: Value = response.json().await?;
    missing.extend(
      body["missing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_string)),
    );
  }
  let total = missing.len();
  report(app, instance_id, Stage::Uploading, 0, total);
  for (index, sha1) in missing.iter().enumerate() {
    let file = blobs
      .get(sha1)
      .ok_or_else(|| USTBLError("平台请求了未知的文件".into()))?;
    let response = put_file(app, &format!("/blobs/{sha1}"), &root.join(&file.path)).await?;
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
    report(app, instance_id, Stage::Uploading, index + 1, total);
  }
  Ok(())
}

/// Publish the selected files of an instance as the new version of its line.
#[tauri::command]
pub async fn publish_campus_modpack(
  app: AppHandle,
  instance_id: String,
  project: String,
  version_number: String,
  changelog: String,
  files: Vec<String>,
) -> USTBLResult<PublishOutcome> {
  ensure_manager(&app)?;
  let instance = instance_of(&app, &instance_id)?;
  let root = instance.version_path.clone();
  let loader_type =
    loader_slug(instance.mod_loader.loader_type).ok_or(InstanceError::ModpackUpdateUnsupported)?;

  report(&app, &instance_id, Stage::Hashing, 0, files.len());
  let selected = files.clone();
  let hash_root = root.clone();
  let hashed = tokio::task::spawn_blocking(move || hash_selection(&hash_root, &selected))
    .await
    .map_err(|e| USTBLError(e.to_string()))??;
  report(&app, &instance_id, Stage::Hashing, files.len(), files.len());
  let plan = plan_files(hashed, &instance.name);
  if plan.files.is_empty() {
    return Err(USTBLError("没有可发布的文件".into()));
  }

  // The published version of this line the new one replaces.
  let line_path = format!(
    "{CAMPUS_API}/projects/{project}/lines/{}/{loader_type}",
    instance.version
  );
  let current = app.state::<reqwest::Client>().get(line_path).send().await?;
  let parent_version_id = match current.status() {
    reqwest::StatusCode::NOT_FOUND => None,
    status if status.is_success() => current.json::<Value>().await?["id"].as_i64(),
    _ => return Err(failure(current).await),
  };

  upload_missing_blobs(&app, &instance_id, &root, &plan).await?;

  let created = authed_json(
    &app,
    reqwest::Method::POST,
    &format!("/projects/{project}/versions"),
    Some(json!({
      "game_version": instance.version,
      "loader_type": loader_type,
      "loader_version": instance.mod_loader.version,
      "version_number": version_number,
      "changelog": changelog,
      "parent_version_id": parent_version_id,
      "manifest": plan.manifest(),
    })),
  )
  .await?;
  if !created.status().is_success() {
    return Err(failure(created).await);
  }
  let version_id = created.json::<Value>().await?["id"]
    .as_i64()
    .ok_or_else(|| USTBLError("平台返回了无法识别的版本".into()))?;

  let result = finish_draft(&app, &instance_id, &root, &plan, version_id).await;
  if result.is_err() {
    // A draft that never published only takes up space on the server.
    let _ = authed_json(
      &app,
      reqwest::Method::DELETE,
      &format!("/versions/{version_id}"),
      None,
    )
    .await;
  }
  result?;
  Ok(PublishOutcome {
    version_id,
    skipped: plan.skipped,
  })
}

async fn finish_draft(
  app: &AppHandle,
  instance_id: &str,
  root: &Path,
  plan: &PublishPlan,
  version_id: i64,
) -> USTBLResult<()> {
  let bundled = plan.bundle_files();
  if !bundled.is_empty() {
    report(app, instance_id, Stage::Bundling, 0, bundled.len());
    let temp =
      TempFile(std::env::temp_dir().join(format!("ustbl-bundle-{}.zip", uuid::Uuid::new_v4())));
    let (zip_root, zip_path) = (root.to_path_buf(), temp.0.clone());
    let entries: Vec<LocalFile> = bundled.into_iter().cloned().collect();
    tokio::task::spawn_blocking(move || {
      let refs: Vec<&LocalFile> = entries.iter().collect();
      write_bundle(&zip_root, &refs, &zip_path)
    })
    .await
    .map_err(|e| USTBLError(e.to_string()))??;
    let response = put_file(app, &format!("/versions/{version_id}/bundle"), &temp.0).await?;
    if !response.status().is_success() {
      return Err(failure(response).await);
    }
  }

  report(app, instance_id, Stage::Publishing, 0, 1);
  let response = authed_json(
    app,
    reqwest::Method::POST,
    &format!("/versions/{version_id}/publish"),
    None,
  )
  .await?;
  if response.status() == reqwest::StatusCode::CONFLICT {
    return Err(InstanceError::ModpackPublishStale.into());
  }
  if !response.status().is_success() {
    return Err(failure(response).await);
  }
  report(app, instance_id, Stage::Publishing, 1, 1);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn instance_dir(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-publish-test-{}", uuid::Uuid::new_v4()));
    for (path, content) in files {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    root
  }

  #[test]
  fn selection_is_hashed_and_cannot_escape_the_instance() {
    let root = instance_dir(&[("config/a.toml", "hello")]);

    let hashed = hash_selection(&root, &["config/a.toml".to_string()]).unwrap();
    assert_eq!(hashed[0].key.size, 5);
    assert_eq!(
      hashed[0].key.sha1,
      "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"
    );

    assert!(hash_selection(&root, &["../outside".to_string()]).is_err());
    assert!(hash_selection(&root, &["missing.txt".to_string()]).is_err());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn bundle_holds_exactly_the_given_files_under_their_paths() {
    let root = instance_dir(&[
      ("config/a.toml", "aaa"),
      ("options.txt", "bbb"),
      ("skip.txt", "ccc"),
    ]);
    let files = hash_selection(
      &root,
      &["config/a.toml".to_string(), "options.txt".to_string()],
    )
    .unwrap();
    let refs: Vec<&LocalFile> = files.iter().collect();
    let dest = root.join("bundle.zip");

    write_bundle(&root, &refs, &dest).unwrap();

    let mut zip = zip::ZipArchive::new(fs::File::open(&dest).unwrap()).unwrap();
    let mut names: Vec<String> = zip.file_names().map(str::to_string).collect();
    names.sort();
    assert_eq!(names, ["config/a.toml", "options.txt"]);
    let mut content = String::new();
    io::Read::read_to_string(&mut zip.by_name("options.txt").unwrap(), &mut content).unwrap();
    assert_eq!(content, "bbb");
    let _ = fs::remove_dir_all(root);
  }
}
