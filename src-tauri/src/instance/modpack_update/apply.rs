//! Applying a modpack update to an instance directory.
//!
//! Nothing in the instance is touched until every new file has been fetched
//! into a staging area and verified. Before the first move a journal is
//! written, and everything that is replaced or removed is moved into a backup
//! folder instead of being deleted. If anything fails, or the launcher dies
//! half way, the journal is enough to put every file back.

use super::diff::{FileKey, Operation, DISABLED_SUFFIX};
use super::packs::{FetchGroup, PackFetch, PackSession};
use super::remote::{ComponentPack, FileSource, RemoteFile, RemoteModpack, UPDATE_DIR};
use crate::error::USTBLResult;
use crate::file_error::{tag, AtPath};
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
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const JOURNAL_FILE: &str = "journal.json";
const JOURNAL_SCHEMA: u32 = 1;
const INSTANCE_CONFIG_FILE: &str = "ustblcfg.json";
const FETCH_CONCURRENCY: usize = 8;
const STAGING_META: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StagingMeta {
  version_id: String,
  plan_hash: String,
}

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

fn staging_meta_path(root: &Path) -> PathBuf {
  // Keep bookkeeping outside the payload namespace (a pack may legitimately
  // ship a root-level manifest.json).
  update_dir(root).join(format!("staging-{STAGING_META}"))
}

fn read_staging_meta(root: &Path) -> io::Result<Option<StagingMeta>> {
  let path = staging_meta_path(root);
  match fs::read(&path) {
    Ok(bytes) => {
      Ok(Some(serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(io::ErrorKind::InvalidData, error)
      })?))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(tag(error, &path, Some(root))),
  }
}

fn write_staging_meta(root: &Path, meta: &StagingMeta) -> io::Result<()> {
  fs::create_dir_all(staging_dir(root)).at_in(&staging_dir(root), root)?;
  let path = staging_meta_path(root);
  let temp = path.with_extension("json.tmp");
  fs::write(&temp, serde_json::to_vec_pretty(meta)?).at_in(&temp, root)?;
  fs::rename(&temp, &path).at_in(&temp, root)
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
  /// The update put a file at the enabled slot. Journals from before the
  /// field existed wrote there unless the op was a removal; they never
  /// distinguish, and the old rollback behaved the same either way.
  #[serde(default)]
  writes_enabled: Option<bool>,
  /// The update left a file at the disabled slot (a disabled ship, or a
  /// variant flip); a rollback removes it. Missing from old journals, which
  /// never wrote there without moving the local file aside first.
  #[serde(default)]
  writes_disabled: bool,
  /// Directories this op's deletion emptied, walked up to and including the
  /// rule's directory; recreated on rollback.
  #[serde(default)]
  pruned_dirs: Vec<String>,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
  WaitingClient,
  /// Fetching the version detail and comparing it with the instance.
  Preparing,
  /// Asking the platform where the needed files sit in its packs.
  Locating,
  Fetching,
  Applying,
  Finalizing,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
  pub stage: Stage,
  pub done: usize,
  pub total: usize,
  pub bytes_done: u64,
  pub bytes_total: u64,
}

/// Everything the update needs from the outside world.
#[async_trait]
pub trait UpdateEnv: Send + Sync {
  /// Refuse when the game is starting or running. Called after the update
  /// guard is held, so a launch cannot slip in after the check.
  fn ensure_not_running(&self) -> USTBLResult<()> {
    Ok(())
  }
  /// Download `param.src` to `param.dest`. When `param.sha1` is set the
  /// download must verify it and fail otherwise (`DownloadTask` does); the
  /// engine then only checks the size.
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
  /// Whether the caller asked to abandon the run. Only honoured until
  /// `begin_commit` succeeds.
  fn cancel_requested(&self) -> bool {
    false
  }
  /// Resolves once the caller asks to abandon the run; never, by default.
  async fn cancelled(&self) {
    std::future::pending::<()>().await
  }
  /// Called once, right before the journal is written. Fails when the run was
  /// cancelled; otherwise the run can no longer be cancelled.
  fn begin_commit(&self) -> USTBLResult<()> {
    Ok(())
  }
}

fn cancelled_error() -> crate::error::USTBLError {
  InstanceError::ModpackInstallInterrupted.into()
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
  /// New-version text bytes the caller already has, keyed by relative path.
  /// Cached as the baseline copy after the update so a locally edited file
  /// keeps a base to diff against.
  pub base_texts: BTreeMap<String, Vec<u8>>,
  /// The baseline entries the update records when it commits, as the decision
  /// computed them (dormant entries included).
  pub next_baseline: Vec<crate::instance::helpers::modpack::baseline::BaselineFile>,
  /// The local ignores as the plan saw them and as the update leaves them;
  /// `None` leaves the file alone. Written inside the transaction.
  pub ignores: Option<super::ignores::IgnoresUpdate>,
  /// Identifies the plan whose staged files may be reused after a failure.
  pub plan_hash: String,
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
  fs::write(&temp, serde_json::to_vec_pretty(journal)?).at_in(&temp, root)?;
  fs::rename(&temp, &path).at_in(&temp, root)
}

fn read_journal(root: &Path) -> io::Result<Option<Journal>> {
  let path = journal_path(root);
  match fs::read(&path) {
    Ok(bytes) => {
      Ok(Some(serde_json::from_slice(&bytes).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, e)
      })?))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(tag(error, &path, Some(root))),
  }
}

/// Rename `from` to `to`. A failure names `from`: that is the file a scanner
/// or another program holds on to.
fn move_file(root: &Path, from: &Path, to: &Path) -> io::Result<()> {
  if let Some(parent) = to.parent() {
    fs::create_dir_all(parent).at_in(parent, root)?;
  }
  fs::rename(from, to).at_in(from, root)
}

fn remove_file_if_exists(root: &Path, path: &Path) -> io::Result<()> {
  match fs::remove_file(path) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => Err(tag(error, path, Some(root))),
    _ => Ok(()),
  }
}

fn meta_files(instance_name: &str, restoring_components: bool) -> Vec<String> {
  let mut names = vec![
    BASELINE_FILE_NAME.to_string(),
    INSTANCE_CONFIG_FILE.to_string(),
    format!("{instance_name}.json"),
    super::ignores::meta_name(),
    super::release::meta_name(),
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
    // Restore or clear the enabled slot. Journals from before the field
    // existed always wrote there unless the op was a removal; the old
    // rollback behaved the same for both, so `unwrap_or(!had_original)`.
    let wrote_enabled = op.writes_enabled.unwrap_or(!op.had_original);
    if op.had_original && saved.is_file() {
      remove_file_if_exists(root, &target)?;
      move_file(root, &saved, &target)?;
    } else if wrote_enabled {
      remove_file_if_exists(root, &target)?;
    }
    let disabled = disabled_path(&op.path);
    if op.moves_disabled {
      let saved = backup.join("files").join(&disabled);
      remove_file_if_exists(root, &root.join(&disabled))?;
      if saved.is_file() {
        move_file(root, &saved, &root.join(&disabled))?;
      }
    } else if op.writes_disabled {
      remove_file_if_exists(root, &root.join(&disabled))?;
    }
  }
  for name in &journal.meta_present {
    let saved = backup.join("meta").join(name);
    if saved.is_file() {
      fs::copy(&saved, root.join(name)).at_in(&root.join(name), root)?;
    }
  }
  for name in &journal.meta_absent {
    remove_file_if_exists(root, &root.join(name))?;
  }
  // Recreate the directories the update pruned, deepest first.
  let mut dirs: Vec<&String> = journal
    .ops
    .iter()
    .flat_map(|op| op.pruned_dirs.iter())
    .collect();
  dirs.sort_by_key(|dir| std::cmp::Reverse(dir.matches('/').count()));
  dirs.dedup();
  for dir in dirs {
    fs::create_dir_all(root.join(dir)).at_in(&root.join(dir), root)?;
  }
  Ok(())
}

/// Drop the staging area and keep this update's backup for the next one.
fn finish_cleanup(root: &Path, keep_backup: bool) {
  let _ = fs::remove_dir_all(staging_dir(root));
  let _ = fs::remove_file(staging_meta_path(root));
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
#[cfg_attr(not(test), allow(dead_code))]
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
  let journal_file = backup.join(LAST_JOURNAL_NAME);
  let journal: Journal = match fs::read(&journal_file) {
    Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| io_error(e.into()))?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(io_error(tag(error, &journal_file, Some(root)))),
  };
  roll_back_from(root, &backup, &journal).map_err(io_error)?;
  fs::remove_dir_all(&backup)
    .at_in(&backup, root)
    .map_err(io_error)?;
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
    // Pre-transaction downloads belong to the next apply attempt. `run`
    // checks their target and hashes before using them; recovery must not
    // erase them merely because no transaction has started yet.
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

/// Writes through to `inner` while hashing what passes.
struct HashingWriter<W> {
  inner: W,
  hasher: Sha1,
  size: u64,
}

impl<W: io::Write> io::Write for HashingWriter<W> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let written = self.inner.write(buf)?;
    self.hasher.update(&buf[..written]);
    self.size += written as u64;
    Ok(written)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.inner.flush()
  }
}

/// Extract `entry` to `dest`, hashing it on the way out. A file that does not
/// match `key` is removed and rejected.
fn extract_entry(archive: &Path, entry: &str, dest: &Path, key: &FileKey) -> USTBLResult<()> {
  let mut zip = zip::ZipArchive::new(fs::File::open(archive).at(archive).map_err(io_error)?)?;
  extract_entry_from(&mut zip, entry, dest, key)
}

