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
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, HasZipMetadata, ZipArchive, ZipWriter};

const COPY_BUFFER: usize = 256 * 1024;

/// Local header length `build_pack` writes for every entry: 30 fixed bytes +
/// the entry name + the ZIP64 extended field `large_file(true)` adds. The
/// extra field is 20 bytes (two sizes) while the entry's own offset still
/// fits in 32 bits, 28 bytes (plus that offset) once it does not (§5.2).
/// Entry names are `objects/<sha1 of the content>`; the
/// length only depends on the name and where the entry starts, never its
/// content. Measured against this crate's actual `zip` version; cross-check
/// against vUSTB's verify constant once it lands.
const LOCAL_HEADER_FIXED_LEN: u64 = 30;
const ZIP64_EXTRA_SHORT_LEN: u64 = 20;
const ZIP64_EXTRA_LONG_LEN: u64 = 28;
/// Entry offsets at or above this no longer fit the plain 32-bit field, so
/// `large_file(true)` widens the ZIP64 extra field to carry the offset too.
const ZIP64_OFFSET_THRESHOLD: u64 = 0xFFFF_FFFF;

/// `header_start` is the entry's own offset into the file, the quantity
/// §5.2's threshold is defined against (not, e.g., the central directory's
/// offset).
fn local_header_len(name: &str, header_start: u64) -> u64 {
  let extra = if header_start < ZIP64_OFFSET_THRESHOLD {
    ZIP64_EXTRA_SHORT_LEN
  } else {
    ZIP64_EXTRA_LONG_LEN
  };
  LOCAL_HEADER_FIXED_LEN + name.len() as u64 + extra
}

/// Everything §5.2 requires of one entry that can be read off its metadata
/// without decompressing it: a `objects/<sha1>` name that strictly follows
/// the previous one (so entries are sorted and no name repeats), no
/// encryption or data descriptor, a supported compression method, and a data
/// offset that follows from its own header length alone. Pulled out as a
/// pure function of plain facts so the structural rules are testable without
/// hand-crafting zip byte streams for each violation.
#[allow(clippy::too_many_arguments)]
fn entry_structurally_valid(
  name: &str,
  previous_name: Option<&str>,
  encrypted: bool,
  using_data_descriptor: bool,
  compression: CompressionMethod,
  header_start: u64,
  data_start: u64,
  expected_header_start: u64,
) -> bool {
  if name
    .strip_prefix("objects/")
    .filter(|sha1| sha1.len() == 40)
    .is_none()
  {
    return false;
  }
  if previous_name.is_some_and(|previous| previous >= name) {
    return false;
  }
  if encrypted || using_data_descriptor {
    return false;
  }
  if !matches!(
    compression,
    CompressionMethod::Stored | CompressionMethod::Deflated
  ) {
    return false;
  }
  if header_start != expected_header_start {
    return false;
  }
  data_start == header_start + local_header_len(name, header_start)
}

/// The whole-file sha1 and size of a pack that passed `verify_pack`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPack {
  pub sha1: String,
  pub size: u64,
}

/// Write the pack for `objects` to `dest`. Every object is hashed on the way
/// in: a file that changed since it was scanned fails the build rather than
/// ending up under the wrong name.
///
/// The pack's own sha1 is not computed here: every pack goes through
/// `verify_pack` before it is used, and that read yields the hash. (Hashing
/// the bytes as they are written does not work: `ZipWriter` seeks back to
/// patch each entry's local header once its data is written.)
pub fn build_pack(objects: &[PackObject], dest: &Path) -> USTBLResult<()> {
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
  Ok(())
}

/// Build the pack again for an upload resumed after its pack went missing.
/// It has to come out as the pack the platform was promised; one that does
/// not is removed again. The result has passed `verify_pack`.
pub fn rebuild_pack(
  objects: &[PackObject],
  dest: &Path,
  expected_sha1: &str,
  expected_size: u64,
) -> USTBLResult<()> {
  build_pack(objects, dest)?;
  let built = verify_pack(dest, None)?;
  if built.sha1 != expected_sha1 || built.size != expected_size {
    let _ = std::fs::remove_file(dest);
    return Err(USTBLError(
      "实例中的文件在上次发布后发生了变化，无法继续，请放弃这个任务后重新发布".into(),
    ));
  }
  Ok(())
}

