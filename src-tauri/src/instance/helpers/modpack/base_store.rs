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

/// Cache text files from the just-committed baseline and remove stale entries.
pub fn sync(root: &Path, files: &[BaselineFile]) -> io::Result<()> {
  let dir = base_dir(root);
  fs::create_dir_all(&dir)?;
  let mut referenced = std::collections::BTreeSet::new();
  for file in files {
    let source = root.join(&file.path);
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

/// Bootstrap a legacy instance that predates the text-baseline cache.
///
/// The old baseline stores only hashes, so the original bytes cannot be
/// reconstructed locally. Keeping the current text bytes under the recorded
/// baseline hash gives legacy instances a stable comparison point until the
/// remote historical-object fetch is available to the update command.
pub fn bootstrap_legacy(root: &Path, files: &[BaselineFile]) -> io::Result<bool> {
  let dir = base_dir(root);
  fs::create_dir_all(&dir)?;
  let mut created = false;
  for file in files {
    let target = base_path(root, &file.sha1);
    if target.exists() {
      continue;
    }
    let source = root.join(&file.path);
    let bytes = match fs::read(source) {
      Ok(bytes) if super::text::is_text_path(&file.path, &bytes) => bytes,
      Ok(_) => continue,
      Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error),
    };
    fs::write(target, bytes)?;
    created = true;
  }
  Ok(created)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::instance::helpers::modpack::baseline::BaselineFile;

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
      &[BaselineFile {
        path: "config/a.properties".into(),
        sha1: sha1.clone(),
        size: text.len() as u64,
      }],
    )
    .unwrap();
    assert_eq!(load(&root, &sha1).unwrap(), Some(text.to_vec()));
    assert!(!base_path(&root, "old").exists());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn bootstrap_legacy_keeps_current_text_under_the_old_baseline_key() {
    let root = std::env::temp_dir().join(format!("ustbl-base-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("config")).unwrap();
    let bytes = b"a=1\r\n";
    fs::write(root.join("config/a.properties"), bytes).unwrap();
    let key = "baseline-sha1";
    let files = [BaselineFile { path: "config/a.properties".into(), sha1: key.into(), size: 4 }];
    assert!(bootstrap_legacy(&root, &files).unwrap());
    assert_eq!(load(&root, key).unwrap(), Some(bytes.to_vec()));
    assert!(!bootstrap_legacy(&root, &files).unwrap());
    let _ = fs::remove_dir_all(root);
  }
}
