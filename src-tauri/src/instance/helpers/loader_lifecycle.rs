use crate::error::USTBLResult;
use crate::instance::models::misc::{Instance, InstanceError, ModLoaderStatus};

/// Scans publish a whole instance map. Keep their read/modify/save/publish cycle
/// serialized with loader claims and final state publication, but not processors.
pub(crate) struct LoaderLifecycle(tokio::sync::Mutex<()>);

impl LoaderLifecycle {
  pub const fn new() -> Self {
    Self(tokio::sync::Mutex::const_new(()))
  }

  pub async fn run<T>(&self, work: impl std::future::Future<Output = T>) -> T {
    let _guard = self.0.lock().await;
    work.await
  }
}

pub(crate) static LOADER_LIFECYCLE: LoaderLifecycle = LoaderLifecycle::new();

pub(crate) fn recover_interrupted(status: &mut ModLoaderStatus, is_first_run: bool) {
  if is_first_run
    && matches!(
      status,
      ModLoaderStatus::Downloading | ModLoaderStatus::Installing
    )
  {
    *status = ModLoaderStatus::DownloadFailed;
  }
}

/// Persist the claim before creating any task (including retry tasks).
pub(crate) async fn claim_download(instance: &mut Instance, optifine: bool) -> USTBLResult<bool> {
  let status = if optifine {
    &instance.optifine.as_ref().unwrap().status
  } else {
    &instance.mod_loader.status
  };
  if !matches!(
    status,
    ModLoaderStatus::NotDownloaded | ModLoaderStatus::DownloadFailed
  ) {
    return Ok(false);
  }
  if optifine {
    instance.optifine.as_mut().unwrap().status = ModLoaderStatus::Downloading;
  } else {
    instance.mod_loader.status = ModLoaderStatus::Downloading;
  }
  instance.save_json_cfg().await?;
  Ok(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InstallSteps {
  pub loader: bool,
  pub optifine: bool,
}

/// Claim processor work while holding LOADER_LIFECYCLE. The Installing marker is
/// persisted before the lock is released, so refreshes preserve it and duplicate
/// callbacks cannot start another processor run.
pub(crate) async fn claim_install(instance: &mut Instance) -> USTBLResult<Option<InstallSteps>> {
  let pending = |status: &ModLoaderStatus| -> USTBLResult<bool> {
    match status {
      ModLoaderStatus::Installed => Ok(false),
      ModLoaderStatus::Downloading => Ok(true),
      ModLoaderStatus::Installing => Err(InstanceError::InstallationDuplicated.into()),
      _ => Err(InstanceError::ProcessorExecutionFailed.into()),
    }
  };
  let steps = InstallSteps {
    loader: pending(&instance.mod_loader.status)?,
    optifine: instance
      .optifine
      .as_ref()
      .map(|o| pending(&o.status))
      .transpose()?
      .unwrap_or(false),
  };
  if !steps.loader && !steps.optifine {
    return Ok(None);
  }
  set_install_status(instance, steps, ModLoaderStatus::Installing);
  instance.save_json_cfg().await?;
  Ok(Some(steps))
}

/// Persist the processor result while holding LOADER_LIFECYCLE. The processor
/// itself deliberately runs outside the lock because it may take minutes.
pub(crate) async fn complete_install(
  instance: &mut Instance,
  steps: InstallSteps,
  result: USTBLResult<()>,
) -> USTBLResult<()> {
  set_install_status(
    instance,
    steps,
    if result.is_ok() {
      ModLoaderStatus::Installed
    } else {
      ModLoaderStatus::DownloadFailed
    },
  );
  instance.save_json_cfg().await?;
  result
}

fn set_install_status(instance: &mut Instance, steps: InstallSteps, status: ModLoaderStatus) {
  if steps.loader {
    instance.mod_loader.status = status.clone();
  }
  if steps.optifine {
    instance.optifine.as_mut().unwrap().status = status;
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::OptiFine;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[test]
  fn only_startup_marks_interrupted_work_failed() {
    for original in [ModLoaderStatus::Downloading, ModLoaderStatus::Installing] {
      let mut status = original.clone();
      recover_interrupted(&mut status, false);
      assert_eq!(status, original);
      recover_interrupted(&mut status, true);
      assert_eq!(status, ModLoaderStatus::DownloadFailed);
    }
    for mut status in [
      ModLoaderStatus::Installed,
      ModLoaderStatus::NotDownloaded,
      ModLoaderStatus::DownloadFailed,
    ] {
      let original = status.clone();
      recover_interrupted(&mut status, true);
      assert_eq!(status, original);
    }
  }

  struct Fixture(Instance);

  impl Fixture {
    async fn new(status: ModLoaderStatus, optifine: bool) -> Self {
      let root = std::env::temp_dir().join(format!("ustbl-loader-{}", uuid::Uuid::new_v4()));
      tokio::fs::create_dir_all(&root).await.unwrap();
      let mut instance = Instance {
        version_path: root,
        ..Default::default()
      };
      if optifine {
        instance.optifine = Some(OptiFine {
          status,
          ..Default::default()
        });
      } else {
        instance.mod_loader.status = status;
      }
      instance.save_json_cfg().await.unwrap();
      Self(instance)
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0.version_path);
    }
  }

  #[tokio::test]
  async fn concurrent_refreshes_claim_and_queue_only_once() {
    for optifine in [false, true] {
      for status in [
        ModLoaderStatus::NotDownloaded,
        ModLoaderStatus::DownloadFailed,
      ] {
        let fixture = Fixture::new(status, optifine).await;
        let lifecycle = LoaderLifecycle::new();
        let queued = AtomicUsize::new(0);
        let refresh = || {
          lifecycle.run(async {
            let mut instance = fixture.0.load_json_cfg().await.unwrap();
            // Force the second caller to contend before the claim is saved.
            tokio::task::yield_now().await;
            if claim_download(&mut instance, optifine).await.unwrap() {
              let persisted = instance.load_json_cfg().await.unwrap();
              let status = if optifine {
                persisted.optifine.unwrap().status
              } else {
                persisted.mod_loader.status
              };
              assert_eq!(status, ModLoaderStatus::Downloading);
              queued.fetch_add(1, Ordering::SeqCst);
            }
          })
        };
        tokio::join!(refresh(), refresh());
        assert_eq!(queued.load(Ordering::SeqCst), 1);
      }
    }
  }

  #[tokio::test]
  async fn duplicate_callbacks_execute_processors_once_even_with_a_refresh() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, false).await;
    let lifecycle = LoaderLifecycle::new();
    let runs = &AtomicUsize::new(0);
    let mut instance = fixture.0.load_json_cfg().await.unwrap();
    let steps = lifecycle
      .run(async { claim_install(&mut instance).await.unwrap().unwrap() })
      .await;
    assert!(steps.loader);
    assert_eq!(instance.mod_loader.status, ModLoaderStatus::Installing);
    let refresh = lifecycle.run(async {
      let mut instance = fixture.0.load_json_cfg().await.unwrap();
      assert!(!claim_download(&mut instance, false).await.unwrap());
    });
    let processor = async {
      runs.fetch_add(1, Ordering::SeqCst);
      tokio::time::sleep(std::time::Duration::from_millis(20)).await;
      Ok(())
    };
    let ((), processor_result) = tokio::join!(refresh, processor);
    lifecycle
      .run(async {
        complete_install(&mut instance, steps, processor_result)
          .await
          .unwrap();
      })
      .await;
    let duplicate = lifecycle
      .run(async {
        let mut duplicate = fixture.0.load_json_cfg().await.unwrap();
        claim_install(&mut duplicate).await
      })
      .await;
    assert!(duplicate.unwrap().is_none());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
      fixture.0.load_json_cfg().await.unwrap().mod_loader.status,
      ModLoaderStatus::Installed
    );
  }

  #[tokio::test]
  async fn optifine_finishes_when_the_main_loader_is_already_installed() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, true).await;
    let mut instance = fixture.0.clone();
    let runs = &AtomicUsize::new(0);
    for _ in 0..2 {
      if let Some(steps) = claim_install(&mut instance).await.unwrap() {
        assert_eq!(
          steps,
          InstallSteps {
            loader: false,
            optifine: true
          }
        );
        runs.fetch_add(1, Ordering::SeqCst);
        complete_install(&mut instance, steps, Ok(()))
          .await
          .unwrap();
      }
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
      instance.optifine.unwrap().status,
      ModLoaderStatus::Installed
    );
  }

  #[tokio::test]
  async fn a_download_claim_that_cannot_be_saved_never_queues_work() {
    let fixture = Fixture::new(ModLoaderStatus::NotDownloaded, false).await;
    let mut instance = fixture.0.clone();
    // A file in place of the instance directory makes saving fail reliably.
    instance.version_path = fixture.0.get_json_cfg_path();
    assert!(claim_download(&mut instance, false).await.is_err());
    assert_eq!(
      fixture.0.load_json_cfg().await.unwrap().mod_loader.status,
      ModLoaderStatus::NotDownloaded
    );
  }

  #[tokio::test]
  async fn forge_and_optifine_complete_together_once() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, true).await;
    let mut instance = fixture.0.clone();
    instance.mod_loader.status = ModLoaderStatus::Downloading;
    instance.save_json_cfg().await.unwrap();
    let runs = &AtomicUsize::new(0);
    for _ in 0..2 {
      if let Some(steps) = claim_install(&mut instance).await.unwrap() {
        assert!(steps.loader && steps.optifine);
        runs.fetch_add(1, Ordering::SeqCst);
        complete_install(&mut instance, steps, Ok(()))
          .await
          .unwrap();
      }
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let saved = instance.load_json_cfg().await.unwrap();
    assert_eq!(saved.mod_loader.status, ModLoaderStatus::Installed);
    assert_eq!(saved.optifine.unwrap().status, ModLoaderStatus::Installed);
  }

  #[tokio::test]
  async fn failed_processors_are_not_reexecuted_by_a_duplicate_callback() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, false).await;
    let mut instance = fixture.0.clone();
    let runs = AtomicUsize::new(0);
    let steps = claim_install(&mut instance).await.unwrap().unwrap();
    runs.fetch_add(1, Ordering::SeqCst);
    assert!(complete_install(
      &mut instance,
      steps,
      Err(InstanceError::ProcessorExecutionFailed.into())
    )
    .await
    .is_err());
    assert!(claim_install(&mut instance).await.is_err());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
      instance.load_json_cfg().await.unwrap().mod_loader.status,
      ModLoaderStatus::DownloadFailed
    );
    assert!(claim_download(&mut instance, false).await.unwrap());
  }
}
