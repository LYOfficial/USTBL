use crate::error::{USTBLError, USTBLResult};
use crate::launcher_config::commands::retrieve_launcher_config;
use crate::resource::helpers::mcim::{
  content_file_source_candidates, get_content_source_priority_list, ContentSource,
};
use crate::tasks::streams::desc::{PDesc, PStatus};
use crate::tasks::streams::reporter::Reporter;
use crate::tasks::streams::ProgressStream;
use crate::tasks::*;
use crate::utils::fs::validate_sha1;
use async_speed_limit::Limiter;
use futures::stream::{BoxStream, TryStreamExt};
use futures::StreamExt;
use log::warn;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_http::reqwest;
use tauri_plugin_http::reqwest::header::{CONTENT_RANGE, RANGE};
use tokio::io::{AsyncRead, AsyncSeekExt, AsyncWriteExt};
use tokio_util::bytes;
use tokio_util::compat::FuturesAsyncReadCompatExt;

/// Attempt budget of the default (`Standard`) policy.
///
/// The budget is shared by every source: a single source is retried this many
/// times, several sources are visited round-robin until it runs out.
const STANDARD_MAX_ATTEMPTS: usize = 8;

/// Upper bound of the exponential delay between two attempts.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);

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
  /// Default retry budget, plus other equivalent URLs for the same file. The
  /// first failing source never blocks the others.
  pub fn with_fallbacks(fallback_sources: Vec<Url>) -> Self {
    Self {
      fallback_sources,
      retry_policy: DownloadRetryPolicy::Standard,
    }
  }

  /// Like [`Self::with_fallbacks`] with an explicit attempt budget.
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

/// Both policies resume partial files, rotate sources and back off between
/// attempts. They only differ in the attempt budget: `Standard` uses
/// [`STANDARD_MAX_ATTEMPTS`], `Resumable` the caller's number. `Resumable`
/// stays as a variant because it is part of persisted task descriptors.
impl DownloadRetryPolicy {
  fn max_attempts(&self) -> usize {
    match self {
      Self::Standard => STANDARD_MAX_ATTEMPTS,
      Self::Resumable { max_attempts } => (*max_attempts).max(1),
    }
  }
}

/// What the retry loop should do after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
  /// Transient failure; try again (on the next source if there is one).
  Retry,
  /// This source cannot serve the file (missing, forbidden, ...). Never ask it
  /// again, go straight to the next source.
  NextSource,
  /// Retrying cannot help.
  Abort,
}

#[derive(Debug)]
struct AttemptError {
  verdict: Verdict,
  /// The bytes on disk cannot be trusted or extended; restart from zero.
  reset_progress: bool,
  error: USTBLError,
}

impl AttemptError {
  fn new(verdict: Verdict, message: impl Into<String>) -> Self {
    Self {
      verdict,
      reset_progress: false,
      error: USTBLError(message.into()),
    }
  }

  fn retry(message: impl Into<String>) -> Self {
    Self::new(Verdict::Retry, message)
  }

  fn from_scratch(message: impl Into<String>) -> Self {
    Self {
      reset_progress: true,
      ..Self::new(Verdict::Retry, message)
    }
  }
}

/// Local file errors (including a scanner briefly locking the file on Windows)
/// are retried within the same budget as network errors.
impl From<std::io::Error> for AttemptError {
  fn from(error: std::io::Error) -> Self {
    Self::retry(error.to_string())
  }
}

/// Delay before the attempt after `attempt` (0-based): 1s, 2s, 4s, then 8s.
fn retry_delay(attempt: usize) -> Duration {
  Duration::from_secs(1u64 << attempt.min(3)).min(MAX_RETRY_DELAY)
}

