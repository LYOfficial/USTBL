//! The version of a modpack an instance is being updated to, independent of
//! which platform it came from.

use super::decide::{Policy, ShipInput};
use super::diff::FileKey;
use super::packs::PackSession;
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::baseline::{
  hash_content_lines, BaselineLoader, ComponentRef, ContentHashFile,
};
use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::models::misc::InstanceError;
use crate::tasks::download::DownloadParam;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

pub fn safe_relative_path(path: &str) -> USTBLResult<PathBuf> {
  let path = PathBuf::from(path);
  if path.is_absolute() || path.components().any(|component| matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_))) {
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  Ok(path)
}

/// Directory inside an instance that holds the staging area and the journal of
/// an update in progress. Nothing in it is ever managed by a modpack.
pub const UPDATE_DIR: &str = ".ustbl-update";

#[derive(Debug, Clone)]
pub enum FileSource {
  /// Fetched over the network; `dest` is replaced by the staging path.
  Download(Box<DownloadParam>),
  /// Taken out of an archive that was already downloaded.
  ArchiveEntry { archive: PathBuf, entry: String },
  /// Stored in the campus platform's packs, found by the file's sha1. The
  /// session is told which files are wanted before any is fetched.
  Pack(Arc<PackSession>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum StoredFileSource {
  Download { param: Box<DownloadParam> },
  ArchiveEntry { archive: PathBuf, entry: String },
  Pack { version_id: i64 },
}

impl From<&FileSource> for StoredFileSource {
  fn from(source: &FileSource) -> Self {
    match source {
      FileSource::Download(param) => StoredFileSource::Download {
        param: param.clone(),
      },
      FileSource::ArchiveEntry { archive, entry } => StoredFileSource::ArchiveEntry {
        archive: archive.clone(),
        entry: entry.clone(),
      },
      FileSource::Pack(session) => StoredFileSource::Pack {
        version_id: session.version_id(),
      },
    }
  }
}

#[derive(Debug, Clone)]
pub struct RemoteFile {
  /// Relative to the instance directory, always with `/`.
  pub path: String,
  pub key: FileKey,
  pub source: FileSource,
  /// The publisher disabled the mod; the file ships under its enabled name.
  pub disabled: bool,
  /// Only the first install writes the file; later versions never touch it.
  pub once: bool,
}

/// Whether a publish rule covers one file or a whole directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleKind {
  Dir,
  File,
}

impl RuleKind {
  pub fn as_str(&self) -> &'static str {
    match self {
      RuleKind::Dir => "dir",
      RuleKind::File => "file",
    }
  }
}

/// What a publish rule asks the instances to do with the covered files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleState {
  Ignore,
  Delete,
}

impl RuleState {
  pub fn as_str(&self) -> &'static str {
    match self {
      RuleState::Ignore => "ignore",
      RuleState::Delete => "delete",
    }
  }
}

/// One ignore/delete rule of a `ustbl.modpack/3` manifest. `path` is relative
/// to the instance directory, always with `/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRule {
  pub path: String,
  pub kind: RuleKind,
  pub state: RuleState,
}

/// The most rules one manifest may carry.
pub const MAX_MANIFEST_RULES: usize = 2000;

/// What it takes to rebuild a component profile on an instance that has none:
/// the declared chain and the files the pack ships itself.
#[derive(Debug, Clone)]
pub struct ComponentPack {
  /// `mmc-pack.json` as published.
  pub mmc_pack: serde_json::Value,
  pub instance_cfg: BTreeMap<String, String>,
  /// Named relative to the pack: `patches/<uid>.json`, `libraries/<file>`.
  pub files: Vec<RemoteFile>,
}

/// Case-folded lookups over the file and rule lists. Built once per remote
/// and shared by its clones, so deciding a pack's worth of paths is linear in
/// the lists, not quadratic. `dir` rules are matched by prefix and stay a
/// linear scan.
#[derive(Debug, Default)]
pub struct PathIndex {
  /// Case-folded path → index into `files`.
  files: HashMap<String, usize>,
  /// Case-folded path → index into `rules`, `file` rules only.
  file_rules: HashMap<String, usize>,
}

