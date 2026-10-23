//! Applying a modpack update to an instance directory.
//!
//! Nothing in the instance is touched until every new file has been fetched
//! into a staging area and verified. Before the first move a journal is
//! written, and everything that is replaced or removed is moved into a backup
//! folder instead of being deleted. If anything fails, or the launcher dies
//! half way, the journal is enough to put every file back.

use super::diff::{Operation, DISABLED_SUFFIX};
use super::packs::PackSession;
use super::remote::{ComponentPack, FileSource, RemoteFile, RemoteModpack, UPDATE_DIR};
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::base_store;
use crate::instance::helpers::modpack::baseline::{
  hash_file, BaselineLoader, ComponentRef, ModpackBaseline, BASELINE_FILE_NAME,
};
use crate::instance::helpers::modpack::multimc::snapshot::{
  COMPONENTS_FILE_NAME, SNAPSHOT_FILE_NAME,
};
use crate::instance::models::misc::{InstanceError, InstanceOrigin};
use crate::tasks::download::DownloadParam;
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const JOURNAL_FILE: &str = "journal.json";
const JOURNAL_SCHEMA: u32 = 1;
const INSTANCE_CONFIG_FILE: &str = "ustblcfg.json";
const FETCH_CONCURRENCY: usize = 8;

fn update_dir(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR)
}

fn staging_dir(root: &Path) -> PathBuf {
  update_dir(root).join("staging")
}

/// Where the files of a component pack are fetched to.
fn components_dir(root: &Path) -> PathBuf {
  update_dir(root).join("components")
}

fn backup_dir(root: &Path) -> PathBuf {
  update_dir(root).join("backup")
}

/// The previous update's backup, kept until the next update replaces it.
const LAST_JOURNAL_NAME: &str = "journal.json";

fn last_backup_dir(root: &Path) -> PathBuf {
  update_dir(root).join("last-backup")
}

fn journal_path(root: &Path) -> PathBuf {
  update_dir(root).join(JOURNAL_FILE)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum JournalState {
  /// Files are being moved; a crash here is undone.
  Committing,
  /// Everything is in place; only cleanup is left.
  Committed,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JournalOp {
  path: String,
  /// A regular file sat at `path` before the update.
  had_original: bool,
  /// `<path>.disabled` existed and is moved aside too.
  moves_disabled: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
  schema: u32,
  state: JournalState,
  from_version: String,
  to_version: String,
  ops: Vec<JournalOp>,
  /// Instance files rewritten by the update, saved under `backup/meta`.
  meta_present: Vec<String>,
  meta_absent: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
  Fetching,
  Applying,
  Finalizing,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
  pub stage: Stage,
  pub done: usize,
  pub total: usize,
}

/// Everything the update needs from the outside world.
#[async_trait]
pub trait UpdateEnv: Send + Sync {
  /// Refuse when the game is starting or running. Called after the update
  /// guard is held, so a launch cannot slip in after the check.
  fn ensure_not_running(&self) -> USTBLResult<()> {
    Ok(())
  }
  /// Download `param.src` to `param.dest`. The engine verifies the result itself.
  async fn fetch(&self, param: DownloadParam) -> USTBLResult<()>;
  /// Switch the instance to another loader version. Runs after the journal is
  /// written, so the instance files it rewrites are restored on failure.
  async fn switch_loader(&self, loader: &BaselineLoader) -> USTBLResult<()>;
  /// Rebuild the component profile (client json, libraries, launch arguments)
  /// from the pack files fetched into `staged`. Same journal guarantee as
  /// `switch_loader`: the instance files it rewrites are restored on failure.
  async fn restore_components(&self, pack: &ComponentPack, staged: &Path) -> USTBLResult<()>;
  /// Point the instance at the new version once its files are in place.
  async fn commit_origin(&self, version_id: &str) -> USTBLResult<()>;
  fn report(&self, progress: Progress);
}

pub struct UpdateRequest {
  pub root: PathBuf,
  pub instance_name: String,
  pub operations: Vec<Operation>,
  pub remote: RemoteModpack,
  pub origin: InstanceOrigin,
  pub previous_loader: BaselineLoader,
  /// Component chain recorded in the new baseline.
  pub components: Option<Vec<ComponentRef>>,
  /// Install the remote version's component profile into an instance that has
  /// none. Takes the place of the loader switch.
  pub restore_components: bool,
  /// Content for automatically merged text files. Other operations fetch the
  /// remote file through its normal source.
  pub merged_contents: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
  Nothing,
  RolledBack,
  CleanedUp,
}

fn active_updates() -> &'static Mutex<HashSet<PathBuf>> {
  static ACTIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
  ACTIVE.get_or_init(Default::default)
}

/// Holds the right to modify one instance; at most one update runs at a time.
pub struct UpdateGuard(PathBuf);

impl UpdateGuard {
  /// Run `action` only while no update holds `root`; the registry stays locked
  /// for the duration, so no update can start in between.
  pub fn while_idle<T>(root: &Path, action: impl FnOnce() -> T) -> USTBLResult<T> {
    let active = active_updates().lock()?;
    if active.contains(root) {
      return Err(InstanceError::ModpackUpdateBusy.into());
    }
    Ok(action())
  }
  pub fn acquire(root: &Path) -> USTBLResult<Self> {
    let mut active = active_updates().lock()?;
    if !active.insert(root.to_path_buf()) {
      return Err(InstanceError::ModpackUpdateBusy.into());
    }
    Ok(Self(root.to_path_buf()))
  }
}

impl Drop for UpdateGuard {
  fn drop(&mut self) {
    if let Ok(mut active) = active_updates().lock() {
      active.remove(&self.0);
    }
  }
}

fn io_error(error: io::Error) -> crate::error::USTBLError {
  crate::error::USTBLError(error.to_string())
}

fn write_journal(root: &Path, journal: &Journal) -> io::Result<()> {
  let path = journal_path(root);
  let temp = path.with_extension("json.tmp");
  fs::write(&temp, serde_json::to_vec_pretty(journal)?)?;
  fs::rename(temp, path)
}

fn read_journal(root: &Path) -> io::Result<Option<Journal>> {
  match fs::read(journal_path(root)) {
    Ok(bytes) => {
      Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, e)
      })?))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
  if let Some(parent) = to.parent() {
    fs::create_dir_all(parent)?;
  }
  fs::rename(from, to)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
  match fs::remove_file(path) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
    _ => Ok(()),
  }
}

