//! Building the delta pack a version is published with.
//!
//! The pack holds each object once, as `objects/<sha1 of the content>`, so the
//! platform can index it and hand out single entries later. The bytes depend
//! only on the objects: entries are ordered by hash, timestamps are fixed and no
//! data descriptors are used. A pack that went missing from disk can therefore
//! be built again and still match the hash the platform was promised.

use super::manifest::PackObject;
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::baseline::hash_file;
use sha1::{Digest, Sha1};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

const COPY_BUFFER: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltPack {
  pub sha1: String,
  pub size: u64,
}

/// Write the pack for `objects` to `dest`. Every object is hashed on the way
/// in: a file that changed since it was scanned fails the build rather than
/// ending up under the wrong name.
pub fn build_pack(objects: &[PackObject], dest: &Path) -> USTBLResult<BuiltPack> {
  let mut ordered: Vec<&PackObject> = objects.iter().collect();
  ordered.sort_by(|a, b| a.sha1.cmp(&b.sha1));

  let mut zip = ZipWriter::new(File::create(dest)?);
  let mut buffer = vec![0u8; COPY_BUFFER];
  for object in ordered {
    let method = if object.stored {
      CompressionMethod::Stored
    } else {
      CompressionMethod::Deflated
    };
    let options = SimpleFileOptions::default()
      .compression_method(method)
      .last_modified_time(DateTime::default())
      .unix_permissions(0o644)
      .large_file(true);
    zip.start_file(format!("objects/{}", object.sha1), options)?;

    let mut source = File::open(&object.path)?;
    let mut hasher = Sha1::new();
    loop {
      let read = source.read(&mut buffer)?;
      if read == 0 {
        break;
      }
      hasher.update(&buffer[..read]);
      zip.write_all(&buffer[..read])?;
    }
    if hex::encode(hasher.finalize()) != object.sha1 {
      return Err(USTBLError(format!(
        "文件在发布过程中发生了变化: {}",
        object.path.display()
      )));
    }
  }
  zip.finish()?.flush()?;

  let (sha1, size) = hash_file(dest)?;
  Ok(BuiltPack { sha1, size })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use std::path::PathBuf;

  fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ustbl-pack-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn object(root: &Path, name: &str, content: &[u8], stored: bool) -> PackObject {
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    PackObject {
      sha1: hex::encode(Sha1::digest(content)),
      path,
      stored,
    }
  }

  #[test]
  fn the_pack_holds_each_object_under_its_hash_and_reads_back() {
    let root = dir();
    let text = b"hello pack ".repeat(100);
    let binary: Vec<u8> = (0..=255u8).collect();
    let objects = vec![
      object(&root, "a.txt", &text, false),
      object(&root, "b.bin", &binary, true),
      object(&root, "empty", b"", false),
    ];
    let dest = root.join("pack.zip");

    let built = build_pack(&objects, &dest).unwrap();

    assert_eq!(built.size, fs::metadata(&dest).unwrap().len());
    assert_eq!(built.sha1, hash_file(&dest).unwrap().0);
    let mut zip = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
    assert_eq!(zip.len(), 3);
    for object in &objects {
      let mut entry = zip.by_name(&format!("objects/{}", object.sha1)).unwrap();
      assert_eq!(
        entry.compression(),
        if object.stored {
          CompressionMethod::Stored
        } else {
          CompressionMethod::Deflated
        }
      );
      let mut content = Vec::new();
      entry.read_to_end(&mut content).unwrap();
      assert_eq!(content, fs::read(&object.path).unwrap());
    }
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn the_same_objects_give_the_same_bytes_in_any_order_and_any_place() {
    let root = dir();
    let mut objects = vec![
      object(&root, "a", b"alpha alpha alpha", false),
      object(&root, "b", b"beta", true),
      object(&root, "c", &b"gamma".repeat(500), false),
    ];
    let first = build_pack(&objects, &root.join("one.zip")).unwrap();
    objects.reverse();
    let second = build_pack(&objects, &root.join("two.zip")).unwrap();
    assert_eq!(first, second);

    // Rebuilt later from copies of the files, the pack is still the same.
    let moved = dir();
    let copies: Vec<PackObject> = objects
      .iter()
      .map(|o| {
        let path = moved.join(o.path.file_name().unwrap());
        fs::copy(&o.path, &path).unwrap();
        PackObject { path, ..o.clone() }
      })
      .collect();
    assert_eq!(
      build_pack(&copies, &moved.join("three.zip")).unwrap(),
      first
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(moved);
  }

  #[test]
  fn a_file_that_changed_after_scanning_fails_the_build() {
    let root = dir();
    let objects = vec![object(&root, "a", b"before", false)];
    fs::write(&objects[0].path, b"after").unwrap();
    assert!(build_pack(&objects, &root.join("p.zip")).is_err());
    let _ = fs::remove_dir_all(root);
  }
}
