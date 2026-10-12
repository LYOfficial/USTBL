//! MCIM (Modrinth & CurseForge mirror) support.
//!
//! MCIM mirrors Modrinth and CurseForge metadata and files for mainland China.
//! Interface paths, parameters and response shapes are identical to the official
//! APIs, so only the domain (and, for CurseForge, the API key requirement) has to
//! be replaced:
//!
//! | original                        | mirror                                  |
//! | ------------------------------- | --------------------------------------- |
//! | `api.modrinth.com`              | `mod.mcimirror.top/modrinth`            |
//! | `cdn.modrinth.com`              | `mod.mcimirror.top`                     |
//! | `api.curseforge.com`            | `mod.mcimirror.top/curseforge`          |
//! | `edge.forgecdn.net`             | `mod.mcimirror.top`                     |
//! | `mediafilez.forgecdn.net`       | **must not** be replaced                |
//!
//! Requests sent to the mirror are expected to carry the launcher's own
//! User-Agent (`USTBL/<version>`, see [`crate::utils::web::build_ustbl_client`]),
//! and must not be wrapped into another service.
//!
//! See <https://www.mcimirror.top/guide/start/getting-started>.

use crate::launcher_config::models::LauncherConfig;
use std::sync::LazyLock;
use tokio::sync::{Semaphore, SemaphorePermit};
use url::Url;

/// Base URL of the MCIM mirror.
pub const MCIM_BASE: &str = "https://mod.mcimirror.top";

/// Official Modrinth API base, including the API version segment.
pub const MODRINTH_OFFICIAL_API_BASE: &str = "https://api.modrinth.com/v2";
/// Modrinth API base served by MCIM, including the API version segment.
pub const MODRINTH_MCIM_API_BASE: &str = "https://mod.mcimirror.top/modrinth/v2";
/// Official CurseForge API base, including the API version segment.
pub const CURSEFORGE_OFFICIAL_API_BASE: &str = "https://api.curseforge.com/v1";
/// CurseForge API base served by MCIM, including the API version segment.
pub const CURSEFORGE_MCIM_API_BASE: &str = "https://mod.mcimirror.top/curseforge/v1";

const MODRINTH_CDN_HOST: &str = "cdn.modrinth.com";
const FORGE_CDN_HOST: &str = "edge.forgecdn.net";
const MCIM_HOST: &str = "mod.mcimirror.top";
/// Registrable domain of the mirror, so that every mirror host is recognized.
const MCIM_DOMAIN: &str = "mcimirror.top";

/// Where a resource (mod, shader pack, ...) API request or file download should
/// be resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentSource {
  /// The upstream official service.
  Official,
  /// The MCIM mirror.
  Mcim,
}

/// Ordered content sources for the configured strategy.
///
/// `auto` follows the detected network location, matching the behaviour of the
/// game file sources in [`crate::resource::helpers::misc::get_source_priority_list`].
pub fn get_content_source_priority_list(launcher_config: &LauncherConfig) -> Vec<ContentSource> {
  match launcher_config.download.resource.strategy.as_str() {
    "official" => vec![ContentSource::Official, ContentSource::Mcim],
    "mirror" => vec![ContentSource::Mcim, ContentSource::Official],
    "auto" => match launcher_config.basic_info.is_china_mainland_ip {
      true => vec![ContentSource::Mcim, ContentSource::Official],
      false => vec![ContentSource::Official, ContentSource::Mcim],
    },
    _ => vec![ContentSource::Mcim, ContentSource::Official],
  }
}

pub fn modrinth_api_base(source: ContentSource) -> &'static str {
  match source {
    ContentSource::Official => MODRINTH_OFFICIAL_API_BASE,
    ContentSource::Mcim => MODRINTH_MCIM_API_BASE,
  }
}

pub fn curseforge_api_base(source: ContentSource) -> &'static str {
  match source {
    ContentSource::Official => CURSEFORGE_OFFICIAL_API_BASE,
    ContentSource::Mcim => CURSEFORGE_MCIM_API_BASE,
  }
}