fn meta_files(instance_name: &str, restoring_components: bool) -> Vec<String> {
  let mut names = vec![
    BASELINE_FILE_NAME.to_string(),
    INSTANCE_CONFIG_FILE.to_string(),
    format!("{instance_name}.json"),
  ];
  if restoring_components {
    names.extend([
      format!("{instance_name}.jar"),
      COMPONENTS_FILE_NAME.to_string(),
      SNAPSHOT_FILE_NAME.to_string(),
    ]);
  }
  names
}

fn disabled_path(path: &str) -> String {
  format!("{path}{DISABLED_SUFFIX}")
}

/// Undo a journal that never reached `Committed`. Safe to run repeatedly.
fn roll_back(root: &Path, journal: &Journal) -> io::Result<()> {
  roll_back_from(root, &backup_dir(root), journal)
}

fn roll_back_from(root: &Path, backup: &Path, journal: &Journal) -> io::Result<()> {
  for op in journal.ops.iter().rev() {
    let target = root.join(&op.path);
    let saved = backup.join("files").join(&op.path);
    if op.had_original {
      if saved.is_file() {
        remove_file_if_exists(&target)?;
        move_file(&saved, &target)?;
      }
    } else {
      remove_file_if_exists(&target)?;
    }
    if op.moves_disabled {
      let disabled = disabled_path(&op.path);
      let saved = backup.join("files").join(&disabled);
      if saved.is_file() {
        move_file(&saved, &root.join(&disabled))?;
      }
    }
  }
  for name in &journal.meta_present {
    let saved = backup.join("meta").join(name);
    if saved.is_file() {
      fs::copy(&saved, root.join(name))?;
    }
  }
  for name in &journal.meta_absent {
    remove_file_if_exists(&root.join(name))?;
  }
  Ok(())
}

/// Drop the staging area and keep this update's backup for the next one.
fn finish_cleanup(root: &Path, keep_backup: bool) {
  let _ = fs::remove_dir_all(staging_dir(root));
  let _ = fs::remove_dir_all(components_dir(root));
  let _ = fs::remove_dir_all(update_dir(root).join("remote"));
  if keep_backup && backup_dir(root).exists() {
    let _ = fs::remove_dir_all(last_backup_dir(root));
    let _ = fs::rename(backup_dir(root), last_backup_dir(root));
    // The journal says what the backup holds, which rolling back needs.
    let _ = fs::rename(
      journal_path(root),
      last_backup_dir(root).join(LAST_JOURNAL_NAME),
    );
  } else {
    let _ = fs::remove_dir_all(backup_dir(root));
  }
  let _ = fs::remove_file(journal_path(root));
}

/// Whether the previous update can still be rolled back.
pub fn can_roll_back_last(root: &Path) -> bool {
  last_backup_dir(root).join(LAST_JOURNAL_NAME).is_file()
}

/// Put the instance back the way it was before the last update. Files the
/// player changed since then are overwritten, so callers confirm first. The
/// backup is spent afterwards; returns whether there was one.
pub fn roll_back_last(root: &Path) -> USTBLResult<bool> {
  roll_back_last_checked(root, || Ok(()))
}

/// [`roll_back_last`], running `check` once the update guard is held.
pub fn roll_back_last_checked(
  root: &Path,
  check: impl FnOnce() -> USTBLResult<()>,
) -> USTBLResult<bool> {
  let _guard = UpdateGuard::acquire(root)?;
  check()?;
  recover_locked(root)?;
  let backup = last_backup_dir(root);
  let journal: Journal = match fs::read(backup.join(LAST_JOURNAL_NAME)) {
    Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| io_error(e.into()))?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(io_error(error)),
  };
  roll_back_from(root, &backup, &journal).map_err(io_error)?;
  fs::remove_dir_all(&backup).map_err(io_error)?;
  Ok(true)
}

/// Refuse unlinking while an update is mid-flight: the journal means the
/// instance is between two versions and has to be recovered or rolled back
/// first. The guard keeps an update from starting after the check.
pub fn ensure_unlink_safe(root: &Path) -> USTBLResult<()> {
  let _guard = UpdateGuard::acquire(root)?;
  if journal_path(root).exists() {
    return Err(InstanceError::ModpackUpdateUnlinkPending.into());
  }
  Ok(())
}

/// Finish or undo whatever an interrupted update left behind.
pub fn recover(root: &Path) -> USTBLResult<Recovery> {
  recover_checked(root, || Ok(()))
}

/// [`recover`], running `check` once the update guard is held.
pub fn recover_checked(
  root: &Path,
  check: impl FnOnce() -> USTBLResult<()>,
) -> USTBLResult<Recovery> {
  let _guard = UpdateGuard::acquire(root)?;
  check()?;
  recover_locked(root)
}

