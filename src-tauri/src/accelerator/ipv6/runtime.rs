use crate::error::{USTBLError, USTBLResult};
use log::{info, warn};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub const VERSION: &str = "v1.19.31";
const MAX_DOWNLOAD: usize = 150 * 1024 * 1024;
const MIRROR_BASES: [&str; 4] = [
  "https://gh-proxy.com/",
  "https://ghproxy.net/",
  "https://ghfast.top/",
  "https://gh-proxy.net/",
];

pub fn artifact() -> USTBLResult<(&'static str, &'static str)> {
  match std::env::consts::ARCH {
    "x86_64" => Ok((
      "mihomo-windows-amd64-compatible-v1.19.31.zip",
      "93d14e9a13b49b2f2d256202d02cc8d14a7c4695edf084cae0f941986bc9c218",
    )),
    "aarch64" => Ok((
      "mihomo-windows-arm64-v1.19.31.zip",
      "68659624a38ae1dbc4e1b92f45d4dc75da0eaedd4d732b0a6f01a308da945792",
    )),
    _ => Err(USTBLError("IPv6 免流核心只支持 Windows x64 / ARM64".into())),
  }
}

pub fn binary(root: &Path) -> PathBuf {
  root.join(format!("mihomo-{VERSION}.exe"))
}

