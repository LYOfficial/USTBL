mod config;
mod runtime;
mod warp;

use crate::error::{USTBLError, USTBLResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6Settings {
  pub bypass: Vec<String>,
}

impl Default for Ipv6Settings {
  fn default() -> Self {
    Self {
      bypass: vec!["ustb.edu.cn".into(), "202.204.0.0/16".into()],
    }
  }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6Status {
  pub supported: bool,
  pub elevated: bool,
  pub installed: bool,
  pub running: bool,
  pub started_at: Option<u64>,
  pub error: Option<String>,
  pub settings: Ipv6Settings,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ipv6Node {
  pub name: String,
  pub latency_ms: Option<u64>,
  pub selected: bool,
}

#[derive(Default)]
pub struct Ipv6Service {
  process: Option<runtime::CoreProcess>,
  controller: u16,
  secret: String,
  started_at: Option<u64>,
  error: Option<String>,
}

fn root(app: &AppHandle) -> USTBLResult<PathBuf> {
  Ok(app.path().app_local_data_dir()?.join("ipv6"))
}

fn read_settings(root: &Path) -> USTBLResult<Ipv6Settings> {
  match std::fs::read(root.join("settings.json")) {
    Ok(data) => Ok(serde_json::from_slice(&data)?),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ipv6Settings::default()),
    Err(error) => Err(error.into()),
  }
}

impl Ipv6Service {
  fn status(&mut self, root: &Path) -> USTBLResult<Ipv6Status> {
    if let Some(process) = &mut self.process {
      if let Some(exit) = process.child.try_wait()? {
        self.error = Some(format!(
          "IPv6 免流核心已退出 ({exit})，请查看 {}",
          root.join("core.log").display()
        ));
        self.process = None;
        self.started_at = None;
        let _ = std::fs::remove_file(root.join("config.json"));
      }
    }
    Ok(Ipv6Status {
      supported: cfg!(windows) && matches!(std::env::consts::ARCH, "x86_64" | "aarch64"),
      elevated: runtime::is_elevated(),
      installed: runtime::binary(root).exists(),
      running: self.process.is_some(),
      started_at: self.started_at,
      error: self.error.clone(),
      settings: read_settings(root)?,
    })
  }

  async fn api(
    &self,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
  ) -> USTBLResult<Value> {
    controller_request(self.controller, &self.secret, method, path, body).await
  }
}

async fn controller_request(
  controller: u16,
  secret: &str,
  method: reqwest::Method,
  path: &str,
  body: Option<Value>,
) -> USTBLResult<Value> {
  let client = reqwest::Client::builder()
    .no_proxy()
    .timeout(Duration::from_secs(8))
    .build()?;
  let mut request = client
    .request(method, format!("http://127.0.0.1:{controller}{path}"))
    .bearer_auth(secret);
  if let Some(body) = body {
    request = request.json(&body);
  }
  let response = request.send().await?.error_for_status()?;
  if response.status() == reqwest::StatusCode::NO_CONTENT {
    return Ok(Value::Null);
  }
  Ok(response.json().await?)
}

impl Ipv6Service {
  pub async fn stop(&mut self, root: &Path) -> USTBLResult<()> {
    if self.process.is_some() {
      let _ = self
        .api(
          reqwest::Method::PATCH,
          "/configs",
          Some(json!({"tun": {"enable": false}})),
        )
        .await;
    }
    self.process = None;
    self.started_at = None;
    self.secret.clear();
    match tokio::fs::remove_file(root.join("config.json")).await {
      Ok(()) => (),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
      Err(error) => return Err(error.into()),
    }
    Ok(())
  }

  async fn start(&mut self, root: &Path, settings: Ipv6Settings) -> USTBLResult<()> {
    if !cfg!(windows) {
      return Err(USTBLError("IPv6 免流当前仅支持 Windows".into()));
    }
    if !runtime::is_elevated() {
      return Err(USTBLError(
        "TUN 需要管理员权限，请退出后以管理员身份运行 USTBL".into(),
      ));
    }
    self.status(root)?;
    if self.process.is_some() {
      return Err(USTBLError("IPv6 免流已在运行".into()));
    }
    runtime::verify_binary(root).await?;
    let bypass = config::normalize_bypass(&settings.bypass)?;
    let probe = tokio::net::UdpSocket::bind("[::]:0").await?;
    probe.connect("[2606:4700:103::1]:443").await.map_err(|_| {
      USTBLError("未检测到可用 IPv6 路由，请先认证校园网并连接 USTB-Student 或 USTB-V6".into())
    })?;
    let public_ipv6 = match probe.local_addr()?.ip() {
      std::net::IpAddr::V6(address) => address.segments()[0] & 0xe000 == 0x2000,
      _ => false,
    };
    if !public_ipv6 {
      return Err(USTBLError("未检测到可用 IPv6 地址".into()));
    }
    drop(probe);
    let device_path = root.join("device.json");
    let device = match tokio::fs::read(&device_path).await {
      Ok(data) => serde_json::from_slice::<warp::WarpDevice>(&data)?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
        let device = warp::register().await?;
        tokio::fs::write(&device_path, serde_json::to_vec(&device)?).await?;
        device
      }
      Err(error) => return Err(error.into()),
    };
    let controller = std::net::TcpListener::bind("127.0.0.1:0")?;
    let mixed = std::net::TcpListener::bind("127.0.0.1:0")?;
    self.controller = controller.local_addr()?.port();
    self.secret = uuid::Uuid::new_v4().simple().to_string();
    let configuration = config::render(
      &device,
      &bypass,
      self.controller,
      mixed.local_addr()?.port(),
      &self.secret,
      true,
    )?;
    tokio::fs::write(
      root.join("settings.json"),
      serde_json::to_vec(&Ipv6Settings { bypass })?,
    )
    .await?;
    tokio::fs::write(
      root.join("config.json"),
      serde_json::to_vec_pretty(&configuration)?,
    )
    .await?;
    drop(controller);
    drop(mixed);
    self.process = match runtime::CoreProcess::spawn(root) {
      Ok(process) => Some(process),
      Err(error) => {
        self.stop(root).await?;
        return Err(error);
      }
    };
    let ready = tokio::time::timeout(Duration::from_secs(20), async {
      loop {
        if !self.status(root)?.running {
          return Err(USTBLError(self.error.clone().unwrap_or_default()));
        }
        if let Ok(value) = self.api(reqwest::Method::GET, "/configs", None).await {
          if value["tun"]["enable"].as_bool() == Some(true) {
            return Ok(());
          }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
      }
    })
    .await;
    match ready {
      Ok(Ok(())) => (),
      other => {
        let error = match other {
          Ok(Err(error)) => error,
          _ => USTBLError("免流核心启动超时，请检查 core.log".into()),
        };
        self.stop(root).await?;
        return Err(error);
      }
    }
    self.error = None;
    let port = configuration["mixed-port"].as_u64().unwrap_or_default();
    let connected = async {
      let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?)
        .timeout(Duration::from_secs(8))
        .build()?;
      for _ in 0..3 {
        if let Ok(response) = client.get("https://api6.ipify.org").send().await {
          if let Ok(response) = response.error_for_status() {
            if let Ok(address) = response.text().await {
              if address.trim().parse::<std::net::Ipv6Addr>().is_ok() {
                return Ok::<_, USTBLError>(());
              }
            }
          }
        }
      }
      Err(USTBLError(
        "IPv6 WARP 通道未连通，已停止并恢复网络，请检查校园网认证或接入点可达性".into(),
      ))
    }
    .await;
    if let Err(error) = connected {
      self.stop(root).await?;
      return Err(error);
    }
    self.started_at = Some(chrono::Utc::now().timestamp_millis() as u64);
    Ok(())
  }
}

#[tauri::command]
pub async fn retrieve_ipv6_status(app: AppHandle) -> USTBLResult<Ipv6Status> {
  app
    .state::<Mutex<Ipv6Service>>()
    .lock()
    .await
    .status(&root(&app)?)
}

#[tauri::command]
pub async fn install_ipv6_core(app: AppHandle) -> USTBLResult<Ipv6Status> {
  let root = root(&app)?;
  let state = app.state::<Mutex<Ipv6Service>>();
  let mut service = state.lock().await;
  if service.status(&root)?.running {
    return Err(USTBLError("请先停止 IPv6 免流再安装核心".into()));
  }
  let secure_root = root.clone();
  tokio::task::spawn_blocking(move || runtime::secure_directory(&secure_root)).await??;
  runtime::install(&root).await?;
  service.status(&root)
}

#[tauri::command]
pub async fn start_ipv6_acceleration(
  app: AppHandle,
  settings: Ipv6Settings,
) -> USTBLResult<Ipv6Status> {
  let root = root(&app)?;
  let state = app.state::<Mutex<Ipv6Service>>();
  let mut service = state.lock().await;
  let secure_root = root.clone();
  tokio::task::spawn_blocking(move || runtime::secure_directory(&secure_root)).await??;
  service.start(&root, settings).await?;
  service.status(&root)
}

#[tauri::command]
pub async fn stop_ipv6_acceleration(app: AppHandle) -> USTBLResult<Ipv6Status> {
  let root = root(&app)?;
  let state = app.state::<Mutex<Ipv6Service>>();
  let mut service = state.lock().await;
  service.stop(&root).await?;
  service.status(&root)
}

#[tauri::command]
pub async fn retrieve_ipv6_nodes(app: AppHandle, test: bool) -> USTBLResult<Vec<Ipv6Node>> {
  let state = app.state::<Mutex<Ipv6Service>>();
  let mut service = state.lock().await;
  if !service.status(&root(&app)?)?.running {
    return Err(USTBLError("请先启动 IPv6 免流".into()));
  }
  let proxies = service.api(reqwest::Method::GET, "/proxies", None).await?;
  let controller = service.controller;
  let secret = service.secret.clone();
  drop(service);
  let selected = proxies["proxies"][config::GROUP]["now"]
    .as_str()
    .unwrap_or(config::AUTO);
  use futures::StreamExt;
  let tests = std::iter::once(config::AUTO.to_string())
    .chain(config::node_names())
    .map(|name| {
      let secret = &secret;
      let proxies = &proxies;
      async move {
        let delay = if test && name != config::AUTO {
          controller_request(
            controller,
            secret,
            reqwest::Method::GET,
            &format!(
              "/proxies/{}/delay?timeout=3000&url=https%3A%2F%2Fwww.gstatic.com%2Fgenerate_204",
              urlencoding::encode(&name)
            ),
            None,
          )
          .await
          .ok()
          .and_then(|v| v["delay"].as_u64())
          .filter(|delay| *delay > 0)
        } else {
          proxies["proxies"][&name]["history"]
            .as_array()
            .and_then(|v| v.last())
            .and_then(|v| v["delay"].as_u64())
            .filter(|delay| *delay > 0)
        };
        Ipv6Node {
          selected: selected == name,
          name,
          latency_ms: delay,
        }
      }
    });
  Ok(futures::stream::iter(tests).buffered(6).collect().await)
}

#[tauri::command]
pub async fn select_ipv6_node(app: AppHandle, name: String) -> USTBLResult<()> {
  if name != config::AUTO && !config::node_names().contains(&name) {
    return Err(USTBLError("无效的 IPv6 接入点".into()));
  }
  let state = app.state::<Mutex<Ipv6Service>>();
  let mut service = state.lock().await;
  if !service.status(&root(&app)?)?.running {
    return Err(USTBLError("请先启动 IPv6 免流".into()));
  }
  service
    .api(
      reqwest::Method::PUT,
      &format!("/proxies/{}", config::GROUP),
      Some(json!({"name": name})),
    )
    .await?;
  Ok(())
}

pub async fn shutdown(app: &AppHandle) {
  if let Ok(root) = root(app) {
    if let Err(error) = app
      .state::<Mutex<Ipv6Service>>()
      .lock()
      .await
      .stop(&root)
      .await
    {
      log::error!("IPv6 cleanup failed: {}", error.0);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn stop_is_idempotent_and_removes_sensitive_config() {
    let root = std::env::temp_dir().join(format!("ustbl-ipv6-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.json"), "secret").unwrap();
    let mut service = Ipv6Service::default();
    service.stop(&root).await.unwrap();
    service.stop(&root).await.unwrap();
    assert!(!root.join("config.json").exists());
    assert!(!service.status(&root).unwrap().running);
    std::fs::remove_dir(&root).unwrap();
  }

  #[cfg(windows)]
  #[tokio::test]
  async fn real_core_accepts_masque_config_and_authenticated_node_control() {
    let Some(binary) = std::env::var_os("USTBL_TEST_MIHOMO") else {
      return;
    };
    let root = std::env::temp_dir().join(format!("ustbl-core-{}", uuid::Uuid::new_v4()));
    runtime::secure_directory(&root).unwrap();
    std::fs::copy(binary, runtime::binary(&root)).unwrap();
    let controller = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mixed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = controller.local_addr().unwrap().port();
    let config = config::render(
      &warp::WarpDevice::test_device(),
      &Ipv6Settings::default().bypass,
      port,
      mixed.local_addr().unwrap().port(),
      "test-secret",
      false,
    )
    .unwrap();
    std::fs::write(
      root.join("config.json"),
      serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let checked = runtime::command(runtime::binary(&root))
      .args(["-t", "-d"])
      .arg(&root)
      .arg("-f")
      .arg(root.join("config.json"))
      .output()
      .unwrap();
    assert!(
      checked.status.success(),
      "{} {}",
      String::from_utf8_lossy(&checked.stdout),
      String::from_utf8_lossy(&checked.stderr)
    );
    drop(controller);
    drop(mixed);
    let mut service = Ipv6Service {
      process: Some(runtime::CoreProcess::spawn(&root).unwrap()),
      controller: port,
      secret: "test-secret".into(),
      ..Default::default()
    };
    tokio::time::timeout(Duration::from_secs(15), async {
      while service
        .api(reqwest::Method::GET, "/version", None)
        .await
        .is_err()
      {
        assert!(
          service.status(&root).unwrap().running,
          "{}",
          std::fs::read_to_string(root.join("core.log")).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
      }
    })
    .await
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
      client
        .get(format!("http://127.0.0.1:{port}/version"))
        .send()
        .await
        .unwrap()
        .status(),
      reqwest::StatusCode::UNAUTHORIZED
    );
    let proxies = service
      .api(reqwest::Method::GET, "/proxies", None)
      .await
      .unwrap();
    for name in config::node_names() {
      assert_eq!(proxies["proxies"][&name]["type"], "Masque");
    }
    service
      .api(
        reqwest::Method::PUT,
        "/proxies/USTBL-WARP",
        Some(json!({"name": config::node_names()[0]})),
      )
      .await
      .unwrap();
    let proxies = service
      .api(reqwest::Method::GET, "/proxies", None)
      .await
      .unwrap();
    assert_eq!(
      proxies["proxies"][config::GROUP]["now"],
      config::node_names()[0]
    );
    service.stop(&root).await.unwrap();
    assert!(!service.status(&root).unwrap().running);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    std::fs::remove_dir_all(&root).unwrap();
  }
}
