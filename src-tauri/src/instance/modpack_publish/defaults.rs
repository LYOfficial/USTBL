use super::commands::{PreviousFile, PreviousRelease};
use crate::instance::modpack_update::remote::{RemoteRule, RuleKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};

const DISABLED_SUFFIX: &str = ".disabled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskFile {
  pub path: String,
  pub disabled: bool,
  pub conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostEntry {
  pub path: String,
  pub kind: RuleKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishDefaults {
  pub files: Vec<DiskFile>,
  pub ghosts: Vec<GhostEntry>,
  pub rules: Vec<RemoteRule>,
  pub included: Vec<String>,
  pub once: Vec<String>,
}

impl PublishDefaults {
  /// Whether a file on disk is published in this starting state: ticked
  /// explicitly, or not covered by an ignore/delete rule. This is what the
  /// publish tree shows for the file before the author touches it.
  pub fn publishes(&self, path: &str) -> bool {
    let folded = key(path);
    if self.included.binary_search(&folded).is_ok() {
      return true;
    }
    let own_rule = self
      .rules
      .iter()
      .any(|rule| rule.kind == RuleKind::File && key(&rule.path) == folded);
    !own_rule && covering_rule(&self.rules, path).is_none()
  }
}

fn key(path: &str) -> String {
  path.to_lowercase()
}

pub fn normalize_disk_files(all: &[String]) -> Vec<DiskFile> {
  let mut files: HashMap<String, DiskFile> = HashMap::new();
  let mut order = Vec::new();
  for path in all {
    let folded = key(path);
    let disabled = folded.ends_with(DISABLED_SUFFIX);
    if disabled {
      let base = &path[..path.len() - DISABLED_SUFFIX.len()];
      if base.is_empty() || !key(base).starts_with("mods/") {
        continue;
      }
      let folded = key(base);
      if let Some(existing) = files.get_mut(&folded) {
        existing.conflict = true;
      } else {
        files.insert(
          folded.clone(),
          DiskFile {
            path: base.to_string(),
            disabled: true,
            conflict: false,
          },
        );
        order.push(folded);
      }
      continue;
    }
    if let Some(existing) = files.get_mut(&folded) {
      existing.conflict = true;
      existing.path = path.clone();
      existing.disabled = false;
    } else {
      files.insert(
        folded.clone(),
        DiskFile {
          path: path.clone(),
          disabled: false,
          conflict: false,
        },
      );
      order.push(folded);
    }
  }
  order
    .into_iter()
    .filter_map(|folded| files.remove(&folded))
    .collect()
}

fn dir_keys(keys: &HashSet<String>) -> HashSet<String> {
  let mut dirs = HashSet::new();
  for path in keys {
    let mut cut = path.rfind('/');
    while let Some(index) = cut {
      if index == 0 || !dirs.insert(path[..index].to_string()) {
        break;
      }
      cut = path[..index].rfind('/');
    }
  }
  dirs
}

pub(crate) fn covering_rule<'a>(rules: &'a [RemoteRule], path: &str) -> Option<&'a RemoteRule> {
  let folded = key(path);
  rules
    .iter()
    .filter(|rule| rule.kind == RuleKind::Dir)
    .filter(|rule| folded.starts_with(&format!("{}/", key(&rule.path))))
    .max_by_key(|rule| key(&rule.path).len())
}

fn strip_ghosts(
  ghosts: Vec<GhostEntry>,
  disk_keys: &HashSet<String>,
  disk_dirs: &HashSet<String>,
) -> Vec<GhostEntry> {
  let mut taken = HashSet::new();
  let file_ghosts: HashSet<String> = ghosts
    .iter()
    .filter(|ghost| ghost.kind == RuleKind::File)
    .map(|ghost| key(&ghost.path))
    .collect();
  ghosts
    .into_iter()
    .filter(|ghost| {
      let folded = key(&ghost.path);
      if !taken.insert(folded.clone()) || disk_keys.contains(&folded) {
        return false;
      }
      if ghost.kind == RuleKind::File && disk_dirs.contains(&folded) {
        return false;
      }
      let mut current = folded.as_str();
      while let Some(index) = current.rfind('/') {
        let parent = &current[..index];
        if disk_keys.contains(parent) || file_ghosts.contains(parent) {
          return false;
        }
        current = parent;
      }
      true
    })
    .collect()
}

