//! Instance identity. The id is a UUID kept in each instance's `ustblcfg.json`;
//! the folder name is only the on-disk name and the display name is only for
//! people. Instances written by older versions carry the id `<game dir>:<folder>`.

use crate::instance::constants::INSTANCE_CFG_FILE_NAME;
use crate::instance::models::misc::Instance;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// Serializes the read-modify-write of a single config file by the helpers here,
/// so the main refresh and the pending-install scan cannot assign two ids.
static CFG_PATCH_LOCK: Mutex<()> = Mutex::new(());

pub fn new_instance_id() -> String {
  Uuid::new_v4().to_string()
}

pub fn is_instance_id(id: &str) -> bool {
  id.len() == 36 && Uuid::parse_str(id).is_ok()
}

/// Rewrites one instance config through the typed model. Returns false when the
/// file is missing or unreadable; the caller then persists the value itself.
fn patch_cfg(version_path: &Path, patch: impl FnOnce(&mut Instance)) -> bool {
  let _guard = CFG_PATCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
  let path = version_path.join(INSTANCE_CFG_FILE_NAME);
  let Ok(text) = fs::read_to_string(&path) else {
    return false;
  };
  let Ok(mut instance) = serde_json::from_str::<Instance>(&text) else {
    return false;
  };
  patch(&mut instance);
  serde_json::to_string_pretty(&instance)
    .ok()
    .is_some_and(|text| fs::write(&path, text).is_ok())
}

/// The id stored for an instance, assigning and writing a new one when the
/// loaded value is not a UUID. The file is read again under the lock, so a
/// concurrent assignment by another path is adopted instead of replaced.
pub fn read_or_assign_id(version_path: &Path, loaded: &str) -> String {
  if is_instance_id(loaded) {
    return loaded.to_string();
  }
  let mut assigned = String::new();
  let written = patch_cfg(version_path, |instance| {
    if !is_instance_id(&instance.id) {
      instance.id = new_instance_id();
    }
    assigned = instance.id.clone();
  });
  if written {
    assigned
  } else {
    // No readable config yet; the caller saves the whole config with this id.
    new_instance_id()
  }
}

/// Gives the instance a fresh id, for a copy of a folder that shares one.
pub fn reassign_id(version_path: &Path) -> String {
  let id = new_instance_id();
  patch_cfg(version_path, |instance| instance.id = id.clone());
  id
}

/// Persists a new display name without touching any other field.
pub fn write_display_name(version_path: &Path, display_name: &str) -> bool {
  patch_cfg(version_path, |instance| {
    instance.display_name = display_name.to_string()
  })
}

/// Keeps the first instance of every id in the given order and gives the others
/// new ids, written back to their configs. The order decides who keeps the id.
pub fn dedup_instance_ids<'a>(
  instances: impl IntoIterator<Item = &'a mut Instance>,
  already_taken: &HashSet<String>,
) {
  let mut seen: HashSet<String> = already_taken.clone();
  for instance in instances {
    if !seen.insert(instance.id.clone()) {
      instance.id = reassign_id(&instance.version_path);
      seen.insert(instance.id.clone());
    }
  }
}

/// The id older versions gave an instance.
#[cfg(test)]
pub fn legacy_instance_id(game_dir: &str, folder: &str) -> String {
  format!("{game_dir}:{folder}")
}

/// Finds an instance by its old `<game dir>:<folder>` id. A UUID never matches.
/// Folder names cannot hold a colon on Windows, so the last colon splits the id.
pub fn find_by_legacy_id<'a>(
  instances: impl IntoIterator<Item = &'a Instance>,
  id: &str,
) -> Option<&'a Instance> {
  if is_instance_id(id) {
    return None;
  }
  let (game_dir, folder) = id.rsplit_once(':')?;
  instances
    .into_iter()
    .find(|instance| instance.game_dir == game_dir && instance.name == folder)
}

/// Looks an instance up by its current id, then by its old id.
pub fn find_instance<'a>(
  instances: &'a HashMap<String, Instance>,
  id: &str,
) -> Option<&'a Instance> {
  instances
    .get(id)
    .or_else(|| find_by_legacy_id(instances.values(), id))
}

