//! The optional files of a modpack version, listed before any instance exists.

use super::optional::{self, OptionalFile};
use super::range_zip::{read_listing, HttpSource, RangeSource};
use super::sources::{download_archive, plain_download};
use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::misc::{
  parse_platform_manifest, parse_platform_manifest_entry, ManifestBox, ModpackDownload,
  PLATFORM_MANIFEST_ENTRIES,
};
use crate::instance::models::misc::InstanceError;
use crate::tasks::download::source_urls;
use std::path::Path;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

/// Read the manifest out of an archive through byte ranges, without
/// downloading it.
pub async fn manifest_by_ranges(source: &dyn RangeSource) -> USTBLResult<ManifestBox> {
  let listing = read_listing(source).await?;
  for name in PLATFORM_MANIFEST_ENTRIES {
    let Some(entry) = listing.find(name) else {
      continue;
    };
    let bytes = listing
      .read_entry_bytes(source, entry, MAX_MANIFEST_BYTES)
      .await?;
    if let Some(manifest) = parse_platform_manifest_entry(name, &bytes) {
      return Ok(manifest);
    }
  }
  Err(InstanceError::ModpackManifestParseError.into())
}

/// The downloads of a manifest that the update engine would track and the pack
/// author marked optional, as the install dialog offers them.
pub fn optional_of_downloads(downloads: &[ModpackDownload], root: &Path) -> Vec<OptionalFile> {
  optional::table(downloads.iter().filter_map(|download| {
    // A download outside the instance is not installed, so there is nothing
    // to choose. One without a hash or size is fetched and installed.
    let relative = download.param.dest.strip_prefix(root).ok()?;
    download.optional.then(|| {
      let path = crate::instance::helpers::modpack::baseline::normalize_relative_path(
        &relative.to_string_lossy(),
      );
      (
        download.select_key.clone(),
        optional::without_disabled_suffix(path),
      )
    })
  }))
}

/// List the optional files of the archive at `url`. The manifest is read by
/// byte ranges when the server allows it, and from a downloaded copy of the
/// archive otherwise. Metadata of the files is only asked of the platform when
/// the manifest marks some as optional.
#[tauri::command]
pub async fn list_modpack_optional_files(
  app: AppHandle,
  url: String,
  sha1: String,
) -> USTBLResult<Vec<OptionalFile>> {
  let manifest = match ranged_manifest(&app, &url).await {
    Ok(manifest) => manifest,
    Err(error) => {
      log::info!("modpack optional files: range read failed, downloading the archive: {error:?}");
      downloaded_manifest(&app, &url, &sha1).await?
    }
  };
  optional_of_manifest(&app, manifest).await
}

/// List the optional files of an archive the user picked from disk.
#[tauri::command]
pub async fn list_local_modpack_optional_files(
  app: AppHandle,
  path: String,
) -> USTBLResult<Vec<OptionalFile>> {
  let manifest = tokio::task::spawn_blocking(move || {
    let file = std::fs::File::open(&path)?;
    parse_platform_manifest(&file)
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))?;
  // A MultiMC pack has no manifest of this kind and no optional files; an
  // archive that is no pack at all is refused by the import itself.
  let Ok(manifest) = manifest else {
    return Ok(Vec::new());
  };
  optional_of_manifest(&app, manifest).await
}

async fn optional_of_manifest(
  app: &AppHandle,
  manifest: ManifestBox,
) -> USTBLResult<Vec<OptionalFile>> {
  if !manifest.has_optional_files() {
    return Ok(Vec::new());
  }
  let root = Path::new("instance");
  let downloads = manifest.get_download_params(app, root).await?;
  Ok(optional_of_downloads(&downloads, root))
}

async fn ranged_manifest(app: &AppHandle, url: &str) -> USTBLResult<ManifestBox> {
  let param = plain_download(url)?;
  let source = HttpSource::new(
    app.state::<reqwest::Client>().inner().clone(),
    source_urls(app, &param),
    None,
  );
  manifest_by_ranges(&source).await
}

async fn downloaded_manifest(app: &AppHandle, url: &str, sha1: &str) -> USTBLResult<ManifestBox> {
  let dir = std::env::temp_dir().join(format!("ustbl-optional-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir_all(&dir)?;
  let archive = dir.join("pack.zip");
  let manifest = async {
    download_archive(app, url, &archive, sha1, &dir).await?;
    let file = std::fs::File::open(&archive)?;
    parse_platform_manifest(&file)
  }
  .await;
  let _ = std::fs::remove_dir_all(&dir);
  manifest.map_err(|error: USTBLError| error)
}

#[cfg(test)]
mod tests {
  use super::super::range_zip::testing::{build_zip, BytesSource, TestEntry};
  use super::*;
  use crate::tasks::download::DownloadParam;
  use std::path::PathBuf;

  const MRPACK: &str = r#"{
    "formatVersion": 1, "game": "minecraft", "versionId": "1", "name": "pack",
    "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.15.0" },
    "files": [
      { "path": "mods/req.jar", "hashes": { "sha1": "a", "sha512": "b" },
        "env": { "client": "required", "server": "required" },
        "downloads": ["https://cdn.modrinth.com/data/AAAA1111/versions/v1/req.jar"], "fileSize": 3 },
      { "path": "mods/opt.jar", "hashes": { "sha1": "a", "sha512": "b" },
        "env": { "client": "optional", "server": "required" },
        "downloads": ["https://cdn.modrinth.com/data/BBBB2222/versions/v1/opt.jar"], "fileSize": 3 }
    ]
  }"#;

  #[tokio::test]
  async fn the_manifest_is_read_by_ranges_and_lists_the_optional_files() {
    let zip = build_zip(
      &[
        TestEntry::new("overrides/config/a.toml", b"x"),
        TestEntry::new("modrinth.index.json", MRPACK.as_bytes()),
      ],
      b"",
      false,
    );
    let source = BytesSource::new(zip);
    let manifest = manifest_by_ranges(&source).await.unwrap();
    assert!(manifest.has_optional_files());
  }

  #[tokio::test]
  async fn an_archive_without_a_manifest_is_refused() {
    let zip = build_zip(&[TestEntry::new("readme.txt", b"x")], b"", false);
    assert!(manifest_by_ranges(&BytesSource::new(zip)).await.is_err());
  }

  #[test]
  fn downloads_without_a_hash_or_size_are_offered_and_outside_ones_are_not() {
    let root = Path::new("instance");
    let make = |dest: PathBuf, sha1: Option<&str>, size: Option<u64>| ModpackDownload {
      param: DownloadParam {
        src: url::Url::parse("https://example.test/a.jar").unwrap(),
        dest,
        sha1: sha1.map(String::from),
        filename: None,
        custom_headers: None,
        transfer_options: Default::default(),
      },
      size,
      optional: true,
      select_key: "curseforge:1".into(),
    };
    let inside = || root.join("mods/a.jar");
    for (sha1, size) in [(Some("a"), Some(1)), (None, Some(1)), (Some("a"), None)] {
      assert_eq!(
        optional_of_downloads(&[make(inside(), sha1, size)], root).len(),
        1
      );
    }
    let outside = make(PathBuf::from("elsewhere/a.jar"), Some("a"), Some(1));
    assert!(optional_of_downloads(&[outside], root).is_empty());
  }
}
