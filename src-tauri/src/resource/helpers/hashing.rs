//! Streaming file hashes used to identify local mod jars on Modrinth and CurseForge.
//!
//! Jars can be hundreds of megabytes, so they are read in fixed-size chunks
//! instead of being loaded into memory, and the async wrappers run the blocking
//! file I/O on the blocking thread pool so it never stalls the async runtime.

use sha1::{Digest, Sha1};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const CHUNK_SIZE: usize = 64 * 1024;

const MURMUR2_M: u32 = 0x5bd1_e995;
const MURMUR2_R: u32 = 24;
const MURMUR2_SEED: u32 = 1;

/// Lowercase hex SHA-1 of everything `reader` yields.
pub fn sha1_of_reader<R: Read>(mut reader: R) -> io::Result<String> {
  let mut hasher = Sha1::new();
  let mut buffer = vec![0u8; CHUNK_SIZE];
  loop {
    let read = reader.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
  }
  Ok(hex::encode(hasher.finalize()))
}

/// Blocking SHA-1 of a file. Prefer [`sha1_file_async`] inside async code.
pub fn sha1_file(path: &Path) -> io::Result<String> {
  sha1_of_reader(File::open(path)?)
}

pub async fn sha1_file_async(path: PathBuf) -> io::Result<String> {
  tokio::task::spawn_blocking(move || sha1_file(&path))
    .await
    .map_err(io::Error::other)?
}

/// CurseForge ignores tab, line feed, carriage return and space when hashing.
fn is_fingerprint_ignored(byte: u8) -> bool {
  matches!(byte, 0x09 | 0x0a | 0x0d | 0x20)
}

/// Incremental MurmurHash2 (32-bit). The input length goes into the initial
/// state, so it has to be known before the first byte is fed.
struct Murmur2 {
  hash: u32,
  tail: [u8; 4],
  tail_len: usize,
}

impl Murmur2 {
  fn new(length: u32) -> Self {
    Self {
      hash: MURMUR2_SEED ^ length,
      tail: [0; 4],
      tail_len: 0,
    }
  }

  fn mix_block(&mut self, block: u32) {
    let mut k = block.wrapping_mul(MURMUR2_M);
    k ^= k >> MURMUR2_R;
    k = k.wrapping_mul(MURMUR2_M);
    self.hash = self.hash.wrapping_mul(MURMUR2_M) ^ k;
  }

  fn update(&mut self, byte: u8) {
    self.tail[self.tail_len] = byte;
    self.tail_len += 1;
    if self.tail_len == 4 {
      self.mix_block(u32::from_le_bytes(self.tail));
      self.tail_len = 0;
    }
  }

  fn finish(mut self) -> u32 {
    if self.tail_len > 0 {
      for (index, byte) in self.tail[..self.tail_len].iter().enumerate() {
        self.hash ^= (*byte as u32) << (8 * index);
      }
      self.hash = self.hash.wrapping_mul(MURMUR2_M);
    }
    self.hash ^= self.hash >> 13;
    self.hash = self.hash.wrapping_mul(MURMUR2_M);
    self.hash ^= self.hash >> 15;
    self.hash
  }
}

/// CurseForge fingerprint (MurmurHash2, seed 1, whitespace bytes removed) of a
/// seekable reader. Two passes: the first counts the bytes that take part in the
/// hash, the second feeds them.
pub fn curseforge_fingerprint_of_reader<R: Read + Seek>(mut reader: R) -> io::Result<u32> {
  let mut buffer = vec![0u8; CHUNK_SIZE];

  let mut length: u64 = 0;
  loop {
    let read = reader.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    length += buffer[..read]
      .iter()
      .filter(|byte| !is_fingerprint_ignored(**byte))
      .count() as u64;
  }
  // The reference implementation hashes a 32-bit length.
  let mut murmur = Murmur2::new(length as u32);
  debug_assert!(length <= u32::MAX as u64);

  reader.seek(SeekFrom::Start(0))?;
  loop {
    let read = reader.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    for byte in &buffer[..read] {
      if !is_fingerprint_ignored(*byte) {
        murmur.update(*byte);
      }
    }
  }
  Ok(murmur.finish())
}

pub fn curseforge_fingerprint_file(path: &Path) -> io::Result<u32> {
  curseforge_fingerprint_of_reader(File::open(path)?)
}

pub async fn curseforge_fingerprint_file_async(path: PathBuf) -> io::Result<u32> {
  tokio::task::spawn_blocking(move || curseforge_fingerprint_file(&path))
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Cursor;

  fn reference_fingerprint(content: &[u8]) -> u32 {
    murmur2::murmur2(
      &content
        .iter()
        .copied()
        .filter(|byte| !is_fingerprint_ignored(*byte))
        .collect::<Vec<u8>>(),
      1,
    )
  }

  fn sample(len: usize) -> Vec<u8> {
    // Deterministic pseudo-random bytes with plenty of whitespace in them.
    let mut state = 0x1234_5678u32;
    (0..len)
      .map(|_| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        match (state >> 24) % 5 {
          0 => b' ',
          1 => b'\n',
          _ => (state >> 16) as u8,
        }
      })
      .collect()
  }

  #[test]
  fn sha1_matches_known_digests() {
    assert_eq!(
      sha1_of_reader(Cursor::new(b"")).unwrap(),
      "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    assert_eq!(
      sha1_of_reader(Cursor::new(b"abc")).unwrap(),
      "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
  }

  #[test]
  fn sha1_is_independent_of_chunk_boundaries() {
    let content = sample(CHUNK_SIZE * 3 + 17);
    let mut hasher = Sha1::new();
    hasher.update(&content);
    assert_eq!(
      sha1_of_reader(Cursor::new(&content)).unwrap(),
      hex::encode(hasher.finalize())
    );
  }

  #[test]
  fn streaming_fingerprint_matches_the_murmur2_crate() {
    for len in [
      0,
      1,
      2,
      3,
      4,
      5,
      7,
      100,
      CHUNK_SIZE - 1,
      CHUNK_SIZE,
      CHUNK_SIZE + 1,
      CHUNK_SIZE * 2 + 3,
    ] {
      let content = sample(len);
      assert_eq!(
        curseforge_fingerprint_of_reader(Cursor::new(&content)).unwrap(),
        reference_fingerprint(&content),
        "length {}",
        len
      );
    }
  }

  #[test]
  fn fingerprint_ignores_whitespace_bytes() {
    assert_eq!(
      curseforge_fingerprint_of_reader(Cursor::new(b"a b\tc\r\nd")).unwrap(),
      curseforge_fingerprint_of_reader(Cursor::new(b"abcd")).unwrap()
    );
  }

  #[tokio::test]
  async fn file_wrappers_hash_on_disk_content() {
    let path = std::env::temp_dir().join(format!("ustbl-hashing-test-{}.bin", std::process::id()));
    let content = sample(CHUNK_SIZE + 5);
    std::fs::write(&path, &content).unwrap();

    let sha1 = sha1_file_async(path.clone()).await.unwrap();
    let fingerprint = curseforge_fingerprint_file_async(path.clone())
      .await
      .unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(sha1, sha1_of_reader(Cursor::new(&content)).unwrap());
    assert_eq!(fingerprint, reference_fingerprint(&content));
    assert!(sha1_file_async(path).await.is_err());
  }
}