/// The packs this process has already run the self-check on, keyed by path.
/// `ensure_pack` consults this before reading a pack back: a pack that was
/// just built and verified here does not need the second read, while a pack
/// resumed from disk after a restart (never marked) always gets it.
static VERIFIED_PACKS: LazyLock<Mutex<HashMap<PathBuf, (u64, SystemTime)>>> =
  LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remember that this process verified the pack at `path` with this size and
/// modification time.
pub fn mark_verified(path: &Path, size: u64, modified: SystemTime) {
  if let Ok(mut packs) = VERIFIED_PACKS.lock() {
    packs.insert(path.to_path_buf(), (size, modified));
  }
}

/// Whether the file at `path` is still exactly what this process verified:
/// same size, same modification time. A missing entry (a pack from an
/// earlier run) or any change answers no.
pub fn is_verified(path: &Path) -> bool {
  let Ok(meta) = std::fs::metadata(path) else {
    return false;
  };
  let Ok(modified) = meta.modified() else {
    return false;
  };
  VERIFIED_PACKS
    .lock()
    .ok()
    .and_then(|packs| packs.get(path).copied())
    .is_some_and(|marked| marked == (meta.len(), modified))
}

/// Checks a pack against the rules `build_pack` guarantees and the server's
/// verify step relies on (§5.2, §8.2): entries sit back to back in sorted,
/// unique-name order with the central directory immediately after the last
/// one and no trailing comment; no entry is encrypted, uses a data
/// descriptor, or is compressed with anything but STORED/DEFLATE; each entry
/// decompresses to the sha1 its name promises and the CRC32 the central
/// directory records; data offsets follow from the local header length
/// alone; and, given `expected_sha1`, the whole file's sha1 matches it.
/// `None` is for a pack just built, whose sha1 is not known yet; the caller
/// then takes the returned one (and compares it itself where a record says
/// what it has to be). Used before the first upload, whenever the pack is
/// rebuilt during resume, and on a pack resumed from disk.
pub fn verify_pack(path: &Path, expected_sha1: Option<&str>) -> USTBLResult<VerifiedPack> {
  let bad = || USTBLError("本地包的内容校验未通过，无法继续，请放弃这个任务后重新发布".into());

  let (sha1, size) = hash_file(path)?;
  if expected_sha1.is_some_and(|expected| expected != sha1) {
    return Err(bad());
  }

  let mut zip = ZipArchive::new(File::open(path)?)?;
  if !zip.comment().is_empty() {
    return Err(bad());
  }

  let mut expected_header_start = 0u64;
  let mut previous_name: Option<String> = None;
  for i in 0..zip.len() {
    let mut entry = zip.by_index(i)?;
    let name = entry.name().to_string();
    let object_sha1 = name
      .strip_prefix("objects/")
      .filter(|sha1| sha1.len() == 40)
      .ok_or_else(bad)?;
    if !entry_structurally_valid(
      &name,
      previous_name.as_deref(),
      entry.encrypted(),
      entry.get_metadata().using_data_descriptor,
      entry.compression(),
      entry.header_start(),
      entry.data_start(),
      expected_header_start,
    ) {
      return Err(bad());
    }
    previous_name = Some(name.clone());

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
  Ok(VerifiedPack { sha1, size })
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

    build_pack(&objects, &dest).unwrap();
    let built = verify_pack(&dest, None).unwrap();

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
  /// and its own starting offset alone, so the server's verify step (and our
  /// own self-check) can derive every data offset from the central directory
  /// without reading local headers. Record this measured length in the
  /// delivery report for cross-check against vUSTB's own constant once it
  /// lands. Both entries here start well below the ZIP64 offset threshold;
  /// the "entry starts at or past 4 GiB" branch is covered by the pure
  /// formula test below, since building a pack that large is impractical.
  #[test]
  fn the_local_header_length_matches_the_ss2_formula_below_the_offset_threshold() {
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
        local_header_len(&name, entry.header_start()),
        "measured local header length for {name} was {measured} bytes; \
         update LOCAL_HEADER_FIXED_LEN/ZIP64_EXTRA_SHORT_LEN if this changes"
      );
    }
    let _ = fs::remove_dir_all(root);
  }

  /// The formula's two branches (§5.2): a 20-byte ZIP64 extra field while
  /// the entry's own offset still fits 32 bits, 28 bytes (room for the
  /// offset itself) once it does not.
  #[test]
  fn the_local_header_length_formula_switches_at_the_offset_threshold() {
    let name = "objects/".to_string() + &"a".repeat(40);
    assert_eq!(local_header_len(&name, 0), 30 + name.len() as u64 + 20);
    assert_eq!(
      local_header_len(&name, ZIP64_OFFSET_THRESHOLD - 1),
      30 + name.len() as u64 + 20
    );
    assert_eq!(
      local_header_len(&name, ZIP64_OFFSET_THRESHOLD),
      30 + name.len() as u64 + 28
    );
    assert_eq!(
      local_header_len(&name, ZIP64_OFFSET_THRESHOLD + 1_000_000),
      30 + name.len() as u64 + 28
    );
  }

  #[test]
  fn verify_pack_accepts_a_pack_build_pack_produced() {
    let root = dir();
    let objects = vec![
      object(&root, "a", b"alpha alpha", false),
      object(&root, "b", &b"beta".repeat(1000), true),
    ];
    let dest = root.join("pack.zip");
    build_pack(&objects, &dest).unwrap();
    let built = verify_pack(&dest, None).unwrap();
    assert_eq!(verify_pack(&dest, Some(&built.sha1)).unwrap(), built);
    let _ = fs::remove_dir_all(root);
  }

  /// §5.2: the EOCD must sit right at the end of the file with no
  /// trailing comment, even though the file's whole-sha1 still matches
  /// (which only covers the bytes, not that nothing trails the EOCD record
  /// logically — a comment is itself part of those bytes, so this exercises
  /// `verify_pack`'s dedicated comment check rather than the sha1 one).
  #[test]
  fn verify_pack_rejects_a_pack_whose_eocd_carries_a_comment() {
    let root = dir();
    let objects = vec![object(&root, "a", b"alpha", false)];
    let dest = root.join("pack.zip");
    {
      let objects: Vec<&PackObject> = objects.iter().collect();
      let mut zip = ZipWriter::new(File::create(&dest).unwrap());
      for object in objects {
        let options = SimpleFileOptions::default()
          .compression_method(CompressionMethod::Deflated)
          .last_modified_time(DateTime::default())
          .unix_permissions(0o644)
          .large_file(true);
        zip
          .start_file(format!("objects/{}", object.sha1), options)
          .unwrap();
        zip.write_all(&fs::read(&object.path).unwrap()).unwrap();
      }
      zip.set_comment("not empty");
      zip.finish().unwrap();
    }
    let (sha1, _) = hash_file(&dest).unwrap();
    assert!(verify_pack(&dest, Some(&sha1)).is_err());
    assert!(verify_pack(&dest, None).is_err());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn an_entry_name_must_be_the_objects_prefix_plus_a_sha1() {
    assert!(entry_structurally_valid(
      &format!("objects/{}", "a".repeat(40)),
      None,
      false,
      false,
      CompressionMethod::Stored,
      0,
      30 + 8 + 40 + 20,
      0,
    ));
    assert!(!entry_structurally_valid(
      "not-objects/aaaa",
      None,
      false,
      false,
      CompressionMethod::Stored,
      0,
      0,
      0,
    ));
    assert!(!entry_structurally_valid(
      &format!("objects/{}", "a".repeat(10)),
      None,
      false,
      false,
      CompressionMethod::Stored,
      0,
      0,
      0,
    ));
  }

  #[test]
  fn entries_must_be_strictly_sorted_with_no_repeated_name() {
    let a = format!("objects/{}", "a".repeat(40));
    let b = format!("objects/{}", "b".repeat(40));
    // Strictly greater than the previous entry: fine.
    assert!(entry_structurally_valid(
      &b,
      Some(&a),
      false,
      false,
      CompressionMethod::Stored,
      100,
      100 + local_header_len(&b, 100),
      100,
    ));
    // The same name twice (a duplicate object entry) is rejected...
    assert!(!entry_structurally_valid(
      &a,
      Some(&a),
      false,
      false,
      CompressionMethod::Stored,
      0,
      0,
      0,
    ));
    // ...as is one that is out of order.
    assert!(!entry_structurally_valid(
      &a,
      Some(&b),
      false,
      false,
      CompressionMethod::Stored,
      0,
      0,
      0,
    ));
  }

  #[test]
  fn encryption_a_data_descriptor_or_an_unsupported_method_is_rejected() {
    let name = format!("objects/{}", "a".repeat(40));
    let valid = |encrypted, using_data_descriptor, compression| {
      entry_structurally_valid(
        &name,
        None,
        encrypted,
        using_data_descriptor,
        compression,
        0,
        local_header_len(&name, 0),
        0,
      )
    };
    assert!(valid(false, false, CompressionMethod::Stored));
    assert!(valid(false, false, CompressionMethod::Deflated));
    assert!(!valid(true, false, CompressionMethod::Stored));
    assert!(!valid(false, true, CompressionMethod::Stored));
    assert!(!valid(false, false, CompressionMethod::BZIP2));
  }

  #[test]
  fn verify_pack_rejects_a_pack_whose_whole_sha1_does_not_match() {
    let root = dir();
    let objects = vec![object(&root, "a", b"alpha", false)];
    let dest = root.join("pack.zip");
    build_pack(&objects, &dest).unwrap();
    assert!(verify_pack(&dest, Some(&"0".repeat(40))).is_err());
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
    build_pack(&objects, &dest).unwrap();
    let built = verify_pack(&dest, None).unwrap();

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

    assert!(verify_pack(&dest, Some(&built.sha1)).is_err());
    // A just-built pack is checked without a sha1 to compare against; the
    // entry check alone still catches the change.
    assert!(verify_pack(&dest, None).is_err());
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
    let built = |objects: &[PackObject], dest: PathBuf| {
      build_pack(objects, &dest).unwrap();
      hash_file(&dest).unwrap()
    };
    let first = built(&objects, root.join("one.zip"));
    objects.reverse();
    let second = built(&objects, root.join("two.zip"));
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
    assert_eq!(built(&copies, moved.join("three.zip")), first);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(moved);
  }

  /// Deterministic bytes that do not compress (a fixed xorshift stream).
  fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
      .map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as u8
      })
      .collect()
  }

  /// A fixed sample's pack, pinned to the bytes `build_pack` produced before
  /// the whole-file hash moved into `verify_pack` (7fa2320). Objects span
  /// several copy buffers, both methods, text and incompressible data, and an
  /// empty file. A change here means the platform-side pack hash of an
  /// unchanged instance changed too.
  #[test]
  fn a_fixed_sample_still_packs_to_the_same_bytes() {
    let root = dir();
    let text = b"[general]\nkey = value\n".repeat(40_000);
    let objects = vec![
      object(&root, "text", &text, false),
      object(&root, "noise-deflated", &noise(700_000, 1), false),
      object(&root, "noise-stored", &noise(300_000, 2), true),
      object(&root, "small", b"small", true),
      object(&root, "empty", b"", false),
    ];
    let dest = root.join("pack.zip");
    build_pack(&objects, &dest).unwrap();
    let expected = VerifiedPack {
      sha1: "9f3f8b0b84b9c4e8096f7ec8fb035b1bb3311146".into(),
      size: 1_003_380,
    };
    assert_eq!(verify_pack(&dest, None).unwrap(), expected);
    let (sha1, size) = hash_file(&dest).unwrap();
    assert_eq!(VerifiedPack { sha1, size }, expected);
    let _ = fs::remove_dir_all(root);
  }

  /// Resume after the pack went missing: the same objects come out as the
  /// recorded pack and pass the self-check.
  #[test]
  fn a_rebuilt_pack_that_matches_the_record_is_kept() {
    let root = dir();
    let objects = vec![
      object(&root, "a", b"alpha alpha", false),
      object(&root, "b", &noise(5_000, 3), true),
    ];
    let first = root.join("first.zip");
    build_pack(&objects, &first).unwrap();
    let recorded = verify_pack(&first, None).unwrap();

    let dest = root.join("pack.zip");
    rebuild_pack(&objects, &dest, &recorded.sha1, recorded.size).unwrap();
    assert_eq!(hash_file(&dest).unwrap(), (recorded.sha1, recorded.size));
    let _ = fs::remove_dir_all(root);
  }

  /// A rebuild that does not reproduce the recorded sha1 (or size) fails and
  /// leaves no pack behind for the upload to pick up.
  #[test]
  fn a_rebuilt_pack_that_differs_from_the_record_fails_and_is_removed() {
    let root = dir();
    let objects = vec![object(&root, "a", b"alpha alpha", false)];
    let first = root.join("first.zip");
    build_pack(&objects, &first).unwrap();
    let recorded = verify_pack(&first, None).unwrap();

    let dest = root.join("pack.zip");
    let error = rebuild_pack(&objects, &dest, &"0".repeat(40), recorded.size).unwrap_err();
    assert!(error.0.contains("发生了变化"), "{}", error.0);
    assert!(!dest.exists());

    let error = rebuild_pack(&objects, &dest, &recorded.sha1, recorded.size + 1).unwrap_err();
    assert!(error.0.contains("发生了变化"), "{}", error.0);
    assert!(!dest.exists());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_file_that_changed_after_scanning_fails_the_build() {
    let root = dir();
    let objects = vec![object(&root, "a", b"before", false)];
    fs::write(&objects[0].path, b"after").unwrap();
    assert!(build_pack(&objects, &root.join("p.zip")).is_err());
    let _ = fs::remove_dir_all(root);
  }

  /// Fresh publish: the pack was just built and verified in this process, so
  /// `ensure_pack` may skip the second read-back.
  #[test]
  fn a_pack_marked_verified_stays_verified_while_its_stat_holds() {
    let root = dir();
    let dest = root.join("pack.zip");
    fs::write(&dest, b"pack-bytes").unwrap();
    let meta = fs::metadata(&dest).unwrap();
    mark_verified(&dest, meta.len(), meta.modified().unwrap());
    assert!(is_verified(&dest));
    let _ = fs::remove_dir_all(root);
  }

  /// Resume: a pack from an earlier run is not in this process's registry and
  /// gets the full self-check, as does one whose bytes moved since the mark.
  #[test]
  fn an_unmarked_or_moved_pack_is_verified_again() {
    let root = dir();
    // Never marked: a pack resumed from disk after a restart.
    assert!(!is_verified(&root.join("resumed.zip")));

    let dest = root.join("pack.zip");
    fs::write(&dest, b"pack-bytes").unwrap();
    let meta = fs::metadata(&dest).unwrap();
    mark_verified(&dest, meta.len(), meta.modified().unwrap());
    assert!(is_verified(&dest));

    // Same length, new write: the modification time drops the mark.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&dest, b"PACK-BYTES").unwrap();
    assert!(!is_verified(&dest));

    // A different size drops it too.
    fs::write(&dest, b"longer pack-bytes").unwrap();
    assert!(!is_verified(&dest));
    let _ = fs::remove_dir_all(root);
  }
}
