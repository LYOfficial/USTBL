use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

const PROXY_STATUS_URL: &str = "https://www.ustb.world/api/resource-acceleration/github/status";
pub const LOCAL_PROXY_HOST: &str = "127.0.0.1";
pub const LOCAL_PROXY_PORT: u16 = 17880;

#[derive(Deserialize)]
struct ProxyStatus {
  proxy_enabled: bool,
  browser_proxy: ProxyEndpoint,
}

#[derive(Deserialize)]
struct ProxyEndpoint {
  host: String,
  port: u16,
  configured: bool,
}

impl ProxyEndpoint {
  fn address(&self) -> io::Result<String> {
    let host = self.host.trim();
    if self.port == 0
      || host.is_empty()
      || host
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#'))
    {
      return Err(io::Error::other(
        "vUSTB 代理公网地址无效，请填写公网 IP 或域名，不含协议、路径或端口",
      ));
    }
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
      Ok(format!("[{host}]:{}", self.port))
    } else if host.contains(':') {
      Err(io::Error::other("vUSTB 代理公网地址不应包含端口"))
    } else {
      Ok(format!("{host}:{}", self.port))
    }
  }
}

async fn remote_proxy_address() -> io::Result<String> {
  let client = reqwest::Client::builder()
    .no_proxy()
    .timeout(Duration::from_secs(10))
    .build()
    .map_err(io::Error::other)?;
  let status = client
    .get(PROXY_STATUS_URL)
    .send()
    .await
    .and_then(reqwest::Response::error_for_status)
    .map_err(|error| io::Error::other(format!("读取 vUSTB 代理配置失败: {error}")))?
    .json::<ProxyStatus>()
    .await
    .map_err(|error| io::Error::other(format!("vUSTB 代理配置格式错误: {error}")))?;
  if !status.proxy_enabled || !status.browser_proxy.configured {
    return Err(io::Error::other(
      "vUSTB 未启用 GitHub 中转或未配置浏览器代理密钥",
    ));
  }
  status.browser_proxy.address()
}

pub struct LocalBrowserProxy {
  stop: watch::Sender<bool>,
  task: tokio::task::JoinHandle<()>,
}

impl LocalBrowserProxy {
  pub async fn start() -> io::Result<Self> {
    let auth = remote_proxy_auth().ok_or_else(|| {
      io::Error::other("此启动器未配置 GitHub 代理密钥，请使用已配置密钥的官方构建")
    })?;
    let address = remote_proxy_address().await?;
    // 验证远端代理与认证成功后，再接管系统代理。
    connect_remote(&address, "api.github.com:443", &auth).await?;
    let listener = TcpListener::bind((LOCAL_PROXY_HOST, LOCAL_PROXY_PORT)).await?;
    let (stop, mut stop_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
      loop {
        tokio::select! {
          _ = stop_rx.changed() => break,
          accepted = listener.accept() => {
            let Ok((stream, _)) = accepted else { continue };
            tokio::spawn(handle_client(stream, address.clone(), auth.clone()));
          }
        }
      }
    });
    Ok(Self { stop, task })
  }

  pub async fn stop(self) {
    let _ = self.stop.send(true);
    let _ = self.task.await;
  }
}

