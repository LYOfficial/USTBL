//! The update preview of a platform pack read by byte ranges: when it is tried,
//! how the archive listing is cached, and the hand-over to the plan. Anything
//! that goes wrong is logged and answered with `None`, so the caller takes the
//! full download.

use super::preview::{preview_remote, run_preview, PreviewRun};
use super::range_zip::{read_listing, HttpSource, RangeSource, ZipListing};
use super::sources::{
  archive_facts, downloaded_archive, matching_remote_cache, plain_download, remote_dir,
  LatestVersion,
};
use crate::error::USTBLResult;
use crate::instance::helpers::modpack::baseline::ModpackBaseline;
use crate::instance::helpers::modpack::misc::{
  parse_platform_manifest_entry, ManifestBox, PLATFORM_MANIFEST_ENTRIES,
};
use crate::instance::models::misc::{InstanceError, InstanceOrigin, OriginPlatform};
use crate::tasks::download::source_urls;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

const LISTING_SCHEMA: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

/// What the archive's central directory and manifest said, kept per version
/// next to (and apart from) the version's `RemoteCache`. It is the raw listing,
/// not the keys the mapping derives from it: those depend on the instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListingCache {
  schema: u32,
  platform: OriginPlatform,
  version_id: String,
  archive_sha1: String,
  listing: ZipListing,
  manifest_entry: String,
  manifest: String,
}

fn listing_cache_path(root: &Path, version_id: &str) -> PathBuf {
  remote_dir(root).join(format!("{version_id}.listing.json"))
}

fn load_listing_cache(
  root: &Path,
  platform: OriginPlatform,
  version_id: &str,
  archive_sha1: &str,
) -> Option<ListingCache> {
  let bytes = fs::read(listing_cache_path(root, version_id)).ok()?;
  let cache: ListingCache = serde_json::from_slice(&bytes).ok()?;
  (cache.schema == LISTING_SCHEMA
    && cache.platform == platform
    && cache.version_id == version_id
    && cache.archive_sha1 == archive_sha1)
    .then_some(cache)
}

fn write_listing_cache(root: &Path, cache: &ListingCache) -> std::io::Result<()> {
  let path = listing_cache_path(root, &cache.version_id);
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let temp = path.with_extension("json.tmp");
  fs::write(&temp, serde_json::to_vec(cache)?)?;
  fs::rename(temp, path)
}

/// Whether the preview of `latest` is read by ranges, and from where: the URL
/// and SHA-1 of its archive. Not for a fresh instance, a campus pack, or a
/// version whose full remote or archive is already on this machine (those are
/// exact and cost no download).
pub fn range_target(
  origin: &InstanceOrigin,
  root: &Path,
  latest: &LatestVersion,
  fresh: bool,
) -> Option<(String, String)> {
  if fresh
    || matches!(
      origin.platform,
      OriginPlatform::Campus | OriginPlatform::Local
    )
  {
    return None;
  }
  let (sha1, url) = latest.archive_ref()?;
  if matching_remote_cache(origin, root, latest).is_some()
    || downloaded_archive(root, latest).is_some()
  {
    return None;
  }
  Some((url.to_string(), sha1.to_string()))
}

/// The listing and the manifest of an archive, from the cache when it has them.
pub async fn listing_and_manifest(
  source: &dyn RangeSource,
  root: &Path,
  platform: OriginPlatform,
  version_id: &str,
  archive_sha1: &str,
  cached: Option<ListingCache>,
) -> USTBLResult<(ZipListing, ManifestBox)> {
  if let Some(cache) = cached {
    if let Some(manifest) =
      parse_platform_manifest_entry(&cache.manifest_entry, cache.manifest.as_bytes())
    {
      log::info!(
        "modpack preview range: listing of {} entries from the cache",
        cache.listing.entries.len()
      );
      return Ok((cache.listing, manifest));
    }
  }
  let listing = read_listing(source).await?;
  for name in PLATFORM_MANIFEST_ENTRIES {
    let Some(entry) = listing.find(name) else {
      continue;
    };
    let bytes = listing
      .read_entry_bytes(source, entry, MAX_MANIFEST_BYTES)
      .await?;
    let Some(manifest) = parse_platform_manifest_entry(name, &bytes) else {
      continue;
    };
    if let Ok(text) = String::from_utf8(bytes) {
      let cache = ListingCache {
        schema: LISTING_SCHEMA,
        platform,
        version_id: version_id.to_string(),
        archive_sha1: archive_sha1.to_string(),
        listing: listing.clone(),
        manifest_entry: name.to_string(),
        manifest: text,
      };
      if let Err(error) = write_listing_cache(root, &cache) {
        log::warn!("cannot write modpack listing cache: {error:?}");
      }
    }
    return Ok((listing, manifest));
  }
  Err(InstanceError::ModpackManifestParseError.into())
}

