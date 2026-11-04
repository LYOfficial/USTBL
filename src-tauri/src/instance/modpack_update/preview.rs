//! The preview of a platform modpack update, made without downloading the
//! archive.
//!
//! A downloaded archive is described by [`super::sources::remote_from_archive`]:
//! every override entry is extracted and hashed. The preview reads the central
//! directory instead (see [`super::range_zip`]), which has each entry's size
//! and CRC32 but no SHA-1, and keys the override files like this:
//!
//! * a local file (either variant) or the cached baseline copy of the path with
//!   the same size and CRC32 is that content: the entry takes the candidate's
//!   SHA-1;
//! * when the baseline has the path at the same size but nothing local can be
//!   checked against it, the entry is read and hashed for real;
//! * everything else differs from every candidate, so a key derived from path,
//!   CRC32 and size stands in. It is never equal to a real key.
//!
//! Files the merge needs the content of are fetched after the first comparison,
//! given their real key, and compared once more. The result is only a preview:
//! its files are [`FileSource::Listed`] and can never be applied. Confirming a
//! preview still downloads the whole archive, and [`accepts`] decides whether
//! the plan computed from that archive is the one the player looked at.

use super::commands::{compute_loader_change, LoaderChange};
use super::diff::{ConflictKind, FileKey, DISABLED_SUFFIX};
use super::plan::{compare, finish, requires_reinstall, Prepared};
use super::range_zip::{read_listing, LocalSource, RangeSource, ZipEntry, ZipListing};
use super::remote::{FileSource, RemoteFile, RemoteModpack};
use super::sources::{assemble_remote, ArchiveFacts, LatestVersion};
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use crate::instance::helpers::modpack::misc::is_launcher_metadata_path;
use crate::instance::helpers::modpack::multimc::profile::safe_relative;
use crate::instance::helpers::modpack::{base_store, text};
use crate::instance::models::misc::InstanceError;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The largest text file read through ranges to be merged.
const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;

/// How many times fetched contents may change the comparison before the
/// preview gives up and leaves the exact path to decide.
const MAX_ROUNDS: usize = 8;

/// The files of the archive's override directories, by instance path. The
/// listing's counterpart of `list_override_entries`: later layers replace
/// earlier ones, launcher metadata is skipped, and symlinks are an error.
pub fn override_entries(
  listing: &ZipListing,
  overrides_paths: &[String],
  instance_name: &str,
) -> USTBLResult<BTreeMap<String, ZipEntry>> {
  let mut found = BTreeMap::new();
  for overrides_path in overrides_paths {
    let prefix = format!("{}/", overrides_path.trim_end_matches('/'));
    for entry in &listing.entries {
      let relative = match entry.name.strip_prefix(&prefix) {
        Some(relative) if !relative.is_empty() && !entry.is_dir() => relative.to_string(),
        _ => continue,
      };
      safe_relative(&relative)?;
      if entry.is_symlink() {
        return Err(USTBLError("Modpack symlinks are not supported".into()));
      }
      if is_launcher_metadata_path(&relative, instance_name) {
        continue;
      }
      found.insert(relative, entry.clone());
    }
  }
  Ok(found)
}

/// The key standing in for an entry whose content is known to differ from
/// every local candidate. Forty hex digits like a real SHA-1, derived from
/// what the central directory says, so the same entry gets the same key
/// whenever it is read.
pub fn synthetic_key(path: &str, crc32: u32, size: u64) -> FileKey {
  let mut hasher = Sha1::new();
  hasher.update(b"ustbl-preview\0");
  hasher.update(path.as_bytes());
  hasher.update(b"\0");
  hasher.update(crc32.to_le_bytes());
  hasher.update(size.to_le_bytes());
  FileKey {
    sha1: hex::encode(hasher.finalize()),
    size,
  }
}

/// SHA-1 and CRC32 of a local file of exactly `size` bytes; `None` for any
/// other file or when it cannot be read.
fn probe(path: &Path, size: u64) -> Option<(String, u32)> {
  let meta = std::fs::metadata(path).ok()?;
  if !meta.is_file() || meta.len() != size {
    return None;
  }
  let mut file = File::open(path).ok()?;
  let (mut sha1, mut crc) = (Sha1::new(), crc32fast::Hasher::new());
  let mut buffer = vec![0u8; 64 * 1024];
  loop {
    let read = file.read(&mut buffer).ok()?;
    if read == 0 {
      break;
    }
    sha1.update(&buffer[..read]);
    crc.update(&buffer[..read]);
  }
  Some((hex::encode(sha1.finalize()), crc.finalize()))
}

