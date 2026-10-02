use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

const REMOTE_PROXY: &str = "www.ustb.world:18080";
pub const LOCAL_PROXY_HOST: &str = "127.0.0.1";
pub const LOCAL_PROXY_PORT: u16 = 17880;

pub struct LocalBrowserProxy {
  stop: watch::Sender<bool>,
  task: tokio::task::JoinHandle<()>,
}

impl LocalBrowserProxy {
  pub async fn start() -> io::Result<Self> {
    let listener = TcpListener::bind((LOCAL_PROXY_HOST, LOCAL_PROXY_PORT)).await?;
    let (stop, mut stop_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
      loop {
        tokio::select! {
          _ = stop_rx.changed() => break,
          accepted = listener.accept() => {
            let Ok((stream, _)) = accepted else { continue };
            tokio::spawn(handle_client(stream));
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

async fn handle_client(mut client: TcpStream) {
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
  let auth = remote_proxy_auth().unwrap_or_default();
  let mut remote = if use_remote {
    if auth.is_empty() {
      let _ = client
        .write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n\r\n")
        .await;
      return;
    }
    let Ok(remote) = TcpStream::connect(REMOTE_PROXY).await else {
      let _ = client
        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
        .await;
      return;
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
  let remote_request = format!(
    "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: {auth}\r\nConnection: keep-alive\r\n\r\n"
  );
  if remote.write_all(remote_request.as_bytes()).await.is_err() {
    return;
  }
  let mut remote_reader = BufReader::new(&mut remote);
  let mut response = Vec::with_capacity(4096);
  loop {
    let mut line = Vec::new();
    let Ok(size) = remote_reader.read_until(b'\n', &mut line).await else {
      return;
    };
    if size == 0 || response.len() + line.len() > 64 * 1024 {
      return;
    }
    response.extend_from_slice(&line);
    if response.ends_with(b"\r\n\r\n") || response.ends_with(b"\n\n") {
      break;
    }
  }
  if !response.starts_with(b"HTTP/1.1 200") && !response.starts_with(b"HTTP/1.0 200") {
    let _ = client.write_all(&response).await;
    return;
  }
  if client
    .write_all(b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: USTBL\r\n\r\n")
    .await
    .is_err()
  {
    return;
  }
  let _ = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
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
