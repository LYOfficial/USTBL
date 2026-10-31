//! Background completion of the local mod list.
//!
//! `retrieve_local_mod_list` returns as soon as the jars are read, with whatever
//! the cache already knows. The mods it could not answer are handed to
//! [`spawn_mod_info_refresh`], which hashes them, asks Modrinth and MCIM in
//! batches and then emits [`MOD_UPDATES_EVENT`] with the names and descriptions it
//! found.
//!
//! There is at most one refresh per instance: starting another one for the same
//! instance cancels the running one, and a refresh that has been replaced never
//! emits. Every payload names its instance, and every update names the jar by
//! path, so the frontend can drop what no longer applies.

use crate::instance::helpers::mods::remote::{hash_mods, refresh_remote_infos};
use crate::instance::helpers::mods::translation::{
  persist_local_mod_cache, LocalModTranslationEntry,
};
use serde::Serialize;
use std::collections::HashMap;
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
/// out so the frontend keeps what it has.
pub fn build_updates(
  paths: &[PathBuf],
  hashes: &[Option<String>],
  entries: &HashMap<String, LocalModTranslationEntry>,
) -> Vec<LocalModUpdate> {
  paths
    .iter()
    .zip(hashes)
    .filter_map(|(path, hash)| {
      let entry = entries.get(hash.as_ref()?)?;
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

async fn run_refresh(app: &AppHandle, instance_id: &str, generation: u64, pending: Vec<PathBuf>) {
  let started = std::time::Instant::now();
  let hashes = hash_mods(app, &pending).await;
  let wanted: Vec<String> = hashes.iter().flatten().cloned().collect();
  let entries = refresh_remote_infos(app, &wanted).await;

  let cache_app = app.clone();
  let _ = tokio::task::spawn_blocking(move || persist_local_mod_cache(&cache_app)).await;

  let updates = build_updates(&pending, &hashes, &entries);
  log::debug!(
    "Mod list refresh for {}: {} pending, {} updates, {} ms",
    instance_id,
    pending.len(),
    updates.len(),
    started.elapsed().as_millis()
  );
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

#[cfg(test)]
mod tests {
  use super::*;

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