/// The id to select instead of `selected`, when `selected` is an old id of an
/// instance in the table. None when nothing needs rewriting.
pub fn migrated_selected_id(
  instances: &HashMap<String, Instance>,
  selected: &str,
) -> Option<String> {
  if selected.is_empty() || instances.contains_key(selected) {
    return None;
  }
  find_by_legacy_id(instances.values(), selected).map(|instance| instance.id.clone())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-identity-{tag}-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
  }

  fn write_cfg(root: &Path, instance: &Instance) {
    fs::write(
      root.join(INSTANCE_CFG_FILE_NAME),
      serde_json::to_string_pretty(instance).unwrap(),
    )
    .unwrap();
  }

  fn read_cfg(root: &Path) -> Instance {
    serde_json::from_str(&fs::read_to_string(root.join(INSTANCE_CFG_FILE_NAME)).unwrap()).unwrap()
  }

  fn instance(id: &str, game_dir: &str, name: &str) -> Instance {
    Instance {
      id: id.into(),
      name: name.into(),
      game_dir: game_dir.into(),
      ..Default::default()
    }
  }

  #[test]
  fn a_legacy_id_is_replaced_by_a_uuid_and_written_back() {
    let root = temp_root("assign");
    write_cfg(&root, &instance("games:pack", "games", "pack"));
    let id = read_or_assign_id(&root, "games:pack");
    assert!(is_instance_id(&id));
    assert_eq!(read_cfg(&root).id, id);
    // Later reads keep it.
    assert_eq!(read_or_assign_id(&root, &id), id);
    assert_eq!(read_or_assign_id(&root, "games:pack"), id);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_missing_id_is_assigned_and_other_fields_survive() {
    let root = temp_root("fields");
    let mut original = instance("", "games", "pack");
    original.display_name = "My pack".into();
    original.starred = true;
    write_cfg(&root, &original);
    let id = read_or_assign_id(&root, "");
    let stored = read_cfg(&root);
    assert_eq!(stored.id, id);
    assert_eq!(stored.display_name, "My pack");
    assert!(stored.starred);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_valid_id_is_not_touched() {
    let root = temp_root("keep");
    let existing = new_instance_id();
    write_cfg(&root, &instance(&existing, "games", "pack"));
    assert_eq!(read_or_assign_id(&root, &existing), existing);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn duplicate_ids_are_reassigned_for_all_but_the_first() {
    let (first, second, third) = (temp_root("dup-a"), temp_root("dup-b"), temp_root("dup-c"));
    let shared = new_instance_id();
    let mut instances = vec![
      Instance {
        version_path: first.clone(),
        ..instance(&shared, "games", "a")
      },
      Instance {
        version_path: second.clone(),
        ..instance(&shared, "games", "b")
      },
      Instance {
        version_path: third.clone(),
        ..instance(&shared, "other", "a")
      },
    ];
    for instance in &instances {
      write_cfg(&instance.version_path, instance);
    }
    dedup_instance_ids(instances.iter_mut(), &HashSet::new());
    assert_eq!(instances[0].id, shared);
    assert_ne!(instances[1].id, shared);
    assert_ne!(instances[2].id, shared);
    assert_ne!(instances[1].id, instances[2].id);
    assert_eq!(read_cfg(&first).id, shared);
    assert_eq!(read_cfg(&second).id, instances[1].id);
    assert_eq!(read_cfg(&third).id, instances[2].id);
    for root in [first, second, third] {
      fs::remove_dir_all(root).unwrap();
    }
  }

  #[test]
  fn an_id_taken_elsewhere_counts_as_a_duplicate() {
    let root = temp_root("taken");
    let shared = new_instance_id();
    let mut copy = Instance {
      version_path: root.clone(),
      ..instance(&shared, "games", "copy")
    };
    write_cfg(&root, &copy);
    dedup_instance_ids([&mut copy], &HashSet::from([shared.clone()]));
    assert_ne!(copy.id, shared);
    assert_eq!(read_cfg(&root).id, copy.id);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn legacy_ids_resolve_by_game_dir_and_folder() {
    let mut table = HashMap::new();
    for (game_dir, folder) in [("games", "pack"), ("other", "pack"), ("games", "alpha")] {
      let id = new_instance_id();
      table.insert(id.clone(), instance(&id, game_dir, folder));
    }
    let found = find_instance(&table, "other:pack").unwrap();
    assert_eq!(
      (found.game_dir.as_str(), found.name.as_str()),
      ("other", "pack")
    );
    assert!(find_instance(&table, "games:missing").is_none());
    assert!(find_instance(&table, "pack").is_none());
    // A game directory name may hold a colon; the folder never does.
    table.insert("x".into(), instance("x", "D:\\mc", "pack"));
    assert_eq!(find_instance(&table, "D:\\mc:pack").unwrap().id, "x");
    // A current id is found directly.
    let current = table
      .keys()
      .find(|key| is_instance_id(key))
      .unwrap()
      .clone();
    assert_eq!(find_instance(&table, &current).unwrap().id, current);
  }

  #[test]
  fn the_selected_id_is_migrated_only_when_it_is_an_old_id() {
    let mut table = HashMap::new();
    let id = new_instance_id();
    table.insert(id.clone(), instance(&id, "games", "pack"));
    assert_eq!(
      migrated_selected_id(&table, &legacy_instance_id("games", "pack")),
      Some(id.clone())
    );
    assert_eq!(migrated_selected_id(&table, &id), None);
    assert_eq!(migrated_selected_id(&table, ""), None);
    assert_eq!(migrated_selected_id(&table, "games:gone"), None);
    // Repeating after the rewrite changes nothing.
    assert_eq!(migrated_selected_id(&table, &id), None);
  }

  #[test]
  fn writing_the_display_name_changes_only_that_field() {
    let root = temp_root("rename");
    let mut original = instance(&new_instance_id(), "games", "pack");
    original.description = "keep".into();
    write_cfg(&root, &original);
    assert!(write_display_name(&root, "Renamed"));
    let stored = read_cfg(&root);
    assert_eq!(stored.display_name, "Renamed");
    assert_eq!(stored.name, "pack");
    assert_eq!(stored.description, "keep");
    assert_eq!(stored.id, original.id);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn display_name_is_persisted_but_game_dir_is_not() {
    let mut inst = instance("id", "games", "pack");
    inst.display_name = "Mine".into();
    let value = serde_json::to_value(&inst).unwrap();
    assert_eq!(value["displayName"], "Mine");
    assert!(value.get("gameDir").is_none() && value.get("game_dir").is_none());
    // A config written before display names existed still loads.
    let old: Instance = serde_json::from_str(r#"{"id":"games:pack","name":"pack"}"#).unwrap();
    assert_eq!(old.display_name, "");
    assert_eq!(old.shown_name(), "pack");
  }

  #[test]
  fn shown_name_falls_back_to_the_folder() {
    let mut inst = instance("id", "games", "pack");
    assert_eq!(inst.shown_name(), "pack");
    inst.display_name = "  ".into();
    assert_eq!(inst.shown_name(), "pack");
    inst.display_name = "Mine".into();
    assert_eq!(inst.shown_name(), "Mine");
  }
}