/// What the mapping decided for one override entry.
#[derive(Debug, PartialEq, Eq)]
pub enum Mapped {
  /// The entry is this exact content.
  Same(FileKey),
  /// It may be the baseline's content and nothing local can say so: the
  /// entry has to be hashed for real.
  Unknown,
  /// It differs from every candidate.
  Different(FileKey),
}

/// Map one override entry to a key. `path` is the entry's instance path as
/// the archive spells it; the `.disabled` suffix of a disabled mod belongs to
/// the variant, not to the path the baseline knows.
fn map_entry(
  root: &Path,
  baseline: &BTreeMap<String, FileKey>,
  path: &str,
  entry: &ZipEntry,
) -> Mapped {
  let enabled = if path.to_lowercase().ends_with(DISABLED_SUFFIX) {
    &path[..path.len() - DISABLED_SUFFIX.len()]
  } else {
    path
  };
  let candidates = [
    root.join(enabled),
    root.join(format!("{enabled}{DISABLED_SUFFIX}")),
  ];
  let probes: Vec<(String, u32)> = candidates
    .iter()
    .filter_map(|candidate| probe(candidate, entry.size))
    .collect();
  if let Some((sha1, _)) = probes.iter().find(|(_, crc)| *crc == entry.crc32) {
    return Mapped::Same(FileKey {
      sha1: sha1.clone(),
      size: entry.size,
    });
  }
  if let Some(base) = baseline.get(enabled).filter(|base| base.size == entry.size) {
    let copy = base_store::load(root, &base.sha1)
      .ok()
      .flatten()
      .filter(|bytes| bytes.len() as u64 == base.size);
    match copy {
      Some(bytes) if crc32fast::hash(&bytes) == entry.crc32 => {
        return Mapped::Same(base.clone());
      }
      Some(_) => {}
      None => {
        // A local file that is the baseline would have been probed above and
        // its CRC32 told the entry apart from it.
        if !probes.iter().any(|(sha1, _)| *sha1 == base.sha1) {
          return Mapped::Unknown;
        }
      }
    }
  }
  Mapped::Different(synthetic_key(path, entry.crc32, entry.size))
}

/// Describe the version a listing stands for, the preview counterpart of
/// `remote_from_archive`. Fetches content only for entries the mapping cannot
/// settle from local state.
pub async fn preview_remote(
  facts: ArchiveFacts,
  latest: &LatestVersion,
  listing: &ZipListing,
  source: &dyn RangeSource,
  root: &Path,
  instance_name: &str,
  baseline: &ModpackBaseline,
) -> USTBLResult<RemoteModpack> {
  let entries = override_entries(listing, &facts.overrides_paths, instance_name)?;
  let baseline_keys: BTreeMap<String, FileKey> = baseline
    .files
    .iter()
    .filter_map(|file| file.key().map(|key| (file.path.clone(), key)))
    .collect();
  let root_owned = root.to_path_buf();
  let mapped = tokio::task::spawn_blocking(move || {
    entries
      .into_iter()
      .map(|(path, entry)| {
        let mapped = map_entry(&root_owned, &baseline_keys, &path, &entry);
        (path, entry, mapped)
      })
      .collect::<Vec<_>>()
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))?;

  let mut overrides = Vec::with_capacity(mapped.len());
  let mut hashed = 0usize;
  for (path, entry, mapped) in mapped {
    let (key, exact) = match mapped {
      Mapped::Same(key) => (key, true),
      Mapped::Different(key) => (key, false),
      Mapped::Unknown => {
        hashed += 1;
        (listing.hash_entry(source, &entry).await?, true)
      }
    };
    overrides.push(RemoteFile {
      path,
      key,
      source: FileSource::Listed {
        entry: entry.name,
        exact,
      },
      disabled: false,
      once: false,
    });
  }
  log::info!(
    "modpack preview overrides: {} entries, {hashed} read for their hash",
    overrides.len()
  );
  let remote = assemble_remote(latest, facts, overrides);
  remote.validate(instance_name)?;
  Ok(remote)
}

