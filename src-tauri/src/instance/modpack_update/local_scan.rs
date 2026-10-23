//! In-process SHA-1 cache for the local-change scan.
//!
//! Counting local changes hashes every managed file on every visit to the sync
//! page. The files of a GTNH-sized instance are several gigabytes, so the scan
//! remembers what it already hashed: an entry keyed by the file's absolute
//! path stays valid while the file's size and modification time are unchanged.
//!
//! Only the local-change count and list read this cache. Preview and apply
//! keep hashing everything themselves (their result decides what is written),
//! but they refresh the entries of the files they just hashed.

use super::diff::{DISABLED_SUFFIX, FileKey, LocalState};
use crate::instance::helpers::modpack::baseline::hash_file;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// What the scan last saw of one managed file.
struct ScanEntry {
  size: u64,
  /// Modification time of the hashed file; `None` stamps nothing.
  modified: Option<SystemTime>,
  state: LocalState,
}

#[derive(Default)]
pub struct LocalScanCache {
  /// Managed paths of one instance, keyed by its root directory.
  instances: HashMap<PathBuf, HashMap<String, ScanEntry>>,
}

impl LocalScanCache {
  /// Forget the instance's entries: it was deleted or stopped following.
  pub fn invalidate(&mut self, root: &Path) {
    self.instances.remove(root);
  }

  /// The cached state of `path`, while the file's stat still matches.
  fn lookup(&self, root: &Path, path: &str) -> Option<LocalState> {
    let entry = self.instances.get(root)?.get(path)?;
    let (size, modified) = stat_of(&file_of(root, path, &entry.state))?;
    if entry.size == size && entry.modified == modified {
      Some(entry.state.clone())
    } else {
      None
    }
  }

  /// Remember a just-computed state, replacing any older entry of the path.
  fn record(&mut self, root: &Path, path: &str, state: &LocalState) {
    // A placeholder (empty sha1) is not a computed state: it only says "not
    // the baseline it was scanned against". Caching it would report an
    // untouched file as changed under every later baseline.
    let placeholder = match state {
      LocalState::Present(key) | LocalState::Disabled(key) => key.sha1.is_empty(),
      LocalState::Absent => false,
    };
    if placeholder {
      return;
    }
    let file = file_of(root, path, state);
    let (size, modified) = match stat_of(&file) {
      Some(stat) => stat,
      // The file vanished between scan and record; caching it would keep a
      // ghost alive.
      None => return,
    };
    self
      .instances
      .entry(root.to_path_buf())
      .or_default()
      .insert(
        path.to_string(),
        ScanEntry {
          size,
          modified,
          state: state.clone(),
        },
      );
  }
}

/// The file a path's state was taken from: `<path>.disabled` for a switched
/// off file, the path itself otherwise.
fn file_of(root: &Path, path: &str, state: &LocalState) -> PathBuf {
  match state {
    LocalState::Disabled(_) => root.join(format!("{path}{DISABLED_SUFFIX}")),
    _ => root.join(path),
  }
}

fn stat_of(file: &Path) -> Option<(u64, Option<SystemTime>)> {
  let metadata = std::fs::metadata(file).ok()?;
  Some((metadata.len(), metadata.modified().ok()))
}

/// Hash the file like `collect_local_states` does, but reuse `cache` for the
/// paths whose stat has not moved since they were last hashed. Cached states
/// and freshly computed ones are recorded back, so a scan that ran with an
/// empty cache primes it for the next one.
///
/// Unlike `collect_local_states` a file whose size differs from `baseline`
/// is reported as a change without hashing it at all: different sizes can
/// never be the same content.
pub fn collect_states_cached(
  cache: &Mutex<LocalScanCache>,
  root: &Path,
  baseline: &BTreeMap<String, FileKey>,
) -> io::Result<BTreeMap<String, LocalState>> {
  let paths: BTreeSet<String> = baseline.keys().cloned().collect();
  let mut states = BTreeMap::new();
  for path in paths {
    let cached = cache.lock().ok().and_then(|c| c.lookup(root, &path));
    let state = match cached {
      Some(state) => state,
      None => {
        let state = scan_one(root, &path, baseline.get(&path))?;
        if let Ok(mut c) = cache.lock() {
          c.record(root, &path, &state);
        }
        state
      }
    };
    states.insert(path, state);
  }
  Ok(states)
}

fn scan_one(root: &Path, path: &str, baseline: Option<&FileKey>) -> io::Result<LocalState> {
  let file = root.join(path);
  if file.is_file() {
    // A size that already differs can never hash to the baseline key.
    if let Some(key) = baseline {
      let size = std::fs::metadata(&file)?.len();
      if size != key.size {
        return Ok(LocalState::Present(FileKey {
          sha1: String::new(),
          size,
        }));
      }
    }
    let (sha1, size) = hash_file(&file)?;
    return Ok(LocalState::Present(FileKey { sha1, size }));
  }
  if file.exists() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("{path} is not a regular file"),
    ));
  }
  let disabled = root.join(format!("{path}{DISABLED_SUFFIX}"));
  if disabled.is_file() {
    let (sha1, size) = hash_file(&disabled)?;
    return Ok(LocalState::Disabled(FileKey { sha1, size }));
  }
  Ok(LocalState::Absent)
}

