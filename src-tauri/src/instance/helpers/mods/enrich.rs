//! Background completion of the local mod list.
//!
//! `retrieve_local_mod_list` returns as soon as the jars are read, with whatever
//! the cache already knows. The mods it could not answer are handed to
//! [`spawn_mod_info_refresh`], which hashes them, asks Modrinth and MCIM in
//! batches and then emits [`MOD_UPDATES_EVENT`] with the names and descriptions it
//! found. The jars Modrinth does not know are then asked about on CurseForge,
//! and a second event carries those answers.
//!
//! There is at most one refresh per instance: starting another one for the same
//! instance cancels the running one, and a refresh that has been replaced never
//! emits. Deleting the instance cancels its refresh the same way. Every payload
//! names its instance, and every update names the jar by path, so the frontend
//! can drop what no longer applies.

use crate::instance::helpers::mods::remote::{
  hash_mods, refresh_curseforge_infos, refresh_remote_infos,
};
use crate::instance::helpers::mods::translation::{
  persist_local_mod_cache, LocalModTranslationEntry,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::task::AbortHandle;

pub const MOD_UPDATES_EVENT: &str = "ustbl:local-mod-updates";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModUpdate {
  pub file_path: PathBuf,
  pub translated_name: Option<String>,
  pub translated_description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalModUpdatesPayload {
  instance_id: String,
  mods: Vec<LocalModUpdate>,
}

struct Job {
  generation: u64,
  handle: Option<AbortHandle>,
}

/// The refresh in flight for each instance.
#[derive(Default)]
pub struct JobRegistry {
  next_generation: u64,
  jobs: HashMap<String, Job>,
}

impl JobRegistry {
  /// Claim the instance for a new refresh, cancelling the one that was running.
  pub fn begin(&mut self, instance_id: &str) -> u64 {
    self.next_generation += 1;
    let generation = self.next_generation;
    let previous = self.jobs.insert(
      instance_id.to_string(),
      Job {
        generation,
        handle: None,
      },
    );
    if let Some(handle) = previous.and_then(|job| job.handle) {
      handle.abort();
    }
    generation
  }

  /// Remember how to cancel the task; if another refresh took over in the
  /// meantime, the task is cancelled right away.
  pub fn attach(&mut self, instance_id: &str, generation: u64, handle: AbortHandle) {
    match self.jobs.get_mut(instance_id) {
      Some(job) if job.generation == generation => job.handle = Some(handle),
      _ => handle.abort(),
    }
  }

  /// Abort the refresh running for the instance and forget it. A task that is
  /// attached afterwards finds no registration and is aborted by `attach`, and
  /// `is_current` turns false, so nothing more is emitted for the instance.
  pub fn cancel(&mut self, instance_id: &str) {
    if let Some(handle) = self.jobs.remove(instance_id).and_then(|job| job.handle) {
      handle.abort();
    }
  }

  pub fn is_current(&self, instance_id: &str, generation: u64) -> bool {
    self
      .jobs
      .get(instance_id)
      .is_some_and(|job| job.generation == generation)
  }

  pub fn finish(&mut self, instance_id: &str, generation: u64) {
    if self.is_current(instance_id, generation) {
      self.jobs.remove(instance_id);
    }
  }
}

static JOBS: LazyLock<Mutex<JobRegistry>> = LazyLock::new(|| Mutex::new(JobRegistry::default()));

fn jobs() -> std::sync::MutexGuard<'static, JobRegistry> {
  JOBS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Updates for the jars whose hash resolved to an entry; the others are left
/// out so the frontend keeps what it has. An entry that only says "not on
/// Modrinth" has nothing to show and is left out too, so it cannot blank a name
/// the list already displays while CurseForge is being asked.
pub fn build_updates(
  paths: &[PathBuf],
  hashes: &[Option<String>],
  entries: &HashMap<String, LocalModTranslationEntry>,
) -> Vec<LocalModUpdate> {
  paths
    .iter()
    .zip(hashes)
    .filter_map(|(path, hash)| {
      let entry = entries
        .get(hash.as_ref()?)
        .filter(|entry| !entry.needs_curseforge())?;
      Some(LocalModUpdate {
        file_path: path.clone(),
        translated_name: entry.translated_name.clone(),
        translated_description: entry.translated_description.clone(),
      })
    })
    .collect()
}

/// Complete `pending` (jar paths of `instance_id`) in the background and tell
/// the frontend when done. Cancels any refresh still running for the instance,
/// even when there is nothing to do, because that one was started for an older
/// state of the folder.
pub fn spawn_mod_info_refresh(app: &AppHandle, instance_id: &str, pending: Vec<PathBuf>) {
  let generation = jobs().begin(instance_id);
  if pending.is_empty() {
    jobs().finish(instance_id, generation);
    return;
  }

  let task_app = app.clone();
  let task_instance = instance_id.to_string();
  let task = tokio::spawn(async move {
    run_refresh(&task_app, &task_instance, generation, pending).await;
    jobs().finish(&task_instance, generation);
  });
  jobs().attach(instance_id, generation, task.abort_handle());
}

/// Stop the background refresh of an instance that is being deleted. The abort
/// takes effect at the task's next `.await`; work already running in
/// `spawn_blocking` (hashing, writing the cache) finishes, but nothing is
/// emitted afterwards.
pub fn cancel_mod_info_refresh(instance_id: &str) {
  jobs().cancel(instance_id);
}

/// One refresh runs in two steps. The Modrinth answers are sent as soon as they
/// are in; the jars Modrinth does not know are then looked up on CurseForge, and
/// their answers follow in a second event. The two are kept apart because the
/// second needs the jars to be read again for their fingerprints.
async fn run_refresh(app: &AppHandle, instance_id: &str, generation: u64, pending: Vec<PathBuf>) {
  let started = std::time::Instant::now();
  let hashes = hash_mods(app, &pending).await;
  let wanted: Vec<String> = hashes.iter().flatten().cloned().collect();
  let entries = refresh_remote_infos(app, &wanted).await;
  persist(app).await;

  let updates = build_updates(&pending, &hashes, &entries);
  log::debug!(
    "Mod list refresh for {}: {} pending, {} updates, {} ms",
    instance_id,
    pending.len(),
    updates.len(),
    started.elapsed().as_millis()
  );
  emit_updates(app, instance_id, generation, updates);

  let targets = curseforge_targets(&pending, &hashes, &entries);
  if targets.is_empty() {
    return;
  }
  let curseforge_entries = refresh_curseforge_infos(app, &targets).await;
  persist(app).await;

  let updates = build_updates(&pending, &hashes, &curseforge_entries);
  log::debug!(
    "Mod list refresh for {}: {} on CurseForge candidates, {} updates, {} ms",
    instance_id,
    targets.len(),
    updates.len(),
    started.elapsed().as_millis()
  );
  emit_updates(app, instance_id, generation, updates);
}

async fn persist(app: &AppHandle) {
  let cache_app = app.clone();
  let _ = tokio::task::spawn_blocking(move || persist_local_mod_cache(&cache_app)).await;
}

fn emit_updates(app: &AppHandle, instance_id: &str, generation: u64, updates: Vec<LocalModUpdate>) {
  if updates.is_empty() || !jobs().is_current(instance_id, generation) {
    return;
  }
  let _ = app.emit(
    MOD_UPDATES_EVENT,
    LocalModUpdatesPayload {
      instance_id: instance_id.to_string(),
      mods: updates,
    },
  );
}

/// One jar per distinct hash among those Modrinth does not know and CurseForge
/// has not settled. Mods Modrinth knows never appear here.
pub fn curseforge_targets(
  paths: &[PathBuf],
  hashes: &[Option<String>],
  entries: &HashMap<String, LocalModTranslationEntry>,
) -> Vec<(String, PathBuf)> {
  let mut seen = HashSet::new();
  paths
    .iter()
    .zip(hashes)
    .filter_map(|(path, hash)| {
      let hash = hash.as_ref()?;
      entries.get(hash)?.needs_curseforge().then_some(())?;
      seen
        .insert(hash.clone())
        .then(|| (hash.clone(), path.clone()))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::mods::translation::EntryOrigin;

  fn idle_task() -> tokio::task::JoinHandle<()> {
    tokio::spawn(std::future::pending())
  }

  #[tokio::test]
  async fn a_new_refresh_cancels_the_running_one_for_the_same_instance() {
    let mut registry = JobRegistry::default();
    let first = registry.begin("a");
    let first_task = idle_task();
    registry.attach("a", first, first_task.abort_handle());

    let second = registry.begin("a");
    assert!(first_task.await.unwrap_err().is_cancelled());
    assert!(!registry.is_current("a", first));
    assert!(registry.is_current("a", second));
  }

  #[tokio::test]
  async fn refreshes_of_other_instances_are_left_alone() {
    let mut registry = JobRegistry::default();
    let a = registry.begin("a");
    let task = idle_task();
    registry.attach("a", a, task.abort_handle());

    let b = registry.begin("b");
    assert!(registry.is_current("a", a) && registry.is_current("b", b));
    assert!(!task.is_finished());
    task.abort();
  }

  #[tokio::test]
  async fn a_task_attached_after_being_replaced_is_cancelled() {
    let mut registry = JobRegistry::default();
    let stale = registry.begin("a");
    registry.begin("a");
    let task = idle_task();
    registry.attach("a", stale, task.abort_handle());
    assert!(task.await.unwrap_err().is_cancelled());
  }

  #[tokio::test]
  async fn cancelling_aborts_the_running_refresh_and_stops_its_emits() {
    let mut registry = JobRegistry::default();
    let generation = registry.begin("a");
    let task = idle_task();
    registry.attach("a", generation, task.abort_handle());

    registry.cancel("a");
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!registry.is_current("a", generation));
  }

  #[tokio::test]
  async fn a_task_attached_after_cancelling_is_aborted() {
    let mut registry = JobRegistry::default();
    let generation = registry.begin("a");
    registry.cancel("a");

    let task = idle_task();
    registry.attach("a", generation, task.abort_handle());
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!registry.is_current("a", generation));
  }

  #[tokio::test]
  async fn cancelling_leaves_other_instances_and_unknown_ids_alone() {
    let mut registry = JobRegistry::default();
    let b = registry.begin("b");
    let task = idle_task();
    registry.attach("b", b, task.abort_handle());

    registry.cancel("a");
    assert!(registry.is_current("b", b));
    assert!(!task.is_finished());
    task.abort();
  }

  #[test]
  fn finishing_a_replaced_refresh_does_not_release_its_successor() {
    let mut registry = JobRegistry::default();
    let old = registry.begin("a");
    let new = registry.begin("a");
    registry.finish("a", old);
    assert!(registry.is_current("a", new));
    registry.finish("a", new);
    assert!(!registry.is_current("a", new));
  }

  #[test]
  fn updates_cover_only_the_jars_that_resolved() {
    let paths = vec![
      PathBuf::from("/m/a.jar"),
      PathBuf::from("/m/b.jar"),
      PathBuf::from("/m/c.jar"),
    ];
    let hashes = vec![Some("h1".to_string()), None, Some("h3".to_string())];
    let entries = HashMap::from([(
      "h1".to_string(),
      LocalModTranslationEntry::new(Some("名".into()), None),
    )]);

    assert_eq!(
      build_updates(&paths, &hashes, &entries),
      vec![LocalModUpdate {
        file_path: PathBuf::from("/m/a.jar"),
        translated_name: Some("名".into()),
        translated_description: None,
      }]
    );
  }

  fn entry(origin: EntryOrigin) -> LocalModTranslationEntry {
    LocalModTranslationEntry::with_origin(origin, None, None)
  }

  #[test]
  fn only_jars_modrinth_lacks_go_to_curseforge_once_per_hash() {
    let paths: Vec<PathBuf> = ["a", "b", "c", "d", "e"]
      .iter()
      .map(|name| PathBuf::from(format!("/m/{name}.jar")))
      .collect();
    let hashes = vec![
      Some("on-modrinth".to_string()),
      Some("missing".to_string()),
      // The same jar twice.
      Some("missing".to_string()),
      Some("settled".to_string()),
      None,
    ];
    let entries = HashMap::from([
      ("on-modrinth".to_string(), entry(EntryOrigin::Modrinth)),
      ("missing".to_string(), entry(EntryOrigin::NotOnModrinth)),
      ("settled".to_string(), entry(EntryOrigin::Nowhere)),
    ]);

    assert_eq!(
      curseforge_targets(&paths, &hashes, &entries),
      vec![("missing".to_string(), PathBuf::from("/m/b.jar"))]
    );
  }

  #[test]
  fn a_not_on_modrinth_entry_is_not_sent_as_an_update() {
    let paths = vec![PathBuf::from("/m/a.jar"), PathBuf::from("/m/b.jar")];
    let hashes = vec![Some("pending".to_string()), Some("done".to_string())];
    let entries = HashMap::from([
      ("pending".to_string(), entry(EntryOrigin::NotOnModrinth)),
      ("done".to_string(), entry(EntryOrigin::CurseForge)),
    ]);
    let updates = build_updates(&paths, &hashes, &entries);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].file_path, PathBuf::from("/m/b.jar"));
  }

  #[test]
  fn a_named_not_on_modrinth_entry_is_neither_sent_nor_skipped_for_curseforge() {
    let paths = vec![PathBuf::from("/m/a.jar")];
    let hashes = vec![Some("h".to_string())];
    let entries = HashMap::from([(
      "h".to_string(),
      LocalModTranslationEntry::with_origin(EntryOrigin::NotOnModrinth, Some("旧名".into()), None),
    )]);
    // The list keeps showing the name it got from the cache.
    assert!(build_updates(&paths, &hashes, &entries).is_empty());
    assert_eq!(curseforge_targets(&paths, &hashes, &entries).len(), 1);
  }

  #[test]
  fn payload_uses_camel_case_keys() {
    let payload = LocalModUpdatesPayload {
      instance_id: "i".into(),
      mods: vec![LocalModUpdate {
        file_path: PathBuf::from("a.jar"),
        translated_name: None,
        translated_description: Some("d".into()),
      }],
    };
    let json = serde_json::to_value(&payload).unwrap();
    assert_eq!(json["instanceId"], "i");
    assert_eq!(json["mods"][0]["filePath"], "a.jar");
    assert_eq!(json["mods"][0]["translatedDescription"], "d");
    assert!(json["mods"][0]["translatedName"].is_null());
  }
}