/// A comparison against a preview remote.
pub struct PreviewRun {
  pub prepared: Prepared,
  pub remote: RemoteModpack,
  pub loader_change: Option<LoaderChange>,
}

/// Compare the instance with a preview remote the way the exact path compares
/// it with a full one. Conflicting text files need their content to merge: it
/// is read through `source`, the file gets its real key, and the comparison
/// runs again before the merge starts.
pub async fn run_preview(
  root: &Path,
  baseline: &ModpackBaseline,
  mut remote: RemoteModpack,
  listing: &ZipListing,
  source: &dyn RangeSource,
  fresh: bool,
) -> USTBLResult<PreviewRun> {
  if requires_reinstall(baseline, &remote, fresh) {
    // The exact path words this; the preview does not.
    return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
  }
  let loader_change = compute_loader_change(&baseline.loader, &remote.loader);
  let mut contents: BTreeMap<String, Vec<u8>> = BTreeMap::new();
  let mut fetched: BTreeSet<String> = BTreeSet::new();
  let mut rounds = 0;
  let compared = loop {
    let compared = compare(root, baseline, &remote).await?;
    if !compared.both_variants.is_empty() {
      return Err(InstanceError::ModpackUpdateBothVariants.into());
    }
    let wanted: Vec<String> = compared
      .diff
      .conflicts
      .iter()
      .filter(|conflict| {
        conflict.kind == ConflictKind::ModifiedLocallyChangedRemotely
          && text::is_text_path(&conflict.path, &[])
          && !fetched.contains(&conflict.path)
      })
      .map(|conflict| conflict.path.clone())
      .collect();
    if wanted.is_empty() {
      break compared;
    }
    rounds += 1;
    if rounds > MAX_ROUNDS {
      return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
    }
    for path in wanted {
      let file = remote
        .files
        .iter_mut()
        .find(|file| file.path == path)
        .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
      let FileSource::Listed { entry, exact } = &mut file.source else {
        return Err(InstanceError::ModpackUpdateRemoteInvalid.into());
      };
      let zip_entry = listing
        .find(entry)
        .ok_or(InstanceError::ModpackUpdateRemoteInvalid)?;
      let bytes = listing
        .read_entry_bytes(source, zip_entry, MAX_TEXT_BYTES)
        .await?;
      // The key follows the content that was read, never the other way round.
      file.key = FileKey {
        sha1: hex::encode(Sha1::digest(&bytes)),
        size: bytes.len() as u64,
      };
      *exact = true;
      if text::is_text_path(&path, &bytes) {
        contents.insert(path.clone(), bytes);
      }
      fetched.insert(path);
    }
    remote.path_index = Arc::new(OnceLock::new());
  };
  let prepared = finish(compared, root, &remote, &contents)?;
  Ok(PreviewRun {
    prepared,
    remote,
    loader_change,
  })
}

/// What a player sees of a plan, and what applying it does, without the keys
/// of the files to write (those differ between a preview and a full plan by
/// design). Compared line by line, so a difference can be named.
pub fn shape(prepared: &Prepared, loader_change: &Option<LoaderChange>) -> Vec<String> {
  let size = |key: &Option<FileKey>| key.as_ref().map(|key| key.size);
  let mut lines = Vec::new();
  for (label, changes) in [
    ("change", &prepared.diff.changes),
    ("merged", &prepared.diff.auto_merged),
  ] {
    for change in changes {
      lines.push(format!(
        "{label}|{}|{:?}|{}|{:?}|{:?}",
        change.path,
        change.kind,
        change.restored,
        size(&change.remote),
        change.local_size
      ));
    }
  }
  for conflict in &prepared.diff.conflicts {
    lines.push(format!(
      "conflict|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}",
      conflict.path,
      conflict.kind,
      conflict.baseline,
      conflict.local,
      size(&conflict.remote),
      conflict.remote_disabled,
      conflict.local_disabled,
      serde_json::to_string(&conflict.hunks).unwrap_or_default()
    ));
  }
  for path in &prepared.diff.already_current {
    lines.push(format!("current|{path}"));
  }
  for (path, disabled) in &prepared.variants {
    lines.push(format!("variant|{path}|{disabled}"));
  }
  for (path, dirs) in &prepared.prune_dirs {
    lines.push(format!("prune|{path}|{}", dirs.join(",")));
  }
  lines.push(format!("loader|{:?}", loader_change));
  if prepared.restore_components {
    lines.push("components|restore".to_string());
  }
  lines
}