impl PathIndex {
  fn build(files: &[RemoteFile], rules: &[RemoteRule]) -> Self {
    let files = files
      .iter()
      .enumerate()
      .map(|(index, file)| (file.path.to_lowercase(), index))
      .collect();
    let file_rules = rules
      .iter()
      .enumerate()
      .filter(|(_, rule)| rule.kind == RuleKind::File)
      .map(|(index, rule)| (rule.path.to_lowercase(), index))
      .collect();
    Self { files, file_rules }
  }
}

#[derive(Debug, Clone)]
pub struct RemoteModpack {
  pub version_id: String,
  /// Human readable version name for the interface.
  pub label: String,
  pub game_version: String,
  pub loader: BaselineLoader,
  /// Component chain of a component-profile pack in canonical order, `None`
  /// when the pack has none.
  pub components: Option<Vec<ComponentRef>>,
  /// Present exactly when `components` is.
  pub component_pack: Option<ComponentPack>,
  pub files: Vec<RemoteFile>,
  /// Publish rules of a `ustbl.modpack/3` manifest; empty for the other
  /// platforms, where every unlisted path is unmentioned.
  pub rules: Vec<RemoteRule>,
  /// The manifest declared the `/3` format, so rules and the file attributes
  /// (`disabled`, `once`) are authoritative. Archives never have them.
  pub rules_manifest: bool,
  /// Case-folded lookup over `files` and the `file` rules, built on first
  /// use and shared by every clone. Filled only by [`RemoteModpack::index`].
  pub(crate) path_index: Arc<OnceLock<PathIndex>>,
}

