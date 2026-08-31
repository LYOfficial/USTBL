use crate::error::{USTBLError, USTBLResult};
use crate::launcher_config::commands::retrieve_launcher_config;
use crate::tasks::streams::desc::{PDesc, PStatus};
use crate::tasks::streams::reporter::Reporter;
use crate::tasks::streams::ProgressStream;
use crate::tasks::*;
use crate::utils::fs::validate_sha1;
use crate::utils::web::with_retry;
use async_speed_limit::Limiter;
use futures::stream::TryStreamExt;
use futures::StreamExt;
use log::warn;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_http::reqwest;
use tauri_plugin_http::reqwest::header::RANGE;
use tokio::io::AsyncSeekExt;
use tokio_util::bytes;
use tokio_util::compat::FuturesAsyncReadCompatExt;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DownloadParam {
  pub src: Url,
  pub dest: PathBuf,
  pub filename: Option<String>,
  pub sha1: Option<String>,
  #[serde(default)]
  pub custom_headers: Option<std::collections::HashMap<String, String>>,
  #[serde(default)]
  pub transfer_options: DownloadTransferOptions,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadTransferOptions {
  pub fallback_sources: Vec<Url>,
  pub retry_policy: DownloadRetryPolicy,
}

impl DownloadTransferOptions {
  pub fn resumable(fallback_sources: Vec<Url>, max_attempts: usize) -> Self {
    Self {
      fallback_sources,
      retry_policy: DownloadRetryPolicy::Resumable {
        max_attempts: max_attempts.max(1),
      },
    }
  }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(
  tag = "strategy",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum DownloadRetryPolicy {
  #[default]
  Standard,
  Resumable {
    max_attempts: usize,
  },
}

impl DownloadRetryPolicy {
  fn max_attempts(&self) -> usize {
    match self {
      Self::Standard => 1,
      Self::Resumable { max_attempts } => (*max_attempts).max(1),
    }
  }

  fn uses_request_middleware(&self) -> bool {
    matches!(self, Self::Standard)
  }
}

/// Format a download error with its full source chain so the frontend can
/// surface a useful failure reason. Avoids formatting `Option::None` as the
/// literal string "None" when the error has no `source()`.
fn format_download_error<E: std::error::Error>(e: &E) -> String {
  // Prefer the error's own Display, which often includes status/url.
  let primary = e.to_string();
  if let Some(src) = e.source() {
    format!("{}: {}", primary, src)
  } else {
    primary
  }
}

pub struct DownloadTask {
  p_handle: PTaskHandle,
  param: DownloadParam,
  dest_path: PathBuf,
  #[allow(dead_code)]
  report_interval: Duration,
}

impl DownloadTask {
  pub fn new(
    app_handle: AppHandle,
    task_id: u32,
    task_group: Option<String>,
    param: DownloadParam,
    report_interval: Duration,
  ) -> Self {
    let cache_dir = retrieve_launcher_config(app_handle.clone())
      .unwrap()
      .download
      .cache
      .directory;
    DownloadTask {
      p_handle: PTaskHandle::new(
        PDesc::<PTaskParam>::new(
          task_id,
          task_group.clone(),
          0,
          PTaskParam::Download(param.clone()),
          PStatus::InProgress,
        ),
        Duration::from_secs(1),
        cache_dir.clone().join(format!("task-{task_id}.json")),
        Reporter::new(
          0,
          Duration::from_secs(1),
          TauriEventSink::new(app_handle.clone()),
        ),
      ),
      param: param.clone(),
      dest_path: cache_dir.clone().join(param.dest.clone()),
      report_interval,
    }
  }

  pub fn from_descriptor(
    app_handle: AppHandle,
    desc: PTaskDesc,
    report_interval: Duration,
    reset: bool,
  ) -> Self {
    let param = match &desc.payload {
      PTaskParam::Download(param) => param.clone(),
    };

    let cache_dir = retrieve_launcher_config(app_handle.clone())
      .unwrap()
      .download
      .cache
      .directory;
    let task_id = desc.task_id;
    let path = cache_dir.join(format!("task-{task_id}.json"));
    DownloadTask {
      p_handle: PTaskHandle::new(
        if reset {
          PTaskDesc {
            status: PStatus::Waiting,
            current: 0,
            ..desc
          }
        } else {
          PTaskDesc {
            status: PStatus::Waiting,
            ..desc
          }
        },
        Duration::from_secs(1),
        path,
        Reporter::new(
          desc.total,
          Duration::from_secs(1),
          TauriEventSink::new(app_handle.clone()),
        ),
      ),
      param: param.clone(),
      dest_path: cache_dir.clone().join(param.dest.clone()),
      report_interval,
    }
  }

  fn sources(param: &DownloadParam) -> Vec<Url> {
    let mut sources = Vec::with_capacity(1 + param.transfer_options.fallback_sources.len());
    for source in std::iter::once(&param.src).chain(&param.transfer_options.fallback_sources) {
      if !sources.contains(source) {
        sources.push(source.clone());
      }
    }
    sources
  }

  async fn send_request(
    app_handle: &AppHandle,
    current: i64,
    param: &DownloadParam,
    source: &Url,
    use_request_retry: bool,
  ) -> USTBLResult<reqwest::Response> {
    let state = app_handle.state::<reqwest::Client>();
    let response = if use_request_retry {
      let client = with_retry(state.inner().clone());
      let mut request = if current == 0 {
        client.get(source.clone())
      } else {
        client
          .get(source.clone())
          .header(RANGE, format!("bytes={current}-"))
      };
      if let Some(ref headers) = param.custom_headers {
        for (key, value) in headers {
          if let Ok(header_name) = key.parse::<reqwest::header::HeaderName>() {
            if let Ok(header_value) = value.parse::<reqwest::header::HeaderValue>() {
              request = request.header(header_name, header_value);
            }
          }
        }
      }
      request
        .send()
        .await
        .map_err(|error| USTBLError(format_download_error(&error)))?
    } else {
      let mut request = if current == 0 {
        state.get(source.clone())
      } else {
        state
          .get(source.clone())
          .header(RANGE, format!("bytes={current}-"))
      };
      if let Some(ref headers) = param.custom_headers {
        for (key, value) in headers {
          if let Ok(header_name) = key.parse::<reqwest::header::HeaderName>() {
            if let Ok(header_value) = value.parse::<reqwest::header::HeaderValue>() {
              request = request.header(header_name, header_value);
            }
          }
        }
      }
      request
        .send()
        .await
        .map_err(|error| USTBLError(format_download_error(&error)))?
    };

    let response = response
      .error_for_status()
      .map_err(|e| USTBLError(format_download_error(&e)))?;

    Ok(response)
  }

  async fn create_resp_stream(
    app_handle: &AppHandle,
    current: i64,
    param: &DownloadParam,
    source: &Url,
    use_request_retry: bool,
  ) -> USTBLResult<(
    impl Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send,
    i64,
  )> {
    let resp = Self::send_request(app_handle, current, param, source, use_request_retry).await?;
    // Content-Length may be absent for chunked transfer encoding or redirects;
    // fall back to -1 so the download still proceeds without total progress info.
    let total_progress = if current == 0 {
      resp.content_length().map_or(-1, |length| length as i64)
    } else {
      -1
    };
    Ok((
      resp.bytes_stream().map(|res| match res {
        Ok(bytes) => Ok(bytes),
        Err(error) => Err(std::io::Error::other(error)),
      }),
      total_progress,
    ))
  }

  async fn download_once(
    app_handle: &AppHandle,
    limiter: Option<Limiter>,
    task_handle: Arc<RwLock<PTaskHandle>>,
    param: &DownloadParam,
    dest_path: &PathBuf,
    source: &Url,
    use_request_retry: bool,
  ) -> USTBLResult<()> {
    let current = task_handle.read().unwrap().desc.current;
    let (resp, total_progress) =
      Self::create_resp_stream(app_handle, current, param, source, use_request_retry).await?;
    let stream = ProgressStream::new(resp, task_handle.clone());
    if let Some(parent) = dest_path.parent() {
      tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = if current == 0 {
      tokio::fs::File::create(dest_path).await?
    } else {
      let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(dest_path)
        .await?;
      file.seek(std::io::SeekFrom::Start(current as u64)).await?;
      file
    };
    {
      let mut task_handle = task_handle.write().unwrap();
      task_handle.set_total(total_progress);
      task_handle.mark_started();
    }
    if let Some(limiter) = limiter {
      tokio::io::copy(
        &mut limiter.limit(stream.into_async_read()).compat(),
        &mut file,
      )
      .await?;
    } else {
      tokio::io::copy(&mut stream.into_async_read().compat(), &mut file).await?;
    }
    drop(file);
    if task_handle.read().unwrap().status().is_cancelled() {
      tokio::fs::remove_file(dest_path).await?;
      Ok(())
    } else {
      match &param.sha1 {
        Some(sha1) => validate_sha1(param.dest.clone(), sha1.clone()),
        None => Ok(()),
      }
    }
  }

  async fn future_impl(
    self,
    app_handle: AppHandle,
    limiter: Option<Limiter>,
  ) -> USTBLResult<(
    impl Future<Output = USTBLResult<()>> + Send,
    Arc<RwLock<PTaskHandle>>,
  )> {
    let handle = Arc::new(RwLock::new(self.p_handle));
    let task_handle = handle.clone();
    let param = self.param.clone();
    let dest_path = self.dest_path.clone();
    Ok((
      async move {
        let sources = Self::sources(&param);
        let attempts = param.transfer_options.retry_policy.max_attempts();
        let use_request_retry = param
          .transfer_options
          .retry_policy
          .uses_request_middleware();
        let mut last_error = None;

        for attempt in 0..attempts {
          let source = &sources[attempt % sources.len()];
          match Self::download_once(
            &app_handle,
            limiter.clone(),
            task_handle.clone(),
            &param,
            &dest_path,
            source,
            use_request_retry,
          )
          .await
          {
            Ok(()) => return Ok(()),
            Err(error) => {
              last_error = Some(error);
              if attempt + 1 < attempts {
                warn!(
                  "Resumable download failed (attempt {}/{} from {}); retrying with cached progress",
                  attempt + 1,
                  attempts,
                  source
                );
                tokio::time::sleep(Duration::from_secs((attempt + 1).min(5) as u64)).await;
              }
            }
          }
        }

        Err(last_error.expect("a download attempt always produces an error"))
      },
      handle,
    ))
  }

  pub async fn future(
    self,
    app_handle: AppHandle,
    limiter: Option<Limiter>,
  ) -> USTBLResult<(
    impl Future<Output = USTBLResult<()>> + Send,
    Arc<RwLock<PTaskHandle>>,
  )> {
    Self::future_impl(self, app_handle, limiter).await
  }
}