/// Whether the plan computed from the downloaded archive is the one the player
/// confirmed. The confirmed hash came from a preview, whose keys differ from
/// the full plan's; so the preview is rebuilt from the archive now on disk
/// (through the same code, with the archive as the byte source). If that gives
/// the confirmed hash, the instance and the version are what the player looked
/// at; the full plan must then show them the same changes, conflicts and merges.
pub async fn accepts(
  facts: ArchiveFacts,
  latest: &LatestVersion,
  archive: &Path,
  root: &Path,
  instance_name: &str,
  baseline: &ModpackBaseline,
  fresh: bool,
  optional_choice: &[String],
  confirmed: &str,
  full: &Prepared,
  full_loader_change: &Option<LoaderChange>,
) -> bool {
  let attempt = async {
    let source = LocalSource::new(PathBuf::from(archive));
    let listing = read_listing(&source).await?;
    let mut remote = preview_remote(
      facts,
      latest,
      &listing,
      &source,
      root,
      instance_name,
      baseline,
    )
    .await?;
    super::optional::select(&mut remote, root, baseline, optional_choice);
    run_preview(root, baseline, remote, &listing, &source, fresh).await
  };
  let run = match attempt.await {
    Ok(run) => run,
    Err(error) => {
      log::info!("modpack update: preview cannot be rebuilt from the archive: {error:?}");
      return false;
    }
  };
  if run.prepared.plan_hash != confirmed {
    log::info!(
      "modpack update: rebuilt preview plan_hash={} differs from confirmed {confirmed}",
      run.prepared.plan_hash
    );
    return false;
  }
  let (previewed, applied) = (
    shape(&run.prepared, &run.loader_change),
    shape(full, full_loader_change),
  );
  if previewed != applied {
    let first = previewed
      .iter()
      .zip(&applied)
      .find(|(a, b)| a != b)
      .map(|(a, b)| format!("preview `{a}` vs full `{b}`"))
      .unwrap_or_else(|| format!("{} vs {} lines", previewed.len(), applied.len()));
    log::info!("modpack update: preview and full plan differ: {first}");
    return false;
  }
  true
}

#[cfg(test)]
mod tests {
  use super::super::range_zip::testing::{build_zip, BytesSource, TestEntry};
  use super::super::sources::{LatestVersion, Payload};
  use super::*;
  use crate::instance::helpers::modpack::baseline::{BaselineFile, BaselineLoader};
  use crate::instance::models::misc::{InstanceOrigin, ModLoaderType, OriginPlatform};
  use crate::tasks::download::DownloadParam;

  fn loader() -> BaselineLoader {
    BaselineLoader {
      loader_type: ModLoaderType::Fabric,
      version: "0.16.0".into(),
    }
  }

  fn origin() -> InstanceOrigin {
    InstanceOrigin {
      platform: OriginPlatform::Modrinth,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
      pack_format: None,
    }
  }

  fn latest() -> LatestVersion {
    LatestVersion {
      version_id: "2".into(),
      label: "2.0".into(),
      payload: Some(Payload::Archive {
        url: "https://cdn.example/pack.mrpack".into(),
        sha1: "ab".repeat(20),
      }),
    }
  }

  fn real_key(content: &[u8]) -> FileKey {
    FileKey {
      sha1: hex::encode(Sha1::digest(content)),
      size: content.len() as u64,
    }
  }

  /// The platform-side facts: one downloaded mod and the `overrides` layer.
  fn facts() -> ArchiveFacts {
    ArchiveFacts {
      game_version: "1.21.1".into(),
      loader: loader(),
      overrides_paths: vec!["overrides".into()],
      recommended_memory_mb: None,
      optional: Vec::new(),
      downloads: vec![RemoteFile {
        path: "mods/a.jar".into(),
        key: real_key(b"jar-a"),
        source: FileSource::Download(Box::new(DownloadParam {
          src: "https://cdn.example/a.jar".parse().unwrap(),
          dest: PathBuf::new(),
          filename: None,
          sha1: None,
          custom_headers: None,
          transfer_options: Default::default(),
        })),
        disabled: false,
        once: false,
      }],
    }
  }