fn mentioned(entry: &SuggestedEntry, previous: &[PreviousFile], rules: &[RemoteRule]) -> bool {
  let wanted = key(&entry.path);
  let inside = |other: &str| other == wanted || other.starts_with(&format!("{wanted}/"));
  previous.iter().any(|file| inside(&key(&file.path)))
    || rules.iter().any(|rule| {
      let rule_key = key(&rule.path);
      inside(&rule_key)
        || (rule.kind == RuleKind::Dir && wanted.starts_with(&format!("{rule_key}/")))
    })
}

#[derive(Debug, Clone)]
pub struct SuggestedEntry {
  pub path: String,
  pub kind: RuleKind,
}

pub fn create_initial_model(
  all: &[String],
  previous: Option<&PreviousRelease>,
  suggested: &[SuggestedEntry],
) -> PublishDefaults {
  let files = normalize_disk_files(all);
  let disk_keys: HashSet<String> = files.iter().map(|file| key(&file.path)).collect();
  let disk_dirs = dir_keys(&disk_keys);
  let previous_files = previous
    .map(|release| release.files.as_slice())
    .unwrap_or(&[]);
  let mut rules = previous
    .map(|release| release.rules.clone())
    .unwrap_or_default();
  let mut ghosts = Vec::new();
  for file in previous_files {
    if !disk_keys.contains(&key(&file.path)) {
      ghosts.push(GhostEntry {
        path: file.path.clone(),
        kind: RuleKind::File,
      });
    }
  }
  for rule in &rules {
    let present = match rule.kind {
      RuleKind::File => disk_keys.contains(&key(&rule.path)),
      RuleKind::Dir => disk_dirs.contains(&key(&rule.path)),
    };
    if !present {
      ghosts.push(GhostEntry {
        path: rule.path.clone(),
        kind: rule.kind,
      });
    }
  }
  ghosts = strip_ghosts(ghosts, &disk_keys, &disk_dirs);
  for entry in suggested {
    if !mentioned(entry, previous_files, &rules) {
      rules.push(RemoteRule {
        path: entry.path.clone(),
        kind: entry.kind,
        state: crate::instance::modpack_update::remote::RuleState::Ignore,
      });
    }
  }
  let previous_by_key: HashMap<String, &PreviousFile> = previous_files
    .iter()
    .map(|file| (key(&file.path), file))
    .collect();
  let mut included = BTreeSet::new();
  let mut once = BTreeSet::new();
  for file in &files {
    let folded = key(&file.path);
    let Some(previous) = previous_by_key.get(&folded) else {
      continue;
    };
    if previous.once {
      once.insert(folded.clone());
    }
    if !rules
      .iter()
      .any(|rule| rule.kind == RuleKind::File && key(&rule.path) == folded)
      && covering_rule(&rules, &file.path).is_some()
    {
      included.insert(folded);
    }
  }
  PublishDefaults {
    files,
    ghosts,
    rules,
    included: included.into_iter().collect(),
    once: once.into_iter().collect(),
  }
}

#[cfg(test)]
mod tests {
  //! Ported from `scripts/tests/modpack-publish-tree.test.cjs` (the cases that
  //! assert the initial result), one test per TS case. The state helpers below
  //! mirror `viewOf`/`toPublishPayload` of `src/utils/modpack-publish-tree.ts`.
  use super::*;
  use crate::instance::modpack_update::remote::RuleState;

  fn file(path: &str, once: bool) -> PreviousFile {
    PreviousFile {
      path: path.into(),
      sha1: String::new(),
      size: 0,
      disabled: false,
      once,
    }
  }