/// Plan the update of a platform pack without downloading its archive.
/// `None` means the preview is not available or failed; the caller then
/// downloads the archive and plans from it.
pub async fn plan_by_ranges(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  instance_name: &str,
  baseline: &ModpackBaseline,
  latest: &LatestVersion,
  fresh: bool,
) -> Option<PreviewRun> {
  let (url, sha1) = range_target(origin, root, latest, fresh)?;
  let started = Instant::now();
  match attempt(
    app,
    origin,
    root,
    instance_name,
    baseline,
    latest,
    &url,
    &sha1,
  )
  .await
  {
    Ok(run) => {
      log::info!(
        "modpack preview range: planned version={} plan_hash={} elapsed_ms={}",
        latest.version_id,
        run.prepared.plan_hash,
        started.elapsed().as_millis()
      );
      Some(run)
    }
    Err(error) => {
      log::warn!(
        "modpack preview range: failed, falling back to the full download: {error:?} elapsed_ms={}",
        started.elapsed().as_millis()
      );
      None
    }
  }
}

#[allow(clippy::too_many_arguments)]
async fn attempt(
  app: &AppHandle,
  origin: &InstanceOrigin,
  root: &Path,
  instance_name: &str,
  baseline: &ModpackBaseline,
  latest: &LatestVersion,
  url: &str,
  sha1: &str,
) -> USTBLResult<PreviewRun> {
  let param = plain_download(url)?;
  let cached = load_listing_cache(root, origin.platform, &latest.version_id, sha1);
  let source = HttpSource::new(
    app.state::<reqwest::Client>().inner().clone(),
    source_urls(app, &param),
    cached.as_ref().map(|cache| cache.listing.total_len),
  );
  let (listing, manifest) = listing_and_manifest(
    &source,
    root,
    origin.platform,
    &latest.version_id,
    sha1,
    cached,
  )
  .await?;
  let facts = archive_facts(app, &manifest, root, origin).await?;
  let remote = preview_remote(
    facts,
    latest,
    &listing,
    &source,
    root,
    instance_name,
    baseline,
  )
  .await?;
  let run = run_preview(root, baseline, remote, &listing, &source, false).await?;
  log::info!(
    "modpack preview range: served by {}",
    source
      .pinned()
      .and_then(|url| url.host_str().map(str::to_string))
      .unwrap_or_else(|| "the cached listing only".into())
  );
  Ok(run)
}

#[cfg(test)]
mod tests {
  use super::super::range_zip::testing::{build_zip, BytesSource, TestEntry};
  use super::super::remote::{FileSource, RemoteFile, RemoteModpack};
  use super::super::sources::{assemble_remote, ArchiveFacts, Payload, RemoteCache};
  use super::*;
  use crate::instance::helpers::modpack::baseline::BaselineLoader;
  use crate::instance::models::misc::ModLoaderType;

  const MANIFEST: &str = r#"{"formatVersion":1,"game":"minecraft","versionId":"2","name":"p","files":[],"dependencies":{"minecraft":"1.21.1","fabric-loader":"0.16.0"}}"#;

  fn origin(platform: OriginPlatform) -> InstanceOrigin {
    InstanceOrigin {
      platform,
      project_id: "p".into(),
      version_id: "1".into(),
      game_version: "1.21.1".into(),
      loader_type: ModLoaderType::Fabric,
      follow: true,
    }
  }

  fn latest(payload: Option<Payload>) -> LatestVersion {
    LatestVersion {
      version_id: "2".into(),
      label: "2.0".into(),
      payload,
    }
  }

  fn archive_payload() -> Option<Payload> {
    Some(Payload::Archive {
      url: "https://cdn.example/pack.mrpack".into(),
      sha1: "ab".repeat(20),
    })
  }