/// Refresh the entries of everything `states` covers. Preview and apply hash
/// the files themselves instead of reading this cache; the states they just
/// computed are the freshest there are, so the scan can have them.
pub fn record_states(
  cache: &Mutex<LocalScanCache>,
  root: &Path,
  states: &BTreeMap<String, LocalState>,
) {
  if let Ok(mut cache) = cache.lock() {
    for (path, state) in states {
      cache.record(root, path, state);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use sha1::{Digest, Sha1};
  use std::fs;

  fn key(content: &str) -> FileKey {
    FileKey {
      sha1: hex::encode(Sha1::digest(content.as_bytes())),
      size: content.len() as u64,
    }
  }

  fn root(files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-scan-test-{}", uuid::Uuid::new_v4()));
    for (path, content) in files {
      let file = root.join(path);
      fs::create_dir_all(file.parent().unwrap()).unwrap();
      fs::write(file, content).unwrap();
    }
    root
  }

  #[test]
  fn a_cached_state_is_reused_while_the_file_is_untouched() {
    let root = root(&[("mods/a.jar", "content")]);
    let baseline = BTreeMap::from([("mods/a.jar".to_string(), key("content"))]);
    let cache = Mutex::new(LocalScanCache::default());

    let first = collect_states_cached(&cache, &root, &baseline).unwrap();
    assert_eq!(first["mods/a.jar"], LocalState::Present(key("content")));

    // A poisoned cache entry must win over the disk while the stat holds.
    if let Ok(mut c) = cache.lock() {
      c.record(&root, "mods/a.jar", &LocalState::Present(key("other")));
    }
    let second = collect_states_cached(&cache, &root, &baseline).unwrap();
    assert_eq!(second["mods/a.jar"], LocalState::Present(key("other")));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_changed_file_is_hashed_again() {
    let root = root(&[("mods/a.jar", "content")]);
    let baseline = BTreeMap::from([("mods/a.jar".to_string(), key("content"))]);
    let cache = Mutex::new(LocalScanCache::default());
    collect_states_cached(&cache, &root, &baseline).unwrap();

    // Same length, new write: the modification time drops the cached entry,
    // so the file is re-hashed and the new content reported.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(root.join("mods/a.jar"), "CONTENT").unwrap();
    let states = collect_states_cached(&cache, &root, &baseline).unwrap();
    assert_eq!(states["mods/a.jar"], LocalState::Present(key("CONTENT")));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_size_mismatch_is_a_change_without_a_hash() {
    let root = root(&[("mods/a.jar", "content")]);
    // The baseline expects another size: the scan must not read the file,
    // and the reported sha1 is a placeholder that can never match.
    let baseline = BTreeMap::from([("mods/a.jar".to_string(), key("x"))]);
    let cache = Mutex::new(LocalScanCache::default());
    let states = collect_states_cached(&cache, &root, &baseline).unwrap();
    match &states["mods/a.jar"] {
      LocalState::Present(found) => {
        assert_eq!(found.size, 7);
        assert!(found.sha1.is_empty());
      }
      other => panic!("{other:?}"),
    }
    let _ = fs::remove_dir_all(root);
  }

  /// The placeholder a size mismatch reports is not cached: a later baseline
  /// that does match the file's size must hash it and see no change. Happens
  /// when an update keeps the player's file and the new baseline records its
  /// size.
  #[test]
  fn a_size_placeholder_is_forgotten_under_the_next_baseline() {
    let root = root(&[("mods/a.jar", "content")]);
    let cache = Mutex::new(LocalScanCache::default());

    // First scan: the baseline expects another size, so the file is a change
    // reported without a hash.
    let short = BTreeMap::from([("mods/a.jar".to_string(), key("x"))]);
    let states = collect_states_cached(&cache, &root, &short).unwrap();
    match &states["mods/a.jar"] {
      LocalState::Present(found) => assert!(found.sha1.is_empty()),
      other => panic!("{other:?}"),
    }

    // Second scan: the new baseline matches the file exactly. Had the
    // placeholder been cached, the lookup would hand back the empty sha1 and
    // the file would still count as changed; instead it is hashed now.
    let exact = BTreeMap::from([("mods/a.jar".to_string(), key("content"))]);
    let states = collect_states_cached(&cache, &root, &exact).unwrap();
    assert_eq!(states["mods/a.jar"], LocalState::Present(key("content")));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn invalidate_forgets_the_instance() {
    let root = root(&[("mods/a.jar", "content")]);
    let baseline = BTreeMap::from([("mods/a.jar".to_string(), key("content"))]);
    let cache = Mutex::new(LocalScanCache::default());
    collect_states_cached(&cache, &root, &baseline).unwrap();
    if let Ok(mut c) = cache.lock() {
      c.record(&root, "mods/a.jar", &LocalState::Present(key("other")));
      c.invalidate(&root);
    }
    let states = collect_states_cached(&cache, &root, &baseline).unwrap();
    assert_eq!(states["mods/a.jar"], LocalState::Present(key("content")));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn record_states_primes_the_cache_for_the_scan() {
    let root = root(&[("mods/a.jar", "content")]);
    let baseline = BTreeMap::from([("mods/a.jar".to_string(), key("content"))]);
    let cache = Mutex::new(LocalScanCache::default());
    let computed = BTreeMap::from([("mods/a.jar".to_string(), LocalState::Present(key("other")))]);
    record_states(&cache, &root, &computed);
    let states = collect_states_cached(&cache, &root, &baseline).unwrap();
    assert_eq!(states["mods/a.jar"], LocalState::Present(key("other")));
    let _ = fs::remove_dir_all(root);
  }
}
