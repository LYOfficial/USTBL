use super::browser_proxy::LocalBrowserProxy;
use super::commands::{ResourceAccelerationLatency, ResourceAccelerationStatus};
use super::system_proxy::{enable_local_proxy, restore_proxy, SystemProxySnapshot};
use crate::error::{USTBLError, USTBLResult};
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::watch;

const GITHUB_PROGRESS_HOSTS: [&str; 16] = [
  "github.com",
  "api.github.com",
  "codeload.github.com",
  "raw.githubusercontent.com",
  "github.githubassets.com",
  "avatars.githubusercontent.com",
  "objects.githubusercontent.com",
  "cloud.githubusercontent.com",
  "camo.githubusercontent.com",
  "desktop.githubusercontent.com",
  "favicons.githubusercontent.com",
  "github-production-release-asset-2e65be.s3.amazonaws.com",
  "github-production-repository-file-5c1aeb.s3.amazonaws.com",
  "github-production-user-asset-6210df.s3.amazonaws.com",
  "github-com.s3.amazonaws.com",
  "github-cloud.s3.amazonaws.com",
];

pub struct ResourceAccelerationService {
  stop: Option<watch::Sender<bool>>,
  task: Option<tokio::task::JoinHandle<()>>,
  github_enabled: bool,
  started_at: Option<u64>,
  browser_proxy: Option<LocalBrowserProxy>,
  system_proxy: Option<SystemProxySnapshot>,
}

impl Default for ResourceAccelerationService {
  fn default() -> Self {
    Self {
      stop: None,
      task: None,
      github_enabled: false,
      started_at: None,
      browser_proxy: None,
      system_proxy: None,
    }
  }
}

impl ResourceAccelerationService {
  pub fn status(&self) -> ResourceAccelerationStatus {
    ResourceAccelerationStatus {
      running: self.task.is_some(),
      loading: false,
      github_enabled: self.github_enabled,
      hosts_total: 0,
      hosts_completed: 0,
      started_at: self.started_at,
    }
  }

  pub async fn start(
    &mut self,
    _app: &AppHandle,
    github_enabled: bool,
  ) -> USTBLResult<ResourceAccelerationStatus> {
    if !github_enabled {
      return Err(USTBLError("请选择 GitHub 加速项目".into()));
    }
    if self.task.is_some() {
      self.stop().await?;
    }
    if !cfg!(windows) {
      return Err(USTBLError("浏览器代理加速当前只支持 Windows 系统".into()));
    }
    let browser_proxy = LocalBrowserProxy::start()
      .await
      .map_err(|error| USTBLError(format!("启动本地浏览器代理失败: {error}")))?;
    let system_proxy = match enable_local_proxy(
      super::browser_proxy::LOCAL_PROXY_HOST,
      super::browser_proxy::LOCAL_PROXY_PORT,
    ) {
      Ok(snapshot) => snapshot,
      Err(error) => {
        browser_proxy.stop().await;
        return Err(USTBLError(format!("接管 Windows 浏览器代理失败: {error}")));
      }
    };
    let (stop_tx, stop_rx) = watch::channel(false);
    self.stop = Some(stop_tx);
    self.task = Some(tokio::spawn(async move {
      let mut stop = stop_rx;
      let _ = stop.changed().await;
    }));
    self.browser_proxy = Some(browser_proxy);
    self.system_proxy = Some(system_proxy);
    self.github_enabled = github_enabled;
    self.started_at = Some(unix_millis());
    log::info!("Resource acceleration started in browser proxy mode");
    Ok(self.status())
  }

  pub async fn stop(&mut self) -> USTBLResult<ResourceAccelerationStatus> {
    if let Some(proxy) = self.browser_proxy.take() {
      proxy.stop().await;
    }
    if let Some(snapshot) = self.system_proxy.take() {
      restore_proxy(snapshot).map_err(|error| USTBLError(error.to_string()))?;
    }
    if let Some(stop) = self.stop.take() {
      let _ = stop.send(true);
    }
    if let Some(task) = self.task.take() {
      let _ = task.await;
    }
    self.github_enabled = false;
    self.started_at = None;
    log::info!("Resource acceleration stopped and Windows proxy restored");
    Ok(self.status())
  }
}

pub async fn test_latency(github: bool) -> Vec<ResourceAccelerationLatency> {
  let mut hosts = Vec::new();
  if github {
    hosts.extend(GITHUB_PROGRESS_HOSTS);
  }
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(8))
    .build();
  let Ok(client) = client else {
    return hosts
      .into_iter()
      .map(|host| ResourceAccelerationLatency {
        host: host.to_string(),
        latency_ms: None,
        available: false,
      })
      .collect();
  };
  let mut tasks = Vec::with_capacity(hosts.len());
  for host in hosts {
    let client = client.clone();
    tasks.push(tokio::spawn(async move {
      let started = std::time::Instant::now();
      let available = client
        .head(format!("https://{host}/"))
        .send()
        .await
        .map(|_| true)
        .unwrap_or(false);
      ResourceAccelerationLatency {
        host: host.to_string(),
        latency_ms: available.then(|| started.elapsed().as_millis() as u64),
        available,
      }
    }));
  }
  let mut results = Vec::new();
  for task in tasks {
    if let Ok(result) = task.await {
      results.push(result);
    }
  }
  results
}

fn unix_millis() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|duration| duration.as_millis() as u64)
    .unwrap_or_default()
}
