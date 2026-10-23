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
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

const COPY_BUFFER: usize = 256 * 1024;

/// Local header length `build_pack` writes for every entry: 30 fixed bytes +
/// the entry name + the ZIP64 extended field `large_file(true)` adds. Entry
/// names are `objects/<sha1 of the content>`, so every entry has exactly this
/// length regardless of content (§5.2). Measured against this crate's actual
/// `zip` version; cross-check against vUSTB's verify constant once it lands.
const LOCAL_HEADER_FIXED_LEN: u64 = 30;
const ZIP64_EXTRA_FIXED_LEN: u64 = 20;
fn local_header_len(name: &str) -> u64 {
  LOCAL_HEADER_FIXED_LEN + name.len() as u64 + ZIP64_EXTRA_FIXED_LEN
}

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

/// Checks a pack against the rules `build_pack` guarantees and the server's
/// verify step relies on (§5.2, §8.2): each entry decompresses to the sha1 its
/// name promises and the CRC32 the central directory records; data offsets
/// follow from the local header length alone; and the whole file's sha1
/// matches `expected`. Used before the first upload and whenever the pack is
/// rebuilt during resume.
pub fn verify_pack(path: &Path, expected_sha1: &str) -> USTBLResult<()> {
  let bad = || USTBLError("本地包的内容校验未通过，无法继续，请放弃这个任务后重新发布".into());

  let (sha1, _) = hash_file(path)?;
  if sha1 != expected_sha1 {
    return Err(bad());
  }

  let mut zip = ZipArchive::new(File::open(path)?)?;
  let mut expected_header_start = 0u64;
  for i in 0..zip.len() {
    let mut entry = zip.by_index(i)?;
    let name = entry.name().to_string();
    let object_sha1 = name
      .strip_prefix("objects/")
      .filter(|sha1| sha1.len() == 40)
      .ok_or_else(bad)?;

    if entry.header_start() != expected_header_start {
      return Err(bad());
    }
    if entry.data_start() != entry.header_start() + local_header_len(&name) {
      return Err(bad());
    }

    // `zip` checks the CRC32 itself (via `Crc32Reader`) as the entry is read
    // to the end; an I/O error here means the entry's CRC32 did not match the
    // central directory's record.
    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; COPY_BUFFER];
    loop {
      let read = entry.read(&mut buffer).map_err(|_| bad())?;
      if read == 0 {
        break;
      }
      hasher.update(&buffer[..read]);
    }
    if hex::encode(hasher.finalize()) != object_sha1 {
      return Err(bad());
    }

    expected_header_start = entry.data_start() + entry.compressed_size();
  }
  // The central directory starts right where the last entry's data ends.
  if !zip.is_empty() && zip.central_directory_start() != expected_header_start {
    return Err(bad());
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use std::io::Seek;
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

  /// §5.2: the local header length must be computable from the entry name
  /// alone, so the server's verify step (and our own self-check) can derive
  /// every data offset from the central directory without reading local
  /// headers. Record this measured length in the delivery report for
  /// cross-check against vUSTB's own constant once it lands.
  #[test]
  fn the_local_header_length_matches_the_ss2_formula() {
    let root = dir();
    let objects = vec![
      object(&root, "a", b"alpha", false),
      object(&root, "b", &b"beta".repeat(10_000), true),
    ];
    let dest = root.join("pack.zip");
    build_pack(&objects, &dest).unwrap();

    let mut zip = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
    for object in &objects {
      let name = format!("objects/{}", object.sha1);
      let entry = zip.by_name(&name).unwrap();
      let measured = entry.data_start() - entry.header_start();
      assert_eq!(
        measured,
        local_header_len(&name),
        "measured local header length for {name} was {measured} bytes; \
         update LOCAL_HEADER_FIXED_LEN/ZIP64_EXTRA_FIXED_LEN if this changes"
      );
    }
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn verify_pack_accepts_a_pack_build_pack_produced() {
    let root = dir();
    let objects = vec![
      object(&root, "a", b"alpha alpha", false),
      object(&root, "b", &b"beta".repeat(1000), true),
    ];
    let dest = root.join("pack.zip");
    let built = build_pack(&objects, &dest).unwrap();
    verify_pack(&dest, &built.sha1).unwrap();
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn verify_pack_rejects_a_pack_whose_whole_sha1_does_not_match() {
    let root = dir();
    let objects = vec![object(&root, "a", b"alpha", false)];
    let dest = root.join("pack.zip");
    build_pack(&objects, &dest).unwrap();
    assert!(verify_pack(&dest, &"0".repeat(40)).is_err());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn verify_pack_rejects_an_entry_whose_content_was_tampered_with_but_still_decompresses() {
    let root = dir();
    let objects = vec![
      object(&root, "a", b"alpha alpha alpha alpha", false),
      object(&root, "b", b"beta beta beta beta", false),
    ];
    let dest = root.join("pack.zip");
    let built = build_pack(&objects, &dest).unwrap();

    // Flip a byte inside the first (stored or deflated) entry's compressed
    // data without touching lengths, so the file still parses as a zip but
    // an entry's content (and so its CRC32/sha1) no longer matches.
    let mut file = fs::OpenOptions::new().write(true).open(&dest).unwrap();
    let mut zip = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
    let entry = zip.by_index(0).unwrap();
    let data_start = entry.data_start();
    drop(entry);
    drop(zip);
    file.seek(std::io::SeekFrom::Start(data_start)).unwrap();
    file.write_all(&[0xFF]).unwrap();

    assert!(verify_pack(&dest, &built.sha1).is_err());
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