/// Whether both URLs point at the same host and port.
fn same_origin(a: &Url, b: &Url) -> bool {
  a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

/// Decide what an HTTP error status means for the retry loop.
///
/// `redirected` is set when the error came from a different host than the one
/// that was asked: BMCLAPI answers with a redirect to one of its volunteer
/// nodes, and a node that has not synced a file yet reports 404 for a file the
/// mirror really has. Asking again lands on another node.
fn classify_status(
  status: reqwest::StatusCode,
  redirected: bool,
  resumed: bool,
  message: String,
) -> AttemptError {
  match status.as_u16() {
    // The saved offset does not fit the file any more.
    416 if resumed => AttemptError::from_scratch(message),
    403 | 408 | 425 | 429 | 500..=599 => AttemptError::retry(message),
    404 | 410 if redirected => AttemptError::retry(message),
    _ => AttemptError::new(Verdict::NextSource, message),
  }
}

/// Run `attempt` against `sources` until it succeeds or the budget is spent.
///
/// Sources are visited round-robin. A source that reports
/// [`Verdict::NextSource`] is dropped for good; the loop ends early once no
/// source is left. Waiting happens between attempts, except right after a
/// source was dropped and another one is ready.
async fn run_attempts<F, Fut>(
  sources: &[Url],
  max_attempts: usize,
  backoff: impl Fn(usize) -> Duration,
  mut attempt: F,
) -> USTBLResult<()>
where
  F: FnMut(Url) -> Fut,
  Fut: Future<Output = Result<(), AttemptError>>,
{
  if sources.is_empty() {
    return Err(USTBLError("no download source available".to_string()));
  }
  let max_attempts = max_attempts.max(1);
  let mut unusable = vec![false; sources.len()];
  let mut next = 0;
  let mut last_error = None;

  for n in 0..max_attempts {
    let Some(index) = (0..sources.len())
      .map(|offset| (next + offset) % sources.len())
      .find(|index| !unusable[*index])
    else {
      break;
    };
    let source = &sources[index];
    let failure = match attempt(source.clone()).await {
      Ok(()) => return Ok(()),
      Err(failure) => failure,
    };
    warn!(
      "Download failed (attempt {}/{} from {}, {:?}): {}",
      n + 1,
      max_attempts,
      source,
      failure.verdict,
      failure.error.0
    );

    let skip_wait = match failure.verdict {
      Verdict::Abort => return Err(failure.error),
      Verdict::NextSource => {
        unusable[index] = true;
        true
      }
      Verdict::Retry => false,
    };
    last_error = Some(failure.error);
    next = index + 1;

    let source_left = unusable.iter().any(|unusable| !unusable);
    if n + 1 < max_attempts && source_left && !skip_wait {
      tokio::time::sleep(backoff(n)).await;
    }
  }

  Err(last_error.expect("a download attempt always produces an error"))
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

  fn base_sources(param: &DownloadParam) -> Vec<Url> {
    let mut sources = Vec::with_capacity(2 + param.transfer_options.fallback_sources.len() * 2);
    for source in std::iter::once(&param.src).chain(&param.transfer_options.fallback_sources) {
      if let Some(proxy) = github_proxy_url(source) {
        if !sources.contains(&proxy) {
          sources.push(proxy);
        }
      }
      if !sources.contains(source) {
        sources.push(source.clone());
      }
    }
    sources
  }

  /// Expand every source with its MCIM mirror counterpart, ordered by the
  /// configured content source strategy.
  ///
  /// This is what makes the mirror a fallback (or the preferred source) for mods,
  /// shader packs and every other file MCIM mirrors, including downloads that are
  /// scheduled by the backend rather than by the resource browser.
  fn sources(param: &DownloadParam, priority: &[ContentSource]) -> Vec<Url> {
    let mut urls: Vec<Url> = Vec::new();

    for source in Self::base_sources(param) {
      for candidate in content_file_source_candidates(&source, priority) {
        if !urls.contains(&candidate) {
          urls.push(candidate);
        }
      }
    }

    urls
  }

  async fn download_once(
    app_handle: &AppHandle,
    limiter: Option<Limiter>,
    task_handle: Arc<RwLock<PTaskHandle>>,
    param: &DownloadParam,
    dest_path: &Path,
    source: &Url,
  ) -> Result<(), AttemptError> {
    if task_handle.read().unwrap().status().is_cancelled() {
      return Ok(());
    }
    let client = app_handle.state::<reqwest::Client>().inner().clone();
    let recorded = task_handle.read().unwrap().desc.current;
    let current = resume_offset(dest_path, recorded).await;

    let progress_handle = task_handle.clone();
    let (on_disk, outcome) = transfer(
      &client,
      param,
      source,
      dest_path,
      current,
      limiter,
      move |stream, start, total| {
        {
          let mut handle = progress_handle.write().unwrap();
          handle.desc.current = start;
          handle.set_total(total);
          handle.mark_started();
        }
        ProgressStream::new(stream, progress_handle).boxed()
      },
    )
    .await;

    // The file is the truth about what can be resumed, not the byte counter.
    if outcome.is_err() {
      task_handle.write().unwrap().desc.current = on_disk;
    }

    if task_handle.read().unwrap().status().is_cancelled() {
      tokio::fs::remove_file(dest_path).await?;
      return Ok(());
    }
    ensure_complete(on_disk, outcome?)?;

    if let Some(sha1) = &param.sha1 {
      if let Err(error) = validate_sha1(param.dest.clone(), sha1.clone()) {
        // A completed but invalid file cannot be resumed.
        return Err(AttemptError::from_scratch(error.0));
      }
    }
    task_handle.write().unwrap().mark_completed();
    Ok(())
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
        let sources = Self::sources(&param, &content_source_priority(&app_handle));
        // More than one source only helps if the loop is allowed to visit each
        // of them at least once.
        let attempts = param
          .transfer_options
          .retry_policy
          .max_attempts()
          .max(sources.len());

        run_attempts(&sources, attempts, retry_delay, |source| {
          let app_handle = app_handle.clone();
          let limiter = limiter.clone();
          let task_handle = task_handle.clone();
          let param = param.clone();
          let dest_path = dest_path.clone();
          async move {
            let result = Self::download_once(
              &app_handle,
              limiter,
              task_handle.clone(),
              &param,
              &dest_path,
              &source,
            )
            .await;
            if matches!(&result, Err(failure) if failure.reset_progress) {
              task_handle.write().unwrap().desc.current = 0;
            }
            result
          }
        })
        .await
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

/// Where a new attempt may continue: the part of the file that is really on
/// disk and that the task has accounted for.
async fn resume_offset(dest: &Path, recorded: i64) -> i64 {
  if recorded <= 0 {
    return 0;
  }
  match tokio::fs::metadata(dest).await {
    Ok(meta) => (meta.len() as i64).min(recorded),
    Err(_) => 0,
  }
}

/// A response whose body is ready to be written to disk.
struct Opened {
  stream: BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
  /// Offset of the first body byte in the file.
  start: i64,
  /// Size of the whole file, or -1 when the server did not say.
  total: i64,
}

/// Start of a `Content-Range: bytes <start>-<end>/<size>` header.
fn content_range_start(response: &reqwest::Response) -> Option<i64> {
  let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
  value
    .strip_prefix("bytes ")?
    .split('-')
    .next()?
    .trim()
    .parse()
    .ok()
}

/// Send the request and check that the answer can be written to the file.
async fn open_response(
  client: &reqwest::Client,
  current: i64,
  param: &DownloadParam,
  source: &Url,
) -> Result<Opened, AttemptError> {
  let mut request = client.get(source.clone());
  if current > 0 {
    request = request.header(RANGE, format!("bytes={current}-"));
  }
  if let Some(headers) = &param.custom_headers {
    for (key, value) in headers {
      if let (Ok(name), Ok(value)) = (
        key.parse::<reqwest::header::HeaderName>(),
        value.parse::<reqwest::header::HeaderValue>(),
      ) {
        request = request.header(name, value);
      }
    }
  }

  let response = request.send().await.map_err(|error| {
    let message = format_download_error(&error);
    if error.is_builder() {
      AttemptError::new(Verdict::Abort, message)
    } else {
      AttemptError::retry(message)
    }
  })?;

  let status = response.status();
  if status.is_client_error() || status.is_server_error() {
    let message = response
      .error_for_status_ref()
      .err()
      .map(|error| format_download_error(&error))
      .unwrap_or_else(|| status.to_string());
    return Err(classify_status(
      status,
      !same_origin(source, response.url()),
      current > 0,
      message,
    ));
  }
  if !status.is_success() {
    return Err(AttemptError::new(
      Verdict::NextSource,
      format!("unexpected HTTP status {status} for url ({source})"),
    ));
  }

  let start = if current > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
    if content_range_start(&response) != Some(current) {
      return Err(AttemptError::from_scratch(format!(
        "{source} answered a range request with a different range"
      )));
    }
    current
  } else {
    if current > 0 {
      // A fallback server may ignore Range. Restart instead of appending its
      // full response to a partial file.
      warn!(
        "Download source {} ignored Range at byte {}; restarting from zero",
        source, current
      );
    }
    0
  };
  // Content-Length may be absent for chunked transfer encoding or redirects;
  // fall back to -1 so the download still proceeds without total progress info.
  let total = response
    .content_length()
    .map_or(-1, |length| start + length as i64);

  Ok(Opened {
    stream: response
      .bytes_stream()
      .map_err(|error| std::io::Error::other(format_download_error(&error)))
      .boxed(),
    start,
    total,
  })
}

/// Write `reader` into `dest` from byte `start` on (a smaller file is cut to
/// that length first).
///
/// Always returns how many bytes are on disk, even on failure, so the next
/// attempt can continue exactly there.
async fn write_body<R>(dest: &Path, start: i64, reader: &mut R) -> (i64, std::io::Result<()>)
where
  R: AsyncRead + Unpin + ?Sized,
{
  let open = async {
    if let Some(parent) = dest.parent() {
      tokio::fs::create_dir_all(parent).await?;
    }
    if start == 0 {
      return tokio::fs::File::create(dest).await;
    }
    let mut file = tokio::fs::OpenOptions::new().write(true).open(dest).await?;
    file.set_len(start as u64).await?;
    file.seek(std::io::SeekFrom::Start(start as u64)).await?;
    Ok(file)
  };
  let mut file = match open.await {
    Ok(file) => file,
    Err(error) => return (start, Err(error)),
  };

  let copied = tokio::io::copy(reader, &mut file).await;
  // `copy` leaves buffered data unwritten when the reader fails. That data was
  // received, so keep it.
  let flushed = file.flush().await;
  let on_disk = file
    .metadata()
    .await
    .map_or(start, |meta| meta.len() as i64);
  (on_disk, copied.map(|_| ()).and(flushed))
}

/// One transfer attempt: request, then stream the body into the file.
///
/// `wrap` sees the body stream before it is written (the task uses it for
/// progress reporting and pause/cancel) with the offset it starts at and the
/// expected file size. Returns the number of bytes on disk and, on success, the
/// expected file size.
async fn transfer(
  client: &reqwest::Client,
  param: &DownloadParam,
  source: &Url,
  dest: &Path,
  current: i64,
  limiter: Option<Limiter>,
  wrap: impl FnOnce(
      BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
      i64,
      i64,
    ) -> BoxStream<'static, Result<bytes::Bytes, std::io::Error>>
    + Send,
) -> (i64, Result<i64, AttemptError>) {
  let opened = match open_response(client, current, param, source).await {
    Ok(opened) => opened,
    Err(error) => return (current, Err(error)),
  };
  let Opened {
    stream,
    start,
    total,
  } = opened;
  let reader = wrap(stream, start, total).into_async_read();
  let (on_disk, result) = match limiter {
    Some(limiter) => write_body(dest, start, &mut limiter.limit(reader).compat()).await,
    None => write_body(dest, start, &mut reader.compat()).await,
  };
  (on_disk, result.map(|()| total).map_err(AttemptError::from))
}

/// A stream that ended without an error can still be short, for instance when
/// the transfer was cut at a message boundary.
fn ensure_complete(on_disk: i64, total: i64) -> Result<(), AttemptError> {
  if total >= 0 && on_disk != total {
    return Err(AttemptError::retry(format!(
      "incomplete download: {on_disk} of {total} bytes"
    )));
  }
  Ok(())
}

/// Content source order for the current configuration.
///
/// The MCIM mirror is the only content source that works without extra
/// credentials, so it leads the fallback order when the configuration cannot be
/// read.
fn content_source_priority(app_handle: &AppHandle) -> Vec<ContentSource> {
  retrieve_launcher_config(app_handle.clone())
    .map(|config| get_content_source_priority_list(&config))
    .unwrap_or_else(|_| vec![ContentSource::Mcim, ContentSource::Official])
}

fn github_proxy_url(source: &Url) -> Option<Url> {
  const PROXY_BASE: &str = "https://www.ustb.world/api/resource-acceleration/github/proxy";
  const HOSTS: [&str; 18] = [
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
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
  ];
  if source.scheme() != "https" || !source.host_str().is_some_and(|host| HOSTS.contains(&host)) {
    return None;
  }
  let mut proxy = Url::parse(PROXY_BASE).ok()?;
  proxy.query_pairs_mut().append_pair("url", source.as_str());
  Some(proxy)
}

#[cfg(test)]
mod tests {
  use super::{
    github_proxy_url, ContentSource, DownloadParam, DownloadRetryPolicy, DownloadTask,
    DownloadTransferOptions,
  };
  use serde_json::json;
  use std::path::PathBuf;
  use tauri::Url;

  fn download_param(src: Url) -> DownloadParam {
    DownloadParam {
      src,
      dest: PathBuf::from("file.jar"),
      filename: None,
      sha1: None,
      custom_headers: None,
      transfer_options: Default::default(),
    }
  }

  #[test]
  fn old_download_params_use_standard_transfer_defaults() {
    let param: DownloadParam = serde_json::from_value(json!({
      "src": "https://example.com/file.jar",
      "dest": "file.jar",
      "filename": null,
      "sha1": null
    }))
    .unwrap();

    assert_eq!(param.transfer_options, DownloadTransferOptions::default());
  }

  #[test]
  fn explicit_transfer_options_control_sources_and_attempts() {
    let primary = Url::parse("https://mirror.example.com/file.jar").unwrap();
    let fallback = Url::parse("https://official.example.com/file.jar").unwrap();
    let param = DownloadParam {
      src: primary.clone(),
      dest: PathBuf::from("file.jar"),
      filename: None,
      sha1: None,
      custom_headers: None,
      transfer_options: DownloadTransferOptions::resumable(
        vec![primary.clone(), fallback.clone()],
        10,
      ),
    };

    let sources = DownloadTask::sources(&param, &[ContentSource::Mcim, ContentSource::Official]);
    assert_eq!(sources, vec![primary, fallback]);
    assert_eq!(param.transfer_options.retry_policy.max_attempts(), 10);
    assert_eq!(
      param.transfer_options.retry_policy,
      DownloadRetryPolicy::Resumable { max_attempts: 10 }
    );
    assert_eq!(
      serde_json::to_value(&param.transfer_options.retry_policy).unwrap(),
      json!({ "strategy": "resumable", "maxAttempts": 10 })
    );
  }

  #[test]
  fn content_mirror_is_ordered_by_the_configured_strategy() {
    let official =
      Url::parse("https://cdn.modrinth.com/data/AANobbMI/versions/RncWhTxD/sodium.jar").unwrap();
    let mirror =
      Url::parse("https://mod.mcimirror.top/data/AANobbMI/versions/RncWhTxD/sodium.jar").unwrap();
    let param = download_param(official.clone());

    let mirror_first =
      DownloadTask::sources(&param, &[ContentSource::Mcim, ContentSource::Official]);
    assert_eq!(mirror_first, vec![mirror.clone(), official.clone()]);

    let official_first =
      DownloadTask::sources(&param, &[ContentSource::Official, ContentSource::Mcim]);
    assert_eq!(official_first, vec![official, mirror]);
  }

  #[test]
  fn downloads_without_a_mirror_equivalent_keep_their_sources() {
    let only = Url::parse("https://mediafilez.forgecdn.net/files/1/2/file.jar").unwrap();
    let param = download_param(only.clone());

    let sources = DownloadTask::sources(&param, &[ContentSource::Mcim, ContentSource::Official]);
    assert_eq!(sources, vec![only]);
  }

  #[test]
  fn github_download_sources_put_proxy_before_direct_source() {
    let source =
      Url::parse("https://github.com/example/project/releases/download/v1/file.zip").unwrap();
    let proxy = github_proxy_url(&source).expect("GitHub URL should be proxied");
    assert_eq!(proxy.host_str(), Some("www.ustb.world"));
    assert_eq!(proxy.path(), "/api/resource-acceleration/github/proxy");
    assert_eq!(
      proxy
        .query_pairs()
        .find(|(key, _)| key == "url")
        .map(|(_, value)| value),
      Some(source.as_str().into())
    );

    let sources = DownloadTask::sources(&download_param(source.clone()), &[ContentSource::Mcim]);
    assert_eq!(sources, vec![proxy, source]);
  }
}

/// Behaviour of the retry loop and of the transfer against a server that
/// misbehaves the way real mirrors and networks do.
#[cfg(test)]
mod resilience_tests {
  use super::*;
  use std::collections::VecDeque;
  use std::net::SocketAddr;
  use std::sync::Mutex;
  use tokio::io::AsyncReadExt;
  use tokio::net::{TcpListener, TcpStream};

  fn url(value: &str) -> Url {
    Url::parse(value).unwrap()
  }

  fn status(code: u16) -> reqwest::StatusCode {
    reqwest::StatusCode::from_u16(code).unwrap()
  }

  // ---- the retry loop ----------------------------------------------------

  /// Run the loop with scripted verdicts. Returns the result and the sources
  /// that were asked, in order.
  async fn run_scripted(
    sources: &[Url],
    max_attempts: usize,
    script: impl Fn(usize, &Url) -> Result<(), AttemptError>,
  ) -> (USTBLResult<()>, Vec<Url>) {
    let asked = Mutex::new(Vec::new());
    let result = run_attempts(
      sources,
      max_attempts,
      |_| Duration::ZERO,
      |source| {
        let mut asked = asked.lock().unwrap();
        let outcome = script(asked.len(), &source);
        asked.push(source);
        async move { outcome }
      },
    )
    .await;
    (result, asked.into_inner().unwrap())
  }

  #[tokio::test]
  async fn a_transient_failure_is_retried_until_it_succeeds() {
    let a = url("https://a.example/f");
    let (result, asked) = run_scripted(std::slice::from_ref(&a), 8, |n, _| {
      if n < 2 {
        Err(AttemptError::retry("connection reset"))
      } else {
        Ok(())
      }
    })
    .await;
    assert_eq!(result, Ok(()));
    assert_eq!(asked, vec![a.clone(), a.clone(), a]);
  }

  #[tokio::test]
  async fn retries_are_bounded_and_report_the_last_error() {
    let a = url("https://a.example/f");
    let (result, asked) = run_scripted(&[a], 4, |n, _| {
      Err(AttemptError::retry(format!("failure {n}")))
    })
    .await;
    assert_eq!(result, Err(USTBLError("failure 3".to_string())));
    assert_eq!(asked.len(), 4);
  }

  #[tokio::test]
  async fn attempts_rotate_over_the_sources() {
    let (a, b) = (url("https://a.example/f"), url("https://b.example/f"));
    let (result, asked) = run_scripted(&[a.clone(), b.clone()], 4, |_, _| {
      Err(AttemptError::retry("timeout"))
    })
    .await;
    assert!(result.is_err());
    assert_eq!(asked, vec![a.clone(), b.clone(), a, b]);
  }

  #[tokio::test]
  async fn a_source_that_cannot_serve_the_file_is_not_asked_again() {
    let (a, b) = (url("https://a.example/f"), url("https://b.example/f"));
    let bad = a.clone();
    let (result, asked) = run_scripted(&[a.clone(), b.clone()], 5, move |_, source| {
      if *source == bad {
        Err(AttemptError::new(Verdict::NextSource, "404"))
      } else {
        Err(AttemptError::retry("timeout"))
      }
    })
    .await;
    assert!(result.is_err());
    assert_eq!(asked, vec![a, b.clone(), b.clone(), b.clone(), b]);
  }

  #[tokio::test]
  async fn the_loop_ends_early_when_no_source_is_left() {
    let (a, b) = (url("https://a.example/f"), url("https://b.example/f"));
    let (result, asked) = run_scripted(&[a.clone(), b.clone()], 8, |_, _| {
      Err(AttemptError::new(Verdict::NextSource, "404"))
    })
    .await;
    assert_eq!(result, Err(USTBLError("404".to_string())));
    assert_eq!(asked, vec![a, b]);
  }

  #[tokio::test]
  async fn an_abort_stops_at_once() {
    let a = url("https://a.example/f");
    let (result, asked) = run_scripted(&[a], 8, |_, _| {
      Err(AttemptError::new(Verdict::Abort, "invalid url"))
    })
    .await;
    assert_eq!(result, Err(USTBLError("invalid url".to_string())));
    assert_eq!(asked.len(), 1);
  }

  #[tokio::test]
  async fn the_delay_grows_and_is_only_paid_between_attempts() {
    let waits = Mutex::new(Vec::new());
    let result = run_attempts(
      &[url("https://a.example/f")],
      4,
      |n| {
        waits.lock().unwrap().push(n);
        Duration::ZERO
      },
      |_| async { Err(AttemptError::retry("x")) },
    )
    .await;
    assert!(result.is_err());
    // Nothing to wait for after the last attempt.
    assert_eq!(waits.into_inner().unwrap(), vec![0, 1, 2]);

    assert_eq!(
      (0..6).map(retry_delay).collect::<Vec<_>>(),
      [1, 2, 4, 8, 8, 8].map(Duration::from_secs)
    );
  }

  #[test]
  fn the_default_policy_retries_and_stays_bounded() {
    let policy = DownloadRetryPolicy::Standard;
    assert!(policy.max_attempts() > 1);
    assert!(policy.max_attempts() <= 10);
    assert_eq!(
      DownloadRetryPolicy::Resumable { max_attempts: 0 }.max_attempts(),
      1
    );
  }

  #[test]
  fn statuses_are_sorted_into_retry_skip_and_reset() {
    let verdict = |code, redirected, resumed| {
      classify_status(status(code), redirected, resumed, String::new()).verdict
    };
    for code in [403, 408, 425, 429, 500, 502, 503, 504] {
      assert_eq!(verdict(code, false, false), Verdict::Retry, "{code}");
    }
    // A mirror node that is behind answers 404 for a file the mirror has.
    assert_eq!(verdict(404, true, false), Verdict::Retry);
    assert_eq!(verdict(404, false, false), Verdict::NextSource);
    assert_eq!(verdict(410, false, false), Verdict::NextSource);
    assert_eq!(verdict(401, false, false), Verdict::NextSource);

    let stale_offset = classify_status(status(416), false, true, String::new());
    assert_eq!(stale_offset.verdict, Verdict::Retry);
    assert!(stale_offset.reset_progress);
    assert_eq!(verdict(416, false, false), Verdict::NextSource);
  }

  #[test]
  fn origins_compare_host_and_port() {
    let base = url("https://mirror.example/maven/a.jar");
    assert!(same_origin(&base, &url("https://mirror.example/other")));
    assert!(same_origin(&base, &url("https://mirror.example:443/other")));
    assert!(!same_origin(
      &base,
      &url("https://node.example/maven/a.jar")
    ));
    assert!(!same_origin(&base, &url("https://mirror.example:8443/a")));
  }

  // ---- files ---------------------------------------------------------------

  struct TempDir(PathBuf);

  impl TempDir {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("ustbl-download-{}", uuid::Uuid::new_v4()));
      std::fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
      self.0.join(name)
    }
  }

  impl Drop for TempDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  #[tokio::test]
  async fn resuming_starts_at_what_is_really_on_disk() {
    let dir = TempDir::new();
    let dest = dir.file("part.bin");

    assert_eq!(resume_offset(&dest, 100).await, 0, "no file");
    assert_eq!(resume_offset(&dest, 0).await, 0);

    std::fs::write(&dest, vec![1u8; 40]).unwrap();
    assert_eq!(resume_offset(&dest, 100).await, 40, "counter ahead of file");
    assert_eq!(resume_offset(&dest, 25).await, 25, "file ahead of counter");
  }

  #[tokio::test]
  async fn writing_continues_at_the_offset_and_cuts_stale_bytes() {
    let dir = TempDir::new();
    let dest = dir.file("part.bin");
    std::fs::write(&dest, vec![b'x'; 100]).unwrap();

    let mut body: &[u8] = &[b'y'; 20];
    let (on_disk, result) = write_body(&dest, 10, &mut body).await;
    assert!(result.is_ok());
    assert_eq!(on_disk, 30);
    let content = std::fs::read(&dest).unwrap();
    assert_eq!(&content[..10], &[b'x'; 10]);
    assert_eq!(&content[10..], &[b'y'; 20]);
  }

  #[test]
  fn a_short_but_clean_transfer_is_not_a_success() {
    assert!(ensure_complete(10, 10).is_ok());
    assert!(ensure_complete(10, -1).is_ok());
    let short = ensure_complete(9, 10).unwrap_err();
    assert_eq!(short.verdict, Verdict::Retry);
    assert!(!short.reset_progress);
  }

  // ---- a server that misbehaves -------------------------------------------

  #[derive(Clone, Debug)]
  enum Fault {
    /// Answer normally (honours `Range`).
    Serve,
    /// Close the connection cleanly after this many body bytes.
    Cut(usize),
    /// Abort the connection (RST) after this many body bytes.
    Reset(usize),
    /// Go silent after this many body bytes.
    Hang(usize),
    /// Answer 200 with the whole file even when a range was asked for.
    IgnoreRange,
    /// Close the connection without answering.
    Drop,
    Status(u16),
    Redirect(String),
  }

  struct TestServer {
    addr: SocketAddr,
    /// `Range` start of every request that arrived.
    requests: Arc<Mutex<Vec<Option<i64>>>>,
  }

  impl TestServer {
    fn url(&self) -> Url {
      url(&format!("http://{}/file.bin", self.addr))
    }

    fn requests(&self) -> Vec<Option<i64>> {
      self.requests.lock().unwrap().clone()
    }
  }

  fn payload() -> Arc<Vec<u8>> {
    Arc::new((0..200_000u32).map(|i| (i % 251) as u8).collect())
  }

  /// Serve the payload; connection number `n` follows `faults[n]`, later
  /// connections are served normally.
  async fn spawn_server(faults: Vec<Fault>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let script = Arc::new(Mutex::new(VecDeque::from(faults)));
    let data = payload();
    let log = requests.clone();
    tokio::spawn(async move {
      while let Ok((stream, _)) = listener.accept().await {
        let fault = script.lock().unwrap().pop_front().unwrap_or(Fault::Serve);
        tokio::spawn(handle(stream, fault, data.clone(), log.clone()));
      }
    });
    TestServer { addr, requests }
  }

  async fn handle(
    mut stream: TcpStream,
    fault: Fault,
    data: Arc<Vec<u8>>,
    requests: Arc<Mutex<Vec<Option<i64>>>>,
  ) {
    if matches!(fault, Fault::Drop) {
      return;
    }
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
      match stream.read(&mut chunk).await {
        Ok(0) | Err(_) => return,
        Ok(n) => head.extend_from_slice(&chunk[..n]),
      }
    }
    let head = String::from_utf8_lossy(&head).to_lowercase();
    let range = head
      .lines()
      .find_map(|line| line.strip_prefix("range: bytes="))
      .and_then(|value| value.trim().trim_end_matches('-').parse::<i64>().ok());
    requests.lock().unwrap().push(range);

    let simple =
      |line: String| format!("HTTP/1.1 {line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let (start, header) = match &fault {
      Fault::Status(code) => {
        let _ = stream
          .write_all(simple(format!("{code} Test")).as_bytes())
          .await;
        return;
      }
      Fault::Redirect(location) => {
        let line = format!("302 Found\r\nLocation: {location}");
        let _ = stream.write_all(simple(line).as_bytes()).await;
        return;
      }
      Fault::IgnoreRange => (0, "200 OK".to_string()),
      _ => match range {
        Some(start) => (
          start as usize,
          format!(
            "206 Partial Content\r\nContent-Range: bytes {start}-{}/{}",
            data.len() - 1,
            data.len()
          ),
        ),
        None => (0, "200 OK".to_string()),
      },
    };
    let body = &data[start..];
    let sent = match &fault {
      Fault::Cut(n) | Fault::Reset(n) | Fault::Hang(n) => (*n).min(body.len()),
      _ => body.len(),
    };
    let response = format!(
      "HTTP/1.1 {header}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
      body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body[..sent]).await;
    let _ = stream.flush().await;

    match fault {
      Fault::Reset(_) => {
        #[allow(deprecated)]
        let _ = stream.set_linger(Some(Duration::ZERO));
      }
      Fault::Hang(_) => tokio::time::sleep(Duration::from_secs(60)).await,
      _ => {}
    }
  }

  fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
      .no_proxy()
      .connect_timeout(Duration::from_secs(2))
      // Short on purpose: this is what ends a connection that went silent.
      .read_timeout(Duration::from_millis(500))
      .build()
      .unwrap()
  }

  /// The attempt loop of a download without the task bookkeeping: the same
  /// functions `DownloadTask` uses, driven the way `download_once` drives them.
  async fn fetch(sources: &[Url], max_attempts: usize, dest: &Path) -> USTBLResult<()> {
    let client = test_client();
    let param = DownloadParam {
      src: sources[0].clone(),
      dest: dest.to_path_buf(),
      filename: None,
      sha1: None,
      custom_headers: None,
      transfer_options: Default::default(),
    };
    let recorded = Arc::new(Mutex::new(0i64));
    run_attempts(
      sources,
      max_attempts,
      |_| Duration::ZERO,
      |source| {
        let (client, param, recorded) = (client.clone(), param.clone(), recorded.clone());
        async move {
          let known = *recorded.lock().unwrap();
          let current = resume_offset(&param.dest, known).await;
          let (on_disk, outcome) = transfer(
            &client,
            &param,
            &source,
            &param.dest,
            current,
            None,
            |s, _, _| s,
          )
          .await;
          let result = outcome.and_then(|total| ensure_complete(on_disk, total));
          if let Err(failure) = &result {
            *recorded.lock().unwrap() = if failure.reset_progress { 0 } else { on_disk };
          }
          result
        }
      },
    )
    .await
  }

  async fn assert_downloaded(dest: &Path) {
    let content = tokio::fs::read(dest).await.unwrap();
    assert_eq!(content.len(), payload().len());
    assert!(content == *payload(), "content differs from the payload");
  }

  #[tokio::test]
  async fn a_connection_closed_mid_body_resumes_with_a_range_request() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Cut(50_000)]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], None);
    assert!(requests[1].is_some_and(|start| start > 0), "{requests:?}");
  }

  #[tokio::test]
  async fn a_reset_connection_is_recovered() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Reset(40_000), Fault::Reset(10_000)]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    assert_eq!(server.requests().len(), 3);
  }

  #[tokio::test]
  async fn an_idle_connection_times_out_and_resumes() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Hang(30_000)]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    assert_eq!(server.requests().len(), 2);
  }

  #[tokio::test]
  async fn connections_dropped_before_any_answer_are_retried() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Drop, Fault::Drop]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
  }

  #[tokio::test]
  async fn a_server_error_is_retried() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Status(503), Fault::Status(502)]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    assert_eq!(server.requests().len(), 3);
  }

  #[tokio::test]
  async fn a_mirror_node_that_is_behind_is_asked_again_through_the_mirror() {
    // The mirror redirects to a node; the first node has not synced the file.
    let dir = TempDir::new();
    let node = spawn_server(vec![Fault::Status(404)]).await;
    let location = format!("http://localhost:{}/file.bin", node.addr.port());
    let mirror = spawn_server(vec![
      Fault::Redirect(location.clone()),
      Fault::Redirect(location),
    ])
    .await;
    let dest = dir.file("a.bin");

    fetch(&[mirror.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    assert_eq!(mirror.requests().len(), 2);
    assert_eq!(node.requests().len(), 2);
  }

  #[tokio::test]
  async fn a_source_without_the_file_hands_over_to_the_next_without_retrying() {
    let dir = TempDir::new();
    let missing = spawn_server(vec![Fault::Status(404); 8]).await;
    let good = spawn_server(vec![]).await;
    let dest = dir.file("a.bin");

    fetch(&[missing.url(), good.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    assert_eq!(missing.requests().len(), 1);
    assert_eq!(good.requests().len(), 1);
  }

  #[tokio::test]
  async fn a_missing_file_on_the_only_source_fails_at_once() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Status(404); 8]).await;
    let dest = dir.file("a.bin");

    let error = fetch(&[server.url()], 8, &dest).await.unwrap_err();

    assert!(error.0.contains("404"), "{}", error.0);
    assert_eq!(server.requests().len(), 1);
  }

  #[tokio::test]
  async fn a_server_that_ignores_range_restarts_from_zero() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Cut(30_000), Fault::IgnoreRange]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].is_some(), "the resume was attempted");
  }

  #[tokio::test]
  async fn a_rejected_offset_restarts_from_zero() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Cut(30_000), Fault::Status(416)]).await;
    let dest = dir.file("a.bin");

    fetch(&[server.url()], 8, &dest).await.unwrap();

    assert_downloaded(&dest).await;
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2], None);
  }

  #[tokio::test]
  async fn endless_failures_stop_after_the_attempt_budget() {
    let dir = TempDir::new();
    let server = spawn_server(vec![Fault::Cut(10_000); 20]).await;
    let dest = dir.file("a.bin");

    let result = fetch(&[server.url()], 3, &dest).await;

    assert!(result.is_err());
    assert_eq!(server.requests().len(), 3);
  }
}
