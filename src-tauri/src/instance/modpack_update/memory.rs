//! The memory a modpack recommends, remembered per instance until the player
//! has answered it.
//!
//! A successful install or update records what the new version recommends in
//! `.ustbl-update/memory-suggestion.json`. The launcher asks the player about
//! it only while the recommendation (capped to the device) is higher than what
//! the instance would use now, and never changes a setting without an answer.

use super::remote::{RemoteModpack, UPDATE_DIR};
use crate::error::USTBLResult;
use crate::instance::models::misc::{Instance, InstanceError};
use crate::launch::helpers::command_generator::max_memory_mb;
use crate::launcher_config::helpers::misc::get_global_game_config;
use crate::launcher_config::models::{GameConfig, MemoryInfo};
use crate::utils::sys_info::get_memory_info;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

const MEMORY_FILE: &str = "memory-suggestion.json";

/// The smallest allocation the settings page offers.
const FLOOR_MB: u32 = 256;

/// Share of the device's memory a recommendation may take at most.
const DEVICE_SHARE_PERCENT: u64 = 90;

/// Where a recommendation was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionSource {
  /// A field of the platform manifest (CurseForge `recommendedRam`).
  Manifest,
  /// The memory keys of a component pack's `instance.cfg`.
  InstanceCfg,
}

/// What the file under the update folder holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySuggestion {
  pub suggested_mb: u32,
  pub source: SuggestionSource,
  pub pack_name: String,
  pub pack_version: String,
  /// The recommended value the player last answered, either way.
  #[serde(default)]
  pub answered_mb: Option<u32>,
}

/// Memory recommended by an `instance.cfg`: only while `OverrideMemory` is on,
/// the maximum if there is one, else the minimum. MultiMC's `PermGen` is not
/// memory the game heap uses and is ignored.
pub fn from_instance_cfg(cfg: &BTreeMap<String, String>) -> Option<u32> {
  let overridden = cfg
    .get("OverrideMemory")
    .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
  if !overridden {
    return None;
  }
  let read = |key: &str| {
    cfg
      .get(key)
      .and_then(|value| value.trim().parse::<u32>().ok())
      .filter(|mb| *mb > 0)
  };
  read("MaxMemAlloc").or_else(|| read("MinMemAlloc"))
}

/// What a remote version recommends, taken before the remote is consumed by
/// the update.
#[derive(Debug, Clone)]
pub struct Recommendation {
  memory: Option<(u32, SuggestionSource)>,
  pack_version: String,
}

impl Recommendation {
  pub fn of(remote: &RemoteModpack) -> Self {
    let memory = remote
      .recommended_memory_mb
      .filter(|mb| *mb > 0)
      .map(|mb| (mb, SuggestionSource::Manifest))
      .or_else(|| {
        remote
          .component_pack
          .as_ref()
          .and_then(|pack| from_instance_cfg(&pack.instance_cfg))
          .map(|mb| (mb, SuggestionSource::InstanceCfg))
      });
    Self {
      memory,
      pack_version: remote.label.clone(),
    }
  }

  /// Record it after the install or update has been committed. A version
  /// without a recommendation clears what an earlier one left. The update has
  /// already succeeded by now, so a failure here is only logged.
  pub fn record(&self, root: &Path, pack_name: &str) {
    if let Err(error) = self.try_record(root, pack_name) {
      log::warn!("modpack memory suggestion not recorded: {}", error.0);
    }
  }

  fn try_record(&self, root: &Path, pack_name: &str) -> USTBLResult<()> {
    let Some((suggested_mb, source)) = self.memory else {
      return clear(root);
    };
    // An answer holds for the value it was given for; a new value asks again.
    let answered_mb = read(root)
      .filter(|previous| previous.suggested_mb == suggested_mb)
      .and_then(|previous| previous.answered_mb);
    write(
      root,
      &MemorySuggestion {
        suggested_mb,
        source,
        pack_name: pack_name.to_string(),
        pack_version: self.pack_version.clone(),
        answered_mb,
      },
    )
  }
}

fn memory_path(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join(MEMORY_FILE)
}

/// The recorded suggestion; `None` when there is none or the file is unreadable.
pub fn read(root: &Path) -> Option<MemorySuggestion> {
  serde_json::from_slice(&fs::read(memory_path(root)).ok()?).ok()
}