pub fn secure_directory(root: &Path) -> USTBLResult<()> {
  std::fs::create_dir_all(root)?;
  #[cfg(windows)]
  {
    let literal = root.to_string_lossy().replace('\'', "''");
    let script = format!("$ErrorActionPreference='Stop'; $path='{literal}'; $acl=[System.IO.Directory]::GetAccessControl($path); $acl.SetAccessRuleProtection($true,$false); $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; $rule=[System.Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $acl.SetAccessRule($rule); [System.IO.Directory]::SetAccessControl($path,$acl)");
    let output = command("powershell.exe")
      .args(["-NoProfile", "-NonInteractive", "-Command", &script])
      .output()?;
    if !output.status.success() {
      return Err(USTBLError(format!(
        "无法保护 IPv6 免流凭据目录: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      )));
    }
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

pub async fn install(root: &Path) -> USTBLResult<()> {
  let (name, expected) = artifact()?;
  let client = reqwest::Client::builder()
    .no_gzip()
    .no_proxy()
    .connect_timeout(Duration::from_secs(15))
    .timeout(Duration::from_secs(60))
    .build()?;
  let official_url =
    format!("https://github.com/MetaCubeX/mihomo/releases/download/{VERSION}/{name}");
  let mut candidates = Vec::with_capacity(MIRROR_BASES.len() + 1);
  candidates.push(("GitHub 官方源".to_string(), official_url.clone()));
  candidates.extend(
    MIRROR_BASES
      .iter()
      .map(|base| ((*base).to_string(), format!("{base}{official_url}"))),
  );
  let mut failures = Vec::new();
  let mut bytes = None;
  for (label, url) in candidates {
    info!("尝试下载 IPv6 免流核心: {label} ({url})");
    match download_archive(&client, &url).await {
      Ok(data) => match verify_archive(&data, expected) {
        Ok(()) => {
          info!("IPv6 免流核心下载成功: {label}");
          bytes = Some(data);
          break;
        }
        Err(error) => {
          warn!("IPv6 免流核心校验失败: {label}: {}", error.0);
          failures.push(format!("{label}: {}", error.0));
        }
      },
      Err(error) => {
        warn!("IPv6 免流核心下载失败: {label}: {}", error.0);
        failures.push(format!("{label}: {}", error.0));
      }
    }
  }
  let bytes = bytes.ok_or_else(|| {
    USTBLError(format!(
      "IPv6 免流核心下载失败，已尝试官方源和 gh-proxy 镜像: {}",
      failures.join("；")
    ))
  })?;
  let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
  let mut executable = None;
  for i in 0..archive.len() {
    let mut file = archive.by_index(i)?;
    if file.name().ends_with(".exe") && !file.is_dir() {
      if executable.is_some() || file.size() > MAX_DOWNLOAD as u64 {
        return Err(USTBLError("免流核心压缩包结构无效".into()));
      }
      let mut data = Vec::new();
      (&mut file)
        .take(MAX_DOWNLOAD as u64 + 1)
        .read_to_end(&mut data)?;
      if data.len() > MAX_DOWNLOAD || !data.starts_with(b"MZ") {
        return Err(USTBLError("免流核心文件无效".into()));
      }
      executable = Some(data);
    }
  }
  let data = executable.ok_or_else(|| USTBLError("免流核心压缩包缺少可执行文件".into()))?;
  let temporary = root.join("mihomo.download");
  tokio::fs::write(&temporary, &data).await?;
  let target = binary(root);
  if target.exists() {
    tokio::fs::remove_file(&target).await?;
  }
  tokio::fs::rename(&temporary, &target).await?;
  tokio::fs::write(
    root.join("mihomo.sha256"),
    hex::encode(Sha256::digest(data)),
  )
  .await?;
  tokio::fs::write(
    root.join("FreeV6-MIT.txt"),
    include_str!("../../../../docs/licenses/free-v6-MIT.txt"),
  )
  .await?;
  tokio::fs::write(
    root.join("mihomo-GPL-3.0.txt"),
    include_str!("../../../../LICENSE"),
  )
  .await?;
  Ok(())
}

async fn download_archive(client: &reqwest::Client, url: &str) -> USTBLResult<Vec<u8>> {
  let mut response = client
    .get(url)
    .send()
    .await
    .map_err(|error| USTBLError(format!("请求失败: {error}")))?
    .error_for_status()
    .map_err(|error| USTBLError(format!("HTTP 请求失败: {error}")))?;
  let mut bytes = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|error| USTBLError(format!("读取响应失败: {error}")))?
  {
    if bytes.len() + chunk.len() > MAX_DOWNLOAD {
      return Err(USTBLError("免流核心下载超过大小限制".into()));
    }
    bytes.extend_from_slice(&chunk);
  }
  Ok(bytes)
}

fn verify_archive(data: &[u8], expected: &str) -> USTBLResult<()> {
  if hex::encode(Sha256::digest(data)) != expected {
    return Err(USTBLError("免流核心 SHA-256 校验失败，已拒绝安装".into()));
  }
  Ok(())
}

pub async fn verify_binary(root: &Path) -> USTBLResult<()> {
  let data = tokio::fs::read(binary(root)).await?;
  let expected = tokio::fs::read_to_string(root.join("mihomo.sha256")).await?;
  verify_archive(&data, expected.trim())
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
  let mut command = Command::new(program);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
  }
  command
}

pub fn is_elevated() -> bool {
  #[cfg(windows)]
  unsafe {
    use winapi::um::{
      handleapi::CloseHandle,
      processthreadsapi::{GetCurrentProcess, OpenProcessToken},
      securitybaseapi::GetTokenInformation,
      winnt::{TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    };
    let mut token = std::ptr::null_mut();
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
      return false;
    }
    let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
    let mut size = 0;
    let ok = GetTokenInformation(
      token,
      TokenElevation,
      &mut elevation as *mut _ as *mut _,
      std::mem::size_of::<TOKEN_ELEVATION>() as u32,
      &mut size,
    );
    CloseHandle(token);
    ok != 0 && elevation.TokenIsElevated != 0
  }
  #[cfg(not(windows))]
  {
    false
  }
}

pub struct CoreProcess {
  pub child: Child,
  #[cfg(windows)]
  job: usize,
}

impl CoreProcess {
  pub fn spawn(root: &Path) -> USTBLResult<Self> {
    let log = std::fs::File::create(root.join("core.log"))?;
    let child = command(binary(root))
      .arg("-d")
      .arg(root)
      .arg("-f")
      .arg(root.join("config.json"))
      .stdin(Stdio::null())
      .stdout(log.try_clone()?)
      .stderr(log)
      .spawn()?;
    #[cfg(windows)]
    {
      let mut child = child;
      let job = match attach_job(&child) {
        Ok(job) => job,
        Err(error) => {
          let _ = child.kill();
          let _ = child.wait();
          return Err(error.into());
        }
      };
      Ok(Self { child, job })
    }
    #[cfg(not(windows))]
    {
      Ok(Self { child })
    }
  }
}

#[cfg(windows)]
fn attach_job(child: &Child) -> std::io::Result<usize> {
  use std::os::windows::io::AsRawHandle;
  use winapi::um::{
    handleapi::CloseHandle,
    jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
    winnt::{
      JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
      JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
  };
  unsafe {
    let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
    if job.is_null() {
      return Err(std::io::Error::last_os_error());
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if SetInformationJobObject(
      job,
      JobObjectExtendedLimitInformation,
      &mut limits as *mut _ as *mut _,
      std::mem::size_of_val(&limits) as u32,
    ) == 0
      || AssignProcessToJobObject(job, child.as_raw_handle() as _) == 0
    {
      let error = std::io::Error::last_os_error();
      CloseHandle(job);
      return Err(error);
    }
    Ok(job as usize)
  }
}

impl Drop for CoreProcess {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
    #[cfg(windows)]
    unsafe {
      winapi::um::handleapi::CloseHandle(self.job as _);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mirror_candidates_keep_official_source_first() {
    let official = "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.31/core.zip";
    let mut candidates = vec![official.to_string()];
    candidates.extend(MIRROR_BASES.iter().map(|base| format!("{base}{official}")));
    assert_eq!(candidates[0], official);
    assert!(candidates
      .iter()
      .any(|url| url.starts_with("https://ghproxy.net/")));
    assert!(candidates
      .iter()
      .any(|url| url.starts_with("https://ghfast.top/")));
  }

  #[test]
  fn checksum_rejects_modified_download() {
    let digest = hex::encode(Sha256::digest(b"original"));
    assert!(verify_archive(b"original", &digest).is_ok());
    assert!(verify_archive(b"modified", &digest).is_err());
  }
}