fn recover_locked(root: &Path) -> USTBLResult<Recovery> {
  let Some(journal) = read_journal(root).map_err(io_error)? else {
    let _ = fs::remove_dir_all(staging_dir(root));
    let _ = fs::remove_dir_all(components_dir(root));
    return Ok(Recovery::Nothing);
  };
  match journal.state {
    JournalState::Committing => {
      roll_back(root, &journal).map_err(io_error)?;
      finish_cleanup(root, false);
      Ok(Recovery::RolledBack)
    }
    JournalState::Committed => {
      finish_cleanup(root, true);
      Ok(Recovery::CleanedUp)
    }
  }
}

fn extract_entry(archive: &Path, entry: &str, dest: &Path) -> USTBLResult<()> {
  let mut zip = zip::ZipArchive::new(fs::File::open(archive).map_err(io_error)?)?;
  let mut source = zip.by_name(entry)?;
  if let Some(parent) = dest.parent() {
    fs::create_dir_all(parent).map_err(io_error)?;
  }
  let mut out = fs::File::create(dest).map_err(io_error)?;
  io::copy(&mut source, &mut out).map_err(io_error)?;
  Ok(())
}

/// Tell every pack session which of its files are about to be fetched, so it
/// can locate just those.
pub async fn prepare_packs(files: &[&RemoteFile]) -> USTBLResult<()> {
  let mut sessions: Vec<(Arc<PackSession>, Vec<String>)> = Vec::new();
  for file in files {
    let FileSource::Pack(session) = &file.source else {
      continue;
    };
    match sessions
      .iter_mut()
      .find(|(known, _)| Arc::ptr_eq(known, session))
    {
      Some((_, wanted)) => wanted.push(file.key.sha1.clone()),
      None => sessions.push((session.clone(), vec![file.key.sha1.clone()])),
    }
  }
  for (session, wanted) in sessions {
    session.prepare(&wanted).await?;
  }
  Ok(())
}

async fn stage_file(env: &dyn UpdateEnv, dest: PathBuf, file: &RemoteFile) -> USTBLResult<()> {
  match &file.source {
    FileSource::Download(param) => {
      let mut param = (**param).clone();
      param.dest = dest.clone();
      param.sha1 = Some(file.key.sha1.clone());
      env.fetch(param).await?
    }
    FileSource::ArchiveEntry { archive, entry } => {
      let (archive, entry, dest) = (archive.clone(), entry.clone(), dest.clone());
      tokio::task::spawn_blocking(move || extract_entry(&archive, &entry, &dest))
        .await
        .map_err(|e| crate::error::USTBLError(e.to_string()))??;
    }
    FileSource::Pack(session) => session.fetch(&file.key.sha1, &dest).await?,
  }
  let check = dest.clone();
  let actual = tokio::task::spawn_blocking(move || hash_file(&check))
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()))?
    .map_err(io_error)?;
  if actual.0 != file.key.sha1 || actual.1 != file.key.size {
    let _ = fs::remove_file(&dest);
    return Err(InstanceError::ModpackUpdateVerifyFailed.into());
  }
  Ok(())
}

pub async fn materialize_remote_file(
  env: &dyn UpdateEnv,
  dest: PathBuf,
  file: &RemoteFile,
) -> USTBLResult<()> {
  stage_file(env, dest, file).await
}

/// Move originals aside and put the staged files in place.
fn commit_files(root: &Path, operations: &[Operation], journal: &Journal) -> io::Result<()> {
  let backup = backup_dir(root);
  for (operation, op) in operations.iter().zip(&journal.ops) {
    let target = root.join(&op.path);
    if op.had_original {
      move_file(&target, &backup.join("files").join(&op.path))?;
    }
    if op.moves_disabled {
      let disabled = disabled_path(&op.path);
      move_file(&root.join(&disabled), &backup.join("files").join(&disabled))?;
    }
    if operation.write.is_some() {
      move_file(&staging_dir(root).join(&op.path), &target)?;
    }
  }
  Ok(())
}

fn save_meta_backup(root: &Path, names: &[String]) -> io::Result<(Vec<String>, Vec<String>)> {
  let meta = backup_dir(root).join("meta");
  fs::create_dir_all(&meta)?;
  let (mut present, mut absent) = (Vec::new(), Vec::new());
  for name in names {
    let file = root.join(name);
    if file.is_file() {
      fs::copy(&file, meta.join(name))?;
      present.push(name.clone());
    } else {
      absent.push(name.clone());
    }
  }
  Ok((present, absent))
}

/// Run an update. On any failure the instance is left exactly as it was.
pub async fn apply(env: &dyn UpdateEnv, request: UpdateRequest) -> USTBLResult<()> {
  let root = request.root.clone();
  let _guard = UpdateGuard::acquire(&root)?;
  env.ensure_not_running()?;
  recover_locked(&root)?;
  request.remote.validate(&request.instance_name)?;

  match run(env, &request).await {
    Ok(()) => Ok(()),
    Err(error) => {
      // `run` rolls back after the journal exists; before that only staging
      // has to go.
      if read_journal(&root).ok().flatten().is_none() {
        let _ = fs::remove_dir_all(staging_dir(&root));
        let _ = fs::remove_dir_all(backup_dir(&root));
      }
      Err(error)
    }
  }
}

