#[cfg(target_os = "macos")]
use crate::error::USTBLError;
use crate::error::USTBLResult;
use crate::launcher_config::models::{LauncherConfig, LauncherConfigError, VersionMetaInfo};
use crate::tasks::commands::schedule_progressive_task_group;
use crate::tasks::download::{DownloadParam, DownloadTransferOptions};
use crate::tasks::PTaskParam;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

const LATEST_RELEASE_URL: &str = "https://www.ustb.world/api/launcher/latest";

#[derive(Deserialize)]
struct LauncherLatestRelease {
  tag: String,
  #[serde(default)]
  body: String,
  #[serde(default)]
  published_at: String,
  #[serde(default)]
  assets: Vec<LauncherReleaseAsset>,
}

#[derive(Deserialize)]
struct LauncherReleaseAsset {
  name: String,
  kind: String,
  downloads: LauncherAssetDownloads,
}

#[derive(Default, Deserialize)]
struct LauncherAssetDownloads {
  #[serde(default)]
  github: String,
  #[serde(default)]
  mirror: String,
}

fn release_version(tag: &str) -> Option<String> {
  let version = tag.trim().trim_start_matches('v');
  semver::Version::parse(version).ok()?;
  Some(version.to_string())
}

fn is_safe_filename(filename: &str) -> bool {
  !filename.is_empty()
    && !filename.contains(['/', '\\'])
    && !filename.contains('\0')
    && std::path::Path::new(filename)
      .file_name()
      .is_some_and(|name| name == filename)
}

fn matches_architecture(filename: &str, arch: &str) -> bool {
  let filename = filename.to_ascii_lowercase();
  let markers = match arch {
    "x86_64" => &["x86_64", "x64", "amd64"][..],
    "aarch64" => &["aarch64", "arm64"][..],
    "x86" => &["i686", "x86"][..],
    other => &[other][..],
  };
  markers.iter().any(|marker| filename.contains(marker))
}

fn matches_platform(filename: &str, os: &str) -> bool {
  let filename = filename.to_ascii_lowercase();
  match os {
    "windows" => filename.ends_with(".exe"),
    "macos" => filename.contains("macos") || filename.contains("darwin"),
    "linux" => filename.contains("linux"),
    other => filename.contains(other),
  }
}

fn select_release_asset<'a>(
  assets: &'a [LauncherReleaseAsset],
  os: &str,
  arch: &str,
  is_portable: bool,
) -> Option<&'a LauncherReleaseAsset> {
  let expected_kind = if is_portable { "portable" } else { "setup" };
  assets.iter().find(|asset| {
    asset.kind.eq_ignore_ascii_case(expected_kind)
      && is_safe_filename(&asset.name)
      && matches_platform(&asset.name, os)
      && matches_architecture(&asset.name, arch)
  })
}

fn parse_https_url(url: &str) -> Option<url::Url> {
  let parsed = url::Url::parse(url).ok()?;
  (parsed.scheme() == "https").then_some(parsed)
}

#[cfg(target_os = "macos")]
fn build_local_new_filename(old_name: &str, old_version: &str, new_version: &str) -> String {
  if let Some(idx) = old_name.find(old_version) {
    let mut filename =
      String::with_capacity(old_name.len() - old_version.len() + new_version.len());
    filename.push_str(&old_name[..idx]);
    filename.push_str(new_version);
    filename.push_str(&old_name[idx + old_version.len()..]);
    filename
  } else {
    old_name.to_string()
  }
}

pub async fn fetch_latest_version(app: &AppHandle) -> USTBLResult<Option<VersionMetaInfo>> {
  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let (os, arch, is_portable) = {
    let config_state = config_binding.lock()?;
    (
      config_state.basic_info.os_type.clone(),
      config_state.basic_info.arch.clone(),
      config_state.basic_info.is_portable,
    )
  };
  let client = app.state::<reqwest::Client>();

  let release = client
    .get(LATEST_RELEASE_URL)
    .header(reqwest::header::ACCEPT, "application/json")
    .send()
    .await
    .map_err(|_| LauncherConfigError::FetchError)?
    .error_for_status()
    .map_err(|_| LauncherConfigError::FetchError)?
    .json::<LauncherLatestRelease>()
    .await
    .map_err(|_| LauncherConfigError::FetchError)?;

  let Some(version) = release_version(&release.tag) else {
    return Ok(None);
  };
  let Some(asset) = select_release_asset(&release.assets, &os, &arch, is_portable) else {
    log::warn!(
      "No compatible launcher update asset for os={}, arch={}, portable={}",
      os,
      arch,
      is_portable
    );
    return Ok(None);
  };

  let Some(download_url) =
    parse_https_url(&asset.downloads.mirror).or_else(|| parse_https_url(&asset.downloads.github))
  else {
    return Ok(None);
  };
  let fallback_download_url = parse_https_url(&asset.downloads.github)
    .filter(|github_url| github_url != &download_url)
    .map(|url| url.to_string())
    .unwrap_or_default();

  Ok(Some(VersionMetaInfo {
    version,
    file_name: asset.name.clone(),
    release_notes: release.body,
    published_at: release.published_at,
    download_url: download_url.to_string(),
    fallback_download_url,
  }))
}