  fn instance_dir() -> PathBuf {
    let root = std::env::temp_dir().join(format!("ustbl-fetch-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
  }

  fn pack() -> Vec<u8> {
    build_zip(
      &[
        TestEntry::new("modrinth.index.json", MANIFEST.as_bytes()),
        TestEntry::new("overrides/config/a.toml", b"a = 1"),
      ],
      b"",
      false,
    )
  }

  #[test]
  fn the_manifest_used_by_these_tests_parses() {
    assert!(parse_platform_manifest_entry("modrinth.index.json", MANIFEST.as_bytes()).is_some());
  }

  #[tokio::test]
  async fn the_listing_is_read_once_and_then_served_from_the_cache() {
    let root = instance_dir();
    let source = BytesSource::new(pack());
    let sha1 = "ab".repeat(20);

    let cached = load_listing_cache(&root, OriginPlatform::Modrinth, "2", &sha1);
    assert!(cached.is_none());
    let (listing, _) =
      listing_and_manifest(&source, &root, OriginPlatform::Modrinth, "2", &sha1, cached)
        .await
        .unwrap();
    assert!(source.requests() >= 1);
    assert!(listing.find("overrides/config/a.toml").is_some());

    let cached = load_listing_cache(&root, OriginPlatform::Modrinth, "2", &sha1);
    assert!(cached.is_some());
    let silent = BytesSource::new(Vec::new());
    let (again, _) =
      listing_and_manifest(&silent, &root, OriginPlatform::Modrinth, "2", &sha1, cached)
        .await
        .unwrap();
    assert_eq!(silent.requests(), 0);
    assert_eq!(again, listing);
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn a_listing_of_another_version_platform_or_archive_is_not_used() {
    let root = instance_dir();
    let sha1 = "ab".repeat(20);
    listing_and_manifest(
      &BytesSource::new(pack()),
      &root,
      OriginPlatform::Modrinth,
      "2",
      &sha1,
      None,
    )
    .await
    .unwrap();
    assert!(load_listing_cache(&root, OriginPlatform::Modrinth, "2", &sha1).is_some());
    assert!(load_listing_cache(&root, OriginPlatform::CurseForge, "2", &sha1).is_none());
    assert!(load_listing_cache(&root, OriginPlatform::Modrinth, "3", &sha1).is_none());
    assert!(load_listing_cache(&root, OriginPlatform::Modrinth, "2", &"cd".repeat(20)).is_none());
    // A damaged cache is a miss, not an error.
    fs::write(listing_cache_path(&root, "2"), b"{").unwrap();
    assert!(load_listing_cache(&root, OriginPlatform::Modrinth, "2", &sha1).is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[tokio::test]
  async fn an_archive_without_a_manifest_is_refused() {
    let root = instance_dir();
    let bytes = build_zip(&[TestEntry::new("overrides/a.txt", b"a")], b"", false);
    let result = listing_and_manifest(
      &BytesSource::new(bytes),
      &root,
      OriginPlatform::Modrinth,
      "2",
      "ab",
      None,
    )
    .await;
    assert!(result.is_err());
    assert!(!listing_cache_path(&root, "2").exists());
    let _ = fs::remove_dir_all(root);
  }

  fn empty_remote(latest: &LatestVersion, files: Vec<RemoteFile>) -> RemoteModpack {
    let facts = ArchiveFacts {
      game_version: "1.21.1".into(),
      loader: BaselineLoader {
        loader_type: ModLoaderType::Fabric,
        version: "0.16.0".into(),
      },
      overrides_paths: vec!["overrides".into()],
      recommended_memory_mb: None,
      downloads: Vec::new(),
    };
    assemble_remote(latest, facts, files)
  }

  fn remote_cache_for(origin: &InstanceOrigin, root: &Path, latest: &LatestVersion) {
    let remote = empty_remote(latest, Vec::new());
    let (sha1, _) = latest.archive_ref().unwrap();
    let cache = RemoteCache::new(origin, root, &remote, Some((sha1, 3)));
    super::super::sources::write_remote_cache(root, &cache).unwrap();
  }

  #[test]
  fn ranges_are_tried_only_when_nothing_exact_is_at_hand() {
    let root = instance_dir();
    let origin = origin(OriginPlatform::Modrinth);
    let latest = latest(archive_payload());
    let tried = |origin: &InstanceOrigin, latest: &LatestVersion, fresh: bool| {
      range_target(origin, &root, latest, fresh)
    };

    let (url, sha1) = tried(&origin, &latest, false).unwrap();
    assert_eq!(url, "https://cdn.example/pack.mrpack");
    assert_eq!(sha1, "ab".repeat(20));
    // First installs and campus packs keep their paths.
    assert!(tried(&origin, &latest, true).is_none());
    assert!(tried(&self::origin(OriginPlatform::Campus), &latest, false).is_none());
    assert!(tried(&origin, &self::latest(None), false).is_none());

    // The archive of the version is on disk already, as after a confirmed
    // update that went stale: the exact path is cheaper and is the one taken.
    let archive = super::super::sources::archive_path(&root, &sha1);
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    fs::write(&archive, b"zip").unwrap();
    assert!(tried(&origin, &latest, false).is_none());
    fs::remove_file(&archive).unwrap();
    assert!(tried(&origin, &latest, false).is_some());

    // So is a full remote cache.
    fs::write(&archive, b"zip").unwrap();
    remote_cache_for(&origin, &root, &latest);
    assert!(tried(&origin, &latest, false).is_none());
    let _ = fs::remove_dir_all(root);
  }

  #[test]
  fn a_cache_of_a_preview_is_not_a_hit() {
    let root = instance_dir();
    let origin = origin(OriginPlatform::Modrinth);
    let latest = latest(archive_payload());
    let (sha1, _) = latest.archive_ref().unwrap();
    let archive = super::super::sources::archive_path(&root, sha1);
    fs::create_dir_all(archive.parent().unwrap()).unwrap();
    fs::write(&archive, b"zip").unwrap();
    let listed = RemoteFile {
      path: "config/a.toml".into(),
      key: super::super::diff::FileKey {
        sha1: "ab".repeat(20),
        size: 3,
      },
      source: FileSource::Listed {
        entry: "overrides/config/a.toml".into(),
        exact: false,
      },
      disabled: false,
      once: false,
    };
    let remote = empty_remote(&latest, vec![listed]);
    let cache = RemoteCache::new(&origin, &root, &remote, Some((sha1, 3)));
    super::super::sources::write_remote_cache(&root, &cache).unwrap();
    assert!(matching_remote_cache(&origin, &root, &latest).is_none());
    let _ = fs::remove_dir_all(root);
  }
}