fn extract_entry_from(
  zip: &mut zip::ZipArchive<fs::File>,
  entry: &str,
  dest: &Path,
  key: &FileKey,
) -> USTBLResult<()> {
  let mut source = zip.by_name(entry)?;
  if let Some(parent) = dest.parent() {
    fs::create_dir_all(parent).at(parent).map_err(io_error)?;
  }
  let mut out = HashingWriter {
    inner: fs::File::create(dest).at(dest).map_err(io_error)?,
    hasher: Sha1::new(),
    size: 0,
  };
  io::copy(&mut source, &mut out).at(dest).map_err(io_error)?;
  let HashingWriter { hasher, size, .. } = out;
  if hex::encode(hasher.finalize()) != key.sha1 || size != key.size {
    let _ = fs::remove_file(dest);
    return Err(InstanceError::ModpackUpdateVerifyFailed.into());
  }
  Ok(())
}

/// Tell every pack session which of its files are about to be fetched, so it
/// can locate just those.
pub async fn prepare_packs(files: &[&RemoteFile]) -> USTBLResult<()> {
  for (session, wanted) in pack_wanted(files) {
    let locating = Instant::now();
    let result = session.prepare(&wanted).await;
    log::info!(
      "modpack pack locate: version {}, {} sha1s, {:?} elapsed",
      session.version_id(),
      wanted.len(),
      locating.elapsed()
    );
    result?;
  }
  Ok(())
}

/// The sha1s of `files` per pack session, sorted and deduplicated. Owned, so
/// a locate can start before the hashing it runs next to finishes.
pub fn pack_wanted(files: &[&RemoteFile]) -> Vec<(std::sync::Arc<PackSession>, Vec<String>)> {
  let mut packs = pack_sessions(files);
  for (_, wanted) in &mut packs {
    wanted.sort();
    wanted.dedup();
  }
  packs
}

/// Mark the locate's own failure: a locate that runs next to the hashing and
/// comparison has to tell its error apart from theirs.
pub fn wrap_locate(error: crate::error::USTBLError) -> crate::error::USTBLError {
  log::warn!("modpack pack locate failed: {error:?}");
  // Preserve actionable domain errors (not latest, missing packs, etc.).
  if error.0.starts_with("MODPACK_UPDATE_") {
    return error;
  }
  InstanceError::ModpackUpdateLocateLocating.into()
}

/// The pack sessions behind `files`, each with the sha1s it has to fetch.
fn pack_sessions(files: &[&RemoteFile]) -> Vec<(Arc<PackSession>, Vec<String>)> {
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
  sessions
}

