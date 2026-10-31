use crate::error::USTBLResult;
use crate::instance::models::misc::{Instance, InstanceError, ModLoaderStatus, ModLoaderType};

/// Serializes every read/modify/save of an instance's ustblcfg.json that starts
/// from the file on disk: instance-list scans, loader claims and completions,
/// and `store_instance`. Each of them must read the file inside the lock, so a
/// save never overwrites a change made after its read. Processors run outside.
/// The mutex is not reentrant: code running under it must not call `run` again,
/// which includes `store_instance`.
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
  if !pending_download(status) {
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

pub(crate) fn preparation_allowed(status: &ModLoaderStatus) -> bool {
  matches!(status, ModLoaderStatus::Downloading)
}

fn pending_download(status: &ModLoaderStatus) -> bool {
  matches!(
    status,
    ModLoaderStatus::NotDownloaded | ModLoaderStatus::DownloadFailed
  )
}

pub(crate) fn loader_patch_ready(
  client: &crate::instance::helpers::client_json::McClientInfo,
  loader_type: ModLoaderType,
) -> bool {
  let patch_id = match loader_type {
    ModLoaderType::Forge | ModLoaderType::LegacyForge => "forge",
    ModLoaderType::NeoForge => "neoforge",
    _ => return false,
  };
  client.patches.iter().any(|patch| patch.id == patch_id)
}

pub(crate) fn optifine_patch_ready(
  client: &crate::instance::helpers::client_json::McClientInfo,
) -> bool {
  client.patches.iter().any(|patch| patch.id == "optifine")
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
/// itself deliberately runs outside the lock because it may take minutes, and
/// other writers (a modpack install committing its origin) save the config in
/// the meantime. So `claimed`, the snapshot taken at claim time, is only used to
/// locate the config: the file is read again and only the claimed statuses change.
///
/// Returns the config as saved, for publishing the loader fields to memory, or
/// `None` when it could not be read (for example the instance was deleted while
/// the processors ran). Nothing is written then, so the config file is never
/// recreated.
pub(crate) async fn complete_install(
  claimed: &Instance,
  steps: InstallSteps,
  result: USTBLResult<()>,
) -> (Option<Instance>, USTBLResult<()>) {
  let mut fresh = match claimed.load_json_cfg().await {
    Ok(fresh) => fresh,
    Err(error) => {
      log::warn!(
        "Not saving the loader install result of {:?}: {error}",
        claimed.version_path
      );
      return (None, result.and(Err(error.into())));
    }
  };
  set_install_status(
    &mut fresh,
    steps,
    if result.is_ok() {
      ModLoaderStatus::Installed
    } else {
      ModLoaderStatus::DownloadFailed
    },
  );
  match fresh.save_json_cfg().await {
    Ok(()) => (Some(fresh), result),
    Err(error) => (Some(fresh), result.and(Err(error.into()))),
  }
}

/// Read the config at `at` from disk, apply `change` and save it. Call while
/// holding LOADER_LIFECYCLE, so the read cannot go stale before the save.
pub(crate) async fn edit_config(
  at: &Instance,
  change: impl FnOnce(&mut Instance),
) -> USTBLResult<Instance> {
  let mut instance = at.load_json_cfg().await?;
  change(&mut instance);
  instance.save_json_cfg().await?;
  Ok(instance)
}

/// Mark a failed library download from the file on disk, not from the snapshot
/// the caller holds. Call while holding LOADER_LIFECYCLE.
pub(crate) async fn fail_download(claimed: &Instance, optifine: bool) -> USTBLResult<()> {
  let mut fresh = claimed.load_json_cfg().await?;
  let steps = InstallSteps {
    loader: !optifine,
    optifine,
  };
  set_install_status(&mut fresh, steps, ModLoaderStatus::DownloadFailed);
  fresh.save_json_cfg().await?;
  Ok(())
}

fn set_install_status(instance: &mut Instance, steps: InstallSteps, status: ModLoaderStatus) {
  if steps.loader {
    instance.mod_loader.status = status.clone();
  }
  if steps.optifine {
    match instance.optifine.as_mut() {
      Some(optifine) => optifine.status = status,
      None => log::warn!("OptiFine entry is gone from {:?}", instance.version_path),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::client_json::McClientInfo;
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

  #[test]
  fn legacy_forge_patch_allows_install_without_a_profile() {
    let mut client = McClientInfo::default();
    assert!(!loader_patch_ready(&client, ModLoaderType::Forge));
    client.patches.push(McClientInfo {
      id: "forge".into(),
      ..Default::default()
    });
    assert!(loader_patch_ready(&client, ModLoaderType::LegacyForge));
    // Legacy Forge writes this patch but does not leave install_profile.json.
    assert!(loader_patch_ready(&client, ModLoaderType::Forge));
    assert!(!optifine_patch_ready(&client));
    client.patches.push(McClientInfo {
      id: "optifine".into(),
      ..Default::default()
    });
    assert!(optifine_patch_ready(&client));
  }

  #[test]
  fn missing_loader_patch_rejects_installation_evidence() {
    let client = McClientInfo::default();
    assert!(!loader_patch_ready(&client, ModLoaderType::NeoForge));
  }

  #[test]
  fn preparation_only_accepts_a_completed_installer_claim() {
    assert!(preparation_allowed(&ModLoaderStatus::Downloading));
    assert!(!preparation_allowed(&ModLoaderStatus::NotDownloaded));
    assert!(!preparation_allowed(&ModLoaderStatus::DownloadFailed));
    assert!(!preparation_allowed(&ModLoaderStatus::Installed));
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
        complete(&instance, steps, processor_result).await.unwrap();
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
    let runs = &AtomicUsize::new(0);
    for _ in 0..2 {
      let mut instance = fixture.0.load_json_cfg().await.unwrap();
      if let Some(steps) = claim_install(&mut instance).await.unwrap() {
        assert_eq!(
          steps,
          InstallSteps {
            loader: false,
            optifine: true
          }
        );
        runs.fetch_add(1, Ordering::SeqCst);
        complete(&instance, steps, Ok(())).await.unwrap();
      }
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
      fixture
        .0
        .load_json_cfg()
        .await
        .unwrap()
        .optifine
        .unwrap()
        .status,
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
      let mut instance = fixture.0.load_json_cfg().await.unwrap();
      if let Some(steps) = claim_install(&mut instance).await.unwrap() {
        assert!(steps.loader && steps.optifine);
        runs.fetch_add(1, Ordering::SeqCst);
        complete(&instance, steps, Ok(())).await.unwrap();
      }
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let saved = fixture.0.load_json_cfg().await.unwrap();
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
    assert!(complete(
      &instance,
      steps,
      Err(InstanceError::ProcessorExecutionFailed.into())
    )
    .await
    .is_err());
    let mut instance = fixture.0.load_json_cfg().await.unwrap();
    assert!(matches!(
      claim_install(&mut instance).await,
      Err(error) if error.0 == InstanceError::ProcessorExecutionFailed.to_string()
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
      instance.load_json_cfg().await.unwrap().mod_loader.status,
      ModLoaderStatus::DownloadFailed
    );
    assert!(claim_download(&mut instance, false).await.unwrap());
  }

  async fn complete(
    claimed: &Instance,
    steps: InstallSteps,
    result: USTBLResult<()>,
  ) -> USTBLResult<()> {
    complete_install(claimed, steps, result).await.1
  }

  async fn write_origin(fixture: &Fixture, version_id: &str, modpack_version: Option<&str>) {
    use crate::instance::models::misc::{InstanceOrigin, OriginPlatform};
    let mut on_disk = fixture.0.load_json_cfg().await.unwrap();
    on_disk.modpack_version = modpack_version.map(String::from);
    on_disk.origin = Some(InstanceOrigin {
      platform: OriginPlatform::CurseForge,
      project_id: "925200".into(),
      version_id: version_id.into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::NeoForge,
      follow: true,
    });
    on_disk.save_json_cfg().await.unwrap();
  }

  /// What a modpack install does between the claim and the completion.
  async fn commit_origin_on_disk(fixture: &Fixture) {
    write_origin(fixture, "9190157", Some("1.0")).await;
  }

  fn assert_origin_kept(instance: &Instance) {
    assert_eq!(instance.modpack_version.as_deref(), Some("1.0"));
    assert_eq!(instance.origin.as_ref().unwrap().version_id, "9190157");
  }

  #[tokio::test]
  async fn completion_keeps_changes_saved_after_the_claim() {
    let cases: [(USTBLResult<()>, ModLoaderStatus); 2] = [
      (Ok(()), ModLoaderStatus::Installed),
      (
        Err(InstanceError::ProcessorExecutionFailed.into()),
        ModLoaderStatus::DownloadFailed,
      ),
    ];
    for (result, expected) in cases {
      let fixture = Fixture::new(ModLoaderStatus::Downloading, false).await;
      write_origin(&fixture, "", None).await;
      let mut claimed = fixture.0.load_json_cfg().await.unwrap();
      let steps = claim_install(&mut claimed).await.unwrap().unwrap();
      assert!(claimed.origin.as_ref().unwrap().version_id.is_empty());
      commit_origin_on_disk(&fixture).await;
      let failed = result.is_err();
      let (saved, result) = complete_install(&claimed, steps, result).await;
      assert_eq!(result.is_err(), failed);
      let saved = saved.unwrap();
      assert_eq!(saved.mod_loader.status, expected);
      assert_origin_kept(&saved);
      let on_disk = fixture.0.load_json_cfg().await.unwrap();
      assert_eq!(on_disk.mod_loader.status, expected);
      assert_origin_kept(&on_disk);
    }
  }

  /// A scan reads the file, tells the test it holds that copy, waits for the
  /// go-ahead, then saves the whole instance.
  async fn scan_cycle(
    fixture: &Fixture,
    read: tokio::sync::oneshot::Sender<()>,
    go: tokio::sync::oneshot::Receiver<()>,
  ) {
    let mut scanned = fixture.0.load_json_cfg().await.unwrap();
    read.send(()).unwrap();
    go.await.unwrap();
    scanned.description = "scanned".into();
    scanned.save_json_cfg().await.unwrap();
  }

  async fn store_cycle(fixture: &Fixture) {
    edit_config(&fixture.0, |instance| {
      instance.modpack_version = Some("1.0".into());
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn stored_changes_are_not_overwritten_by_a_concurrent_scan() {
    use tokio::sync::oneshot;

    // Without the lock the order is fixed as: scan reads, store reads and
    // saves, scan saves. The scan's stale copy wins and the change is lost.
    let fixture = Fixture::new(ModLoaderStatus::Installed, false).await;
    let (read_tx, read_rx) = oneshot::channel();
    let (go_tx, go_rx) = oneshot::channel();
    let scan = scan_cycle(&fixture, read_tx, go_rx);
    tokio::pin!(scan);
    tokio::select! {
      biased;
      _ = &mut scan => unreachable!("the scan waits for the go-ahead"),
      _ = read_rx => {}
    }
    store_cycle(&fixture).await;
    go_tx.send(()).unwrap();
    scan.await;
    assert_eq!(
      fixture.0.load_json_cfg().await.unwrap().modpack_version,
      None
    );

    // With the lock the store is queued behind the scan while the scan holds
    // its stale copy, so it reads only after the scan has saved.
    let fixture = Fixture::new(ModLoaderStatus::Installed, false).await;
    let lifecycle = LoaderLifecycle::new();
    let (read_tx, read_rx) = oneshot::channel();
    let (go_tx, go_rx) = oneshot::channel();
    let scan = lifecycle.run(scan_cycle(&fixture, read_tx, go_rx));
    let store = lifecycle.run(store_cycle(&fixture));
    tokio::pin!(scan, store);
    tokio::select! {
      biased;
      _ = &mut scan => unreachable!("the scan waits for the go-ahead"),
      _ = read_rx => {}
    }
    // One poll puts the store in the queue for the lock; it must not get past it.
    tokio::select! {
      biased;
      _ = &mut store => unreachable!("the scan still holds the lock"),
      _ = std::future::ready(()) => {}
    }
    go_tx.send(()).unwrap();
    tokio::join!(scan, store);
    let saved = fixture.0.load_json_cfg().await.unwrap();
    assert_eq!(saved.modpack_version.as_deref(), Some("1.0"));
    assert_eq!(saved.description, "scanned");
  }
  #[tokio::test]
  async fn completion_does_not_recreate_a_deleted_instance() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, false).await;
    let mut claimed = fixture.0.load_json_cfg().await.unwrap();
    let steps = claim_install(&mut claimed).await.unwrap().unwrap();
    std::fs::remove_dir_all(&fixture.0.version_path).unwrap();
    let cases: [USTBLResult<()>; 2] = [Ok(()), Err(InstanceError::ProcessorExecutionFailed.into())];
    for result in cases {
      let failed = result.is_err();
      let (saved, result) = complete_install(&claimed, steps, result).await;
      assert!(saved.is_none());
      assert!(result.is_err());
      if failed {
        assert_eq!(
          result.unwrap_err().0,
          InstanceError::ProcessorExecutionFailed.to_string()
        );
      }
      assert!(!fixture.0.version_path.exists());
    }
  }

  #[tokio::test]
  async fn completion_tolerates_an_optifine_entry_that_is_gone() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, true).await;
    let mut claimed = fixture.0.load_json_cfg().await.unwrap();
    claimed.mod_loader.status = ModLoaderStatus::Downloading;
    claimed.save_json_cfg().await.unwrap();
    let steps = claim_install(&mut claimed).await.unwrap().unwrap();
    assert!(steps.loader && steps.optifine);
    let mut on_disk = fixture.0.load_json_cfg().await.unwrap();
    on_disk.optifine = None;
    on_disk.save_json_cfg().await.unwrap();
    complete(&claimed, steps, Ok(())).await.unwrap();
    let saved = fixture.0.load_json_cfg().await.unwrap();
    assert_eq!(saved.mod_loader.status, ModLoaderStatus::Installed);
    assert!(saved.optifine.is_none());
  }

  #[tokio::test]
  async fn a_failed_download_marks_only_its_component_on_the_current_file() {
    for optifine in [false, true] {
      let fixture = Fixture::new(ModLoaderStatus::Downloading, optifine).await;
      write_origin(&fixture, "", None).await;
      let mut claimed = fixture.0.load_json_cfg().await.unwrap();
      if optifine {
        claimed.mod_loader.status = ModLoaderStatus::Installed;
        claimed.save_json_cfg().await.unwrap();
      }
      commit_origin_on_disk(&fixture).await;
      // `claimed` is older than the file: it must not be written back.
      fail_download(&claimed, optifine).await.unwrap();
      let saved = fixture.0.load_json_cfg().await.unwrap();
      assert_origin_kept(&saved);
      if optifine {
        assert_eq!(saved.mod_loader.status, ModLoaderStatus::Installed);
        assert_eq!(
          saved.optifine.unwrap().status,
          ModLoaderStatus::DownloadFailed
        );
      } else {
        assert_eq!(saved.mod_loader.status, ModLoaderStatus::DownloadFailed);
      }
    }
  }

  #[tokio::test]
  async fn a_failed_download_does_not_recreate_a_deleted_instance() {
    let fixture = Fixture::new(ModLoaderStatus::Downloading, false).await;
    let claimed = fixture.0.clone();
    std::fs::remove_dir_all(&fixture.0.version_path).unwrap();
    assert!(fail_download(&claimed, false).await.is_err());
    assert!(!fixture.0.version_path.exists());
  }
}