/// Maximum number of metadata/API requests kept in flight against the mirror.
///
/// MCIM asks integrations to「合理设置并发与重试，不要对镜像发起压力测试」and may
/// treat an aggressive client as a network attack. Metadata is where the launcher
/// fans out hardest — a version pack expands into up to sixteen paginated
/// requests, and installed mods are translated one by one — so those calls share
/// one budget.
///
/// Reference: <https://www.mcimirror.top/guide/rules/register>.
pub const MCIM_MAX_CONCURRENT_API_REQUESTS: usize = 4;

/// Maximum number of file downloads kept in flight against the mirror.
///
/// Tracked separately from the metadata budget so that a long transfer cannot
/// starve an interactive request, which would leave the resource browser waiting
/// on a download slot.
pub const MCIM_MAX_CONCURRENT_DOWNLOADS: usize = 4;

/// Process-wide mirror budgets, so the caps hold across every task the launcher
/// starts rather than per call site.
static MCIM_API_CONCURRENCY: LazyLock<Semaphore> =
  LazyLock::new(|| Semaphore::new(MCIM_MAX_CONCURRENT_API_REQUESTS));
static MCIM_DOWNLOAD_CONCURRENCY: LazyLock<Semaphore> =
  LazyLock::new(|| Semaphore::new(MCIM_MAX_CONCURRENT_DOWNLOADS));

/// Whether a URL is served by the mirror, and therefore subject to the budgets.
///
/// Matches the registrable domain and its subdomains; a host that merely embeds
/// the mirror name (`mod.mcimirror.top.example.com`) does not count.
pub fn is_mcim_url(url: &str) -> bool {
  let Ok(parsed) = Url::parse(url) else {
    return false;
  };
  parsed.host_str().is_some_and(|host| {
    host == MCIM_DOMAIN
      || host
        .strip_suffix(MCIM_DOMAIN)
        .is_some_and(|prefix| prefix.ends_with('.'))
  })
}

/// Acquire a metadata slot for a request to `url`.
///
/// `None` means the request is not mirror-bound and needs no permit, which keeps
/// the official sources and the GitHub proxy unthrottled. Hold the permit until
/// the response body has been read.
pub async fn acquire_mcim_api_permit_for(url: &str) -> Option<SemaphorePermit<'static>> {
  if !is_mcim_url(url) {
    return None;
  }
  MCIM_API_CONCURRENCY.acquire().await.ok()
}

/// Acquire a download slot for a file transfer from `url`.
///
/// Hold the permit for the whole transfer: an open mirror connection is exactly
/// what the budget is meant to limit.
pub async fn acquire_mcim_download_permit_for(url: &str) -> Option<SemaphorePermit<'static>> {
  if !is_mcim_url(url) {
    return None;
  }
  MCIM_DOWNLOAD_CONCURRENCY.acquire().await.ok()
}

fn with_host(url: &Url, host: &str) -> Option<Url> {
  let mut replaced = url.clone();
  // `set_host` keeps the raw (percent-encoded) path and the query untouched,
  // unlike rebuilding the URL from `url.path()`, which would re-encode escapes.
  replaced.set_host(Some(host)).ok()?;
  Some(replaced)
}

/// Map a file download URL between the official CDN and the MCIM mirror.
///
/// Returns `None` for hosts that MCIM does not mirror; in particular
/// `mediafilez.forgecdn.net` must stay untouched, as documented by MCIM.
pub fn content_file_url(url: &Url, target: ContentSource) -> Option<Url> {
  if url.scheme() != "https" {
    return None;
  }
  let host = url.host_str()?;
  match target {
    ContentSource::Mcim => match host {
      MODRINTH_CDN_HOST | FORGE_CDN_HOST => with_host(url, MCIM_HOST),
      _ => None,
    },
    ContentSource::Official => match host {
      MCIM_HOST => {
        let path = url.path();
        // `/data/...` is the Modrinth file CDN, `/files/<a>/<b>/<name>` the
        // CurseForge one. `/modrinth/`, `/curseforge/` and `/avatars/` are API
        // or avatar routes and have no single official CDN equivalent.
        if path.starts_with("/data/") {
          with_host(url, MODRINTH_CDN_HOST)
        } else if path.starts_with("/files/") {
          with_host(url, FORGE_CDN_HOST)
        } else {
          None
        }
      }
      _ => None,
    },
  }
}