async fn stage_file(env: &dyn UpdateEnv, dest: PathBuf, file: &RemoteFile) -> USTBLResult<()> {
  match &file.source {
    FileSource::Download(param) => {
      let mut param = (**param).clone();
      param.dest = dest.clone();
      param.sha1 = Some(file.key.sha1.clone());
      env.fetch(param).await?;
      // The download already checked the sha1; hashing again would read the
      // whole file for nothing.
      let size = fs::metadata(&dest).at(&dest).map_err(io_error)?.len();
      if size != file.key.size {
        let _ = fs::remove_file(&dest);
        return Err(InstanceError::ModpackUpdateVerifyFailed.into());
      }
      return Ok(());
    }
    FileSource::ArchiveEntry { archive, entry } => {
      // Hashed while extracting.
      let (archive, entry, dest, key) = (
        archive.clone(),
        entry.clone(),
        dest.clone(),
        file.key.clone(),
      );
      return tokio::task::spawn_blocking(move || extract_entry(&archive, &entry, &dest, &key))
        .await
        .map_err(|e| crate::error::USTBLError(e.to_string()))?;
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

/// One unit of fetch work: a single file from a download or an archive, or
/// one group of pack entries a single merged Range request can cover. A
/// group of one file is read on its own.
enum FetchUnit<'a> {
  Single(&'a RemoteFile, PathBuf),
  Group(Arc<PackSession>, FetchGroup),
}

impl FetchUnit<'_> {
  fn file_count(&self) -> usize {
    match self {
      FetchUnit::Single(..) => 1,
      FetchUnit::Group(_, group) => group.file_count(),
    }
  }
}

/// Group `to_fetch` into fetch units. Every pack file goes to its session's
/// batch (wherever it sits in `to_fetch`), which the session splits into
/// merged groups by pack offset; everything else is a single file. The
/// groups are independent of each other and can run concurrently.
fn fetch_units<'a>(
  to_fetch: &[(&'a RemoteFile, PathBuf)],
  planned: Vec<(Arc<PackSession>, Vec<FetchGroup>)>,
) -> Vec<FetchUnit<'a>> {
  let mut units: Vec<FetchUnit<'a>> = Vec::new();
  for (file, dest) in to_fetch {
    if !matches!(file.source, FileSource::Pack(_)) {
      units.push(FetchUnit::Single(file, dest.clone()));
    }
  }
  for (session, groups) in planned {
    for group in groups {
      units.push(FetchUnit::Group(session.clone(), group));
    }
  }
  units
}

/// Move originals aside and put the staged files in place.
fn commit_files(root: &Path, operations: &[Operation], journal: &Journal) -> io::Result<()> {
  let backup = backup_dir(root);
  for (operation, op) in operations.iter().zip(&journal.ops) {
    let target = root.join(&op.path);
    let disabled = disabled_path(&op.path);
    if operation.rename {
      // A variant flip without a content write: the source slot's file moves
      // to the other name. The backup keeps a copy so a rollback can restore
      // the slot in place.
      let (source, dest, backed) = if operation.final_disabled {
        (target.clone(), root.join(&disabled), &op.path)
      } else {
        (root.join(&disabled), target.clone(), &disabled)
      };
      let saved = backup.join("files").join(backed);
      if let Some(parent) = saved.parent() {
        fs::create_dir_all(parent).at_in(parent, root)?;
      }
      fs::copy(&source, &saved).at_in(&source, root)?;
      move_file(root, &source, &dest)?;
      continue;
    }
    if op.had_original {
      move_file(root, &target, &backup.join("files").join(&op.path))?;
    }
    if op.moves_disabled {
      move_file(
        root,
        &root.join(&disabled),
        &backup.join("files").join(&disabled),
      )?;
    }
    if operation.write.is_some() || operation.write_local {
      let staged = staging_dir(root).join(&op.path);
      if operation.final_disabled {
        move_file(root, &staged, &root.join(&disabled))?;
      } else {
        move_file(root, &staged, &target)?;
      }
    }
  }
  Ok(())
}

/// Remove the directories a delete rule emptied: from each deleted file up
/// to and including the rule's own directory, whenever nothing else remains.
fn prune_empty_dirs(root: &Path, operations: &[Operation]) {
  for operation in operations {
    for rule_dir in &operation.prune_dirs {
      let mut dir = PathBuf::from(&operation.path);
      while let Some(parent) = dir.parent().map(Path::to_path_buf) {
        // Never climb past the rule's own directory.
        if parent.components().count() < PathBuf::from(rule_dir).components().count() {
          break;
        }
        match fs::remove_dir(root.join(&parent)) {
          Ok(()) => {}
          // Not empty, gone already, or not ours: stop walking this branch.
          Err(_) => break,
        }
        dir = parent;
      }
    }
  }
}

fn save_meta_backup(root: &Path, names: &[String]) -> io::Result<(Vec<String>, Vec<String>)> {
  let meta = backup_dir(root).join("meta");
  fs::create_dir_all(&meta).at_in(&meta, root)?;
  let (mut present, mut absent) = (Vec::new(), Vec::new());
  for name in names {
    let file = root.join(name);
    if file.is_file() {
      let saved = meta.join(name);
      if let Some(parent) = saved.parent() {
        fs::create_dir_all(parent).at_in(parent, root)?;
      }
      fs::copy(&file, saved).at_in(&file, root)?;
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
      // `run` rolls back after the journal exists. Before that the verified
      // staging files remain available for a retry.
      if read_journal(&root).ok().flatten().is_none() {
        let _ = fs::remove_dir_all(backup_dir(&root));
      }
      Err(error)
    }
  }
}

async fn run(env: &dyn UpdateEnv, request: &UpdateRequest) -> USTBLResult<()> {
  let root = &request.root;
  if env.cancel_requested() {
    return Err(cancelled_error());
  }
  // The plan was made against these ignores; others on disk mean it is stale.
  if let Some(update) = &request.ignores {
    if super::ignores::read(root) != update.from {
      return Err(InstanceError::ModpackUpdateStale.into());
    }
  }
  let remote_files: BTreeMap<&str, &RemoteFile> = request
    .remote
    .files
    .iter()
    .map(|file| (file.path.as_str(), file))
    .collect();
  if request.operations.iter().any(|operation| {
    operation.write_local && !request.merged_contents.contains_key(&operation.path)
  }) {
    return Err(InstanceError::ModpackUpdateStale.into());
  }

  // Phase 1: fetch and verify everything into staging. A matching manifest is
  // deliberately kept across pre-journal failures so a retry can reuse files.
  let reusable = match read_staging_meta(root).ok().flatten() {
    Some(meta)
      if meta.version_id == request.remote.version_id && meta.plan_hash == request.plan_hash =>
    {
      true
    }
    Some(meta) => {
      log::info!(
        "modpack install staging mismatch: version={} plan_hash={} expected_version={} expected_plan_hash={} code={}",
        meta.version_id,
        meta.plan_hash,
        request.remote.version_id,
        request.plan_hash,
        InstanceError::ModpackInstallStagingMismatch,
      );
      false
    }
    None => false,
  };
  if !reusable {
    for path in [staging_dir(root), components_dir(root)] {
      match fs::remove_dir_all(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(tag(error, &path, Some(root)))),
      }
    }
  }
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
  let total = to_fetch.len();
  let bytes_total = to_fetch.iter().map(|(file, _)| file.key.size).sum();
  let mut done = 0;
  let mut bytes_done = 0;
  if reusable {
    let checks: Vec<_> = to_fetch
      .iter()
      .map(|(file, dest)| (file.key.clone(), dest.clone()))
      .collect();
    let verified = tokio::task::spawn_blocking(move || {
      checks
        .into_iter()
        .map(|(key, dest)| {
          hash_file(&dest).is_ok_and(|(sha1, size)| sha1 == key.sha1 && size == key.size)
        })
        .collect::<Vec<_>>()
    })
    .await
    .map_err(|error| crate::error::USTBLError(error.to_string()))?;
    let mut verified = verified.into_iter();
    to_fetch.retain(|(file, dest)| {
      if verified.next().unwrap_or(false) {
        done += 1;
        bytes_done += file.key.size;
        false
      } else {
        let _ = fs::remove_file(dest);
        true
      }
    });
  }
  write_staging_meta(
    root,
    &StagingMeta {
      version_id: request.remote.version_id.clone(),
      plan_hash: request.plan_hash.clone(),
    },
  )
  .map_err(io_error)?;
  let wanted: Vec<&RemoteFile> = to_fetch.iter().map(|(file, _)| *file).collect();
  env.report(Progress {
    stage: Stage::Locating,
    done: 0,
    total: 1,
    bytes_done: 0,
    bytes_total: 0,
  });
  tokio::select! {
    biased;
    result = prepare_packs(&wanted) => result?,
    () = env.cancelled() => return Err(cancelled_error()),
  }
  // Plan every session's merged groups up front: all of its files, wherever
  // they sit in `to_fetch`, are grouped by pack offset. Each group becomes
  // one unit of fetch work, so the groups run concurrently.
  let mut planned: Vec<(Arc<PackSession>, Vec<FetchGroup>)> = Vec::new();
  let mut batches: Vec<(Arc<PackSession>, Vec<PackFetch>)> = Vec::new();
  for (file, dest) in &to_fetch {
    let FileSource::Pack(session) = &file.source else {
      continue;
    };
    match batches
      .iter_mut()
      .find(|(known, _)| Arc::ptr_eq(known, session))
    {
      Some((_, batch)) => batch.push(PackFetch {
        sha1: file.key.sha1.clone(),
        dest: dest.clone(),
      }),
      None => batches.push((
        session.clone(),
        vec![PackFetch {
          sha1: file.key.sha1.clone(),
          dest: dest.clone(),
        }],
      )),
    }
  }
  for (session, batch) in batches {
    let (located, generation) = session.located().await?;
    let groups = session.plan_groups(&located, generation, &batch)?;
    planned.push((session, groups));
  }
  let sessions: Vec<Arc<PackSession>> =
    planned.iter().map(|(session, _)| session.clone()).collect();
  env.report(Progress {
    stage: Stage::Fetching,
    done,
    total,
    bytes_done,
    bytes_total,
  });
  let fetching = Instant::now();
  // Driven by hand rather than with `buffer_unordered`: the combinator's
  // closure makes the future fail the `Send` check of the command handler.
  let mut pending = fetch_units(&to_fetch, planned).into_iter();
  let mut running = FuturesUnordered::new();
  let mut first_error = None;
  let fetching_result: USTBLResult<()> = loop {
    if first_error.is_none() && env.cancel_requested() {
      first_error = Some(cancelled_error());
    }
    while first_error.is_none() && running.len() < FETCH_CONCURRENCY {
      let Some(unit) = pending.next() else {
        break;
      };
      let files = unit.file_count();
      let bytes = match &unit {
        FetchUnit::Single(file, _) => file.key.size,
        FetchUnit::Group(_, group) => group.entries.iter().map(|file| file.entry.size).sum(),
      };
      running.push(async move {
        let result = match &unit {
          FetchUnit::Single(file, dest) => stage_file(env, dest.clone(), file).await,
          FetchUnit::Group(session, group) => {
            session.fetch_group(group).await?;
            let entries = group.entries.clone();
            tokio::task::spawn_blocking(move || {
              for entry in entries {
                let (sha1, size) = hash_file(&entry.dest).map_err(io_error)?;
                if sha1 != entry.sha1 || size != entry.entry.size {
                  let _ = fs::remove_file(&entry.dest);
                  return Err(InstanceError::ModpackUpdateVerifyFailed.into());
                }
              }
              Ok(())
            })
            .await
            .map_err(|error| crate::error::USTBLError(error.to_string()))?
          }
        };
        result.map(|()| (files, bytes))
      });
    }
    let next = tokio::select! {
      biased;
      next = running.next() => next,
      // Stop scheduling and drain what is in flight; `fetch` ends promptly.
      () = env.cancelled(), if first_error.is_none() => {
        first_error = Some(cancelled_error());
        continue;
      }
    };
    let Some(result) = next else {
      break first_error.map_or(Ok(()), Err);
    };
    match result {
      Ok((files, bytes)) => {
        done += files;
        bytes_done += bytes;
        env.report(Progress {
          stage: Stage::Fetching,
          done,
          total,
          bytes_done,
          bytes_total,
        });
      }
      Err(error) => {
        // Stop scheduling, but drain in-flight work: blocking extractors cannot
        // be aborted safely while a retry or discard starts using the staging.
        if first_error.is_none() {
          first_error = Some(error);
        }
      }
    }
  };
  // The measurement behind the "merged or not" comparison, per pack session.
  for session in sessions {
    let stats = session.stats();
    let requests = stats.requests.load(Ordering::Relaxed);
    if requests > 0 {
      log::info!(
        "modpack pack fetch: {requests} requests, {} bytes, {} merged batches, {:?} elapsed",
        stats.bytes.load(Ordering::Relaxed),
        stats.merged.load(Ordering::Relaxed),
        fetching.elapsed()
      );
    }
  }
  fetching_result?;
  for (path, bytes) in &request.merged_contents {
    let dest = staging_dir(root).join(path);
    if let Some(parent) = dest.parent() {
      fs::create_dir_all(parent)
        .at_in(parent, root)
        .map_err(io_error)?;
    }
    fs::write(&dest, bytes)
      .at_in(&dest, root)
      .map_err(io_error)?;
    if !crate::instance::helpers::modpack::text::is_text_path(path, bytes) {
      return Err(InstanceError::ModpackUpdateVerifyFailed.into());
    }
  }

  // Phase 2: write the journal and save the files the update rewrites. From
  // here on the run cannot be cancelled.
  env.begin_commit()?;
  env.report(Progress {
    stage: Stage::Applying,
    done: 0,
    total: request.operations.len(),
    bytes_done: 0,
    bytes_total: 0,
  });
  let ops: Vec<JournalOp> = request
    .operations
    .iter()
    .map(|operation| {
      let has_enabled = root.join(&operation.path).is_file();
      let has_disabled = root.join(disabled_path(&operation.path)).is_file();
      if operation.rename {
        // A flip copies its source slot into the backup; the rollback
        // restores that slot from there and clears the slot the flip wrote.
        // The source slot is the occupied one: enabled for a flip to
        // disabled, the disabled variant for a flip back.
        return JournalOp {
          path: operation.path.clone(),
          had_original: has_enabled,
          moves_disabled: has_disabled,
          writes_enabled: Some(!operation.final_disabled),
          writes_disabled: operation.final_disabled,
          pruned_dirs: Vec::new(),
        };
      }
      JournalOp {
        path: operation.path.clone(),
        had_original: has_enabled,
        moves_disabled: has_disabled,
        writes_enabled: Some(
          (operation.write.is_some() || operation.write_local) && !operation.final_disabled,
        ),
        writes_disabled: (operation.write.is_some() || operation.write_local)
          && operation.final_disabled,
        pruned_dirs: operation.prune_dirs.clone(),
      }
    })
    .collect();
  fs::create_dir_all(backup_dir(root))
    .at_in(&backup_dir(root), root)
    .map_err(io_error)?;
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
    tokio::task::spawn_blocking(move || {
      commit_files(&root_owned, &operations, &snapshot)?;
      prune_empty_dirs(&root_owned, &operations);
      Ok::<_, io::Error>(())
    })
    .await
    .map_err(|e| crate::error::USTBLError(e.to_string()))?
    .map_err(io_error)?;

    env.report(Progress {
      stage: Stage::Finalizing,
      done: 0,
      total: 1,
      bytes_done: 0,
      bytes_total: 0,
    });
    let mut origin = request.origin.clone();
    if origin.version_id.is_empty() {
      origin.loader_type = request.remote.loader.loader_type;
    }
    origin.version_id = request.remote.version_id.clone();
    let mut baseline = ModpackBaseline::build(
      &origin,
      request.remote.loader.clone(),
      request.next_baseline.clone(),
    );
    if let Some(chain) = &request.components {
      baseline = baseline.with_components(chain.clone());
    }
    baseline.save(root).await.map_err(io_error)?;
    super::release::write(root, &super::release::Release::from_remote(&request.remote))
      .await
      .map_err(io_error)?;
    if let Some(update) = &request.ignores {
      super::ignores::write(root, &update.to).map_err(io_error)?;
    }
    // Cache the new text bases fetched during planning. Merged or kept-local
    // files do not match the new baseline on disk, so `sync` below would not
    // pick them up. A failed validation is only logged.
    for (path, bytes) in &request.base_texts {
      if let Some(file) = baseline.files.iter().find(|file| &file.path == path) {
        if let Err(error) = base_store::store(root, file, bytes) {
          log::warn!("cannot store modpack text baseline for {path}: {error:?}");
        }
      }
    }
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
        bytes_done: 0,
        bytes_total: 0,
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
          "{}; rollback failed: {rollback_error}",
          error.0
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
  use crate::instance::modpack_update::release;
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
    delay_url: Option<String>,
    fetched: Mutex<Vec<String>>,
    progress: Mutex<Vec<Progress>>,
    fail_commit: bool,
    switched: Mutex<Vec<String>>,
    restored: Mutex<Vec<String>>,
    committed: Mutex<Vec<String>>,
    root: PathBuf,
    cancel: std::sync::atomic::AtomicBool,
    cancel_after_fetches: Option<usize>,
    cancel_in_commit: bool,
    committing: std::sync::atomic::AtomicBool,
  }

  #[async_trait]
  impl UpdateEnv for FakeEnv {
    fn cancel_requested(&self) -> bool {
      self.cancel.load(Ordering::SeqCst)
    }
    async fn cancelled(&self) {
      while !self.cancel_requested() {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
      }
    }
    fn begin_commit(&self) -> USTBLResult<()> {
      if self.cancel_requested() {
        return Err(cancelled_error());
      }
      self.committing.store(true, Ordering::SeqCst);
      Ok(())
    }
    async fn fetch(&self, param: DownloadParam) -> USTBLResult<()> {
      let (url, dest) = (param.src.as_str(), &param.dest);
      if self.fail_url.as_deref() == Some(url) {
        return Err(crate::error::USTBLError("network".into()));
      }
      if self.delay_url.as_deref() == Some(url) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
      }
      self.fetched.lock().unwrap().push(url.to_string());
      fs::create_dir_all(dest.parent().unwrap()).unwrap();
      fs::write(dest, &self.contents[url]).unwrap();
      // Like `DownloadTask`: a set sha1 is checked, and a mismatch fails.
      if let Some(sha1) = &param.sha1 {
        if hash_file(dest).unwrap().0 != *sha1 {
          return Err(crate::error::USTBLError("SHA1 mismatch".into()));
        }
      }
      if self.cancel_after_fetches == Some(self.fetched.lock().unwrap().len()) {
        self.cancel.store(true, Ordering::SeqCst);
      }
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
      if self.cancel_in_commit {
        self.cancel.store(true, Ordering::SeqCst);
      }
      self.committed.lock().unwrap().push(version_id.into());
      Ok(())
    }
    fn report(&self, progress: Progress) {
      self.progress.lock().unwrap().push(progress);
    }
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
      disabled: false,
      once: false,
    }
  }

  fn request(
    root: &Root,
    files: Vec<RemoteFile>,
    operations: Vec<Operation>,
    new_loader: &str,
  ) -> UpdateRequest {
    let next_baseline = files
      .iter()
      .map(|file| {
        crate::instance::helpers::modpack::baseline::BaselineFile::managed(
          &file.path,
          file.key.clone(),
          false,
        )
      })
      .collect();
    UpdateRequest {
      ignores: None,
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
        rules: Vec::new(),
        rules_manifest: false,
        disabled_authoritative: false,
        path_index: Arc::new(OnceLock::new()),
      },
      origin: origin("1"),
      previous_loader: loader("0.15.0"),
      components: None,
      restore_components: false,
      merged_contents: BTreeMap::new(),
      base_texts: BTreeMap::new(),
      next_baseline,
      plan_hash: "test-plan".into(),
    }
  }

  fn write_op(file: &RemoteFile) -> Operation {
    Operation {
      path: file.path.clone(),
      write: Some(file.key.clone()),
      final_disabled: false,
      rename: false,
      write_local: false,
      prune_dirs: Vec::new(),
    }
  }

  fn remove_op(path: &str) -> Operation {
    Operation {
      path: path.into(),
      write: None,
      final_disabled: false,
      rename: false,
      write_local: false,
      prune_dirs: Vec::new(),
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

  fn ignores_with(path: &str, start: usize, end: usize) -> super::super::ignores::Ignores {
    use super::super::ignores::{IgnoreEntry, Ignores, Region};
    Ignores {
      files: BTreeMap::from([(
        path.to_string(),
        IgnoreEntry {
          base_sha1: "aa".into(),
          whole: false,
          regions: vec![Region { start, end }],
        },
      )]),
      ..Default::default()
    }
  }

  fn ignores_update(
    from: &super::super::ignores::Ignores,
    to: &super::super::ignores::Ignores,
  ) -> Option<super::super::ignores::IgnoresUpdate> {
    Some(super::super::ignores::IgnoresUpdate {
      from: from.clone(),
      to: to.clone(),
    })
  }

  #[tokio::test]
  async fn the_new_ignores_are_written_with_the_update_and_an_empty_set_removes_the_file() {
    use super::super::ignores;
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let first = ignores_with("config/a.toml", 1, 2);
    let none = ignores::Ignores::default();
    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    update.ignores = ignores_update(&none, &first);
    apply(&env, update).await.unwrap();
    assert_eq!(ignores::read(&root.0), first);

    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    update.origin = origin("2");
    update.ignores = ignores_update(&first, &none);
    apply(&env, update).await.unwrap();
    assert!(!root.0.join(UPDATE_DIR).join(ignores::IGNORES_FILE).exists());
  }

  #[tokio::test]
  async fn a_failed_update_puts_the_ignores_back() {
    use super::super::ignores;
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      fail_commit: true,
      ..Default::default()
    };
    let before = ignores_with("config/a.toml", 1, 2);
    ignores::write(&root.0, &before).unwrap();
    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    update.ignores = ignores_update(&before, &ignores_with("config/a.toml", 5, 6));
    assert!(apply(&env, update).await.is_err());
    assert_eq!(ignores::read(&root.0), before);

    // Without a file before, the failed update leaves none behind.
    ignores::remove(&root.0);
    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    update.ignores = ignores_update(&ignores::Ignores::default(), &before);
    assert!(apply(&env, update).await.is_err());
    assert!(!root.0.join(UPDATE_DIR).join(ignores::IGNORES_FILE).exists());
  }

  #[tokio::test]
  async fn ignores_changed_since_the_plan_make_the_update_stale() {
    use super::super::ignores;
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    ignores::write(&root.0, &ignores_with("config/a.toml", 1, 2)).unwrap();
    let mut update = request(&root, Vec::new(), Vec::new(), "0.16.0");
    update.ignores = ignores_update(&ignores::Ignores::default(), &ignores::Ignores::default());
    assert!(apply(&env, update).await.is_err());
    assert!(env.committed.lock().unwrap().is_empty());
    assert_eq!(root.read("pack.json").as_deref(), Some("client-v1"));
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
    // old.jar is updated while its disabled variant is on disk too; the
    // update clears both slots and writes the new enabled file.
    let operations = vec![
      write_op(&new),
      write_op(&updated),
      remove_op("mods/gone.jar"),
    ];

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
    // A failure before the journal keeps verified files for the next attempt.
    assert!(staging_dir(&root.0).exists());
    assert!(env.switched.lock().unwrap().is_empty());
  }

  #[tokio::test]
  async fn a_run_cancelled_before_it_starts_fetches_and_writes_nothing() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let file = url_file("mods/a.jar", "a", &mut env);
    let operations = vec![write_op(&file), remove_op("mods/gone.jar")];
    env.cancel.store(true, Ordering::SeqCst);

    let error = apply(&env, request(&root, vec![file], operations, "0.15.0"))
      .await
      .unwrap_err();

    assert_eq!(
      error.0,
      InstanceError::ModpackInstallInterrupted.to_string()
    );
    assert!(env.fetched.lock().unwrap().is_empty());
    assert!(!env.committing.load(Ordering::SeqCst));
    assert!(!journal_path(&root.0).exists());
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
  }

  #[tokio::test]
  async fn a_cancel_while_fetching_stops_before_the_journal_even_when_every_file_arrived() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let first = url_file("mods/a.jar", "a", &mut env);
    let second = url_file("mods/b.jar", "b", &mut env);
    env.cancel_after_fetches = Some(1);
    let operations = vec![
      write_op(&first),
      write_op(&second),
      remove_op("mods/gone.jar"),
    ];

    assert!(apply(
      &env,
      request(&root, vec![first, second], operations, "0.15.0")
    )
    .await
    .is_err());

    assert!(!env.committing.load(Ordering::SeqCst));
    assert!(!journal_path(&root.0).exists());
    assert!(env.committed.lock().unwrap().is_empty());
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(root.read("mods/a.jar"), None);
  }

  #[tokio::test]
  async fn a_cancel_after_the_commit_point_does_not_stop_the_run() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      cancel_in_commit: true,
      ..Default::default()
    };
    let file = url_file("mods/a.jar", "a", &mut env);
    let operations = vec![write_op(&file), remove_op("mods/gone.jar")];
    let update = request(&root, vec![file], operations, "0.15.0");

    apply(&env, update).await.unwrap();

    assert!(env.cancel_requested());
    assert!(env.committing.load(Ordering::SeqCst));
    assert_eq!(root.read("mods/a.jar").as_deref(), Some("a"));
    assert_eq!(root.read("mods/gone.jar"), None);
  }

  #[tokio::test]
  async fn a_retry_reuses_verified_staging_files_for_the_same_plan() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let good = url_file("mods/a.jar", "a", &mut env);
    let bad = url_file("mods/b.jar", "b", &mut env);
    env.fail_url = Some("https://example.com/mods/b.jar".into());
    let operations = vec![write_op(&good), write_op(&bad)];

    assert!(apply(
      &env,
      request(
        &root,
        vec![good.clone(), bad.clone()],
        operations.clone(),
        "0.16.0"
      )
    )
    .await
    .is_err());
    assert_eq!(
      env.fetched.lock().unwrap().as_slice(),
      ["https://example.com/mods/a.jar"]
    );

    env.fail_url = None;
    env.progress.lock().unwrap().clear();
    apply(&env, request(&root, vec![good, bad], operations, "0.16.0"))
      .await
      .unwrap();
    let fetching: Vec<_> = env
      .progress
      .lock()
      .unwrap()
      .iter()
      .copied()
      .filter(|progress| progress.stage == Stage::Fetching)
      .collect();
    assert_eq!((fetching[0].done, fetching[0].bytes_done), (1, 1));
    assert_eq!((fetching[0].total, fetching[0].bytes_total), (2, 2));
    let last = fetching.last().unwrap();
    assert_eq!((last.done, last.bytes_done), (2, 2));
    assert_eq!(
      env.fetched.lock().unwrap().as_slice(),
      [
        "https://example.com/mods/a.jar",
        "https://example.com/mods/b.jar"
      ]
    );
  }

  #[tokio::test]
  async fn a_failed_fetch_drains_in_flight_writes_before_releasing_the_lock() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      fail_url: Some("https://example.com/mods/bad.jar".into()),
      delay_url: Some("https://example.com/mods/slow.jar".into()),
      ..Default::default()
    };
    let bad = url_file("mods/bad.jar", "bad", &mut env);
    let slow = url_file("mods/slow.jar", "slow", &mut env);
    let operations = vec![write_op(&bad), write_op(&slow)];
    assert!(
      apply(&env, request(&root, vec![bad, slow], operations, "0.15.0"))
        .await
        .is_err()
    );
    assert_eq!(
      fs::read(staging_dir(&root.0).join("mods/slow.jar")).unwrap(),
      b"slow"
    );
    assert!(UpdateGuard::acquire(&root.0).is_ok());
    assert_eq!(root.read("mods/slow.jar"), None);
  }

  #[tokio::test]
  async fn a_changed_plan_clears_old_staging_before_fetching() {
    let root = setup();
    fs::create_dir_all(staging_dir(&root.0).join("mods")).unwrap();
    fs::write(staging_dir(&root.0).join("mods/old.jar"), "old").unwrap();
    write_staging_meta(
      &root.0,
      &StagingMeta {
        version_id: "old-version".into(),
        plan_hash: "old-plan".into(),
      },
    )
    .unwrap();
    let mut env = FakeEnv {
      root: root.0.clone(),
      fail_url: Some("https://example.com/mods/new.jar".into()),
      ..Default::default()
    };
    let new = url_file("mods/new.jar", "new", &mut env);

    let operations = vec![write_op(&new)];
    assert!(apply(&env, request(&root, vec![new], operations, "0.16.0"))
      .await
      .is_err());
    assert!(!staging_dir(&root.0).join("mods/old.jar").exists());
    assert_eq!(
      read_staging_meta(&root.0).unwrap().unwrap(),
      StagingMeta {
        version_id: "2".into(),
        plan_hash: "test-plan".into(),
      }
    );
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

    assert_eq!(error.0, "SHA1 mismatch");
    assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
    assert_eq!(root.read("mods/a.jar"), None);
  }

  #[tokio::test]
  async fn a_download_of_the_wrong_size_is_rejected_without_hashing_it_again() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let mut file = url_file("mods/a.jar", "a", &mut env);
    // The sha1 matches, so only the size check can catch this.
    file.key.size += 1;
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
    assert!(!staging_dir(&root.0).join("mods/a.jar").exists());
  }

  #[tokio::test]
  async fn corrupt_archive_entries_are_rejected_while_extracting() {
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
    for key in [
      sha1_of(b"something else"),
      FileKey {
        size: 99,
        ..sha1_of(b"from-zip")
      },
    ] {
      let file = RemoteFile {
        path: "config/a.toml".into(),
        key,
        source: FileSource::ArchiveEntry {
          archive: archive.clone(),
          entry: "overrides/config/a.toml".into(),
        },
        disabled: false,
        once: false,
      };
      let operations = vec![write_op(&file), remove_op("mods/gone.jar")];

      let error = apply(&env, request(&root, vec![file], operations, "0.15.0"))
        .await
        .unwrap_err();

      assert_eq!(
        error.0,
        InstanceError::ModpackUpdateVerifyFailed.to_string()
      );
      assert_eq!(root.read("mods/gone.jar").as_deref(), Some("gone"));
      assert_eq!(root.read("config/a.toml"), None);
      assert!(!staging_dir(&root.0).join("config/a.toml").exists());
    }
  }

  #[tokio::test]
  async fn retry_rechecks_sha1_and_size_and_fetches_only_missing_or_bad_files() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let files = vec![
      url_file("mods/good.jar", "good", &mut env),
      url_file("mods/hash.jar", "hash", &mut env),
      url_file("mods/size.jar", "size", &mut env),
      url_file("mods/missing.jar", "missing", &mut env),
      url_file("manifest.json", "{}", &mut env),
    ];
    write_staging_meta(
      &root.0,
      &StagingMeta {
        version_id: "2".into(),
        plan_hash: "test-plan".into(),
      },
    )
    .unwrap();
    root.write(".ustbl-update/staging/mods/good.jar", "good");
    root.write(".ustbl-update/staging/mods/hash.jar", "evil");
    root.write(".ustbl-update/staging/mods/size.jar", "s");
    root.write(".ustbl-update/staging/manifest.json", "{}");
    let operations = files.iter().map(write_op).collect();
    apply(&env, request(&root, files, operations, "0.15.0"))
      .await
      .unwrap();
    let mut fetched = env.fetched.lock().unwrap().clone();
    fetched.sort();
    assert_eq!(
      fetched,
      [
        "https://example.com/mods/hash.jar",
        "https://example.com/mods/missing.jar",
        "https://example.com/mods/size.jar",
      ]
    );
    assert_eq!(root.read("manifest.json").as_deref(), Some("{}"));
  }

  #[tokio::test]
  async fn changing_either_version_or_plan_discards_staged_components_and_files() {
    for (version, plan) in [("1", "test-plan"), ("2", "old-plan")] {
      let root = setup();
      let mut env = FakeEnv {
        root: root.0.clone(),
        ..Default::default()
      };
      let file = url_file("mods/a.jar", "a", &mut env);
      write_staging_meta(
        &root.0,
        &StagingMeta {
          version_id: version.into(),
          plan_hash: plan.into(),
        },
      )
      .unwrap();
      root.write(".ustbl-update/staging/mods/a.jar", "a");
      root.write(".ustbl-update/components/old.json", "old");
      apply(
        &env,
        request(&root, vec![file.clone()], vec![write_op(&file)], "0.15.0"),
      )
      .await
      .unwrap();
      assert_eq!(env.fetched.lock().unwrap().len(), 1);
      assert!(!components_dir(&root.0).exists());
    }
  }

  #[tokio::test]
  async fn component_staging_survives_a_pre_journal_failure_and_is_verified_on_retry() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let mut update = request(&root, Vec::new(), Vec::new(), "0.15.0");
    with_component_pack(&mut update, &mut env);
    let bad = url_file("patches/other.json", "other", &mut env);
    update
      .remote
      .component_pack
      .as_mut()
      .unwrap()
      .files
      .push(bad);
    env.fail_url = Some("https://example.com/patches/other.json".into());
    assert!(apply(&env, update).await.is_err());
    assert_eq!(recover(&root.0).unwrap(), Recovery::Nothing);
    assert!(components_dir(&root.0)
      .join("patches/com.example.json")
      .exists());
    env.fail_url = None;
    let mut update = request(&root, Vec::new(), Vec::new(), "0.15.0");
    with_component_pack(&mut update, &mut env);
    let other = url_file("patches/other.json", "other", &mut env);
    update
      .remote
      .component_pack
      .as_mut()
      .unwrap()
      .files
      .push(other);
    apply(&env, update).await.unwrap();
    assert_eq!(
      env
        .fetched
        .lock()
        .unwrap()
        .iter()
        .filter(|url| url.ends_with("com.example.json"))
        .count(),
      1
    );
    assert_eq!(env.restored.lock().unwrap().len(), 2);
  }

  #[test]
  fn journal_recovery_clears_staging_for_both_rollback_and_committed_transactions() {
    for state in [JournalState::Committing, JournalState::Committed] {
      let root = setup();
      write_staging_meta(
        &root.0,
        &StagingMeta {
          version_id: "2".into(),
          plan_hash: "test-plan".into(),
        },
      )
      .unwrap();
      root.write(".ustbl-update/staging/mods/a.jar", "a");
      root.write(".ustbl-update/components/a.json", "a");
      interrupted(&root, state);
      assert_ne!(recover(&root.0).unwrap(), Recovery::Nothing);
      assert!(!staging_dir(&root.0).exists());
      assert!(!staging_meta_path(&root.0).exists());
      assert!(!components_dir(&root.0).exists());
    }
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
          writes_enabled: Some(true),
          writes_disabled: false,
          pruned_dirs: Vec::new(),
        },
        JournalOp {
          path: "mods/new.jar".into(),
          had_original: false,
          moves_disabled: false,
          writes_enabled: Some(true),
          writes_disabled: false,
          pruned_dirs: Vec::new(),
        },
        JournalOp {
          path: "mods/gone.jar".into(),
          had_original: true,
          moves_disabled: false,
          writes_enabled: Some(false),
          writes_disabled: false,
          pruned_dirs: Vec::new(),
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
    assert_eq!(
      error.0,
      InstanceError::ModpackUpdateUnlinkPending.to_string()
    );
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
  async fn rename_ops_flip_variants_and_roll_back_cleanly() {
    let root = setup();
    // A managed enabled file flips to disabled; a disabled one flips back.
    root.write("mods/off.jar.disabled", "off");
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let operations = vec![
      Operation {
        path: "mods/old.jar".into(),
        write: None,
        final_disabled: true,
        rename: true,
        write_local: false,
        prune_dirs: Vec::new(),
      },
      Operation {
        path: "mods/off.jar".into(),
        write: None,
        final_disabled: false,
        rename: true,
        write_local: false,
        prune_dirs: Vec::new(),
      },
    ];
    let mut update = request(&root, Vec::new(), operations, "0.16.0");
    // The baseline records the flipped states.
    update.next_baseline = vec![
      crate::instance::helpers::modpack::baseline::BaselineFile::managed(
        "mods/old.jar",
        sha1_of(b"old"),
        true,
      ),
      crate::instance::helpers::modpack::baseline::BaselineFile::managed(
        "mods/off.jar",
        sha1_of(b"off"),
        false,
      ),
    ];
    apply(&env, update).await.unwrap();

    assert_eq!(root.read("mods/old.jar"), None);
    assert_eq!(root.read("mods/old.jar.disabled").as_deref(), Some("old"));
    assert_eq!(root.read("mods/off.jar").as_deref(), Some("off"));
    assert_eq!(root.read("mods/off.jar.disabled"), None);

    assert!(roll_back_last(&root.0).unwrap());
    assert_eq!(root.read("mods/old.jar").as_deref(), Some("old"));
    assert_eq!(root.read("mods/old.jar.disabled"), None);
    assert_eq!(root.read("mods/off.jar"), None);
    assert_eq!(root.read("mods/off.jar.disabled").as_deref(), Some("off"));
  }

  #[tokio::test]
  async fn delete_rule_ops_prune_empty_dirs_and_the_rollback_recreates_them() {
    let root = Root::new();
    root.write("mods/old/a.jar", "a");
    root.write("mods/old/sub/b.jar.disabled", "b");
    root.write("mods/old/keep.jar", "keep");
    root.write("ustblcfg.json", "cfg-v1");
    root.write("pack.json", "client-v1");
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let delete = |path: &str| Operation {
      path: path.into(),
      write: None,
      final_disabled: false,
      rename: false,
      write_local: false,
      prune_dirs: vec!["mods/old".into()],
    };
    let operations = vec![delete("mods/old/a.jar"), delete("mods/old/sub/b.jar")];

    apply(&env, request(&root, Vec::new(), operations, "0.16.0"))
      .await
      .unwrap();

    assert_eq!(root.read("mods/old/a.jar"), None);
    assert_eq!(root.read("mods/old/sub/b.jar.disabled"), None);
    // sub emptied out and went with its file; mods/old stays: keep.jar lives.
    assert!(!root.0.join("mods/old/sub").exists());
    assert_eq!(root.read("mods/old/keep.jar").as_deref(), Some("keep"));

    assert!(roll_back_last(&root.0).unwrap());
    assert_eq!(root.read("mods/old/a.jar").as_deref(), Some("a"));
    assert_eq!(
      root.read("mods/old/sub/b.jar.disabled").as_deref(),
      Some("b")
    );
  }

  #[tokio::test]
  async fn a_mid_apply_failure_rolls_back_the_new_actions() {
    let root = Root::new();
    root.write("mods/old/a.jar", "a");
    root.write("mods/dormant.jar", "stale");
    root.write("mods/flip.jar", "flip");
    root.write("ustblcfg.json", "cfg-v1");
    root.write("pack.json", "client-v1");
    let mut env = FakeEnv {
      root: root.0.clone(),
      fail_commit: true,
      ..Default::default()
    };
    // A remanage conflict answered "take remote": stale content is replaced.
    let remanaged = url_file("mods/dormant.jar", "fresh", &mut env);
    let delete = Operation {
      path: "mods/old/a.jar".into(),
      write: None,
      final_disabled: false,
      rename: false,
      write_local: false,
      prune_dirs: vec!["mods/old".into()],
    };
    let flip = Operation {
      path: "mods/flip.jar".into(),
      write: None,
      final_disabled: true,
      rename: true,
      write_local: false,
      prune_dirs: Vec::new(),
    };
    let operations = vec![write_op(&remanaged), delete, flip];
    let old_release = release::Release {
      schema: release::RELEASE_SCHEMA,
      version_id: "1".into(),
      label: "old".into(),
      files: Vec::new(),
      rules: Vec::new(),
      rules_manifest: false,
    };
    release::write(&root.0, &old_release).await.unwrap();

    assert!(
      apply(&env, request(&root, vec![remanaged], operations, "0.16.0"))
        .await
        .is_err()
    );

    assert_eq!(root.read("mods/old/a.jar").as_deref(), Some("a"));
    assert_eq!(root.read("mods/dormant.jar").as_deref(), Some("stale"));
    assert_eq!(root.read("mods/flip.jar").as_deref(), Some("flip"));
    assert_eq!(root.read("mods/flip.jar.disabled"), None);
    assert_eq!(release::read(&root.0), Some(old_release));
    assert!(root.0.join("mods/old").exists());
    assert!(!journal_path(&root.0).exists());
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
      disabled: false,
      once: false,
    };
    let operations = vec![write_op(&file)];

    apply(&env, request(&root, vec![file], operations, "0.15.0"))
      .await
      .unwrap();

    assert_eq!(root.read("config/a.toml").as_deref(), Some("from-zip"));
    assert!(env.switched.lock().unwrap().is_empty());
  }

  #[tokio::test]
  async fn a_disabled_ship_is_written_at_the_disabled_name() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let mut off = url_file("mods/off.jar", "off", &mut env);
    off.disabled = true;
    let on = url_file("mods/on.jar", "on", &mut env);
    let mut operations = vec![write_op(&off), write_op(&on)];
    operations[0].final_disabled = true;

    let mut update = request(&root, vec![off, on], operations, "0.15.0");
    update.remote.disabled_authoritative = true;

    apply(&env, update).await.unwrap();

    assert_eq!(root.read("mods/off.jar.disabled").as_deref(), Some("off"));
    assert_eq!(root.read("mods/off.jar"), None);
    assert_eq!(root.read("mods/on.jar").as_deref(), Some("on"));
    assert_eq!(root.read("mods/on.jar.disabled"), None);
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

  /// A platform whose pack holds `files`, keyed by their real sha1. `gaps[i]`
  /// inserts that many filler bytes between entry i and entry i+1, so the
  /// groups can be steered around the merge thresholds. Range reads are
  /// counted, and the peak number of them in flight at once is tracked; each
  /// read sleeps briefly so the peak reflects real overlap.
  struct MultiFilePack {
    pack: Vec<u8>,
    entries: BTreeMap<String, crate::instance::modpack_update::packs::EntryLocation>,
    ranges: Mutex<Vec<(u64, u64)>>,
    /// Version id and number of sha1s of every locate.
    locates: Mutex<Vec<(i64, usize)>>,
    flying: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
  }

  impl MultiFilePack {
    fn new(files: &[(&str, &[u8])]) -> Self {
      Self::spaced(files, &[])
    }

    fn spaced(files: &[(&str, &[u8])], gaps: &[usize]) -> Self {
      use crate::instance::modpack_update::packs::{EntryLocation, DEFLATED};
      use std::io::Write;
      let mut pack = Vec::new();
      let mut entries = BTreeMap::new();
      for (index, (sha1, data)) in files.iter().enumerate() {
        let mut encoder =
          flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut crc = flate2::Crc::new();
        crc.update(data);
        entries.insert(
          sha1.to_string(),
          EntryLocation {
            pack: 1,
            data_offset: pack.len() as u64,
            compress_type: DEFLATED,
            compressed_size: compressed.len() as u64,
            size: data.len() as u64,
            crc32: crc.sum(),
          },
        );
        pack.extend(compressed);
        if let Some(gap) = gaps.get(index) {
          pack.extend(vec![0u8; *gap]);
        }
      }
      Self {
        pack,
        entries,
        ranges: Mutex::default(),
        locates: Mutex::default(),
        flying: std::sync::atomic::AtomicUsize::new(0),
        peak: std::sync::atomic::AtomicUsize::new(0),
      }
    }
  }

  #[async_trait]
  impl crate::instance::modpack_update::packs::PackTransport for MultiFilePack {
    async fn locate(
      &self,
      version_id: i64,
      sha1s: &[String],
    ) -> Result<
      crate::instance::modpack_update::packs::Located,
      crate::instance::modpack_update::packs::TransportError,
    > {
      use crate::instance::modpack_update::packs::{Located, PackLocation};
      self.locates.lock().unwrap().push((version_id, sha1s.len()));
      Ok(Located {
        packs: vec![PackLocation {
          id: 1,
          url: "https://cloud.example/p".into(),
          method: "GET".into(),
          headers: Default::default(),
        }],
        entries: sha1s
          .iter()
          .map(|sha1| (sha1.clone(), self.entries[sha1]))
          .collect(),
      })
    }
    async fn read_range(
      &self,
      _: &crate::instance::modpack_update::packs::PackLocation,
      start: u64,
      length: u64,
      dest: &Path,
    ) -> Result<(), crate::instance::modpack_update::packs::TransportError> {
      let flying = self
        .flying
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
      self
        .peak
        .fetch_max(flying, std::sync::atomic::Ordering::SeqCst);
      self.ranges.lock().unwrap().push((start, length));
      // Give the other groups a chance to overlap with this read.
      tokio::time::sleep(std::time::Duration::from_millis(20)).await;
      fs::create_dir_all(dest.parent().unwrap()).unwrap();
      let (start, length) = (start as usize, length as usize);
      fs::write(dest, &self.pack[start..start + length]).unwrap();
      self
        .flying
        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
      Ok(())
    }
  }

  /// Pack files for `contents` on `session`, keyed by their real sha1s, and
  /// the matching RemoteFile list.
  fn pack_remote_files(contents: &[(&str, &[u8])], session: &Arc<PackSession>) -> Vec<RemoteFile> {
    contents
      .iter()
      .map(|(path, data)| RemoteFile {
        path: path.to_string(),
        key: sha1_of(data),
        source: FileSource::Pack(session.clone()),
        disabled: false,
        once: false,
      })
      .collect()
  }

  fn sha1s_of<'a>(contents: &[(&str, &'a [u8])]) -> Vec<(String, &'a [u8])> {
    contents
      .iter()
      .map(|(_, data)| (hex::encode(Sha1::digest(data)), *data))
      .collect()
  }

  #[tokio::test]
  async fn neighbouring_pack_files_are_fetched_with_one_range_request() {
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let contents: [(&str, &[u8]); 3] = [
      ("mods/a.jar", b"alpha"),
      ("mods/b.jar", b"beta"),
      ("mods/c.jar", b"gamma"),
    ];
    let sha1s = sha1s_of(&contents);
    let transport = Arc::new(MultiFilePack::new(
      &sha1s
        .iter()
        .map(|(sha1, data)| (sha1.as_str(), *data))
        .collect::<Vec<(&str, &[u8])>>(),
    ));
    let session = PackSession::new(transport.clone(), 3);
    let remote_files = pack_remote_files(&contents, &session);
    let operations = remote_files.iter().map(write_op).collect();

    apply(&env, request(&root, remote_files, operations, "0.15.0"))
      .await
      .unwrap();

    // Three entries back to back in one pack: a single merged range.
    assert_eq!(transport.ranges.lock().unwrap().len(), 1);
    assert_eq!(root.read("mods/a.jar").as_deref(), Some("alpha"));
    assert_eq!(root.read("mods/b.jar").as_deref(), Some("beta"));
    assert_eq!(root.read("mods/c.jar").as_deref(), Some("gamma"));
  }

  #[tokio::test]
  async fn pack_files_interrupted_by_another_source_still_merge() {
    let root = setup();
    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    // The two pack files sit at the ends of the operation list, with a
    // download between them: they are not neighbours in `to_fetch`, but they
    // are neighbours in the pack.
    let contents: [(&str, &[u8]); 2] = [("mods/a.jar", b"alpha"), ("mods/c.jar", b"gamma")];
    let sha1s = sha1s_of(&contents);
    let transport = Arc::new(MultiFilePack::new(
      &sha1s
        .iter()
        .map(|(sha1, data)| (sha1.as_str(), *data))
        .collect::<Vec<(&str, &[u8])>>(),
    ));
    let session = PackSession::new(transport.clone(), 3);
    let pack_files = pack_remote_files(&contents, &session);
    let download = url_file("mods/b.jar", "beta", &mut env);
    let remote_files = vec![pack_files[0].clone(), download, pack_files[1].clone()];
    let operations = remote_files.iter().map(write_op).collect();

    apply(&env, request(&root, remote_files, operations, "0.15.0"))
      .await
      .unwrap();

    // Both pack files are read with one merged request despite the download
    // between them in the operation list.
    assert_eq!(transport.ranges.lock().unwrap().len(), 1);
    assert_eq!(root.read("mods/a.jar").as_deref(), Some("alpha"));
    assert_eq!(root.read("mods/b.jar").as_deref(), Some("beta"));
    assert_eq!(root.read("mods/c.jar").as_deref(), Some("gamma"));
  }

  #[tokio::test]
  async fn distant_groups_are_fetched_concurrently() {
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    // Six entries with a gap over MERGE_GAP between every two: six groups.
    let contents: [(&str, &[u8]); 6] = [
      ("mods/a.jar", b"alpha"),
      ("mods/b.jar", b"beta"),
      ("mods/c.jar", b"gamma"),
      ("mods/d.jar", b"delta"),
      ("mods/e.jar", b"epsilon"),
      ("mods/f.jar", b"zeta"),
    ];
    let sha1s = sha1s_of(&contents);
    let gap = crate::instance::modpack_update::packs::MERGE_GAP as usize + 1;
    let transport = Arc::new(MultiFilePack::spaced(
      &sha1s
        .iter()
        .map(|(sha1, data)| (sha1.as_str(), *data))
        .collect::<Vec<(&str, &[u8])>>(),
      &[gap, gap, gap, gap, gap],
    ));
    let session = PackSession::new(transport.clone(), 3);
    let remote_files = pack_remote_files(&contents, &session);
    let operations = remote_files.iter().map(write_op).collect();

    apply(&env, request(&root, remote_files, operations, "0.15.0"))
      .await
      .unwrap();

    let ranges = transport.ranges.lock().unwrap();
    // No merging possible across the gaps: one read per entry.
    assert_eq!(ranges.len(), 6);
    // But the groups run side by side.
    let peak = transport.peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!(peak > 1, "expected concurrent groups, peak was {peak}");
    assert!(peak <= FETCH_CONCURRENCY);
    drop(ranges);
    for (path, data) in contents {
      assert_eq!(
        root.read(path).as_deref(),
        Some(std::str::from_utf8(data).unwrap())
      );
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
      disabled: false,
      once: false,
    };
    let operations = vec![write_op(&file)];

    apply(&env, request(&root, vec![file], operations, "0.15.0"))
      .await
      .unwrap();

    assert_eq!(root.read("config/p.toml").as_deref(), Some("from-pack"));
  }

  const CACHED_FILES: [(&str, &[u8]); 3] = [
    ("mods/a.jar", b"alpha"),
    ("mods/b.jar", b"beta"),
    ("mods/c.jar", b"gamma"),
  ];
  const CACHED_COMPONENTS: [(&str, &[u8]); 2] = [
    ("patches/com.example.json", b"patch-bytes"),
    ("libraries/l.jar", b"lib"),
  ];

  /// A pack holding every file of `CACHED_FILES` and `CACHED_COMPONENTS`.
  fn cached_pack() -> Arc<MultiFilePack> {
    let sha1s: Vec<_> = sha1s_of(&CACHED_FILES)
      .into_iter()
      .chain(sha1s_of(&CACHED_COMPONENTS))
      .collect();
    Arc::new(MultiFilePack::new(
      &sha1s
        .iter()
        .map(|(sha1, data)| (sha1.as_str(), *data))
        .collect::<Vec<(&str, &[u8])>>(),
    ))
  }

  /// An update restoring the component pack whose files are pack files of
  /// version 3, except the second file of each list, which is in version 4.
  fn two_version_request(root: &Root, transport: &Arc<MultiFilePack>) -> UpdateRequest {
    let three = PackSession::new(transport.clone(), 3);
    let four = PackSession::new(transport.clone(), 4);
    let mut files = pack_remote_files(&CACHED_FILES, &three);
    files[1].source = FileSource::Pack(four.clone());
    let mut component = pack_remote_files(&CACHED_COMPONENTS, &three);
    component[1].source = FileSource::Pack(four);
    let operations = files.iter().map(write_op).collect();
    let mut update = request(root, files, operations, "0.16.0");
    update.remote.component_pack = Some(ComponentPack {
      mmc_pack: serde_json::json!({ "components": [], "formatVersion": 1 }),
      instance_cfg: BTreeMap::new(),
      files: component,
    });
    update.restore_components = true;
    update
  }

  /// `remote` written to the remote cache and read back the way
  /// `load_remote_cache` does, with sessions on `transport`. Also returns how
  /// many sessions were made.
  fn through_remote_cache(
    root: &Root,
    remote: &RemoteModpack,
    transport: &Arc<MultiFilePack>,
  ) -> (RemoteModpack, usize) {
    use crate::instance::modpack_update::sources::RemoteCache;
    let cache = RemoteCache::new(&origin("2"), &root.0, remote, None);
    let bytes = serde_json::to_vec(&cache).unwrap();
    let back: RemoteCache = serde_json::from_slice(&bytes).unwrap();
    let made = std::cell::Cell::new(0);
    let remote = RemoteModpack::from_stored(back.remote, |version_id| {
      made.set(made.get() + 1);
      PackSession::new(transport.clone(), version_id)
    });
    (remote, made.get())
  }

  #[test]
  fn a_remote_read_back_from_the_cache_has_one_pack_session_per_version() {
    let root = Root::new();
    let transport = cached_pack();
    let update = two_version_request(&root, &transport);

    let (remote, made) = through_remote_cache(&root, &update.remote, &transport);

    assert_eq!(made, 2);
    let files: Vec<&RemoteFile> = remote
      .files
      .iter()
      .chain(&remote.component_pack.as_ref().unwrap().files)
      .collect();
    let sessions: Vec<_> = pack_sessions(&files)
      .into_iter()
      .map(|(session, wanted)| (session.version_id(), wanted.len()))
      .collect();
    assert_eq!(sessions, [(3, 3), (4, 2)]);
  }

  #[tokio::test]
  async fn an_update_read_back_from_the_cache_locates_each_version_once() {
    let root = setup();
    let env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let transport = cached_pack();
    let mut update = two_version_request(&root, &transport);
    update.remote = through_remote_cache(&root, &update.remote, &transport).0;

    apply(&env, update).await.unwrap();

    assert_eq!(
      transport.locates.lock().unwrap().as_slice(),
      [(3, 3), (4, 2)]
    );
    for (path, data) in CACHED_FILES {
      assert_eq!(
        root.read(path).as_deref(),
        Some(std::str::from_utf8(data).unwrap())
      );
    }
    assert_eq!(
      env.restored.lock().unwrap().as_slice(),
      [
        "patches/com.example.json=patch-bytes",
        "libraries/l.jar=lib"
      ]
    );
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

  /// "Take remote" on every conflict leaves no modified file. The 2 that
  /// remain are the two mods `setup()` puts in the instance (`mods/old.jar`,
  /// `mods/gone.jar`), which the new release does not list, so they read as
  /// added files.
  #[tokio::test]
  async fn taking_every_remote_side_leaves_only_preexisting_added_files() {
    use crate::instance::helpers::modpack::baseline::BaselineFile;
    use crate::instance::modpack_update::commands::selected_hunk_contents;
    use crate::instance::modpack_update::diff::ConflictChoice;
    use crate::instance::modpack_update::plan::{
      count_local_changes, prepare_with_contents, resolve,
    };

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
    .map(|(path, content)| BaselineFile::managed(path, sha1_of(content.as_bytes()), false))
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
    update.operations = resolve(&prepared, &choices);
    update.next_baseline = prepared.next_baseline.clone();

    apply(&env, update).await.unwrap();

    let baseline = ModpackBaseline::load(&root.0).await.unwrap();
    assert_eq!(count_local_changes(&root.0, &baseline).await.unwrap(), 2);
    assert_eq!(root.read("mods/a.jar").as_deref(), Some("a2"));
    assert_eq!(root.read("mods/b.jar").as_deref(), Some("b2"));
    assert_eq!(root.read("config/c.toml").as_deref(), Some(remote_text));
  }

  /// A text conflict answered "keep local" block by block still has its new
  /// base cached, so the local-change list can show the line diff instead of
  /// `BaseMissing`.
  #[tokio::test]
  async fn keeping_local_hunks_still_caches_the_new_base() {
    use crate::instance::helpers::modpack::baseline::BaselineFile;
    use crate::instance::modpack_update::commands::selected_hunk_contents;
    use crate::instance::modpack_update::diff::ConflictChoice;
    use crate::instance::modpack_update::plan::{
      count_local_changes, local_changes, prepare_with_contents, resolve,
    };

    let root = setup();
    root.write("config/c.toml", "x = 2\ny = 1\n");
    let base_text = "x = 1\ny = 1\n";
    let remote_text = "x = 3\ny = 1\n";
    let key = sha1_of(base_text.as_bytes());
    let installed = ModpackBaseline::build(
      &origin("1"),
      loader("0.15.0"),
      vec![BaselineFile::managed("config/c.toml", key.clone(), false)],
    );
    base_store::store(&root.0, &installed.files[0], base_text.as_bytes()).unwrap();

    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let remote_files = vec![url_file("config/c.toml", remote_text, &mut env)];
    let mut update = request(&root, remote_files, Vec::new(), "0.15.0");
    let contents = BTreeMap::from([("config/c.toml".to_string(), remote_text.as_bytes().to_vec())]);
    let prepared = prepare_with_contents(&root.0, &installed, &update.remote, &contents)
      .await
      .unwrap();
    assert_eq!(prepared.diff.conflicts.len(), 1);
    assert!(!prepared.diff.conflicts[0].hunks.is_empty());

    // Keep every local block, as the dialog sends for per-hunk choices.
    let mut choices = BTreeMap::new();
    for index in 0..prepared.diff.conflicts[0].hunks.len() {
      choices.insert(format!("config/c.toml#{index}"), ConflictChoice::KeepLocal);
    }
    update.merged_contents = selected_hunk_contents(&prepared, &mut choices).unwrap();
    update.operations = resolve(&prepared, &choices);
    update.next_baseline = prepared.next_baseline.clone();
    update.base_texts = contents.clone();

    apply(&env, update).await.unwrap();

    let baseline = ModpackBaseline::load(&root.0).await.unwrap();
    // The kept file and the two mods from setup() that the release lacks.
    assert_eq!(count_local_changes(&root.0, &baseline).await.unwrap(), 3);
    let changes = local_changes(&root.0, &baseline, true).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "config/c.toml");
    assert!(changes[0].hunks.is_some());
    assert_eq!(changes[0].no_diff, None);
  }

  /// An automatically merged text file keeps its line diff in the
  /// local-change list as well: the new base survives the update.
  #[tokio::test]
  async fn an_auto_merged_file_keeps_its_line_diff() {
    use crate::instance::helpers::modpack::baseline::BaselineFile;
    use crate::instance::modpack_update::plan::{
      count_local_changes, local_changes, prepare_with_contents,
    };

    let root = setup();
    // Local edits line 1, the new version edits line 3: they merge cleanly.
    root.write("config/c.toml", "x = 2\ny = 1\nz = 1\n");
    let base_text = "x = 1\ny = 1\nz = 1\n";
    let remote_text = "x = 1\ny = 1\nz = 3\n";
    let key = sha1_of(base_text.as_bytes());
    let installed = ModpackBaseline::build(
      &origin("1"),
      loader("0.15.0"),
      vec![BaselineFile::managed("config/c.toml", key.clone(), false)],
    );
    base_store::store(&root.0, &installed.files[0], base_text.as_bytes()).unwrap();

    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let remote_files = vec![url_file("config/c.toml", remote_text, &mut env)];
    let mut update = request(&root, remote_files, Vec::new(), "0.15.0");
    let contents = BTreeMap::from([("config/c.toml".to_string(), remote_text.as_bytes().to_vec())]);
    let prepared = prepare_with_contents(&root.0, &installed, &update.remote, &contents)
      .await
      .unwrap();
    assert!(prepared.diff.conflicts.is_empty());
    assert_eq!(prepared.diff.auto_merged.len(), 1);

    update.merged_contents = prepared.merged_contents.clone();
    update.operations = crate::instance::modpack_update::plan::resolve(&prepared, &BTreeMap::new());
    update.next_baseline = prepared.next_baseline.clone();
    // The command fetched these bytes while the file was still a text
    // conflict, before `finish` merged it, and passes them on for the base
    // cache.
    update.base_texts = contents.clone();

    apply(&env, update).await.unwrap();

    assert_eq!(
      root.read("config/c.toml").as_deref(),
      Some("x = 2\ny = 1\nz = 3\n")
    );
    let baseline = ModpackBaseline::load(&root.0).await.unwrap();
    // The kept file and the two mods from setup() that the release lacks.
    assert_eq!(count_local_changes(&root.0, &baseline).await.unwrap(), 3);
    let changes = local_changes(&root.0, &baseline, true).await.unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "config/c.toml");
    let hunks = changes[0].hunks.as_ref().expect("line diff expected");
    assert!(hunks
      .iter()
      .any(|hunk| hunk.lines.iter().any(|line| line.text == "x = 2")));
    assert_eq!(changes[0].no_diff, None);
  }

  #[tokio::test]
  async fn a_restored_file_is_backed_up_and_rolls_back_with_its_ignore_record() {
    use crate::instance::helpers::modpack::baseline::BaselineFile;
    use crate::instance::modpack_update::ignores;
    use crate::instance::modpack_update::plan::{prepare_with_contents, resolve};

    let root = setup();
    root.write("config/c.toml", "x = 2\n");
    let base_text = "x = 1\n";
    let installed = ModpackBaseline::build(
      &origin("1"),
      loader("0.15.0"),
      vec![BaselineFile::managed(
        "config/c.toml",
        sha1_of(base_text.as_bytes()),
        false,
      )],
    );
    base_store::store(&root.0, &installed.files[0], base_text.as_bytes()).unwrap();
    ignores::add(
      &root.0,
      &installed,
      "config/c.toml",
      &installed.files[0].sha1,
      None,
    )
    .unwrap();

    let mut env = FakeEnv {
      root: root.0.clone(),
      ..Default::default()
    };
    let remote_files = vec![url_file("config/c.toml", base_text, &mut env)];
    let mut update = request(&root, remote_files, Vec::new(), "0.15.0");
    let prepared = prepare_with_contents(&root.0, &installed, &update.remote, &BTreeMap::new())
      .await
      .unwrap();
    assert_eq!(prepared.diff.changes.len(), 1);
    assert!(prepared.diff.changes[0].restored);
    update.operations = resolve(&prepared, &BTreeMap::new());
    update.merged_contents = prepared.merged_contents.clone();
    update.next_baseline = prepared.next_baseline.clone();
    update.ignores = ignores::plan_update(&root.0, &prepared, &update.remote);
    update.plan_hash = prepared.plan_hash.clone();

    apply(&env, update).await.unwrap();

    assert_eq!(root.read("config/c.toml").as_deref(), Some(base_text));
    assert_eq!(
      root
        .0
        .join(UPDATE_DIR)
        .join("last-backup/files/config/c.toml")
        .is_file(),
      true
    );
    assert_eq!(
      fs::read_to_string(
        root
          .0
          .join(UPDATE_DIR)
          .join("last-backup/files/config/c.toml")
      )
      .unwrap(),
      "x = 2\n"
    );
    assert!(ignores::read(&root.0).files.is_empty());

    assert!(roll_back_last(&root.0).unwrap());
    assert_eq!(root.read("config/c.toml").as_deref(), Some("x = 2\n"));
    let restored = ignores::read(&root.0);
    assert!(restored.files["config/c.toml"].whole);
  }

  #[test]
  fn a_failed_file_operation_names_the_file_relative_to_the_instance() {
    let root = setup();
    let from = root.0.join("mods").join("missing.jar");
    let to = root
      .0
      .join(".ustbl-update")
      .join("backup")
      .join("missing.jar");
    let error = move_file(&root.0, &from, &to).unwrap_err();
    let parsed = crate::file_error::parse(&error.to_string()).unwrap();
    assert_eq!(parsed.kind, crate::file_error::FileErrorKind::Missing);
    assert_eq!(
      parsed.file,
      Path::new("mods").join("missing.jar").display().to_string()
    );

    fs::create_dir_all(staging_meta_path(&root.0)).unwrap();
    let error = read_staging_meta(&root.0).unwrap_err();
    let parsed = crate::file_error::parse(&error.to_string()).unwrap();
    assert!(parsed.file.starts_with(".ustbl-update"));
  }
}