  fn rule(path: &str, kind: RuleKind, state: RuleState) -> RemoteRule {
    RemoteRule {
      path: path.into(),
      kind,
      state,
    }
  }

  fn previous(files: Vec<PreviousFile>, rules: Vec<RemoteRule>) -> PreviousRelease {
    PreviousRelease {
      files,
      rules,
      rules_manifest: true,
      version_id: None,
      project_id: None,
    }
  }

  fn suggest(path: &str, kind: RuleKind) -> SuggestedEntry {
    SuggestedEntry {
      path: path.into(),
      kind,
    }
  }

  fn model(
    all: &[&str],
    previous: Option<PreviousRelease>,
    suggested: &[SuggestedEntry],
  ) -> PublishDefaults {
    let all: Vec<String> = all.iter().map(|path| (*path).to_string()).collect();
    create_initial_model(&all, previous.as_ref(), suggested)
  }

  /// `viewOf(...).fileState`
  fn file_state(m: &PublishDefaults, path: &str) -> &'static str {
    let folded = key(path);
    let real = m.files.iter().any(|file| key(&file.path) == folded);
    if real && m.included.contains(&folded) {
      return "include";
    }
    if let Some(own) = m
      .rules
      .iter()
      .find(|rule| rule.kind == RuleKind::File && key(&rule.path) == folded)
    {
      return own.state.as_str();
    }
    if let Some(covering) = covering_rule(&m.rules, path) {
      return covering.state.as_str();
    }
    if real {
      "include"
    } else {
      "remove"
    }
  }

  /// `viewOf(...).dirState`
  fn dir_state(m: &PublishDefaults, path: &str) -> &'static str {
    let dir = key(path);
    let states: BTreeSet<&'static str> = m
      .files
      .iter()
      .filter(|file| key(&file.path).starts_with(&format!("{dir}/")))
      .map(|file| file_state(m, &file.path))
      .collect();
    match states.len() {
      0 => m
        .rules
        .iter()
        .find(|rule| rule.kind == RuleKind::Dir && key(&rule.path) == dir)
        .or_else(|| covering_rule(&m.rules, path))
        .map_or("remove", |rule| rule.state.as_str()),
      1 => states.into_iter().next().unwrap(),
      _ => "mixed",
    }
  }

  fn is_once(m: &PublishDefaults, path: &str) -> bool {
    m.once.contains(&key(path))
  }

  /// `toPublishPayload`: (path, disabled, once) of the ticked files.
  fn payload_files(m: &PublishDefaults) -> Vec<(String, bool, bool)> {
    m.files
      .iter()
      .filter(|file| file_state(m, &file.path) == "include")
      .map(|file| (file.path.clone(), file.disabled, is_once(m, &file.path)))
      .collect()
  }

  fn payload_conflicts(m: &PublishDefaults) -> Vec<String> {
    m.files
      .iter()
      .filter(|file| file.conflict && file_state(m, &file.path) == "include")
      .map(|file| file.path.clone())
      .collect()
  }

  fn paths(m: &PublishDefaults) -> Vec<String> {
    let mut paths: Vec<String> = payload_files(m).into_iter().map(|file| file.0).collect();
    paths.sort();
    paths
  }

  fn ghost(path: &str, kind: RuleKind) -> GhostEntry {
    GhostEntry {
      path: path.into(),
      kind,
    }
  }

  #[test]
  fn first_release_every_file_is_ticked_no_rules_nothing_once_or_disabled() {
    let m = model(&["config/a.toml", "mods/x.jar", "options.txt"], None, &[]);
    assert_eq!(paths(&m), ["config/a.toml", "mods/x.jar", "options.txt"]);
    assert!(m.rules.is_empty());
    assert!(payload_files(&m).iter().all(|file| !file.2 && !file.1));
    assert!(payload_conflicts(&m).is_empty());
    assert!(m.ghosts.is_empty());
  }

  #[test]
  fn first_release_a_suggested_dir_becomes_one_ignore_rule_at_its_own_level() {
    let m = model(
      &[
        "saves/w1/level.dat",
        "saves/w1/region/r.0.mca",
        "saves/w2/level.dat",
        "mods/x.jar",
      ],
      None,
      &[suggest("saves", RuleKind::Dir)],
    );
    assert_eq!(
      m.rules,
      vec![rule("saves", RuleKind::Dir, RuleState::Ignore)]
    );
    assert_eq!(file_state(&m, "saves/w1/level.dat"), "ignore");
    assert_eq!(file_state(&m, "saves/w1/region/r.0.mca"), "ignore");
    assert_eq!(dir_state(&m, "saves"), "ignore");
    assert_eq!(paths(&m), ["mods/x.jar"]);
    assert_eq!(
      m.rules,
      vec![rule("saves", RuleKind::Dir, RuleState::Ignore)]
    );
  }

  #[test]
  fn first_release_a_suggested_single_file_becomes_a_file_rule_a_nested_dir_a_dir_rule() {
    let m = model(
      &[
        "options.txt",
        "mods/VoxelMods/a.json",
        "mods/VoxelMods/b.json",
        "mods/x.jar",
      ],
      None,
      &[
        suggest("options.txt", RuleKind::File),
        suggest("mods/VoxelMods", RuleKind::Dir),
      ],
    );
    assert_eq!(m.rules.len(), 2);
    let mut rules = m.rules.clone();
    rules.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
      rules,
      vec![
        rule("mods/VoxelMods", RuleKind::Dir, RuleState::Ignore),
        rule("options.txt", RuleKind::File, RuleState::Ignore),
      ]
    );
    assert_eq!(paths(&m), ["mods/x.jar"]);
  }

  #[test]
  fn previous_release_files_still_on_disk_stay_ticked_and_keep_their_once_flag() {
    let m = model(
      &["config/a.toml", "config/b.toml"],
      Some(previous(
        vec![file("config/a.toml", true), file("config/b.toml", false)],
        vec![],
      )),
      &[],
    );
    assert_eq!(file_state(&m, "config/a.toml"), "include");
    assert!(is_once(&m, "config/a.toml"));
    assert!(!is_once(&m, "config/b.toml"));
    let mut files: Vec<(String, bool)> = payload_files(&m)
      .into_iter()
      .map(|file| (file.0, file.2))
      .collect();
    files.sort();
    assert_eq!(
      files,
      vec![
        ("config/a.toml".to_string(), true),
        ("config/b.toml".to_string(), false)
      ]
    );
  }

  #[test]
  fn previous_release_new_files_with_no_rule_are_ticked_and_not_once() {
    let m = model(
      &["config/a.toml", "config/new.toml"],
      Some(previous(vec![file("config/a.toml", true)], vec![])),
      &[],
    );
    assert_eq!(file_state(&m, "config/new.toml"), "include");
    assert!(!is_once(&m, "config/new.toml"));
  }

  #[test]
  fn previous_release_its_rules_carry_over_as_they_were_even_when_the_target_is_gone() {
    let prev_rules = vec![
      rule("gone/dir", RuleKind::Dir, RuleState::Delete),
      rule("resourcepacks/old.zip", RuleKind::File, RuleState::Delete),
      rule("config/cache", RuleKind::Dir, RuleState::Ignore),
    ];
    let m = model(
      &[
        "resourcepacks/old.zip",
        "config/cache/x.bin",
        "config/a.toml",
      ],
      Some(previous(
        vec![file("config/a.toml", false)],
        prev_rules.clone(),
      )),
      &[],
    );
    assert_eq!(m.rules, prev_rules);
    assert_eq!(file_state(&m, "resourcepacks/old.zip"), "delete");
    assert_eq!(file_state(&m, "config/cache/x.bin"), "ignore");
    assert!(m.ghosts.contains(&ghost("gone/dir", RuleKind::Dir)));
  }

  #[test]
  fn previous_release_a_new_file_under_a_previous_rule_inherits_it() {
    let m = model(
      &["config/cache/new.bin"],
      Some(previous(
        vec![],
        vec![rule("config/cache", RuleKind::Dir, RuleState::Delete)],
      )),
      &[],
    );
    assert_eq!(file_state(&m, "config/cache/new.bin"), "delete");
    assert!(paths(&m).is_empty());
  }

  #[test]
  fn previous_release_a_file_it_listed_under_its_own_dir_rule_stays_ticked_new_siblings_do_not() {
    let m = model(
      &["saves/keep.dat", "saves/other.dat"],
      Some(previous(
        vec![file("saves/keep.dat", false)],
        vec![rule("saves", RuleKind::Dir, RuleState::Ignore)],
      )),
      &[],
    );
    assert_eq!(file_state(&m, "saves/keep.dat"), "include");
    assert_eq!(file_state(&m, "saves/other.dat"), "ignore");
    assert_eq!(paths(&m), ["saves/keep.dat"]);
    assert_eq!(
      m.rules,
      vec![rule("saves", RuleKind::Dir, RuleState::Ignore)]
    );
  }

  #[test]
  fn previous_release_without_the_suggested_path_suggestion_applies_previous_rules_kept() {
    let m = model(
      &["config/a.toml", "saves/w1/level.dat"],
      Some(previous(
        vec![file("config/a.toml", false)],
        vec![rule("config/cache", RuleKind::Dir, RuleState::Ignore)],
      )),
      &[suggest("saves", RuleKind::Dir)],
    );
    assert!(m
      .rules
      .iter()
      .any(|r| r.path == "config/cache" && r.state == RuleState::Ignore));
    assert_eq!(m.rules.iter().filter(|r| r.path == "saves").count(), 1);
    assert_eq!(
      m.rules.iter().find(|r| r.path == "saves"),
      Some(&rule("saves", RuleKind::Dir, RuleState::Ignore))
    );
    assert_eq!(m.rules.len(), 2);
    assert_eq!(file_state(&m, "saves/w1/level.dat"), "ignore");
  }

  #[test]
  fn suggestion_is_skipped_when_the_previous_release_listed_a_file_inside_the_path() {
    let m = model(
      &["saves/w1/level.dat", "saves/w2/level.dat"],
      Some(previous(vec![file("saves/w1/level.dat", false)], vec![])),
      &[suggest("saves", RuleKind::Dir)],
    );
    assert!(m.rules.is_empty());
    assert_eq!(file_state(&m, "saves/w1/level.dat"), "include");
    assert_eq!(file_state(&m, "saves/w2/level.dat"), "include");
  }

  #[test]
  fn suggestion_is_skipped_when_the_previous_release_had_a_rule_on_or_below_the_path() {
    let on_path = model(
      &["saves/w1/level.dat"],
      Some(previous(
        vec![],
        vec![rule("saves", RuleKind::Dir, RuleState::Delete)],
      )),
      &[suggest("saves", RuleKind::Dir)],
    );
    assert_eq!(
      on_path.rules,
      vec![rule("saves", RuleKind::Dir, RuleState::Delete)]
    );

    let below = model(
      &["saves/w1/level.dat"],
      Some(previous(
        vec![],
        vec![rule("saves/w1", RuleKind::Dir, RuleState::Delete)],
      )),
      &[suggest("saves", RuleKind::Dir)],
    );
    assert_eq!(
      below.rules,
      vec![rule("saves/w1", RuleKind::Dir, RuleState::Delete)]
    );

    let file_rule = model(
      &["options.txt"],
      Some(previous(
        vec![],
        vec![rule("options.txt", RuleKind::File, RuleState::Delete)],
      )),
      &[suggest("options.txt", RuleKind::File)],
    );
    assert_eq!(
      file_rule.rules,
      vec![rule("options.txt", RuleKind::File, RuleState::Delete)]
    );
  }

  #[test]
  fn a_previous_path_that_merely_shares_a_name_prefix_does_not_count_as_mentioning_the_suggestion()
  {
    let m = model(
      &["saves/w1/level.dat", "saves2/a.dat"],
      Some(previous(vec![file("saves2/a.dat", false)], vec![])),
      &[suggest("saves", RuleKind::Dir)],
    );
    assert_eq!(
      m.rules,
      vec![rule("saves", RuleKind::Dir, RuleState::Ignore)]
    );
  }

  #[test]
  fn suggestion_matching_is_case_insensitive() {
    let m = model(
      &["saves/w1/level.dat"],
      Some(previous(vec![file("Saves/W1/Level.dat", false)], vec![])),
      &[suggest("saves", RuleKind::Dir)],
    );
    assert!(m.rules.is_empty());
    assert_eq!(file_state(&m, "saves/w1/level.dat"), "include");
  }

  #[test]
  fn virtual_file_listed_by_the_previous_release_missing_on_disk_shown_as_remove_and_never_published(
  ) {
    let m = model(
      &["a/keep.txt"],
      Some(previous(
        vec![file("a/keep.txt", false), file("a/old.txt", true)],
        vec![],
      )),
      &[],
    );
    assert_eq!(m.ghosts, vec![ghost("a/old.txt", RuleKind::File)]);
    assert_eq!(file_state(&m, "a/old.txt"), "remove");
    assert_eq!(paths(&m), ["a/keep.txt"]);
    assert!(m.rules.is_empty());
  }

  /// The TS case then calls `setFileState(..., "include")`, a draft edit that
  /// only exists in the dialog. Here: the initial state it starts from, which
  /// is what an ignored tick leaves unchanged.
  #[test]
  fn virtual_file_cannot_be_ticked() {
    let m = model(
      &["a/keep.txt"],
      Some(previous(vec![file("a/old.txt", false)], vec![])),
      &[],
    );
    assert_eq!(file_state(&m, "a/old.txt"), "remove");
    assert_eq!(paths(&m), ["a/keep.txt"]);
    assert!(m.rules.is_empty());
    assert!(m.included.is_empty());
  }

  /// The draft edits (`setFileState`, `restoreInheritance`) are dialog code;
  /// each state is reached here by putting the rule the edit would write.
  #[test]
  fn virtual_file_can_be_set_to_ignore_or_delete_file_rule_and_back_to_remove() {
    let mut m = model(
      &["a/keep.txt"],
      Some(previous(vec![file("a/old.txt", false)], vec![])),
      &[],
    );
    m.rules = vec![rule("a/old.txt", RuleKind::File, RuleState::Ignore)];
    assert_eq!(file_state(&m, "a/old.txt"), "ignore");
    m.rules = vec![rule("a/old.txt", RuleKind::File, RuleState::Delete)];
    assert_eq!(file_state(&m, "a/old.txt"), "delete");
    m.rules = vec![];
    assert_eq!(file_state(&m, "a/old.txt"), "remove");
  }

  /// `canRemoveFile` is "no directory rule covers the path".
  #[test]
  fn virtual_file_remove_is_only_offered_when_no_dir_rule_covers_it() {
    let can_remove = |rules: Vec<RemoteRule>, path: &str| covering_rule(&rules, path).is_none();
    assert!(can_remove(vec![], "a/old.txt"));
    assert!(!can_remove(
      vec![rule("a", RuleKind::Dir, RuleState::Ignore)],
      "a/old.txt"
    ));
    assert!(!can_remove(
      vec![rule("A", RuleKind::Dir, RuleState::Delete)],
      "a/b/old.txt"
    ));
    assert!(can_remove(
      vec![rule("a/old.txt", RuleKind::File, RuleState::Ignore)],
      "a/old.txt"
    ));
  }

  /// The TS case also checks the tree node of `gonedir` (`buildPublishNodes`),
  /// which is dialog code; the ghost entries are what the node comes from.
  #[test]
  fn virtual_node_for_a_previous_rule_whose_target_is_gone() {
    let m = model(
      &["config/a.toml"],
      Some(previous(
        vec![],
        vec![
          rule("gone.txt", RuleKind::File, RuleState::Ignore),
          rule("gonedir", RuleKind::Dir, RuleState::Delete),
        ],
      )),
      &[],
    );
    let mut ghosts = m.ghosts.clone();
    ghosts.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
      ghosts,
      vec![
        ghost("gone.txt", RuleKind::File),
        ghost("gonedir", RuleKind::Dir)
      ]
    );
  }

  fn disk(all: &[&str]) -> Vec<String> {
    all.iter().map(|path| (*path).to_string()).collect()
  }

  #[test]
  fn mods_jar_disabled_is_listed_under_its_enabled_name_with_disabled_true() {
    let files = normalize_disk_files(&disk(&[
      "mods/a.jar.disabled",
      "mods/b.jar",
      "mods/sub/c.jar.disabled",
    ]));
    let by_path: HashMap<&str, &DiskFile> = files
      .iter()
      .map(|file| (file.path.as_str(), file))
      .collect();
    assert!(by_path["mods/a.jar"].disabled);
    assert!(!by_path["mods/b.jar"].disabled);
    assert!(by_path["mods/sub/c.jar"].disabled);
    assert_eq!(files.len(), 3);
    assert!(files
      .iter()
      .all(|file| !file.path.ends_with(".disabled") && !file.conflict));
  }

  #[test]
  fn a_disabled_mod_is_published_under_the_enabled_name_with_disabled_true() {
    let m = model(&["mods/a.jar.disabled", "mods/b.jar"], None, &[]);
    let mut files = payload_files(&m);
    files.sort();
    assert_eq!(
      files,
      vec![
        ("mods/a.jar".to_string(), true, false),
        ("mods/b.jar".to_string(), false, false)
      ]
    );
    assert!(files.iter().all(|file| !file.0.ends_with(".disabled")));
  }

  #[test]
  fn the_disabled_suffix_is_recognised_case_insensitively() {
    let files = normalize_disk_files(&disk(&["Mods/A.JAR.DISABLED"]));
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "Mods/A.JAR");
    assert!(files[0].disabled);
  }

  #[test]
  fn previous_release_listing_the_enabled_name_matches_a_disabled_file_on_disk_not_a_virtual_node()
  {
    let m = model(
      &["mods/a.jar.disabled"],
      Some(previous(vec![file("mods/a.jar", true)], vec![])),
      &[],
    );
    assert!(m.ghosts.is_empty());
    assert_eq!(
      payload_files(&m),
      vec![("mods/a.jar".to_string(), true, true)]
    );
  }

  #[test]
  fn the_disk_decides_disabled_a_mod_the_previous_release_shipped_enabled_is_published_disabled_once_disabled_locally(
  ) {
    let m = model(
      &["mods/a.jar.disabled"],
      Some(previous(vec![file("mods/a.jar", false)], vec![])),
      &[],
    );
    assert!(payload_files(&m)[0].1);
  }

  #[test]
  fn both_variants_on_disk_are_flagged_as_a_conflict() {
    let files = normalize_disk_files(&disk(&["mods/a.jar", "mods/a.jar.disabled", "mods/b.jar"]));
    let a: Vec<&DiskFile> = files
      .iter()
      .filter(|file| file.path == "mods/a.jar")
      .collect();
    assert_eq!(a.len(), 1);
    assert!(a[0].conflict);
    assert!(
      !files
        .iter()
        .find(|file| file.path == "mods/b.jar")
        .unwrap()
        .conflict
    );
  }

  #[test]
  fn both_variants_on_disk_the_conflict_is_reported_for_the_ticked_file_only() {
    let mut m = model(
      &["mods/a.jar", "mods/a.jar.disabled", "mods/b.jar"],
      None,
      &[],
    );
    assert_eq!(payload_conflicts(&m), ["mods/a.jar"]);
    m.rules = vec![rule("mods/a.jar", RuleKind::File, RuleState::Ignore)];
    assert!(payload_conflicts(&m).is_empty());
  }

  #[test]
  fn the_conflict_is_detected_whatever_the_listing_order_or_case_of_the_variants() {
    let a = normalize_disk_files(&disk(&["mods/a.jar.disabled", "mods/a.jar"]));
    assert_eq!(a.len(), 1);
    assert!(a[0].conflict);
    let b = normalize_disk_files(&disk(&["Mods/A.jar.disabled", "mods/a.JAR"]));
    assert_eq!(b.len(), 1);
    assert!(b[0].conflict);
  }

  #[test]
  fn a_disabled_file_outside_mods_is_never_published_as_disabled() {
    let all = [
      "config/a.toml.disabled",
      "modsx/a.jar.disabled",
      "resourcepacks/p.zip.disabled",
      "x.disabled",
      "mods/ok.jar",
    ];
    let files = normalize_disk_files(&disk(&all));
    assert!(files
      .iter()
      .all(|file| !(file.disabled && !file.path.to_lowercase().starts_with("mods/"))));
    let m = model(&all, None, &[]);
    let published = payload_files(&m);
    assert!(published
      .iter()
      .all(|file| !file.0.to_lowercase().ends_with(".disabled")));
    assert!(published.iter().all(|file| !file.1));
    assert_eq!(paths(&m), ["mods/ok.jar"]);
  }

  #[test]
  fn previous_release_once_and_ticks_are_matched_case_insensitively() {
    let m = model(
      &["config/a.toml"],
      Some(previous(vec![file("Config/A.toml", true)], vec![])),
      &[],
    );
    assert!(m.ghosts.is_empty());
    assert!(is_once(&m, "config/a.toml"));
  }

  #[test]
  fn file_state_lookups_ignore_the_case_of_the_queried_path() {
    let mut m = model(&["Config/A.toml"], None, &[]);
    m.rules = vec![rule("config/a.toml", RuleKind::File, RuleState::Ignore)];
    assert_eq!(file_state(&m, "CONFIG/A.TOML"), "ignore");
  }

  #[test]
  fn the_payload_never_holds_two_files_whose_paths_differ_only_by_case() {
    let m = model(&["a/X.txt", "a/x.txt", "b/y.txt"], None, &[]);
    let keys: Vec<String> = payload_files(&m).iter().map(|file| key(&file.0)).collect();
    let unique: HashSet<&String> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len());
    assert!(keys.contains(&"b/y.txt".to_string()));
  }

  #[test]
  fn a_dir_rule_that_matches_an_existing_directory_by_case_is_not_a_virtual_node() {
    let m = model(
      &["Config/Cache/a.bin"],
      Some(previous(
        vec![],
        vec![rule("config/cache", RuleKind::Dir, RuleState::Ignore)],
      )),
      &[],
    );
    assert!(m.ghosts.is_empty());
    assert_eq!(file_state(&m, "Config/Cache/a.bin"), "ignore");
  }

  #[test]
  fn publishes_follows_the_rules_and_ticks_of_the_starting_state() {
    let m = model(
      &[
        "saves/keep.dat",
        "saves/other.dat",
        "config/a.toml",
        "config/b.toml",
      ],
      Some(previous(
        vec![file("saves/keep.dat", false)],
        vec![
          rule("saves", RuleKind::Dir, RuleState::Ignore),
          rule("config/b.toml", RuleKind::File, RuleState::Delete),
        ],
      )),
      &[],
    );
    assert!(m.publishes("saves/keep.dat"));
    assert!(!m.publishes("saves/other.dat"));
    assert!(m.publishes("config/a.toml"));
    assert!(!m.publishes("config/b.toml"));
    assert!(m.publishes("brand/new.txt"));
  }
}