async fn handle_client(mut client: TcpStream, address: String, auth: String) {
  let mut request = Vec::with_capacity(4096);
  let mut reader = BufReader::new(&mut client);
  loop {
    let mut line = Vec::new();
    let Ok(size) = reader.read_until(b'\n', &mut line).await else {
      return;
    };
    if size == 0 || request.len() + line.len() > 64 * 1024 {
      return;
    }
    request.extend_from_slice(&line);
    if request.ends_with(b"\r\n\r\n") || request.ends_with(b"\n\n") {
      break;
    }
  }
  let Ok(request_text) = std::str::from_utf8(&request) else {
    return;
  };
  let Some(first_line) = request_text.lines().next() else {
    return;
  };
  let mut parts = first_line.split_whitespace();
  let Some(method) = parts.next() else { return };
  let Some(target) = parts.next() else { return };
  if !method.eq_ignore_ascii_case("CONNECT") || !target.ends_with(":443") {
    let _ = client
      .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nAllow: CONNECT\r\nConnection: close\r\n\r\n")
      .await;
    return;
  }
  let Some((host, _)) = target.rsplit_once(':') else {
    return;
  };
  let use_remote = host.eq_ignore_ascii_case("github.com")
    || host.ends_with(".github.com")
    || host.ends_with("githubusercontent.com")
    || host.ends_with("githubassets.com")
    || host.ends_with(".s3.amazonaws.com");
  let mut remote = if use_remote {
    if auth.is_empty() {
      let _ = client
        .write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n\r\n")
        .await;
      return;
    }
    let remote = match connect_remote(&address, target, &auth).await {
      Ok(remote) => remote,
      Err(error) => {
        log::warn!("GitHub proxy connection failed: {error}");
        let _ = client
          .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
          .await;
        return;
      }
    };
    remote
  } else {
    let Ok(mut remote) = TcpStream::connect(target).await else {
      let _ = client
        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
        .await;
      return;
    };
    if client
      .write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: USTBL\r\n\r\n")
      .await
      .is_err()
    {
      return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
    return;
  };
  if client
    .write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: USTBL\r\n\r\n")
    .await
    .is_err()
  {
    return;
  }
  let _ = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
}

async fn connect_remote(
  address: &str,
  target: &str,
  auth: &str,
) -> io::Result<BufReader<TcpStream>> {
  tokio::time::timeout(Duration::from_secs(10), open_tunnel(address, target, auth))
    .await
    .map_err(|_| {
      io::Error::new(
        io::ErrorKind::TimedOut,
        format!("GitHub 代理 {address} 连接超时，请检查公网地址、TCP 端口放行与转发"),
      )
    })?
}

async fn open_tunnel(address: &str, target: &str, auth: &str) -> io::Result<BufReader<TcpStream>> {
  let mut remote = TcpStream::connect(address).await.map_err(|error| {
    io::Error::new(
      error.kind(),
      format!("无法连接 GitHub 代理 {address}: {error}"),
    )
  })?;
  let remote_request = format!(
    "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: {auth}\r\nConnection: keep-alive\r\n\r\n"
  );
  remote.write_all(remote_request.as_bytes()).await?;
  let mut remote_reader = BufReader::new(remote);
  let mut response = Vec::with_capacity(4096);
  loop {
    let mut line = Vec::new();
    let size = remote_reader.read_until(b'\n', &mut line).await?;
    if size == 0 || response.len() + line.len() > 64 * 1024 {
      return Err(io::Error::other("GitHub 代理返回了无效响应"));
    }
    response.extend_from_slice(&line);
    if response.ends_with(b"\r\n\r\n") || response.ends_with(b"\n\n") {
      break;
    }
  }
  let status = std::str::from_utf8(&response)
    .ok()
    .and_then(|text| text.lines().next())
    .and_then(|line| line.split_whitespace().nth(1))
    .and_then(|code| code.parse::<u16>().ok());
  match status {
    Some(200) => Ok(remote_reader),
    Some(407) => Err(io::Error::other(
      "GitHub 代理认证失败，请确认启动器构建密钥与 vUSTB 管理面板的代理密钥一致",
    )),
    Some(403) => Err(io::Error::other(
      "GitHub 代理未启用、未配置密钥或目标被拒绝，请检查 vUSTB 资源加速设置",
    )),
    Some(502) => Err(io::Error::other(
      "GitHub 代理无法连接目标，请检查日本服务器到 GitHub 的连接",
    )),
    Some(code) => Err(io::Error::other(format!("GitHub 代理返回 HTTP {code}"))),
    None => Err(io::Error::other("GitHub 代理返回了无效响应")),
  }
}

fn remote_proxy_auth() -> Option<String> {
  let password = option_env!("USTBL_GITHUB_PROXY_PASSWORD")?.trim();
  if password.is_empty() {
    return None;
  }
  Some(format!(
    "Basic {}",
    STANDARD.encode(format!("ustbl:{password}"))
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncReadExt;

  #[test]
  fn proxy_endpoint_uses_public_host_and_port() {
    let endpoint = ProxyEndpoint {
      host: "208.68.182.185".into(),
      port: 18080,
      configured: true,
    };
    assert_eq!(endpoint.address().unwrap(), "208.68.182.185:18080");
    for host in [
      "http://208.68.182.185",
      "208.68.182.185:18080",
      "",
      "bad host",
    ] {
      let invalid = ProxyEndpoint {
        host: host.into(),
        port: 18080,
        configured: true,
      };
      assert!(invalid.address().is_err());
    }
    let ipv6 = ProxyEndpoint {
      host: "2001:db8::1".into(),
      port: 18080,
      configured: true,
    };
    assert_eq!(ipv6.address().unwrap(), "[2001:db8::1]:18080");
  }

  async fn mock_proxy(response: &'static [u8]) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let task = tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      let mut reader = BufReader::new(stream);
      let mut request = Vec::new();
      loop {
        reader.read_until(b'\n', &mut request).await.unwrap();
        if request.ends_with(b"\r\n\r\n") {
          break;
        }
      }
      let request = String::from_utf8(request).unwrap();
      assert!(request.starts_with("CONNECT api.github.com:443 HTTP/1.1\r\n"));
      assert!(request.contains("Proxy-Authorization: Basic test\r\n"));
      reader.get_mut().write_all(response).await.unwrap();
    });
    (address, task)
  }

  #[tokio::test]
  async fn successful_tunnel_preserves_buffered_bytes() {
    let (address, task) = mock_proxy(b"HTTP/1.1 200 Connection Established\r\n\r\ndata").await;
    let mut tunnel = connect_remote(&address, "api.github.com:443", "Basic test")
      .await
      .unwrap();
    let mut bytes = [0; 4];
    tunnel.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"data");
    task.await.unwrap();
  }

  #[tokio::test]
  async fn rejected_authentication_reports_configuration_mismatch() {
    let (address, task) = mock_proxy(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
    let error = connect_remote(&address, "api.github.com:443", "Basic test")
      .await
      .unwrap_err();
    assert!(error.to_string().contains("认证失败"));
    assert!(!error.to_string().contains("Basic test"));
    task.await.unwrap();
  }

  #[tokio::test]
  async fn disabled_proxy_does_not_report_success() {
    let (address, task) = mock_proxy(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
    let error = connect_remote(&address, "api.github.com:443", "Basic test")
      .await
      .unwrap_err();
    assert!(error.to_string().contains("未启用"));
    task.await.unwrap();
  }
}