fn write(root: &Path, suggestion: &MemorySuggestion) -> USTBLResult<()> {
  let path = memory_path(root);
  let temporary = path.with_extension("json.tmp");
  let bytes =
    serde_json::to_vec_pretty(suggestion).map_err(|_| InstanceError::FileOperationError)?;
  fs::create_dir_all(path.parent().unwrap_or(root))
    .map_err(|_| InstanceError::FileOperationError)?;
  fs::write(&temporary, bytes).map_err(|_| InstanceError::FileOperationError)?;
  fs::rename(&temporary, &path).map_err(|_| InstanceError::FileOperationError)?;
  Ok(())
}

fn clear(root: &Path) -> USTBLResult<()> {
  match fs::remove_file(memory_path(root)) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
    Err(_) => Err(InstanceError::FileOperationError.into()),
  }
}

/// The recommendation limited to what the device can give: at most
/// [`DEVICE_SHARE_PERCENT`] of its memory, and never below [`FLOOR_MB`].
pub fn capped_mb(suggested_mb: u32, total_bytes: u64) -> u32 {
  let ceiling =
    (total_bytes / 1024 / 1024 * DEVICE_SHARE_PERCENT / 100).min(u32::MAX as u64) as u32;
  suggested_mb.min(ceiling).max(FLOOR_MB)
}

/// Whose setting decides the memory an instance launches with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MemorySource {
  Global,
  Instance,
  Auto,
}