pub async fn download_target_version(app: &AppHandle, version: VersionMetaInfo) -> USTBLResult<()> {
  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let download_cache_dir = {
    let config_state = config_binding.lock()?;
    config_state.download.cache.directory.clone()
  };
  if !is_safe_filename(&version.file_name) {
    return Err(LauncherConfigError::FetchError.into());
  }
  let download_url =
    parse_https_url(&version.download_url).ok_or(LauncherConfigError::FetchError)?;
  let fallback_sources = parse_https_url(&version.fallback_download_url)
    .filter(|fallback_url| fallback_url != &download_url)
    .into_iter()
    .collect();

  schedule_progressive_task_group(
    app.clone(),
    format!("launcher-update?{}", version.version),
    vec![PTaskParam::Download(DownloadParam {
      src: download_url,
      dest: download_cache_dir.join(&version.file_name),
      filename: Some(version.file_name),
      sha1: None,
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(fallback_sources, 2),
    })],
    true,
  )
  .await?;

  Ok(())
}

#[cfg(target_os = "windows")]
pub async fn install_update_windows(
  app: &AppHandle,
  downloaded_filename: String,
  restart: bool,
) -> USTBLResult<()> {
  use std::os::windows::process::CommandExt;

  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let (downloaded_path, is_portable) = {
    let config_state = config_binding.lock()?;
    (
      config_state
        .download
        .cache
        .directory
        .join(&downloaded_filename),
      config_state.basic_info.is_portable,
    )
  };
  if !downloaded_path.is_file() {
    return Err(LauncherConfigError::FetchError.into());
  }
  let cur_exe = std::env::current_exe()?;

  if is_portable {
    // Portable: replace the currently running executable after it exits.
    let pid = std::process::id().to_string();
    let restart_flag = if restart { "1" } else { "0" };

    // write and execute a PowerShell script to wait -> replace -> start -> cleanup
    let script_path = app
      .path()
      .resolve::<PathBuf>("update.ps1".into(), BaseDirectory::AppCache)?;
    let script_content = r#"param(
  [string]$ProcessId,
  [string]$Downloaded,
  [string]$Target,
  [string]$Restart
)

$Backup = "$Target.ustbl-update-backup"

try {
  while (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue) {
    Start-Sleep -Milliseconds 200
  }

  if (Test-Path -LiteralPath $Backup) { Remove-Item -LiteralPath $Backup -Force }
  if (Test-Path -LiteralPath $Target) { Move-Item -LiteralPath $Target -Destination $Backup -Force }
  Move-Item -LiteralPath $Downloaded -Destination $Target -Force
  if (Test-Path -LiteralPath $Backup) { Remove-Item -LiteralPath $Backup -Force }

  if ($Restart -eq '1') {
    Start-Process -FilePath $Target
  }
} catch {
  if (-not (Test-Path -LiteralPath $Target) -and (Test-Path -LiteralPath $Backup)) {
    Move-Item -LiteralPath $Backup -Destination $Target -Force -ErrorAction SilentlyContinue
  }
  Write-Error $_.Exception.Message
  exit 1
}
"#;

    fs::write(&script_path, script_content.as_bytes())?;
    let _ = Command::new("powershell.exe")
      .arg("-NoProfile")
      .arg("-ExecutionPolicy")
      .arg("Bypass")
      .arg("-File")
      .arg(&script_path)
      .arg(&pid)
      .arg(&downloaded_path)
      .arg(&cur_exe)
      .arg(restart_flag)
      .creation_flags(0x08000000)
      .spawn()?;

    if restart {
      app.exit(0);
    }
    Ok(())
  } else {
    // The setup package is an executable installer (NSIS), not an MSI package.
    if restart {
      Command::new(&downloaded_path).spawn()?;
      app.exit(0);
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::{select_release_asset, LauncherAssetDownloads, LauncherReleaseAsset};

  fn asset(name: &str, kind: &str) -> LauncherReleaseAsset {
    LauncherReleaseAsset {
      name: name.to_string(),
      kind: kind.to_string(),
      downloads: LauncherAssetDownloads::default(),
    }
  }

  #[test]
  fn selects_the_matching_windows_package_kind_and_architecture() {
    let assets = vec![
      asset("USTBL_0.5.0_x64-setup.exe", "setup"),
      asset("USTBL-0.5.0_windows_x86_64_portable.exe", "portable"),
      asset("USTBL_0.5.0_arm64-setup.exe", "setup"),
    ];

    assert_eq!(
      select_release_asset(&assets, "windows", "x86_64", false)
        .unwrap()
        .name,
      "USTBL_0.5.0_x64-setup.exe"
    );
    assert_eq!(
      select_release_asset(&assets, "windows", "x86_64", true)
        .unwrap()
        .name,
      "USTBL-0.5.0_windows_x86_64_portable.exe"
    );
    assert_eq!(
      select_release_asset(&assets, "windows", "aarch64", false)
        .unwrap()
        .name,
      "USTBL_0.5.0_arm64-setup.exe"
    );
  }
}

#[cfg(target_os = "macos")]
pub async fn install_update_macos(
  app: &AppHandle,
  downloaded_filename: String,
  restart: bool,
) -> USTBLResult<()> {
  use std::ffi::OsStr;

  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let (old_version, downloaded_path, new_version) = {
    let config_state = config_binding.lock()?;
    (
      config_state.basic_info.launcher_version.clone(),
      config_state
        .download
        .cache
        .directory
        .join(&downloaded_filename),
      downloaded_filename
        .clone()
        .split('_')
        .nth(1)
        .map(|s| s.to_string())
        .unwrap_or_else(|| config_state.basic_info.launcher_version.clone()),
    )
  };
  let cur_exe = std::env::current_exe()?;

  // find app bundle folder by walking up from executable
  let app_bundle = cur_exe
    .ancestors()
    .find(|p| p.extension().and_then(OsStr::to_str) == Some("app"))
    .ok_or_else(|| USTBLError("Not inside .app bundle".to_string()))?
    .to_path_buf();
  let app_dir = app_bundle
    .parent()
    .ok_or_else(|| USTBLError("No parent dir for .app".to_string()))?
    .to_path_buf();
  let old_name = app_bundle
    .file_name()
    .and_then(|s| s.to_str())
    .ok_or_else(|| USTBLError("Invalid .app name".to_string()))?
    .to_string();

  let target_name = build_local_new_filename(&old_name, &old_version, &new_version);
  let target_app = app_dir.join(target_name);
  let pid = std::process::id().to_string();
  let restart_flag = if restart { "1" } else { "0" };

  // write and execute a bash script to wait -> replace -> start -> cleanup
  let script_path = app
    .path()
    .resolve::<PathBuf>("update.sh".to_string().into(), BaseDirectory::AppCache)?;

  let script_content = r#"#!/bin/bash
set -e
PID="$1"
DOWNLOADED="$2"
TARGET_APP="$3"
OLD_APP="$4"
RESTART="$5"

# wait until current process exits
while kill -0 $PID 2>/dev/null; do sleep 0.2; done

TMPDIR="$(mktemp -d)"
tar -xzf "$DOWNLOADED" -C "$TMPDIR"
NEW_APP="$(find "$TMPDIR" -maxdepth 1 -name "*.app" | head -n 1)"
if [ -z "$NEW_APP" ]; then
  echo "No .app found in archive" >&2
  exit 1
fi

rm -rf "$TARGET_APP" || true
rm -rf "$OLD_APP" || true
mv "$NEW_APP" "$TARGET_APP"

if [ "$RESTART" = "1" ]; then
  open -a "$TARGET_APP"
fi

rm -rf "$TMPDIR" || true
"#;

  fs::write(&script_path, script_content.as_bytes())?;
  let _ = Command::new("chmod").arg("+x").arg(&script_path).status();
  let _ = Command::new("bash")
    .arg(&script_path)
    .arg(&pid)
    .arg(&downloaded_path)
    .arg(&target_app)
    .arg(&app_bundle)
    .arg(restart_flag)
    .spawn()?;

  if restart {
    app.exit(0);
  }
  Ok(())
}