/// The `(official, mirror)` forms of a mirrored file URL, accepting either form
/// as input.
pub fn content_file_variants(url: &Url) -> Option<(Url, Url)> {
  if let Some(mirror) = content_file_url(url, ContentSource::Mcim) {
    return Some((url.clone(), mirror));
  }
  content_file_url(url, ContentSource::Official).map(|official| (official, url.clone()))
}

/// Ordered download candidates for a resource file URL.
///
/// URLs that MCIM does not mirror are returned unchanged so callers can still
/// download them, and duplicates are removed to keep the retry loop cheap.
pub fn content_file_source_candidates(url: &Url, priority: &[ContentSource]) -> Vec<Url> {
  let Some((official, mirror)) = content_file_variants(url) else {
    return vec![url.clone()];
  };

  let mut candidates: Vec<Url> = Vec::with_capacity(priority.len());
  for source in priority {
    let candidate = match source {
      ContentSource::Official => official.clone(),
      ContentSource::Mcim => mirror.clone(),
    };
    if !candidates.contains(&candidate) {
      candidates.push(candidate);
    }
  }

  if candidates.is_empty() {
    candidates.push(url.clone());
  }

  candidates
}

#[cfg(test)]
mod tests {
  use super::{
    acquire_mcim_api_permit_for, content_file_source_candidates, content_file_url, is_mcim_url,
    ContentSource, CURSEFORGE_MCIM_API_BASE, CURSEFORGE_OFFICIAL_API_BASE,
    MCIM_MAX_CONCURRENT_API_REQUESTS, MODRINTH_MCIM_API_BASE, MODRINTH_OFFICIAL_API_BASE,
  };
  use url::Url;

  fn url(input: &str) -> Url {
    Url::parse(input).unwrap()
  }

  #[test]
  fn only_mirror_hosts_are_recognized_as_mcim() {
    assert!(is_mcim_url("https://mod.mcimirror.top/curseforge/v1/mods"));
    assert!(is_mcim_url("https://mcimirror.top/data/x.jar"));
    assert!(is_mcim_url("https://a.b.mcimirror.top/data/x.jar"));
    // Lookalikes and the official hosts must stay unthrottled.
    assert!(!is_mcim_url("https://mod.mcimirror.top.example.com/x.jar"));
    assert!(!is_mcim_url("https://notmcimirror.top/x.jar"));
    assert!(!is_mcim_url("https://cdn.modrinth.com/data/x.jar"));
    assert!(!is_mcim_url("https://api.curseforge.com/v1/mods"));
    assert!(!is_mcim_url("not a url"));
  }

  #[tokio::test]
  async fn non_mirror_urls_reserve_no_budget() {
    // A permit would otherwise be held for the whole request, so a `None` here
    // is what keeps official sources from being throttled.
    assert!(
      acquire_mcim_api_permit_for("https://cdn.modrinth.com/x.jar")
        .await
        .is_none()
    );
    assert!(
      acquire_mcim_api_permit_for("https://api.curseforge.com/v1/mods")
        .await
        .is_none()
    );
  }

  #[tokio::test]
  async fn mirror_requests_are_capped_and_released() {
    let url = "https://mod.mcimirror.top/curseforge/v1/mods";
    let mut permits = Vec::new();
    for _ in 0..MCIM_MAX_CONCURRENT_API_REQUESTS {
      permits.push(
        acquire_mcim_api_permit_for(url)
          .await
          .expect("the configured budget is at least one"),
      );
    }

    // The budget is exhausted, so one more request has to wait rather than
    // adding to the load on the mirror.
    assert!(tokio::time::timeout(
      std::time::Duration::from_millis(50),
      acquire_mcim_api_permit_for(url)
    )
    .await
    .is_err());

    // Dropping one permit hands its slot to the waiting request.
    permits.pop();
    assert!(tokio::time::timeout(
      std::time::Duration::from_secs(1),
      acquire_mcim_api_permit_for(url)
    )
    .await
    .is_ok());
  }

