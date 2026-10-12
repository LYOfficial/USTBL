use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

const PROXY_STATUS_URL: &str = "https://www.ustb.world/api/resource-acceleration/github/status";
const DEPLOYED_PROXY_HOST: &str = "208.68.182.185";
const SITE_GATEWAY_HOST: &str = "www.ustb.world";
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
  let mut endpoint = status.browser_proxy;
  if endpoint.host.eq_ignore_ascii_case(SITE_GATEWAY_HOST) {
    log::warn!(
      "GitHub 代理状态仍返回站点网关，使用已验证代理地址 {}",
      DEPLOYED_PROXY_HOST
    );
    endpoint.host = DEPLOYED_PROXY_HOST.to_string();
  }
  endpoint.address()
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
    log::info!("Reading GitHub proxy configuration");
    let address = remote_proxy_address().await?;
    log::info!("Verifying GitHub proxy connection");
    // 验证远端代理与认证成功后，再接管系统代理。
    connect_remote(&address, "api.github.com:443", &auth).await?;
    let listener = TcpListener::bind((LOCAL_PROXY_HOST, LOCAL_PROXY_PORT)).await?;
    let (stop, mut stop_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
      let mut clients = tokio::task::JoinSet::new();
      loop {
        tokio::select! {
          _ = stop_rx.changed() => break,
          accepted = listener.accept() => {
            let Ok((stream, _)) = accepted else { continue };
            clients.spawn(handle_client(stream, address.clone(), auth.clone()));
          }
          _ = clients.join_next(), if !clients.is_empty() => {}
        }
      }
      clients.shutdown().await;
    });
    Ok(Self { stop, task })
  }

  pub async fn stop(mut self) {
    let _ = self.stop.send(true);
    let _ = (&mut self.task).await;
  }
}

impl Drop for LocalBrowserProxy {
  fn drop(&mut self) {
    let _ = self.stop.send(true);
    self.task.abort();
  }
}

fn is_github_host(host: &str) -> bool {
  let host = host.to_ascii_lowercase();
  ["github.com", "githubusercontent.com", "githubassets.com"]
    .iter()
    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    || [
      "github-production-release-asset-2e65be.s3.amazonaws.com",
      "github-production-repository-file-5c1aeb.s3.amazonaws.com",
      "github-production-user-asset-6210df.s3.amazonaws.com",
      "github-com.s3.amazonaws.com",
      "github-cloud.s3.amazonaws.com",
    ]
    .contains(&host.as_str())
}

async fn read_headers<R: tokio::io::AsyncRead + Unpin>(
  reader: &mut BufReader<R>,
) -> io::Result<Vec<u8>> {
  let mut headers = Vec::new();
  loop {
    let buffer = reader.fill_buf().await?;
    if buffer.is_empty() {
      return Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "Incomplete proxy headers",
      ));
    }
    let count = buffer
      .iter()
      .position(|byte| *byte == b'\n')
      .map_or(buffer.len(), |index| index + 1);
    if headers.len() + count > 64 * 1024 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Proxy headers exceed limit",
      ));
    }
    headers.extend_from_slice(&buffer[..count]);
    reader.consume(count);
    if headers.ends_with(b"\r\n\r\n") || headers.ends_with(b"\n\n") {
      return Ok(headers);
    }
  }
}

async fn handle_client(client: TcpStream, address: String, auth: String) {
  let mut reader = BufReader::new(client);
  let result = relay_client(&mut reader, &address, &auth).await;
  if let Err(error) = result {
    let response: &[u8] = if error.kind() == io::ErrorKind::InvalidInput {
      b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n"
    } else {
      b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n"
    };
    let _ = reader.get_mut().write_all(response).await;
  }
}

