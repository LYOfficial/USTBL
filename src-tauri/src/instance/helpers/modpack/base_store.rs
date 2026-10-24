//! Local copies of text baselines used by normalized comparison and merging.

use super::baseline::BaselineFile;
use super::text::is_text_path;
use crate::instance::modpack_update::remote::UPDATE_DIR;
use sha1::{Digest, Sha1};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn base_dir(root: &Path) -> PathBuf {
  root.join(UPDATE_DIR).join("base")
}

pub fn base_path(root: &Path, sha1: &str) -> PathBuf {
  base_dir(root).join(sha1)
}

pub fn load(root: &Path, sha1: &str) -> io::Result<Option<Vec<u8>>> {
  match fs::read(base_path(root, sha1)) {
    Ok(bytes) => Ok(Some(bytes)),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

pub fn store(root: &Path, file: &BaselineFile, bytes: &[u8]) -> io::Result<()> {
  if hex::encode(Sha1::digest(bytes)) != file.sha1
    || bytes.len() as u64 != file.size
    || !is_text_path(&file.path, bytes)
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid text baseline",
    ));
  }
  fs::create_dir_all(base_dir(root))?;
  let target = base_path(root, &file.sha1);
  let temporary = target.with_extension(uuid::Uuid::new_v4().to_string());
  fs::write(&temporary, bytes)?;
  let result = fs::rename(&temporary, &target);
  let _ = fs::remove_file(temporary);
  result
}

/// Cache text files from the just-committed baseline and remove stale entries.
///
/// An entry whose sha1 a baseline text file references is kept even when the
/// file on disk differs from it: the update may have written merged or kept
/// local content, while the base was cached separately. Only entries the new
/// baseline no longer references are removed. A dormant entry keeps its copy
/// too; a disabled file reads from its `.disabled` variant.
pub fn sync(root: &Path, files: &[BaselineFile]) -> io::Result<()> {
  let dir = base_dir(root);
  fs::create_dir_all(&dir)?;
  let mut referenced = std::collections::BTreeSet::new();
  for file in files {
    if file.sha1.is_empty() {
      continue;
    }
    if is_text_path(&file.path, &[]) && base_path(root, &file.sha1).exists() {
      referenced.insert(file.sha1.clone());
    }
    let variant = if file.disabled {
      format!(
        "{}{}",
        file.path,
        crate::instance::modpack_update::diff::DISABLED_SUFFIX
      )
    } else {
      file.path.clone()
    };
    let source = root.join(&variant);
    let bytes = match fs::read(&source) {
      Ok(bytes) if is_text_path(&file.path, &bytes) => bytes,
      Ok(_) => continue,
      Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error),
    };
    let actual = hex::encode(Sha1::digest(&bytes));
    if actual != file.sha1 {
      continue;
    }
    referenced.insert(file.sha1.clone());
    let target = base_path(root, &file.sha1);
    if !target.exists() {
      let temporary = target.with_extension("tmp");
      fs::write(&temporary, bytes)?;
      fs::rename(temporary, target)?;
    }
  }
  for entry in fs::read_dir(&dir)? {
    let entry = entry?;
    if entry.file_type()?.is_file()
      && !referenced.contains(&entry.file_name().to_string_lossy().to_string())
    {
      fs::remove_file(entry.path())?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::BaselineFile;

  #[test]
  fn recovered_base_is_verified_before_caching() {
    let root = std::env::temp_dir().join(format!("ustbl-recovered-{}", uuid::Uuid::new_v4()));
    let bytes = b"a=1\n";
    let file = BaselineFile::managed(
      "config/a.properties",
      crate::instance::modpack_update::diff::FileKey {
        sha1: hex::encode(Sha1::digest(bytes)),
        size: bytes.len() as u64,
      },
      false,
    );
    assert!(store(&root, &file, b"wrong").is_err());
    assert!(load(&root, &file.sha1).unwrap().is_none());
    store(&root, &file, bytes).unwrap();
    assert_eq!(load(&root, &file.sha1).unwrap().unwrap(), bytes);
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn sync_reads_the_disabled_variant_of_a_disabled_entry() {
    let root = std::env::temp_dir().join(format!("ustbl-base-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("config")).unwrap();
    fs::create_dir_all(base_dir(&root)).unwrap();
    let text = b"a=1\n";
    let sha1 = hex::encode(Sha1::digest(text));
    // The file sits at its disabled variant; the enabled name is absent.
    fs::write(root.join("config/a.properties.disabled"), text).unwrap();
    sync(
      &root,
      &[BaselineFile::managed(
        "config/a.properties",
        crate::instance::modpack_update::diff::FileKey {
          sha1: sha1.clone(),
          size: text.len() as u64,
        },
        true,
      )],
    )
    .unwrap();
    assert_eq!(load(&root, &sha1).unwrap(), Some(text.to_vec()));
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn sync_stores_only_matching_text_files_and_prunes_old_entries() {
    let root = std::env::temp_dir().join(format!("ustbl-base-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("config")).unwrap();
    fs::create_dir_all(base_dir(&root)).unwrap();
    let text = b"a=1\r\n";
    let sha1 = hex::encode(Sha1::digest(text));
    fs::write(root.join("config/a.properties"), text).unwrap();
    fs::write(base_path(&root, "old"), b"old").unwrap();
    sync(
      &root,
      &[BaselineFile::managed(
        "config/a.properties",
        crate::instance::modpack_update::diff::FileKey {
          sha1: sha1.clone(),
          size: text.len() as u64,
        },
        false,
      )],
    )
    .unwrap();
    assert_eq!(load(&root, &sha1).unwrap(), Some(text.to_vec()));
    assert!(!base_path(&root, "old").exists());
    let _ = fs::remove_dir_all(root);
  }

  /// A cached base the new baseline references is kept even when the file on
  /// disk differs from it (merged or kept local); only unreferenced entries
  /// are pruned.
  #[test]
  fn sync_keeps_a_referenced_entry_the_disk_no_longer_matches() {
    let root = std::env::temp_dir().join(format!("ustbl-base-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("config")).unwrap();
    fs::create_dir_all(base_dir(&root)).unwrap();
    // The new baseline's text: stored ahead of sync, as `apply` does.
    let remote = b"a=2\r\n";
    let remote_sha1 = hex::encode(Sha1::digest(remote));
    fs::write(base_path(&root, &remote_sha1), remote).unwrap();
    // The merged file on disk differs from both.
    fs::write(root.join("config/a.properties"), b"a=3\r\n").unwrap();
    // An entry nothing references anymore.
    fs::write(base_path(&root, "old"), b"old").unwrap();
    sync(
      &root,
      &[BaselineFile::managed(
        "config/a.properties",
        crate::instance::modpack_update::diff::FileKey {
          sha1: remote_sha1.clone(),
          size: remote.len() as u64,
        },
        false,
      )],
    )
    .unwrap();
    assert_eq!(load(&root, &remote_sha1).unwrap(), Some(remote.to_vec()));
    assert!(!base_path(&root, "old").exists());
    let _ = fs::remove_dir_all(root);
  }
}