async fn run(env: &dyn UpdateEnv, request: &UpdateRequest) -> USTBLResult<()> {
  let root = &request.root;
  let remote_files: BTreeMap<&str, &RemoteFile> = request
    .remote
    .files
    .iter()
    .map(|file| (file.path.as_str(), file))
    .collect();

  // Phase 1: fetch and verify everything into staging.
  let _ = fs::remove_dir_all(staging_dir(root));
  let _ = fs::remove_dir_all(components_dir(root));
  let to_fetch: Vec<&RemoteFile> = request
    .operations
    .iter()
    .filter(|operation| operation.write.is_some())
    .map(|operation| {
      remote_files
        .get(operation.path.as_str())
        .copied()
        .ok_or(InstanceError::ModpackUpdateRemoteInvalid)
    })
    .collect::<Result<_, _>>()?;
  for (operation, file) in request
    .operations
    .iter()
    .filter(|operation| operation.write.is_some())
    .zip(&to_fetch)
  {
    if operation.write.as_ref() != Some(&file.key) {
      return Err(InstanceError::ModpackUpdateStale.into());
    }
  }
  let mut to_fetch: Vec<(&RemoteFile, PathBuf)> = to_fetch
    .into_iter()
    .filter(|file| !request.merged_contents.contains_key(&file.path))
    .map(|file| (file, staging_dir(root).join(&file.path)))
    .collect();
  let pack = if request.restore_components {
    Some(
      request
        .remote
        .component_pack
        .as_ref()
        .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?,
    )
  } else {
    None
  };
  if let Some(pack) = pack {
    to_fetch.extend(
      pack
        .files
        .iter()
        .map(|file| (file, components_dir(root).join(&file.path))),
    );
  }
  let wanted: Vec<&RemoteFile> = to_fetch.iter().map(|(file, _)| *file).collect();
  prepare_packs(&wanted).await?;
  let total = to_fetch.len();
  env.report(Progress {
    stage: Stage::Fetching,
    done: 0,
    total,
  });
  let mut done = 0;
  // Driven by hand rather than with `buffer_unordered`: the combinator's
  // closure makes the future fail the `Send` check of the command handler.
  let mut pending = to_fetch.into_iter();
  let mut running = FuturesUnordered::new();
  loop {
    while running.len() < FETCH_CONCURRENCY {
      let Some((file, dest)) = pending.next() else {
        break;
      };
      running.push(stage_file(env, dest, file));
    }
    let Some(result) = running.next().await else {
      break;
    };
    result?;
    done += 1;
    env.report(Progress {
      stage: Stage::Fetching,
      done,
      total,
    });
  }
  for (path, bytes) in &request.merged_contents {
    let dest = staging_dir(root).join(path);
    if let Some(parent) = dest.parent() {
      fs::create_dir_all(parent).map_err(io_error)?;
    }
    fs::write(&dest, bytes).map_err(io_error)?;
    if !crate::instance::helpers::modpack::text::is_text_path(path, bytes) {
      return Err(InstanceError::ModpackUpdateVerifyFailed.into());
    }
  }

  // Phase 2: write the journal and save the files the update rewrites.
  env.report(Progress {
    stage: Stage::Applying,
    done: 0,
    total: request.operations.len(),
  });
  let ops: Vec<JournalOp> = request
    .operations
    .iter()
    .map(|operation| JournalOp {
      path: operation.path.clone(),
      had_original: root.join(&operation.path).is_file(),
      moves_disabled: operation.clear_disabled
        && root.join(disabled_path(&operation.path)).is_file(),
    })
    .collect();
  fs::create_dir_all(backup_dir(root)).map_err(io_error)?;
  let (meta_present, meta_absent) = save_meta_backup(
    root,
    &meta_files(&request.instance_name, request.restore_components),
  )
  .map_err(io_error)?;
  let mut journal = Journal {
    schema: JOURNAL_SCHEMA,
    state: JournalState::Committing,
    from_version: request.origin.version_id.clone(),
    to_version: request.remote.version_id.clone(),
    ops,
    meta_present,
    meta_absent,
  };
  write_journal(root, &journal).map_err(io_error)?;

  // Phase 3: loader, files, baseline, identity. Any failure undoes all of it.
  let outcome = async {
    if let Some(pack) = pack {
      // The chain carries the loader, so there is no separate switch.
      env.restore_components(pack, &components_dir(root)).await?;
    } else if request.remote.loader != request.previous_loader {
      env.switch_loader(&request.remote.loader).await?;
    }
    let (root_owned, operations, snapshot) =
      (root.clone(), request.operations.clone(), journal.clone());
    tokio::task::spawn_blocking(move || commit_files(&root_owned, &operations, &snapshot))
      .await
      .map_err(|e| crate::error::USTBLError(e.to_string()))?
      .map_err(io_error)?;

    env.report(Progress {
      stage: Stage::Finalizing,
      done: 0,
      total: 1,
    });
    let mut origin = request.origin.clone();
    origin.version_id = request.remote.version_id.clone();
    let mut baseline = ModpackBaseline::build(
      &origin,
      request.remote.loader.clone(),
      request.remote.baseline_files(),
    );
    if let Some(chain) = &request.components {
      baseline = baseline.with_components(chain.clone());
    }
    baseline.save(root).await.map_err(io_error)?;
    tokio::task::spawn_blocking({
      let root = root.clone();
      let files = baseline.files.clone();
      move || base_store::sync(&root, &files)
    })
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()))?
    .map_err(io_error)?;
    env.commit_origin(&request.remote.version_id).await
  }
  .await;

  match outcome {
    Ok(()) => {
      journal.state = JournalState::Committed;
      // If this write fails the next recovery would undo a finished update.
      write_journal(root, &journal).map_err(io_error)?;
      finish_cleanup(root, true);
      env.report(Progress {
        stage: Stage::Finalizing,
        done: 1,
        total: 1,
      });
      Ok(())
    }
    Err(error) => {
      let root_owned = root.clone();
      let snapshot = journal.clone();
      let undone = tokio::task::spawn_blocking(move || roll_back(&root_owned, &snapshot))
        .await
        .map_err(|e| crate::error::USTBLError(e.to_string()))?;
      match undone {
        // Keep the journal so the next start can retry the rollback.
        Err(rollback_error) => Err(crate::error::USTBLError(format!(
          "{error:?}; rollback failed: {rollback_error}"
        ))),
        Ok(()) => {
          finish_cleanup(root, false);
          Err(error)
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::{ModLoaderType, OriginPlatform};
  use crate::instance::modpack_update::diff::FileKey;
  use sha1::{Digest, Sha1};
  use std::io::Write;

  fn sha1_of(content: &[u8]) -> FileKey {
    FileKey {
      sha1: hex::encode(Sha1::digest(content)),
      size: content.len() as u64,
    }
  }

  fn loader(version: &str) -> BaselineLoader {
    BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: version.into(),
    }
  }

  fn origin(version: &str) -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Campus,
      project_id: "p".into(),
      version_id: version.into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    }
  }

  struct Root(PathBuf);

  impl Root {
    fn new() -> Self {
      let root = std::env::temp_dir().join(format!("ustbl-apply-test-{}", uuid::Uuid::new_v4()));
      fs::create_dir_all(&root).unwrap();
      Self(root)
    }
    fn write(&self, path: &str, content: &str) {
      let file = self.0.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    fn read(&self, path: &str) -> Option<String> {
      fs::read_to_string(self.0.join(path)).ok()
    }
  }

  impl Drop for Root {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[derive(Default)]
  struct FakeEnv {
    contents: BTreeMap<String, Vec<u8>>,
    fail_url: Option<String>,
    fail_commit: bool,
    switched: Mutex<Vec<String>>,
    restored: Mutex<Vec<String>>,
    committed: Mutex<Vec<String>>,
    root: PathBuf,
  }

  #[async_trait]
  impl UpdateEnv for FakeEnv {
    async fn fetch(&self, param: DownloadParam) -> USTBLResult<()> {
      let (url, dest) = (param.src.as_str(), &param.dest);
      if self.fail_url.as_deref() == Some(url) {
        return Err(crate::error::USTBLError("network".into()));
      }
      fs::create_dir_all(dest.parent().unwrap()).unwrap();
      fs::write(dest, &self.contents[url]).unwrap();
      Ok(())
    }
    async fn switch_loader(&self, loader: &BaselineLoader) -> USTBLResult<()> {
      self.switched.lock().unwrap().push(loader.version.clone());
      fs::write(self.root.join("ustblcfg.json"), "switched").unwrap();
      Ok(())
    }
    async fn restore_components(&self, pack: &ComponentPack, staged: &Path) -> USTBLResult<()> {
      for file in &pack.files {
        let bytes = fs::read(staged.join(&file.path)).unwrap();
        self.restored.lock().unwrap().push(format!(
          "{}={}",
          file.path,
          String::from_utf8(bytes).unwrap()
        ));
      }
      fs::write(self.root.join("pack.json"), "client-restored").unwrap();
      fs::write(self.root.join("pack.jar"), "jar-restored").unwrap();
      Ok(())
    }
    async fn commit_origin(&self, version_id: &str) -> USTBLResult<()> {
      if self.fail_commit {
        return Err(crate::error::USTBLError("commit".into()));
      }
      self.committed.lock().unwrap().push(version_id.into());
      Ok(())
    }
    fn report(&self, _progress: Progress) {}
  }

  /// Instance at version 1: keep.jar and old.jar are managed, edited.cfg too.
  fn setup() -> Root {
    let root = Root::new();
    root.write("mods/old.jar", "old");
    root.write("mods/gone.jar", "gone");
    root.write("ustblcfg.json", "cfg-v1");
    root.write("pack.json", "client-v1");
    root
  }

  fn url_file(path: &str, content: &str, env: &mut FakeEnv) -> RemoteFile {
    let url = format!("https://example.com/{path}");
    env
      .contents
      .insert(url.clone(), content.as_bytes().to_vec());
    RemoteFile {
      path: path.into(),
      key: sha1_of(content.as_bytes()),
      source: FileSource::Download(Box::new(DownloadParam {
        src: url.parse().unwrap(),
        dest: PathBuf::new(),
        filename: None,
        sha1: None,
        custom_headers: None,
        transfer_options: Default::default(),
      })),
    }
  }

  fn request(
    root: &Root,
    files: Vec<RemoteFile>,
    operations: Vec<Operation>,
    new_loader: &str,
  ) -> UpdateRequest {
    UpdateRequest {
      root: root.0.clone(),
      instance_name: "pack".into(),
      operations,
      remote: RemoteModpack {
        version_id: "2".into(),
        label: "2".into(),
        game_version: "1.21.1".into(),
        loader: loader(new_loader),
        components: None,
        component_pack: None,
        files,
      },
      origin: origin("1"),
      previous_loader: loader("0.15.0"),
      components: None,
      restore_components: false,
      merged_contents: BTreeMap::new(),
    }
  }

  fn write_op(file: &RemoteFile) -> Operation {
    Operation {
      path: file.path.clone(),
      write: Some(file.key.clone()),
      clear_disabled: false,
    }
  }

  fn remove_op(path: &str) -> Operation {
    Operation {
      path: path.into(),
      write: None,
      clear_disabled: false,
    }
  }

  fn with_component_pack(request: &mut UpdateRequest, env: &mut FakeEnv) {
    let patch = url_file("patches/com.example.json", "patch-bytes", env);
    request.remote.component_pack = Some(ComponentPack {
      mmc_pack: serde_json::json!({ "components": [], "formatVersion": 1 }),
      instance_cfg: BTreeMap::new(),
      files: vec![patch],
    });
    request.restore_components = true;
  }

  #[tokio::test]
  async fn a_component_pack_is_fetched_and_restored_instead_of_switching_the_loader() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    with_component_pack(&mut update, &mut env);

    apply(&env, update).await.unwrap();

    assert_eq!(
      env.restored.lock().unwrap().as_slice(),
      ["patches/com.example.json=patch-bytes"]
    );
    assert!(env.switched.lock().unwrap().is_empty());
    assert_eq!(root.read("pack.json").as_deref(), Some("client-restored"));
    assert!(!root.0.join(UPDATE_DIR).join("components").exists());
  }

  #[tokio::test]
  async fn a_failed_restore_commit_puts_the_client_files_back() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      fail_commit: true,
      ..Default::default()
    };
    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    with_component_pack(&mut update, &mut env);

    assert!(apply(&env, update).await.is_err());

    assert_eq!(root.read("pack.json").as_deref(), Some("client-v1"));
    // The jar did not exist before, so it must not be left behind.
    assert_eq!(root.read("pack.jar"), None);
    assert!(!journal_path(&root.0).exists());
  }

  #[tokio::test]
  async fn applies_changes_keeps_a_backup_and_leaves_no_residue() {
    let root = setup();
    root.write("mods/old.jar.disabled", "old-disabled");
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let new = url_file("mods/new.jar", "new", &mut env);
    let updated = url_file("mods/old.jar", "old-v2", &mut env);
    let mut update_op = write_op(&updated);
    update_op.clear_disabled = true;
    let operations = vec![write_op(&new), update_op, remove_op("mods/gone.jar")];

    apply(
      &env,
      request(&root, vec![new, updated], operations, "0.16.0"),
    )
    .await
    .unwrap();

    assert_eq!(root.read("mods/new.jar").as_deref(), Some("new"));
    assert_eq!(root.read("mods/old.jar").as_deref(), Some("old-v2"));
    assert_eq!(root.read("mods/old.jar.disabled"), None);
    assert_eq!(root.read("mods/gone.jar"), None);
    assert_eq!(env.switched.lock().unwrap().as_slice(), ["0.16.0"]);
    assert_eq!(env.committed.lock().unwrap().as_slice(), ["2"]);
    let baseline = ModpackBaseline::load(&root.0).await.unwrap();
    assert_eq!(baseline.origin.version_id, "2");
    assert_eq!(baseline.files.len(), 2);
    let last = root.0.join(UPDATE_DIR).join("last-backup/files");
    assert_eq!(
      fs::read_to_string(last.join("mods/old.jar")).unwrap(),
      "old"
    );
    assert_eq!(
      fs::read_to_string(last.join("mods/gone.jar")).unwrap(),
      "gone"
    );
    assert_eq!(
      fs::read_to_string(last.join("mods/old.jar.disabled")).unwrap(),
      "old-disabled"
    );
    assert!(!root.0.join(UPDATE_DIR).join("staging").exists());
    assert!(!journal_path(&root.0).exists());
    assert!(UpdateGuard::acquire(&root.0).is_ok());

    // The previous update can be taken back once.
    assert!(can_roll_back_last(&root.0));
    assert!(roll_back_last(&root.0).unwrap());
    assert_eq!(root.read("mods/new.jar"), None);
    assert_eq!(root.read("mods/old.jar").as_deref(), Some("old"));
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(
      root.read("mods/old.jar.disabled").as_deref(),
      Some("old-disabled")
    );
    assert!(!can_roll_back_last(&root.0));
    assert!(!roll_back_last(&root.0).unwrap());
  }

  #[tokio::test]
  async fn the_component_chain_is_carried_into_the_new_baseline() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let new = url_file("mods/new.jar", "new", &mut env);
    let operations = vec![write_op(&new)];
    let mut update = request(&root, vec![new], operations, "0.16.0");
    let chain = vec![ComponentRef {
      uid: "net.minecraft".into(),
      version: "1.7.10".into(),
    }];
    update.components = Some(chain.clone());

    apply(&env, update).await.unwrap();

    let baseline = ModpackBaseline::load(&root.0).await.unwrap();
    assert_eq!(baseline.component_refs(), Some(chain));
  }

  #[tokio::test]
  async fn a_failed_download_touches_nothing() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let good = url_file("mods/a.jar", "a", &mut env);
    let bad = url_file("mods/b.jar", "b", &mut env);
    env.fail_url = Some("https://example.com/mods/b.jar".into());
    let operations = vec![write_op(&good), write_op(&bad), remove_op("mods/gone.jar")];

    assert!(
      apply(&env, request(&root, vec![good, bad], operations, "0.15.0"))
        .await
        .is_err()
    );

    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(root.read("mods/a.jar"), None);
    assert!(!journal_path(&root.0).exists());
    assert!(!staging_dir(&root.0).exists());
    assert!(env.switched.lock().unwrap().is_empty());
  }

  #[tokio::test]
  async fn corrupt_downloads_are_rejected_before_anything_changes() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let mut file = url_file("mods/a.jar", "a", &mut env);
    file.key = sha1_of(b"something else");
    let operations = vec![write_op(&file), remove_op("mods/gone.jar")];

    let error = apply(&env, request(&root, vec![file], operations, "0.15.0"))
      .await
      .unwrap_err();

    assert_eq!(
      error.0,
      InstanceError::ModpackUpdateVerifyFailed.to_string()
    );
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(root.read("mods/a.jar"), None);
  }

  #[tokio::test]
  async fn a_failure_after_the_files_moved_restores_everything() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      fail_commit: true,
      ..Default::default()
    };
    let new = url_file("mods/new.jar", "new", &mut env);
    let updated = url_file("mods/old.jar", "old-v2", &mut env);
    let operations = vec![
      write_op(&new),
      write_op(&updated),
      remove_op("mods/gone.jar"),
    ];

    assert!(apply(
      &env,
      request(&root, vec![new, updated], operations, "0.16.0")
    )
    .await
    .is_err());

    assert_eq!(root.read("mods/old.jar").as_deref(), Some("old"));
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(root.read("mods/new.jar"), None);
    assert_eq!(root.read("ustblcfg.json").as_deref(), Some("cfg-v1"));
    assert_eq!(root.read(BASELINE_FILE_NAME), None);
    assert!(!journal_path(&root.0).exists());
    assert!(!backup_dir(&root.0).exists());
  }

  /// Build the on-disk state of an update that died after moving one file.
  fn interrupted(root: &Root, state: JournalState) {
    let backup = backup_dir(&root.0);
    fs::create_dir_all(backup.join("files/mods")).unwrap();
    fs::create_dir_all(backup.join("meta")).unwrap();
    fs::copy(
      root.0.join("ustblcfg.json"),
      backup.join("meta/ustblcfg.json"),
    )
    .unwrap();
    fs::rename(
      root.0.join("mods/old.jar"),
      backup.join("files/mods/old.jar"),
    )
    .unwrap();
    root.write("mods/old.jar", "half-written");
    root.write("mods/new.jar", "new");
    root.write("ustblcfg.json", "cfg-v2");
    let journal = Journal {
      schema: JOURNAL_SCHEMA,
      state,
      from_version: "1".into(),
      to_version: "2".into(),
      ops: vec![
        JournalOp {
          path: "mods/old.jar".into(),
          had_original: true,
          moves_disabled: false,
        },
        JournalOp {
          path: "mods/new.jar".into(),
          had_original: false,
          moves_disabled: false,
        },
        JournalOp {
          path: "mods/gone.jar".into(),
          had_original: true,
          moves_disabled: false,
        },
      ],
      meta_present: vec!["ustblcfg.json".into()],
      meta_absent: vec![BASELINE_FILE_NAME.into()],
    };
    write_journal(&root.0, &journal).unwrap();
    root.write(BASELINE_FILE_NAME, "{}");
  }

  #[test]
  fn recovery_undoes_an_update_that_died_mid_commit() {
    let root = setup();
    interrupted(&root, JournalState::Committing);
    // gone.jar had been moved to the backup before the crash as well.
    let backup = backup_dir(&root.0).join("files/mods/gone.jar");
    fs::rename(root.0.join("mods/gone.jar"), backup).unwrap();

    assert_eq!(recover(&root.0).unwrap(), Recovery::RolledBack);

    assert_eq!(root.read("mods/old.jar").as_deref(), Some("old"));
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(root.read("mods/new.jar"), None);
    assert_eq!(root.read("ustblcfg.json").as_deref(), Some("cfg-v1"));
    assert_eq!(root.read(BASELINE_FILE_NAME), None);
    assert!(!journal_path(&root.0).exists());
    assert_eq!(recover(&root.0).unwrap(), Recovery::Nothing);
  }

  #[test]
  fn recovery_never_undoes_a_committed_update() {
    let root = setup();
    interrupted(&root, JournalState::Committed);

    assert_eq!(recover(&root.0).unwrap(), Recovery::CleanedUp);

    assert_eq!(root.read("mods/old.jar").as_deref(), Some("half-written"));
    assert_eq!(root.read("mods/new.jar").as_deref(), Some("new"));
    assert_eq!(root.read("ustblcfg.json").as_deref(), Some("cfg-v2"));
    assert!(last_backup_dir(&root.0).join("files/mods/old.jar").exists());
    assert!(!journal_path(&root.0).exists());
  }

  #[test]
  fn unlink_is_refused_while_an_update_is_mid_flight() {
    let root = setup();
    // No journal: unlinking is safe.
    assert!(ensure_unlink_safe(&root.0).is_ok());
    // A leftover journal means the instance sits between two versions.
    interrupted(&root, JournalState::Committing);
    let error = ensure_unlink_safe(&root.0).unwrap_err();
    assert_eq!(error.0, InstanceError::ModpackUpdateUnlinkPending.to_string());
    // Once recovered, unlinking is safe again.
    assert_eq!(recover(&root.0).unwrap(), Recovery::RolledBack);
    assert!(ensure_unlink_safe(&root.0).is_ok());
  }

  #[tokio::test]
  async fn only_one_update_runs_per_instance() {
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let _held = UpdateGuard::acquire(&root.0).unwrap();
    assert!(UpdateGuard::while_idle(&root.0, || panic!("launch entered during update")).is_err());

    let error = apply(&env, request(&root, vec![], vec![], "0.15.0"))
      .await
      .unwrap_err();

    assert_eq!(error.0, InstanceError::ModpackUpdateBusy.to_string());
    drop(_held);
    assert_eq!(UpdateGuard::while_idle(&root.0, || 42).unwrap(), 42);
  }

  #[tokio::test]
  async fn files_can_come_from_an_archive() {
    let root = setup();
    let archive = root.0.join("pack.zip");
    {
      let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
      zip
        .start_file(
          "overrides/config/a.toml",
          zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
      zip.write_all(b"from-zip").unwrap();
      zip.finish().unwrap();
    }
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let file = RemoteFile {
      path: "config/a.toml".into(),
      key: sha1_of(b"from-zip"),
      source: FileSource::ArchiveEntry {
        archive,
        entry: "overrides/config/a.toml".into(),
      },
    };
    let operations = vec![write_op(&file)];

    apply(&env, request(&root, vec![file], operations, "0.15.0"))
      .await
      .unwrap();

    assert_eq!(root.read("config/a.toml").as_deref(), Some("from-zip"));
    assert!(env.switched.lock().unwrap().is_empty());
  }

  /// A platform whose single delta pack holds one stored file at offset 0.
  struct OneFilePack(Vec<u8>);

  #[async_trait]
  impl crate::instance::modpack_update::packs::PackTransport for OneFilePack {
    async fn locate(
      &self,
      _: i64,
      sha1s: &[String],
    ) -> Result<
      crate::instance::modpack_update::packs::Located,
      crate::instance::modpack_update::packs::TransportError,
    > {
      use crate::instance::modpack_update::packs::{EntryLocation, Located, PackLocation, STORED};
      let entry = EntryLocation {
        pack: 1,
        data_offset: 0,
        compress_type: STORED,
        compressed_size: self.0.len() as u64,
        size: self.0.len() as u64,
        crc32: {
          let mut crc = flate2::Crc::new();
          crc.update(&self.0);
          crc.sum()
        },
      };
      Ok(Located {
        packs: vec![PackLocation {
          id: 1,
          url: "https://cloud.example/p".into(),
          method: "GET".into(),
          headers: Default::default(),
        }],
        entries: sha1s.iter().map(|sha1| (sha1.clone(), entry)).collect(),
      })
    }
    async fn read_range(
      &self,
      _: &crate::instance::modpack_update::packs::PackLocation,
      _: u64,
      _: u64,
      dest: &Path,
    ) -> Result<(), crate::instance::modpack_update::packs::TransportError> {
      fs::create_dir_all(dest.parent().unwrap()).unwrap();
      fs::write(dest, &self.0).unwrap();
      Ok(())
    }
  }

  #[tokio::test]
  async fn files_can_come_from_the_platform_packs() {
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let session = PackSession::new(std::sync::Arc::new(OneFilePack(b"from-pack".to_vec())), 3);
    let file = RemoteFile {
      path: "config/p.toml".into(),
      key: sha1_of(b"from-pack"),
      source: FileSource::Pack(session),
    };
    let operations = vec![write_op(&file)];

    apply(&env, request(&root, vec![file], operations, "0.15.0"))
      .await
      .unwrap();

    assert_eq!(root.read("config/p.toml").as_deref(), Some("from-pack"));
  }

  #[tokio::test]
  async fn unsafe_remote_paths_are_refused() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let file = url_file("../evil.jar", "x", &mut env);
    let operations = vec![write_op(&file)];

    assert!(
      apply(&env, request(&root, vec![file], operations, "0.15.0"))
        .await
        .is_err()
    );
    assert!(!root.0.parent().unwrap().join("evil.jar").exists());
  }

  /// "Take remote" on every conflict, a text file answered block by block
  /// included, leaves nothing for the local scan to report afterwards.
  #[tokio::test]
  async fn taking_every_remote_side_leaves_no_local_changes() {
    use crate::instance::helpers::modpack::baseline::BaselineFile;
    use crate::instance::modpack_update::commands::selected_hunk_contents;
    use crate::instance::modpack_update::diff::{resolve, ConflictChoice};
    use crate::instance::modpack_update::plan::{count_local_changes, prepare_with_contents};

    let root = setup();
    root.write("mods/a.jar", "a1-edited");
    root.write("mods/b.jar", "b1");
    // Edited here with Windows line endings; the new version changes the same line.
    root.write("config/c.toml", "x = 2\r\ny = 1\r\n");
    let base_text = "x = 1\ny = 1\n";
    let files: Vec<BaselineFile> = [
      ("mods/a.jar", "a1"),
      ("mods/b.jar", "b1"),
      ("config/c.toml", base_text),
    ]
    .iter()
    .map(|(path, content)| {
      let key = sha1_of(content.as_bytes());
      BaselineFile {
        path: path.to_string(),
        sha1: key.sha1,
        size: key.size,
      }
    })
    .collect();
    base_store::store(&root.0, &files[2], base_text.as_bytes()).unwrap();
    let installed = ModpackBaseline::build(&origin("1"), loader("0.15.0"), files);

    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let remote_text = "x = 3\ny = 1\n";
    let remote_files = vec![
      url_file("mods/a.jar", "a2", &mut env),
      url_file("mods/b.jar", "b2", &mut env),
      url_file("config/c.toml", remote_text, &mut env),
    ];
    let mut update = request(&root, remote_files, Vec::new(), "0.15.0");
    let contents = BTreeMap::from([("config/c.toml".to_string(), remote_text.as_bytes().to_vec())]);
    let prepared = prepare_with_contents(&root.0, &installed, &update.remote, &contents)
      .await
      .unwrap();
    assert_eq!(prepared.diff.conflicts.len(), 2);
    assert!(prepared
      .diff
      .conflicts
      .iter()
      .any(|conflict| conflict.path == "config/c.toml" && !conflict.hunks.is_empty()));

    // What the update dialog sends for "take remote for all".
    let mut choices = BTreeMap::new();
    for conflict in &prepared.diff.conflicts {
      if conflict.hunks.is_empty() {
        choices.insert(conflict.path.clone(), ConflictChoice::TakeRemote);
      }
      for index in 0..conflict.hunks.len() {
        choices.insert(
          format!("{}#{index}", conflict.path),
          ConflictChoice::TakeRemote,
        );
      }
    }
    update.merged_contents = selected_hunk_contents(&prepared, &mut choices).unwrap();
    update.operations = resolve(&prepared.diff, &choices);

    apply(&env, update).await.unwrap();

    let baseline = ModpackBaseline::load(&root.0).await.unwrap();
    assert_eq!(count_local_changes(&root.0, &baseline).await.unwrap(), 0);
    assert_eq!(root.read("mods/a.jar").as_deref(), Some("a2"));
    assert_eq!(root.read("mods/b.jar").as_deref(), Some("b2"));
    assert_eq!(root.read("config/c.toml").as_deref(), Some(remote_text));
  }
}