async fn relay_client(
  reader: &mut BufReader<TcpStream>,
  address: &str,
  auth: &str,
) -> io::Result<()> {
  let request = tokio::time::timeout(Duration::from_secs(10), read_headers(reader))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Proxy header timeout"))??;
  let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid proxy request");
  let text = std::str::from_utf8(&request).map_err(|_| invalid())?;
  let mut lines = text.lines();
  let mut parts = lines.next().ok_or_else(invalid)?.split_whitespace();
  let method = parts.next().ok_or_else(invalid)?;
  let target = parts.next().ok_or_else(invalid)?;
  let version = parts.next().ok_or_else(invalid)?;
  if parts.next().is_some()
    || !["HTTP/1.0", "HTTP/1.1"].contains(&version)
    || !method
      .bytes()
      .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
  {
    return Err(invalid());
  }
  let tunnel = method.eq_ignore_ascii_case("CONNECT");
  let url = url::Url::parse(&if tunnel {
    format!("http://{target}")
  } else {
    target.into()
  })
  .map_err(|_| invalid())?;
  if url.scheme() != "http"
    || !url.username().is_empty()
    || url.password().is_some()
    || url.fragment().is_some()
  {
    return Err(invalid());
  }
  let host = url.host_str().ok_or_else(invalid)?;
  let port = if tunnel {
    target
      .rsplit_once(':')
      .and_then(|(_, port)| port.parse::<u16>().ok())
      .ok_or_else(invalid)?
  } else {
    url.port_or_known_default().ok_or_else(invalid)?
  };
  if port == 0 || (tunnel && (url.path() != "/" || url.query().is_some())) {
    return Err(invalid());
  }
  let authority = format!("{host}:{port}");
  let mut remote = if tunnel && port == 443 && is_github_host(host) {
    connect_remote(address, &authority, auth).await?
  } else {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&authority))
      .await
      .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Direct proxy connection timeout"))??;
    BufReader::new(stream)
  };
  if tunnel {
    reader
      .get_mut()
      .write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: USTBL\r\n\r\n")
      .await?;
  } else {
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
      path.push('?');
      path.push_str(query);
    }
    let mut headers =
      format!("{method} {path} {version}\r\nHost: {authority}\r\nConnection: close\r\n");
    for line in lines.take_while(|line| !line.is_empty()) {
      let (name, _) = line.split_once(':').ok_or_else(invalid)?;
      if [
        "host",
        "connection",
        "proxy-connection",
        "proxy-authorization",
      ]
      .iter()
      .any(|header| name.eq_ignore_ascii_case(header))
      {
        continue;
      }
      headers.push_str(line);
      headers.push_str("\r\n");
    }
    headers.push_str("\r\n");
    remote.get_mut().write_all(headers.as_bytes()).await?;
  }
  tokio::io::copy_bidirectional(reader, &mut remote).await?;
  Ok(())
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
  log::debug!("GitHub proxy TCP connection established");
  let remote_request = format!(
    "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: {auth}\r\nConnection: keep-alive\r\n\r\n"
  );
  remote.write_all(remote_request.as_bytes()).await?;
  log::debug!("GitHub proxy tunnel request sent");
  let mut remote_reader = BufReader::new(remote);
  let mut response = Vec::with_capacity(4096);
  loop {
    let mut line = Vec::new();
    use tokio::io::AsyncReadExt;
    let size = (&mut remote_reader)
      .take((64 * 1024 + 1 - response.len()) as u64)
      .read_until(b'\n', &mut line)
      .await?;
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

  #[tokio::test]
  #[ignore = "requires deployed GitHub proxy and official build credentials"]
  async fn deployed_proxy_start_and_stop() {
    let proxy = LocalBrowserProxy::start().await.unwrap();
    proxy.stop().await;
    assert!(TcpListener::bind((LOCAL_PROXY_HOST, LOCAL_PROXY_PORT))
      .await
      .is_ok());
  }

  #[test]
  fn github_host_matching_respects_domain_boundaries() {
    for host in [
      "github.com",
      "API.GITHUB.COM",
      "raw.githubusercontent.com",
      "github-cloud.s3.amazonaws.com",
    ] {
      assert!(is_github_host(host));
    }
    for host in [
      "fakegithub.com",
      "evilgithubusercontent.com",
      "github.com.example.org",
      "unrelated.s3.amazonaws.com",
    ] {
      assert!(!is_github_host(host));
    }
  }

  async fn local_client() -> (TcpStream, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = TcpStream::connect(address).await.unwrap();
    let task = tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      handle_client(stream, "unused".into(), "".into()).await;
    });
    (client, task)
  }

  #[tokio::test]
  async fn http_requests_keep_the_body_and_strip_proxy_credentials() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
      let (stream, _) = origin.accept().await.unwrap();
      let mut reader = BufReader::new(stream);
      let headers = String::from_utf8(read_headers(&mut reader).await.unwrap()).unwrap();
      assert!(headers.starts_with("POST /submit?q=1 HTTP/1.1\r\n"));
      assert!(!headers.contains("Proxy-Authorization"));
      let mut body = [0; 4];
      reader.read_exact(&mut body).await.unwrap();
      assert_eq!(&body, b"data");
      reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
        .await
        .unwrap();
    });
    let (mut client, proxy) = local_client().await;
    client.write_all(format!("POST http://{address}/submit?q=1 HTTP/1.1\r\nHost: fake\r\nContent-Length: 4\r\nProxy-Authorization: secret\r\n\r\ndata").as_bytes()).await.unwrap();
    client.shutdown().await.unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.ends_with("ok"));
    server.await.unwrap();
    proxy.await.unwrap();
  }

  #[tokio::test]
  async fn connect_to_nonstandard_ports_preserves_buffered_client_bytes() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = origin.accept().await.unwrap();
      let mut body = [0; 4];
      stream.read_exact(&mut body).await.unwrap();
      stream.write_all(&body).await.unwrap();
    });
    let (mut client, proxy) = local_client().await;
    client
      .write_all(format!("CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n\r\ndata").as_bytes())
      .await
      .unwrap();
    client.shutdown().await.unwrap();
    let mut reader = BufReader::new(client);
    assert!(String::from_utf8(read_headers(&mut reader).await.unwrap())
      .unwrap()
      .contains("200 Connection Established"));
    let mut body = [0; 4];
    reader.read_exact(&mut body).await.unwrap();
    assert_eq!(&body, b"data");
    server.await.unwrap();
    proxy.await.unwrap();
  }

  #[tokio::test]
  async fn headers_without_newlines_have_a_bounded_size() {
    let mut reader = BufReader::new(std::io::Cursor::new(vec![b'x'; 65 * 1024]));
    assert_eq!(
      read_headers(&mut reader).await.unwrap_err().kind(),
      io::ErrorKind::InvalidInput
    );
  }

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

  #[test]
  fn deployed_proxy_fallback_is_a_public_host_without_port() {
    assert_eq!(DEPLOYED_PROXY_HOST, "208.68.182.185");
    assert!(!DEPLOYED_PROXY_HOST.contains(':'));
    assert_eq!(SITE_GATEWAY_HOST, "www.ustb.world");
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