// ---- stored form (the remote cache) -------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRemoteFile {
  pub path: String,
  pub key: FileKey,
  pub source: StoredFileSource,
  #[serde(default)]
  pub disabled: bool,
  #[serde(default)]
  pub once: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredComponentPack {
  pub mmc_pack: serde_json::Value,
  #[serde(default)]
  pub instance_cfg: BTreeMap<String, String>,
  #[serde(default)]
  pub files: Vec<StoredRemoteFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRemoteModpack {
  pub version_id: String,
  pub label: String,
  pub game_version: String,
  pub loader: BaselineLoader,
  #[serde(default)]
  pub components: Option<Vec<ComponentRef>>,
  #[serde(default)]
  pub component_pack: Option<StoredComponentPack>,
  pub files: Vec<StoredRemoteFile>,
  #[serde(default)]
  pub rules: Vec<RemoteRule>,
  #[serde(default)]
  pub rules_manifest: bool,
}

impl From<&RemoteModpack> for StoredRemoteModpack {
  fn from(remote: &RemoteModpack) -> Self {
    let file = |file: &RemoteFile| StoredRemoteFile {
      path: file.path.clone(),
      key: file.key.clone(),
      source: StoredFileSource::from(&file.source),
      disabled: file.disabled,
      once: file.once,
    };
    Self {
      version_id: remote.version_id.clone(),
      label: remote.label.clone(),
      game_version: remote.game_version.clone(),
      loader: remote.loader.clone(),
      components: remote.components.clone(),
      component_pack: remote
        .component_pack
        .as_ref()
        .map(|pack| StoredComponentPack {
          mmc_pack: pack.mmc_pack.clone(),
          instance_cfg: pack.instance_cfg.clone(),
          files: pack.files.iter().map(file).collect(),
        }),
      files: remote.files.iter().map(file).collect(),
      rules: remote.rules.clone(),
      rules_manifest: remote.rules_manifest,
    }
  }
}

impl RemoteModpack {
  /// Rebuild a remote from its stored form. Pack sources get a fresh session
  /// from `make_session`, one per version shared by all of its files (the
  /// component pack's included), so each version is located once.
  pub fn from_stored(
    stored: StoredRemoteModpack,
    make_session: impl Fn(i64) -> Arc<PackSession>,
  ) -> Self {
    let mut sessions: BTreeMap<i64, Arc<PackSession>> = BTreeMap::new();
    let mut file = |file: StoredRemoteFile| RemoteFile {
      path: file.path,
      key: file.key,
      source: match file.source {
        StoredFileSource::Download { param } => FileSource::Download(param),
        StoredFileSource::ArchiveEntry { archive, entry } => {
          FileSource::ArchiveEntry { archive, entry }
        }
        StoredFileSource::Pack { version_id } => FileSource::Pack(
          sessions
            .entry(version_id)
            .or_insert_with(|| make_session(version_id))
            .clone(),
        ),
      },
      disabled: file.disabled,
      once: file.once,
    };
    let component_pack = stored.component_pack.map(|pack| ComponentPack {
      mmc_pack: pack.mmc_pack,
      instance_cfg: pack.instance_cfg,
      files: pack.files.into_iter().map(&mut file).collect(),
    });
    let files = stored.files.into_iter().map(&mut file).collect();
    Self {
      version_id: stored.version_id,
      label: stored.label,
      game_version: stored.game_version,
      loader: stored.loader,
      components: stored.components,
      component_pack,
      files,
      rules: stored.rules,
      rules_manifest: stored.rules_manifest,
      path_index: Arc::new(OnceLock::new()),
    }
  }
}

impl RemoteModpack {
  /// Reject anything that could write outside the instance or over launcher
  /// owned files, and paths that collide on case-insensitive file systems.
  /// A `/3` manifest's rules get the same checks plus their own: no rule may
  /// cover the whole instance, hit a shipped file, or name another rule.
  pub fn validate(&self, instance_name: &str) -> USTBLResult<()> {
    if let Some(pack) = &self.component_pack {
      for file in &pack.files {
        safe_relative(&file.path)?;
        if !file.path.starts_with("patches/") && !file.path.starts_with("libraries/") {
          return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
        }
      }
    }
    let mut seen = BTreeSet::new();
    for file in &self.files {
      safe_relative(&file.path)?;
      let lowered = file.path.to_lowercase();
      if lowered == UPDATE_DIR
        || lowered.starts_with(&format!("{UPDATE_DIR}/"))
        || is_launcher_metadata_path(&file.path, instance_name)
        || lowered.ends_with(super::diff::DISABLED_SUFFIX)
        || !seen.insert(lowered.clone())
        || (file.disabled && !lowered.starts_with("mods/"))
        || (!self.rules_manifest && (file.disabled || file.once))
      {
        return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
      }
    }
    // Only a /3 manifest may carry rules at all.
    if (!self.rules_manifest && !self.rules.is_empty()) || self.rules.len() > MAX_MANIFEST_RULES {
      return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
    }
    let files: BTreeSet<&str> = self.files.iter().map(|file| file.path.as_str()).collect();
    let mut rules_seen = BTreeSet::new();
    for rule in &self.rules {
      // Same path-shape checks as the shipped files, before any rule's own:
      // no separators beyond `/`, no empty segments, nothing absolute or
      // escaping. The trailing-slash case below doubles as the "whole
      // instance" refusal: an empty path never survives this.
      let path = safe_relative_path(&rule.path)?;
      if path
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || rule.path.split('/').any(|segment| segment.is_empty())
      {
        return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
      }
      let lowered = rule.path.to_lowercase();
      if rule.path.ends_with('/')
        || lowered == UPDATE_DIR
        || lowered.starts_with(&format!("{UPDATE_DIR}/"))
        || lowered.ends_with(super::diff::DISABLED_SUFFIX)
        || lowered == "patches"
        || lowered.starts_with("patches/")
        || lowered == "libraries"
        || lowered.starts_with("libraries/")
        || is_launcher_metadata_path(&rule.path, instance_name)
        || !rules_seen.insert(lowered.clone())
        || (rule.kind == RuleKind::File && files.iter().any(|path| path.to_lowercase() == lowered))
      {
        return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
      }
    }
    Ok(())
  }

  /// The case-folded lookup over the file list and the `file` rules, built
  /// the first time anyone asks. The index matches the lists it was built
  /// from: a remote's `files`/`rules` are final once a plan runs against
  /// them, and test fixtures must ask before mutating.
  pub fn index(&self) -> &PathIndex {
    self
      .path_index
      .get_or_init(|| PathIndex::build(&self.files, &self.rules))
  }

  /// The shipped file whose path folds to `path`, in the manifest's own
  /// spelling. Path comparisons fold case everywhere (《方案》§3).
  pub fn file_of(&self, path: &str) -> Option<&RemoteFile> {
    self.index().files.get(&path.to_lowercase()).map(|i| &self.files[*i])
  }

  /// The remote's side of a shipped path, for the decision function.
  pub fn ship_of(&self, path: &str) -> Option<ShipInput> {
    self.file_of(path).map(|file| ShipInput {
      key: file.key.clone(),
      // Only a `/3` manifest's attributes are authoritative; other remotes
      // never disable and never ship once-only files.
      disabled: self.rules_manifest.then_some(file.disabled),
      once: self.rules_manifest && file.once,
    })
  }

  /// The policy of one path: a shipped file wins, then a `file` rule, then
  /// the deepest `dir` rule; everything else is unmentioned. All matching
  /// folds case, like the file systems the packs land on (《方案》§3).
  pub fn policy_of(&self, path: &str) -> Policy {
    let index = self.index();
    let folded = path.to_lowercase();
    if index.files.contains_key(&folded) {
      return Policy::Ship;
    }
    if let Some(rule) = index.file_rules.get(&folded).map(|i| &self.rules[*i]) {
      return match rule.state {
        RuleState::Ignore => Policy::Ignore,
        RuleState::Delete => Policy::Delete,
      };
    }
    let mut deepest: Option<(usize, &RemoteRule)> = None;
    for rule in &self.rules {
      if rule.kind != RuleKind::Dir {
        continue;
      }
      let rule_folded = rule.path.to_lowercase();
      let covered = folded
        .strip_prefix(&rule_folded)
        .is_some_and(|rest| rest.starts_with('/'));
      if covered && deepest.is_none_or(|(depth, _)| rule_folded.len() > depth) {
        deepest = Some((rule_folded.len(), rule));
      }
    }
    match deepest {
      Some((_, rule)) => match rule.state {
        RuleState::Ignore => Policy::Ignore,
        RuleState::Delete => Policy::Delete,
      },
      None => Policy::Unmentioned,
    }
  }

  pub fn content_hash(&self) -> String {
    let components = self
      .components
      .as_ref()
      .map(|chain| serde_json::to_value(chain).expect("component refs serialize to JSON"));
    let files: Vec<ContentHashFile> = self
      .files
      .iter()
      .map(|file| ContentHashFile {
        path: file.path.clone(),
        sha1: file.key.sha1.clone(),
        size: file.key.size,
        disabled: file.disabled,
        once: file.once,
      })
      .collect();
    hash_content_lines(&self.loader, components.as_ref(), &files, &self.rules)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::models::misc::ModLoaderType;

  fn pack(paths: &[&str]) -> RemoteModpack {
    RemoteModpack {
      version_id: "2".into(),
      label: "2.0".into(),
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16.0".into(),
      },
      components: None,
      component_pack: None,
      files: paths
        .iter()
        .map(|path| RemoteFile {
          path: path.to_string(),
          key: FileKey {
            sha1: "aa".into(),
            size: 1,
          },
          source: FileSource::Download(Box::new(DownloadParam {
            src: "https://example.com/a".parse().unwrap(),
            dest: PathBuf::new(),
            filename: None,
            sha1: None,
            custom_headers: None,
            transfer_options: Default::default(),
          })),
          disabled: false,
          once: false,
        })
        .collect(),
      rules: Vec::new(),
      rules_manifest: false,
      path_index: Arc::new(OnceLock::new()),
    }
  }

  #[test]
  fn ordinary_paths_are_accepted() {
    assert!(pack(&["mods/a.jar", "config/x/y.toml", "options.txt"])
      .validate("pack")
      .is_ok());
  }

  #[test]
  fn escaping_launcher_owned_and_colliding_paths_are_rejected() {
    for bad in [
      "../evil.jar",
      "/abs.jar",
      "mods\\a.jar",
      "C:/a.jar",
      "ustblcfg.json",
      "USTBL-modpack-baseline.json",
      "pack.json",
      "pack.jar",
      ".ustbl-update/journal.json",
      ".USTBL-UPDATE/x",
      "mods/a.jar.disabled",
    ] {
      assert!(pack(&[bad]).validate("pack").is_err(), "{bad} was accepted");
    }
    assert!(pack(&["mods/A.jar", "mods/a.jar"])
      .validate("pack")
      .is_err());
  }

  #[test]
  fn rules_get_the_file_path_checks_plus_their_own() {
    let rule = |path: &str, kind: RuleKind, state: RuleState| RemoteRule {
      path: path.into(),
      kind,
      state,
    };
    let mut pack = pack(&["mods/a.jar", "config/x.toml"]);
    pack.rules_manifest = true;
    pack.rules = vec![
      rule("config/cache", RuleKind::Dir, RuleState::Ignore),
      rule("mods/old.jar", RuleKind::File, RuleState::Delete),
      rule("resourcepacks", RuleKind::Dir, RuleState::Delete),
    ];
    assert!(pack.validate("pack").is_ok());

    let good = pack.rules.clone();
    for bad in [
      rule("../evil", RuleKind::Dir, RuleState::Ignore),
      rule("", RuleKind::Dir, RuleState::Ignore),
      rule("config/", RuleKind::Dir, RuleState::Ignore),
      rule("ustblcfg.json", RuleKind::File, RuleState::Delete),
      rule("pack.json", RuleKind::File, RuleState::Delete),
      rule(".ustbl-update", RuleKind::Dir, RuleState::Delete),
      rule(".USTBL-UPDATE/x", RuleKind::Dir, RuleState::Delete),
      rule(
        "patches/net.minecraftforge.json",
        RuleKind::File,
        RuleState::Delete,
      ),
      rule("libraries", RuleKind::Dir, RuleState::Delete),
      // A rule path never names a variant; the suffix belongs to the engine.
      rule("config/a.toml.disabled", RuleKind::File, RuleState::Delete),
      rule("config/cache.DISABLED", RuleKind::Dir, RuleState::Ignore),
      // A file rule may not cover a shipped file.
      rule("mods/A.jar", RuleKind::File, RuleState::Delete),
    ] {
      let mut candidate = pack.clone();
      candidate.rules = vec![bad.clone()];
      assert!(candidate.validate("pack").is_err(), "{bad:?} was accepted");
    }
    // Duplicate rules fold case; a dir rule may share a file's path.
    let mut duplicate = pack.clone();
    duplicate.rules = vec![
      rule("config/cache", RuleKind::Dir, RuleState::Ignore),
      rule("CONFIG/CACHE", RuleKind::Dir, RuleState::Delete),
    ];
    assert!(duplicate.validate("pack").is_err());
    let mut shared = pack.clone();
    shared.rules = vec![rule("mods/a.jar", RuleKind::Dir, RuleState::Ignore)];
    assert!(shared.validate("pack").is_ok());
    // Too many rules.
    let mut over = pack.clone();
    over.rules = (0..=MAX_MANIFEST_RULES)
      .map(|index| {
        rule(
          &format!("config/r{index}"),
          RuleKind::File,
          RuleState::Ignore,
        )
      })
      .collect();
    assert!(over.validate("pack").is_err());
    // Without the /3 flag the manifest carries neither rules nor attributes.
    let mut plain = pack.clone();
    plain.rules_manifest = false;
    plain.files[0].disabled = true;
    assert!(plain.validate("pack").is_err());
    plain.files[0].disabled = false;
    plain.files[0].once = true;
    assert!(plain.validate("pack").is_err());
    plain.files[0].once = false;
    plain.rules = good;
    assert!(plain.validate("pack").is_err());
    // `disabled` is only for mods.
    let mut misplaced = pack.clone();
    misplaced.files[1].disabled = true;
    assert!(misplaced.validate("pack").is_err());
  }

  #[test]
  fn the_policy_prefers_files_then_file_rules_then_the_deepest_dir_rule() {
    let rule = |path: &str, kind: RuleKind, state: RuleState| RemoteRule {
      path: path.into(),
      kind,
      state,
    };
    let mut pack = pack(&["mods/a.jar"]);
    pack.rules = vec![
      rule("config", RuleKind::Dir, RuleState::Ignore),
      rule("config/deep", RuleKind::Dir, RuleState::Delete),
      rule("config/deep/x.toml", RuleKind::File, RuleState::Ignore),
    ];
    assert_eq!(pack.policy_of("mods/a.jar"), Policy::Ship);
    // Files win over everything, and every lookup folds case (§3).
    assert_eq!(pack.policy_of("MODS/A.JAR"), Policy::Ship);
    assert_eq!(pack.policy_of("config/other.toml"), Policy::Ignore);
    assert_eq!(pack.policy_of("Config/Deep/y.toml"), Policy::Delete);
    assert_eq!(pack.policy_of("config/deep/X.toml"), Policy::Ignore);
    assert_eq!(pack.policy_of("config/deep/x.toml"), Policy::Ignore);
    // A dir rule does not cover its namesake file.
    assert_eq!(pack.policy_of("config"), Policy::Unmentioned);
    assert_eq!(pack.policy_of("mods/b.jar"), Policy::Unmentioned);
  }

  #[test]
  fn ship_and_file_lookups_fold_case_but_keep_the_manifest_spelling() {
    let mut pack = pack(&["Mods/A.JAR"]);
    pack.rules_manifest = true;
    pack.files[0].disabled = true;
    let file = pack.file_of("mods/a.jar").unwrap();
    assert_eq!(file.path, "Mods/A.JAR");
    let ship = pack.ship_of("mods/a.jar").unwrap();
    assert_eq!(ship.disabled, Some(true));
    assert_eq!(ship.key, pack.files[0].key);
    // Not a shipped file: nothing.
    assert!(pack.file_of("mods/b.jar").is_none());
    assert!(pack.ship_of("mods/b.jar").is_none());
  }

  /// Sessions here are only compared, never asked for anything.
  struct NoTransport;

  #[async_trait::async_trait]
  impl super::super::packs::PackTransport for NoTransport {
    async fn locate(
      &self,
      _: i64,
      _: &[String],
    ) -> Result<super::super::packs::Located, super::super::packs::TransportError> {
      unreachable!()
    }
    async fn read_range(
      &self,
      _: &super::super::packs::PackLocation,
      _: u64,
      _: u64,
      _: &std::path::Path,
    ) -> Result<(), super::super::packs::TransportError> {
      unreachable!()
    }
  }

  fn stored_pack_file(path: &str, version_id: i64) -> StoredRemoteFile {
    StoredRemoteFile {
      path: path.into(),
      key: FileKey {
        sha1: format!("{version_id:0>40}"),
        size: 1,
      },
      source: StoredFileSource::Pack { version_id },
      disabled: false,
      once: false,
    }
  }

  #[test]
  fn stored_pack_files_of_one_version_share_one_session() {
    let mut stored = StoredRemoteModpack::from(&pack(&["mods/url.jar"]));
    stored.files.extend([
      stored_pack_file("mods/a.jar", 3),
      stored_pack_file("mods/b.jar", 4),
      stored_pack_file("mods/c.jar", 3),
    ]);
    stored.component_pack = Some(StoredComponentPack {
      mmc_pack: serde_json::json!({ "components": [] }),
      instance_cfg: BTreeMap::new(),
      files: vec![
        stored_pack_file("patches/x.json", 3),
        stored_pack_file("libraries/y.jar", 4),
      ],
    });
    let made = std::cell::RefCell::new(Vec::new());

    let remote = RemoteModpack::from_stored(stored, |version_id| {
      made.borrow_mut().push(version_id);
      PackSession::new(Arc::new(NoTransport), version_id)
    });

    assert_eq!(made.into_inner(), [3, 4]);
    let session = |file: &RemoteFile| match &file.source {
      FileSource::Pack(session) => session.clone(),
      other => panic!("{other:?}"),
    };
    let component = &remote.component_pack.as_ref().unwrap().files;
    assert!(matches!(remote.files[0].source, FileSource::Download(_)));
    let three = session(&remote.files[1]);
    let four = session(&remote.files[2]);
    assert_eq!((three.version_id(), four.version_id()), (3, 4));
    assert!(!Arc::ptr_eq(&three, &four));
    assert!(Arc::ptr_eq(&three, &session(&remote.files[3])));
    assert!(Arc::ptr_eq(&three, &session(&component[0])));
    assert!(Arc::ptr_eq(&four, &session(&component[1])));
  }

  #[test]
  fn content_hash_matches_the_baseline_definition() {
    let remote = pack(&["b", "a"]);
    let mut reversed = remote.clone();
    reversed.files.reverse();
    assert_eq!(remote.content_hash(), reversed.content_hash());
    assert_ne!(remote.content_hash(), pack(&["a"]).content_hash());
  }
}