  #[test]
  fn api_bases_follow_the_documented_replacement_table() {
    assert_eq!(MODRINTH_OFFICIAL_API_BASE, "https://api.modrinth.com/v2");
    assert_eq!(
      MODRINTH_MCIM_API_BASE,
      "https://mod.mcimirror.top/modrinth/v2"
    );
    assert_eq!(
      CURSEFORGE_OFFICIAL_API_BASE,
      "https://api.curseforge.com/v1"
    );
    assert_eq!(
      CURSEFORGE_MCIM_API_BASE,
      "https://mod.mcimirror.top/curseforge/v1"
    );
  }

  #[test]
  fn modrinth_cdn_files_map_to_the_mirror_and_back() {
    let official = url("https://cdn.modrinth.com/data/AANobbMI/versions/RncWhTxD/sodium.jar");
    let mirror = content_file_url(&official, ContentSource::Mcim).unwrap();
    assert_eq!(
      mirror.as_str(),
      "https://mod.mcimirror.top/data/AANobbMI/versions/RncWhTxD/sodium.jar"
    );
    assert_eq!(
      content_file_url(&mirror, ContentSource::Official).unwrap(),
      official
    );
  }

  #[test]
  fn forge_cdn_files_map_to_the_mirror_and_back() {
    let official = url("https://edge.forgecdn.net/files/8285/794/geckolib.jar");
    let mirror = content_file_url(&official, ContentSource::Mcim).unwrap();
    assert_eq!(
      mirror.as_str(),
      "https://mod.mcimirror.top/files/8285/794/geckolib.jar"
    );
    assert_eq!(
      content_file_url(&mirror, ContentSource::Official).unwrap(),
      official
    );
  }

  #[test]
  fn mediafilez_is_never_replaced() {
    let mediafilez = url("https://mediafilez.forgecdn.net/files/8285/794/geckolib.jar");
    assert!(content_file_url(&mediafilez, ContentSource::Mcim).is_none());
    assert_eq!(
      content_file_source_candidates(&mediafilez, &[ContentSource::Mcim]),
      vec![mediafilez]
    );
  }

  #[test]
  fn candidates_are_ordered_by_priority_and_deduplicated() {
    let official = url("https://cdn.modrinth.com/data/example/file.jar");

    let mirror_first =
      content_file_source_candidates(&official, &[ContentSource::Mcim, ContentSource::Official]);
    assert_eq!(
      mirror_first,
      vec![
        url("https://mod.mcimirror.top/data/example/file.jar"),
        official.clone(),
      ]
    );

    let official_first =
      content_file_source_candidates(&official, &[ContentSource::Official, ContentSource::Mcim]);
    assert_eq!(
      official_first,
      vec![
        official.clone(),
        url("https://mod.mcimirror.top/data/example/file.jar"),
      ]
    );

    // A mirror URL yields the same pair, no matter which form was the input.
    let from_mirror = content_file_source_candidates(
      &url("https://mod.mcimirror.top/data/example/file.jar"),
      &[ContentSource::Mcim, ContentSource::Official],
    );
    assert_eq!(from_mirror, mirror_first);
  }

  #[test]
  fn unmappable_urls_are_left_alone() {
    let other = url("https://example.com/file.jar");
    assert_eq!(
      content_file_source_candidates(&other, &[ContentSource::Mcim, ContentSource::Official]),
      vec![other]
    );

    // API routes on the mirror host have no file CDN equivalent.
    let api = url("https://mod.mcimirror.top/curseforge/v1/mods/388172");
    assert!(content_file_url(&api, ContentSource::Official).is_none());
    assert_eq!(
      content_file_source_candidates(&api, &[ContentSource::Mcim, ContentSource::Official]),
      vec![api]
    );
  }
}