/// The `-Xmx` an instance would launch with now, and which setting it comes
/// from. The value is computed by the launch command's own function.
pub fn current_memory(
  instance: &Instance,
  global: &GameConfig,
  suggested_max_alloc: u64,
) -> (u32, MemorySource) {
  let (performance, source) = match (&instance.spec_game_config, instance.use_spec_game_config) {
    (Some(spec), true) => (&spec.performance, MemorySource::Instance),
    _ => (&global.performance, MemorySource::Global),
  };
  let mb = max_memory_mb(
    performance.auto_mem_allocation,
    performance.max_mem_allocation,
    || suggested_max_alloc,
  );
  if performance.auto_mem_allocation {
    (mb, MemorySource::Auto)
  } else {
    (mb, source)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryPrompt {
  pub instance_id: String,
  pub pack_name: String,
  /// The value the pack recommends.
  pub suggested_mb: u32,
  /// What applying would set: the recommendation limited to the device.
  pub capped_mb: u32,
  pub total_mb: u64,
  pub current_mb: u32,
  pub current_source: MemorySource,
}

/// The question to put to the player, or `None` when there is nothing to ask:
/// the value was answered already, or the instance has at least that much.
pub fn prompt_for(
  instance: &Instance,
  suggestion: &MemorySuggestion,
  global: &GameConfig,
  memory: &MemoryInfo,
) -> Option<MemoryPrompt> {
  if suggestion.answered_mb == Some(suggestion.suggested_mb) {
    return None;
  }
  let capped_mb = capped_mb(suggestion.suggested_mb, memory.total);
  let (current_mb, current_source) = current_memory(instance, global, memory.suggested_max_alloc);
  if capped_mb <= current_mb {
    return None;
  }
  Some(MemoryPrompt {
    instance_id: instance.id.clone(),
    pack_name: suggestion.pack_name.clone(),
    suggested_mb: suggestion.suggested_mb,
    capped_mb,
    total_mb: memory.total / 1024 / 1024,
    current_mb,
    current_source,
  })
}

/// Turn on the instance's own game settings, as the player would in the
/// instance settings, and set only the memory: no automatic allocation, the
/// given maximum. An instance that is not using settings of its own starts from
/// a copy of the global ones, even if it kept an older set from before: the
/// player was promised that nothing but the memory changes.
pub fn apply_to_instance(instance: &mut Instance, global: &GameConfig, memory_mb: u32) {
  if !instance.use_spec_game_config {
    instance.spec_game_config = None;
  }
  instance.use_spec_game_config = true;
  let config = instance
    .spec_game_config
    .get_or_insert_with(|| global.clone());
  config.performance.auto_mem_allocation = false;
  config.performance.max_mem_allocation = memory_mb;
}

/// The questions to ask now, over the instances the launcher knows.
#[tauri::command]
pub async fn list_memory_suggestions(app: AppHandle) -> USTBLResult<Vec<MemoryPrompt>> {
  let instances: Vec<Instance> = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    state.values().cloned().collect()
  };
  let global = get_global_game_config(&app);
  let memory = get_memory_info();
  let mut prompts: Vec<MemoryPrompt> = instances
    .iter()
    .filter_map(|instance| {
      let suggestion = read(&instance.version_path)?;
      prompt_for(instance, &suggestion, &global, &memory)
    })
    .collect();
  prompts.sort_by(|a, b| (&a.pack_name, &a.instance_id).cmp(&(&b.pack_name, &b.instance_id)));
  Ok(prompts)
}

/// Record the player's answer to the question for `suggested_mb`. Applying
/// changes the instance's settings first; either way the answer is remembered
/// so the same value is not asked again. An answer to a value that has been
/// replaced since is ignored.
#[tauri::command]
pub async fn answer_memory_suggestion(
  app: AppHandle,
  instance_id: String,
  suggested_mb: u32,
  apply: bool,
) -> USTBLResult<()> {
  let root = {
    let binding = app.state::<Mutex<HashMap<String, Instance>>>();
    let state = binding.lock()?;
    state
      .get(&instance_id)
      .ok_or(InstanceError::InstanceNotFoundByID)?
      .version_path
      .clone()
  };
  let Some(mut suggestion) = read(&root) else {
    return Ok(());
  };
  if suggestion.suggested_mb != suggested_mb {
    return Ok(());
  }
  if apply {
    let capped = capped_mb(suggested_mb, get_memory_info().total);
    let global = get_global_game_config(&app);
    let instance = {
      let binding = app.state::<Mutex<HashMap<String, Instance>>>();
      let mut state = binding.lock()?;
      let instance = state
        .get_mut(&instance_id)
        .ok_or(InstanceError::InstanceNotFoundByID)?;
      apply_to_instance(instance, &global, capped);
      instance.clone()
    };
    instance.save_json_cfg().await?;
  }
  suggestion.answered_mb = Some(suggested_mb);
  write(&root, &suggestion)
}

#[cfg(test)]
mod tests {
  use super::super::remote::ComponentPack;
  use super::*;
  use crate::instance::helpers::modpack::baseline::BaselineLoader;
  use crate::instance::models::misc::ModLoaderType;

  const MIB: u64 = 1024 * 1024;

  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let path =
        std::env::temp_dir().join(format!("ustbl-memory-test-{name}-{}", std::process::id()));
      let _ = fs::remove_dir_all(&path);
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn cfg(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
      .iter()
      .map(|(key, value)| (key.to_string(), value.to_string()))
      .collect()
  }

  fn info(total_mb: u64, auto_mb: u64) -> MemoryInfo {
    MemoryInfo {
      total: total_mb * MIB,
      used: 0,
      suggested_max_alloc: auto_mb * MIB,
    }
  }

  fn global(auto: bool, mb: u32) -> GameConfig {
    let mut config = GameConfig::default();
    config.performance.auto_mem_allocation = auto;
    config.performance.max_mem_allocation = mb;
    config
  }

  fn instance() -> Instance {
    Instance {
      id: "games:pack".into(),
      name: "pack".into(),
      ..Default::default()
    }
  }

  fn suggestion(suggested_mb: u32, answered_mb: Option<u32>) -> MemorySuggestion {
    MemorySuggestion {
      suggested_mb,
      source: SuggestionSource::Manifest,
      pack_name: "Pack".into(),
      pack_version: "1.0".into(),
      answered_mb,
    }
  }

  fn remote(recommended: Option<u32>, cfg: Option<BTreeMap<String, String>>) -> RemoteModpack {
    RemoteModpack {
      version_id: "v1".into(),
      label: "1.2.0".into(),
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: ModLoaderType::NeoForge,
        version: "21.1.0".into(),
      },
      components: None,
      component_pack: cfg.map(|instance_cfg| ComponentPack {
        mmc_pack: serde_json::json!({}),
        instance_cfg,
        files: Vec::new(),
      }),
      recommended_memory_mb: recommended,
      files: Vec::new(),
      rules: Vec::new(),
      rules_manifest: false,
      disabled_authoritative: true,
      path_index: Default::default(),
    }
  }

  #[test]
  fn instance_cfg_needs_override_memory() {
    assert_eq!(from_instance_cfg(&cfg(&[("MaxMemAlloc", "4096")])), None);
    assert_eq!(
      from_instance_cfg(&cfg(&[
        ("OverrideMemory", "false"),
        ("MaxMemAlloc", "4096")
      ])),
      None
    );
    assert_eq!(
      from_instance_cfg(&cfg(&[("OverrideMemory", "true"), ("MaxMemAlloc", "4096")])),
      Some(4096)
    );
    assert_eq!(
      from_instance_cfg(&cfg(&[("OverrideMemory", "TRUE"), ("MaxMemAlloc", "4096")])),
      Some(4096)
    );
  }

  #[test]
  fn instance_cfg_takes_the_maximum_then_the_minimum() {
    let both = cfg(&[
      ("OverrideMemory", "true"),
      ("MinMemAlloc", "2048"),
      ("MaxMemAlloc", "6144"),
      ("PermGen", "512"),
    ]);
    assert_eq!(from_instance_cfg(&both), Some(6144));
    // Our own export writes the minimum only.
    let only_min = cfg(&[("OverrideMemory", "true"), ("MinMemAlloc", "3072")]);
    assert_eq!(from_instance_cfg(&only_min), Some(3072));
    let only_perm_gen = cfg(&[("OverrideMemory", "true"), ("PermGen", "512")]);
    assert_eq!(from_instance_cfg(&only_perm_gen), None);
    let unusable = cfg(&[
      ("OverrideMemory", "true"),
      ("MaxMemAlloc", "lots"),
      ("MinMemAlloc", "1024"),
    ]);
    assert_eq!(from_instance_cfg(&unusable), Some(1024));
    let zero = cfg(&[("OverrideMemory", "true"), ("MaxMemAlloc", "0")]);
    assert_eq!(from_instance_cfg(&zero), None);
  }

  #[test]
  fn the_manifest_value_wins_over_the_instance_cfg() {
    let both = remote(
      Some(8196),
      Some(cfg(&[("OverrideMemory", "true"), ("MaxMemAlloc", "4096")])),
    );
    let picked = Recommendation::of(&both);
    assert_eq!(picked.memory, Some((8196, SuggestionSource::Manifest)));
    assert_eq!(picked.pack_version, "1.2.0");

    let cfg_only = remote(
      None,
      Some(cfg(&[("OverrideMemory", "true"), ("MaxMemAlloc", "4096")])),
    );
    assert_eq!(
      Recommendation::of(&cfg_only).memory,
      Some((4096, SuggestionSource::InstanceCfg))
    );
    assert_eq!(Recommendation::of(&remote(None, None)).memory, None);
    assert_eq!(Recommendation::of(&remote(Some(0), None)).memory, None);
  }

  #[test]
  fn the_recommendation_is_capped_to_the_device_with_a_floor() {
    assert_eq!(capped_mb(8196, 32768 * MIB), 8196);
    // 90% of 8192 MiB.
    assert_eq!(capped_mb(8196, 8192 * MIB), 7372);
    assert_eq!(capped_mb(8192, 16384 * MIB), 8192);
    assert_eq!(capped_mb(64, 16384 * MIB), 256);
    assert_eq!(capped_mb(4096, 128 * MIB), 256);
  }

  #[test]
  fn the_current_memory_follows_the_setting_the_launch_uses() {
    let global_manual = global(false, 2048);
    assert_eq!(
      current_memory(&instance(), &global_manual, 5000 * MIB),
      (2048, MemorySource::Global)
    );

    let mut with_own = instance();
    with_own.use_spec_game_config = true;
    with_own.spec_game_config = Some(global(false, 3072));
    assert_eq!(
      current_memory(&with_own, &global_manual, 5000 * MIB),
      (3072, MemorySource::Instance)
    );

    // Own settings that are switched off are not in use.
    with_own.use_spec_game_config = false;
    assert_eq!(
      current_memory(&with_own, &global_manual, 5000 * MIB),
      (2048, MemorySource::Global)
    );

    let global_auto = global(true, 2048);
    assert_eq!(
      current_memory(&instance(), &global_auto, 5000 * MIB),
      (5000, MemorySource::Auto)
    );
    let mut own_auto = instance();
    own_auto.use_spec_game_config = true;
    own_auto.spec_game_config = Some(global(true, 1024));
    assert_eq!(
      current_memory(&own_auto, &global_manual, 5000 * MIB),
      (5000, MemorySource::Auto)
    );
    // Own settings switched on but never filled in fall back to the global ones.
    let mut empty_own = instance();
    empty_own.use_spec_game_config = true;
    assert_eq!(
      current_memory(&empty_own, &global_manual, 5000 * MIB),
      (2048, MemorySource::Global)
    );
  }

  #[test]
  fn it_asks_only_when_the_capped_value_is_higher_than_the_current_one() {
    let device = info(16384, 6000);
    let asked = prompt_for(
      &instance(),
      &suggestion(8196, None),
      &global(false, 4096),
      &device,
    )
    .unwrap();
    assert_eq!(asked.capped_mb, 8196);
    assert_eq!(asked.suggested_mb, 8196);
    assert_eq!(asked.current_mb, 4096);
    assert_eq!(asked.current_source, MemorySource::Global);
    assert_eq!(asked.total_mb, 16384);
    assert_eq!(asked.pack_name, "Pack");

    for current in [8196, 12000] {
      assert_eq!(
        prompt_for(
          &instance(),
          &suggestion(8196, None),
          &global(false, current),
          &device
        ),
        None
      );
    }

    // Automatic allocation counts as what it would pick now.
    assert_eq!(
      prompt_for(
        &instance(),
        &suggestion(8196, None),
        &global(true, 1024),
        &info(16384, 9000)
      ),
      None
    );
    let auto = prompt_for(
      &instance(),
      &suggestion(8196, None),
      &global(true, 1024),
      &device,
    )
    .unwrap();
    assert_eq!(
      (auto.current_mb, auto.current_source),
      (6000, MemorySource::Auto)
    );

    let mut own = instance();
    own.use_spec_game_config = true;
    own.spec_game_config = Some(global(false, 2048));
    let own_asked = prompt_for(
      &own,
      &suggestion(8196, None),
      &global(false, 16000),
      &device,
    )
    .unwrap();
    assert_eq!(
      (own_asked.current_mb, own_asked.current_source),
      (2048, MemorySource::Instance)
    );
  }

  #[test]
  fn a_small_device_compares_the_capped_value() {
    // The device cannot give 8196; 90% of 8192 is 7372, not above the 7400 in use.
    let device = info(8192, 4000);
    assert_eq!(
      prompt_for(
        &instance(),
        &suggestion(8196, None),
        &global(false, 7400),
        &device
      ),
      None
    );
    let asked = prompt_for(
      &instance(),
      &suggestion(8196, None),
      &global(false, 4096),
      &device,
    )
    .unwrap();
    assert_eq!((asked.suggested_mb, asked.capped_mb), (8196, 7372));
  }

  #[test]
  fn an_answered_value_is_not_asked_again_until_it_changes() {
    let device = info(16384, 6000);
    let current = global(false, 4096);
    assert_eq!(
      prompt_for(
        &instance(),
        &suggestion(8196, Some(8196)),
        &current,
        &device
      ),
      None
    );
    assert!(prompt_for(
      &instance(),
      &suggestion(10240, Some(8196)),
      &current,
      &device
    )
    .is_some());
  }

  #[test]
  fn recording_keeps_an_answer_for_the_same_value_only() {
    let dir = TestDir::new("record");
    let root = &dir.0;
    let fresh = Recommendation::of(&remote(Some(8196), None));
    fresh.record(root, "Pack");
    let first = read(root).unwrap();
    assert_eq!(first.suggested_mb, 8196);
    assert_eq!(first.source, SuggestionSource::Manifest);
    assert_eq!(first.pack_name, "Pack");
    assert_eq!(first.pack_version, "1.2.0");
    assert_eq!(first.answered_mb, None);

    write(
      root,
      &MemorySuggestion {
        answered_mb: Some(8196),
        ..first
      },
    )
    .unwrap();
    // The next version recommends the same: the answer stands.
    let mut same = remote(Some(8196), None);
    same.label = "1.3.0".into();
    Recommendation::of(&same).record(root, "Pack");
    let kept = read(root).unwrap();
    assert_eq!(kept.answered_mb, Some(8196));
    assert_eq!(kept.pack_version, "1.3.0");

    // A different value is a new question.
    Recommendation::of(&remote(Some(10240), None)).record(root, "Pack");
    let changed = read(root).unwrap();
    assert_eq!((changed.suggested_mb, changed.answered_mb), (10240, None));
  }

  #[test]
  fn a_version_without_a_recommendation_clears_the_pending_state() {
    let dir = TestDir::new("clear");
    Recommendation::of(&remote(Some(8196), None)).record(&dir.0, "Pack");
    assert!(read(&dir.0).is_some());
    Recommendation::of(&remote(None, None)).record(&dir.0, "Pack");
    assert!(read(&dir.0).is_none());
    assert!(!memory_path(&dir.0).exists());
    // Nothing recorded, nothing to clear: not an error.
    Recommendation::of(&remote(None, None)).record(&dir.0, "Pack");
  }

  #[test]
  fn an_unreadable_file_means_no_suggestion() {
    let dir = TestDir::new("garbage");
    fs::create_dir_all(dir.0.join(UPDATE_DIR)).unwrap();
    fs::write(memory_path(&dir.0), "not json").unwrap();
    assert!(read(&dir.0).is_none());
    // A later record replaces it.
    Recommendation::of(&remote(Some(4096), None)).record(&dir.0, "Pack");
    assert_eq!(read(&dir.0).unwrap().suggested_mb, 4096);
    assert!(!memory_path(&dir.0).with_extension("json.tmp").exists());
  }

  #[test]
  fn applying_copies_the_global_settings_and_changes_only_the_memory() {
    let mut base = global(true, 1024);
    base.game_window.resolution.width = 1920;
    base.performance.process_priority = crate::launcher_config::models::ProcessPriority::High;
    let mut target = instance();
    assert!(!target.use_spec_game_config && target.spec_game_config.is_none());

    apply_to_instance(&mut target, &base, 6144);

    assert!(target.use_spec_game_config);
    let own = target.spec_game_config.as_ref().unwrap();
    assert!(!own.performance.auto_mem_allocation);
    assert_eq!(own.performance.max_mem_allocation, 6144);
    let mut expected = base.clone();
    expected.performance.auto_mem_allocation = false;
    expected.performance.max_mem_allocation = 6144;
    assert_eq!(own, &expected);
    assert_eq!(own.game_window.resolution.width, 1920);
    // The global settings stay as they were.
    assert!(base.performance.auto_mem_allocation);
    assert_eq!(base.performance.max_mem_allocation, 1024);
  }

  #[test]
  fn applying_keeps_the_instances_own_settings_in_use() {
    let mut own = global(false, 2048);
    own.game_window.resolution.width = 800;
    let mut target = instance();
    target.spec_game_config = Some(own);
    target.use_spec_game_config = true;

    apply_to_instance(&mut target, &global(true, 1024), 4096);

    assert!(target.use_spec_game_config);
    let own = target.spec_game_config.unwrap();
    assert_eq!(own.game_window.resolution.width, 800);
    assert_eq!(own.performance.max_mem_allocation, 4096);
    assert!(!own.performance.auto_mem_allocation);
  }

  #[test]
  fn applying_replaces_unused_settings_with_the_global_ones() {
    let mut old = global(false, 2048);
    old.game_window.resolution.width = 800;
    let mut base = global(true, 1024);
    base.game_window.resolution.width = 1920;
    let mut target = instance();
    target.spec_game_config = Some(old);
    target.use_spec_game_config = false;

    apply_to_instance(&mut target, &base, 4096);

    assert!(target.use_spec_game_config);
    let own = target.spec_game_config.unwrap();
    assert_eq!(own.game_window.resolution.width, 1920);
    assert_eq!(own.performance.max_mem_allocation, 4096);
    assert!(!own.performance.auto_mem_allocation);
  }

  #[test]
  fn an_old_file_without_an_answer_still_reads() {
    let dir = TestDir::new("old");
    fs::create_dir_all(dir.0.join(UPDATE_DIR)).unwrap();
    fs::write(
      memory_path(&dir.0),
      r#"{"suggestedMb":4096,"source":"instanceCfg","packName":"P","packVersion":"1"}"#,
    )
    .unwrap();
    let read = read(&dir.0).unwrap();
    assert_eq!(read.source, SuggestionSource::InstanceCfg);
    assert_eq!(read.answered_mb, None);
  }
}