  fn instance_dir() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-preview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
  }

  fn write(root: &Path, path: &str, content: &[u8]) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
  }

  fn baseline_of(files: &[(&str, &[u8], bool)]) -> ModpackBaseline {
    ModpackBaseline::build(
      &origin(),
      loader(),
      files
        .iter()
        .map(|(path, content, disabled)| BaselineFile::managed(path, real_key(content), *disabled))
        .collect(),
    )
  }

  /// The version's archive: `overrides/<path>` entries, stored.
  fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut entries = vec![TestEntry::new("modrinth.index.json", b"{}")];
    entries.extend(
      files
        .iter()
        .map(|(path, content)| TestEntry::new(&format!("overrides/{path}"), content)),
    );
    build_zip(&entries, b"", false)
  }

  /// The full path: every entry hashed for real, then compared and finished
  /// the way the apply does it.
  async fn full_plan(
    root: &Path,
    baseline: &ModpackBaseline,
    files: &[(&str, &[u8])],
    contents: &BTreeMap<String, Vec<u8>>,
  ) -> (Prepared, RemoteModpack) {
    let overrides = files
      .iter()
      .map(|(path, content)| RemoteFile {
        path: path.to_string(),
        key: real_key(content),
        source: FileSource::ArchiveEntry {
          archive: PathBuf::from("pack.mrpack"),
          entry: format!("overrides/{path}"),
        },
        disabled: false,
        once: false,
      })
      .collect();
    let remote = assemble_remote(&latest(), facts(), overrides);
    remote.validate("pack").unwrap();
    let compared = compare(root, baseline, &remote).await.unwrap();
    let prepared = finish(compared, root, &remote, contents).unwrap();
    (prepared, remote)
  }

  async fn preview(
    root: &Path,
    baseline: &ModpackBaseline,
    bytes: Vec<u8>,
  ) -> (PreviewRun, BytesSource, ZipListing) {
    let source = BytesSource::new(bytes);
    let listing = read_listing(&source).await.unwrap();
    let remote = preview_remote(
      facts(),
      &latest(),
      &listing,
      &source,
      root,
      "pack",
      baseline,
    )
    .await
    .unwrap();
    let run = run_preview(root, baseline, remote, &listing, &source, false)
      .await
      .unwrap();
    (run, source, listing)
  }

  fn listed(run: &PreviewRun, path: &str) -> (FileKey, bool) {
    let file = run
      .remote
      .files
      .iter()
      .find(|file| file.path == path)
      .unwrap();
    match &file.source {
      FileSource::Listed { exact, .. } => (file.key.clone(), *exact),
      other => panic!("{other:?}"),
    }
  }

  #[test]
  fn the_synthetic_key_is_a_stable_sha1_shaped_value_of_the_entry() {
    let key = synthetic_key("config/a.toml", 7, 12);
    assert_eq!(key.sha1.len(), 40);
    assert!(key.sha1.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(key, synthetic_key("config/a.toml", 7, 12));
    assert_ne!(key, synthetic_key("config/b.toml", 7, 12));
    assert_ne!(key, synthetic_key("config/a.toml", 8, 12));
    assert_ne!(key.sha1, real_key(b"x").sha1);
  }

  #[tokio::test]
  async fn the_mapping_covers_every_branch() {
    let root = instance_dir();
    let theirs = b"remote content";
    let padding = vec![7u8; 300 * 1024];
    let files: Vec<(&str, &[u8])> = vec![
      // Local file equals the entry.
      ("config/same.toml", theirs),
      // Local was edited, the baseline copy equals the entry.
      ("config/edited-with-copy.toml", theirs),
      // Local was edited, no copy, same size as the baseline: read for real.
      ("config/edited-no-copy.dat", theirs),
      // Different size than the baseline and the local file.
      ("config/new-size.toml", b"a much longer remote content"),
      // Only the disabled variant of a mod is there.
      ("mods/b.jar", b"jar-b!"),
      // Nothing local, nothing in the baseline.
      ("config/added.toml", b"added"),
      // Past the tail the first request reads, so that a read shows as a request.
      ("config/zz-padding.bin", &padding),
    ];
    write(&root, "config/same.toml", theirs);
    write(&root, "config/edited-with-copy.toml", b"local edit!!!!!");
    write(&root, "config/edited-no-copy.dat", b"local edit!!!!!");
    write(&root, "config/new-size.toml", b"old");
    write(&root, "mods/b.jar.disabled", b"jar-b!");
    let baseline = baseline_of(&[
      ("config/same.toml", theirs, false),
      ("config/edited-with-copy.toml", theirs, false),
      ("config/edited-no-copy.dat", b"other content!", false),
      ("config/new-size.toml", b"old", false),
      ("mods/b.jar", b"jar-b!", true),
    ]);
    base_store::store(
      &root,
      &BaselineFile::managed("config/edited-with-copy.toml", real_key(theirs), false),
      theirs,
    )
    .unwrap();

    let bytes = archive(&files);
    let source = BytesSource::new(bytes);
    let listing = read_listing(&source).await.unwrap();
    let remote = preview_remote(
      facts(),
      &latest(),
      &listing,
      &source,
      &root,
      "pack",
      &baseline,
    )
    .await
    .unwrap();
    let key_of = |path: &str| {
      let file = remote.files.iter().find(|file| file.path == path).unwrap();
      match &file.source {
        FileSource::Listed { exact, .. } => (file.key.clone(), *exact),
        other => panic!("{other:?}"),
      }
    };
    assert_eq!(key_of("config/same.toml"), (real_key(theirs), true));
    assert_eq!(
      key_of("config/edited-with-copy.toml"),
      (real_key(theirs), true)
    );
    assert_eq!(
      key_of("config/edited-no-copy.dat"),
      (real_key(theirs), true)
    );
    assert_eq!(key_of("mods/b.jar"), (real_key(b"jar-b!"), true));
    for path in ["config/new-size.toml", "config/added.toml"] {
      let (key, exact) = key_of(path);
      let content = files.iter().find(|(p, _)| *p == path).unwrap().1;
      assert!(!exact, "{path}");
      assert_eq!(key.size, content.len() as u64);
      assert_ne!(key, real_key(content));
    }
    // Only the one entry nothing could settle was read: the tail and the
    // directory come in one request, then a header and the data.
    assert_eq!(source.requests(), 1 + 2);
    // The downloaded mod keeps the platform's key.
    assert_eq!(
      remote
        .files
        .iter()
        .find(|f| f.path == "mods/a.jar")
        .unwrap()
        .key,
      real_key(b"jar-a")
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_preview_cannot_be_applied_or_stored() {
    let root = instance_dir();
    let baseline = baseline_of(&[]);
    let (run, _, _) = preview(&root, &baseline, archive(&[("config/a.toml", b"a")])).await;
    assert!(run.remote.has_listed_files());
    assert!(super::super::remote::StoredRemoteModpack::from(&run.remote).has_listed_files());
    let json = serde_json::to_string(&super::super::remote::StoredRemoteModpack::from(
      &run.remote,
    ))
    .unwrap();
    let back: super::super::remote::StoredRemoteModpack = serde_json::from_str(&json).unwrap();
    assert!(back.has_listed_files());
    let _ = std::fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn the_preview_plan_shows_what_the_full_plan_shows() {
    let root = instance_dir();
    let files: Vec<(&str, &[u8])> = vec![
      ("config/unchanged.toml", b"same"),
      ("config/updated.toml", b"new version of the file"),
      ("config/added.toml", b"added"),
      ("config/local-wins.toml", b"remote wins"),
    ];
    write(&root, "config/unchanged.toml", b"same");
    write(&root, "config/updated.toml", b"old");
    write(&root, "config/local-wins.toml", b"edited locally");
    write(&root, "config/removed.toml", b"gone");
    write(&root, "mods/a.jar", b"jar-a");
    let baseline = baseline_of(&[
      ("config/unchanged.toml", b"same", false),
      ("config/updated.toml", b"old", false),
      ("config/local-wins.toml", b"orig", false),
      ("config/removed.toml", b"gone", false),
    ]);
    let (run, _, _) = preview(&root, &baseline, archive(&files)).await;
    let (full, _) = full_plan(&root, &baseline, &files, &BTreeMap::new()).await;

    assert_ne!(run.prepared.plan_hash, full.plan_hash);
    assert_eq!(
      shape(&run.prepared, &run.loader_change),
      shape(&full, &None)
    );
    assert!(!full.diff.changes.is_empty() && !full.diff.conflicts.is_empty());
    let _ = std::fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn conflicting_text_is_fetched_given_its_real_key_and_merged() {
    let root = instance_dir();
    let base: &[u8] = b"a=1\nb=2\nc=3\nd=4\ne=5\nf=6\ng=7\n";
    let local: &[u8] = b"a=1\nb=2\nc=3\nd=4\ne=5\nf=6\ng=local\n";
    let remote_text: &[u8] = b"a=remote\nb=2\nc=3\nd=4\ne=5\nf=6\ng=7\n";
    write(&root, "config/merge.properties", local);
    let baseline = baseline_of(&[("config/merge.properties", base, false)]);
    base_store::store(
      &root,
      &BaselineFile::managed("config/merge.properties", real_key(base), false),
      base,
    )
    .unwrap();
    let files: Vec<(&str, &[u8])> = vec![("config/merge.properties", remote_text)];

    let (run, _, _) = preview(&root, &baseline, archive(&files)).await;
    // Merged automatically, with the real key from the fetched content.
    assert_eq!(run.prepared.diff.auto_merged.len(), 1);
    assert!(run.prepared.diff.conflicts.is_empty());
    assert_eq!(
      listed(&run, "config/merge.properties"),
      (real_key(remote_text), true)
    );
    let contents = BTreeMap::from([("config/merge.properties".to_string(), remote_text.to_vec())]);
    let (full, _) = full_plan(&root, &baseline, &files, &contents).await;
    assert_eq!(
      run.prepared.merged_contents, full.merged_contents,
      "the merged bytes are the same"
    );
    assert_eq!(shape(&run.prepared, &None), shape(&full, &None));
    let _ = std::fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn line_conflicts_keep_their_blocks_in_the_preview() {
    let root = instance_dir();
    let base: &[u8] = b"x=1\n";
    let local: &[u8] = b"x=local\n";
    let remote_text: &[u8] = b"x=remote\n";
    write(&root, "config/clash.properties", local);
    let baseline = baseline_of(&[("config/clash.properties", base, false)]);
    base_store::store(
      &root,
      &BaselineFile::managed("config/clash.properties", real_key(base), false),
      base,
    )
    .unwrap();
    let files: Vec<(&str, &[u8])> = vec![("config/clash.properties", remote_text)];
    let (run, _, _) = preview(&root, &baseline, archive(&files)).await;
    let contents = BTreeMap::from([("config/clash.properties".to_string(), remote_text.to_vec())]);
    let (full, _) = full_plan(&root, &baseline, &files, &contents).await;
    assert_eq!(run.prepared.diff.conflicts.len(), 1);
    assert!(!run.prepared.diff.conflicts[0].hunks.is_empty());
    assert_eq!(
      run.prepared.diff.conflicts[0].hunks,
      full.diff.conflicts[0].hunks
    );
    assert_eq!(shape(&run.prepared, &None), shape(&full, &None));
    assert_eq!(
      run.prepared.conflict_sources.keys().collect::<Vec<_>>(),
      full.conflict_sources.keys().collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(root);
  }

  async fn accepted(
    root: &Path,
    baseline: &ModpackBaseline,
    bytes: &[u8],
    files: &[(&str, &[u8])],
    contents: &BTreeMap<String, Vec<u8>>,
    confirmed: &str,
  ) -> bool {
    let path = root.join("pack.mrpack");
    std::fs::write(&path, bytes).unwrap();
    let (full, _) = full_plan(root, baseline, files, contents).await;
    let accepted = accepts(
      facts(),
      &latest(),
      &path,
      root,
      "pack",
      baseline,
      false,
      &[],
      confirmed,
      &full,
      &None,
    )
    .await;
    let _ = std::fs::remove_file(path);
    accepted
  }

  #[tokio::test]
  async fn the_apply_accepts_the_hash_of_the_preview_it_rebuilds() {
    let root = instance_dir();
    let files: Vec<(&str, &[u8])> = vec![
      ("config/updated.toml", b"new version of the file"),
      ("config/added.toml", b"added"),
    ];
    write(&root, "config/updated.toml", b"old");
    let baseline = baseline_of(&[("config/updated.toml", b"old", false)]);
    let bytes = archive(&files);
    let (run, _, _) = preview(&root, &baseline, bytes.clone()).await;
    let confirmed = run.prepared.plan_hash.clone();

    assert!(
      accepted(
        &root,
        &baseline,
        &bytes,
        &files,
        &BTreeMap::new(),
        &confirmed
      )
      .await
    );
    // A hash from some other state is refused.
    assert!(
      !accepted(
        &root,
        &baseline,
        &bytes,
        &files,
        &BTreeMap::new(),
        "0".repeat(40).as_str()
      )
      .await
    );
    // So is the preview of the instance as it was before the player changed a file.
    write(&root, "config/updated.toml", b"edited after the preview");
    assert!(
      !accepted(
        &root,
        &baseline,
        &bytes,
        &files,
        &BTreeMap::new(),
        &confirmed
      )
      .await
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_full_plan_that_shows_something_else_is_not_accepted() {
    let root = instance_dir();
    let files: Vec<(&str, &[u8])> = vec![("config/added.toml", b"added")];
    let baseline = baseline_of(&[]);
    let bytes = archive(&files);
    let (run, _, _) = preview(&root, &baseline, bytes.clone()).await;
    let path = root.join("pack.mrpack");
    std::fs::write(&path, &bytes).unwrap();
    // The full plan of a different instance state.
    write(&root, "config/added.toml", b"in the way");
    let (full, _) = full_plan(&root, &baseline, &files, &BTreeMap::new()).await;
    write(&root, "config/added.toml", b"in the way");
    std::fs::remove_file(root.join("config/added.toml")).unwrap();
    let accepted = accepts(
      facts(),
      &latest(),
      &path,
      &root,
      "pack",
      &baseline,
      false,
      &[],
      &run.prepared.plan_hash,
      &full,
      &None,
    )
    .await;
    assert!(!accepted);
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn override_entries_follow_the_zip_crate_listing() {
    use crate::instance::helpers::modpack::misc::list_override_entries;
    use std::io::Write;
    let root = instance_dir();
    let path = root.join("pack.zip");
    let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for name in [
      "modrinth.index.json",
      "overrides/config/a.toml",
      "overrides/ustblcfg.json",
      "overrides/install_profile.json",
      "client-overrides/config/a.toml",
      "client-overrides/mods/c.jar",
      "server-overrides/config/s.toml",
    ] {
      writer.start_file(name, options).unwrap();
      writer.write_all(b"x").unwrap();
    }
    writer.add_directory("overrides/empty/", options).unwrap();
    writer.finish().unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let listing = tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap()
      .block_on(read_listing(&BytesSource::new(bytes)))
      .unwrap();
    let layers = vec!["overrides".to_string(), "client-overrides".to_string()];
    let mine = override_entries(&listing, &layers, "pack").unwrap();
    let theirs = list_override_entries(&File::open(&path).unwrap(), &layers, "pack").unwrap();
    assert!(!mine.contains_key("install_profile.json"));
    assert_eq!(
      mine
        .iter()
        .map(|(path, entry)| (path.clone(), entry.name.clone()))
        .collect::<BTreeMap<_, _>>(),
      theirs
    );
    let _ = std::fs::remove_dir_all(root);
  }

  #[test]
  fn symlinks_and_escaping_paths_in_overrides_are_errors() {
    let run = |entries: Vec<TestEntry>| {
      let listing = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(read_listing(&BytesSource::new(build_zip(
          &entries, b"", false,
        ))))
        .unwrap();
      override_entries(&listing, &["overrides".to_string()], "pack")
    };
    assert!(run(vec![TestEntry::new("overrides/link", b"t").mode(0o120777)]).is_err());
    assert!(run(vec![TestEntry::new("overrides/../evil", b"t")]).is_err());
    assert!(run(vec![TestEntry::new("overrides/ok", b"t")]).is_ok());
  }
}
