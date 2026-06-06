use crate::discover::helpers::anyshare::{
  get_download_url, list_share_folder, AnyshareDownloadInfo, AnyshareFolderItem,
};
use crate::discover::helpers::mc_news::{fetch_mc_news_page, MC_NEWS_ENDPOINT};
use crate::discover::helpers::rss::{fetch_rss_page, fetch_rss_source_info, is_rss_source};
use crate::discover::models::{NewsPostRequest, NewsPostResponse, NewsSourceInfo};
use crate::error::USTBLResult;
use crate::launcher_config::models::LauncherConfig;
use crate::utils::web::with_retry;
use futures::future;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use tauri_plugin_http::reqwest::cookie::CookieStore;

#[tauri::command]
pub async fn fetch_news_sources_info(app: AppHandle) -> USTBLResult<Vec<NewsSourceInfo>> {
  let post_sources = {
    let binding = app.state::<Mutex<LauncherConfig>>();
    let state = binding.lock().unwrap();
    state.discover_source_endpoints.clone()
  };

  let client = with_retry(app.state::<reqwest::Client>().inner().clone());

  let tasks: Vec<_> = post_sources
    .into_iter()
    .map(|(url, _)| {
      let client = client.clone();
      async move {
        if is_rss_source(&url) {
          return fetch_rss_source_info(&client, &url)
            .await
            .unwrap_or(NewsSourceInfo {
              name: "".to_string(),
              full_name: "".to_string(),
              endpoint_url: url.clone(),
              icon_src: "".to_string(),
            });
        }

        let mut news_source = NewsSourceInfo {
          name: "".to_string(),
          full_name: "".to_string(),
          endpoint_url: url.clone(),
          icon_src: "".to_string(),
        };

        let response = client
          .get(&url)
          .query(&[("pageSize", "0")]) // ?pageSize=0
          .send()
          .await;

        if let Ok(response) = response {
          let json_data: serde_json::Value = response.json().await.unwrap_or_default();

          if let Some(source_info) = json_data.get("sourceInfo") {
            news_source.name = source_info["name"].as_str().unwrap_or("").to_string();
            news_source.full_name = source_info["fullName"].as_str().unwrap_or("").to_string();
            news_source.icon_src = source_info["iconSrc"].as_str().unwrap_or("").to_string();
          }
        }

        news_source
      }
    })
    .collect();

  Ok(future::join_all(tasks).await)
}

#[tauri::command]
pub async fn fetch_news_post_summaries(
  app: AppHandle,
  requests: Vec<NewsPostRequest>,
) -> USTBLResult<NewsPostResponse> {
  let client = with_retry(app.state::<reqwest::Client>().inner().clone());
  let tasks: Vec<_> = requests
    .into_iter()
    .map(|NewsPostRequest { url, cursor }| {
      let client = client.clone();
      async move {
        if url.starts_with(MC_NEWS_ENDPOINT) {
          return fetch_mc_news_page(&client, &url, cursor).await;
        }

        if is_rss_source(&url) {
          return fetch_rss_page(&client, &url, cursor).await;
        }

        let mut req = client.get(&url).query(&[("pageSize", "12")]);

        if let Some(c) = cursor {
          req = req.query(&[("cursor", &c.to_string())]);
        }

        let resp = req.send().await;
        match resp {
          Ok(resp) if resp.status().is_success() => {
            let parsed: Result<NewsPostResponse, _> = resp.json().await;
            parsed.ok().map(|mut p| {
              for post in &mut p.posts {
                post.source.endpoint_url = url.clone();
              }
              (url.clone(), p)
            })
          }
          _ => None,
        }
      }
    })
    .collect();

  let results = futures::future::join_all(tasks).await;

  let mut all_posts = Vec::new();
  let mut cursors_map = HashMap::new();

  for result in results.into_iter().flatten() {
    let (url, post_response) = result;
    all_posts.extend(post_response.posts);
    if let Some(next_cursor) = post_response.next {
      cursors_map.insert(url, next_cursor);
    }
  }

  all_posts.sort_by(|a, b| b.create_at.cmp(&a.create_at));

  Ok(NewsPostResponse {
    posts: all_posts,
    next: None,
    cursors: Some(cursors_map),
  })
}

#[tauri::command]
pub async fn fetch_anyshare_folder_list(
  _app: AppHandle,
  share_url: String,
) -> USTBLResult<Vec<AnyshareFolderItem>> {
  list_share_folder(&share_url).await
}

#[tauri::command]
pub async fn fetch_anyshare_download_url(
  _app: AppHandle,
  share_url: String,
  docid: String,
  file_name: String,
) -> USTBLResult<AnyshareDownloadInfo> {
  // Build a dedicated client with cookie jar to obtain the link_token,
  // then call osdownload to get the pre-signed URL and required headers.
  // We include the link_token as a Cookie header in the response so the
  // frontend can pass it to the progressive download system's custom_headers.
  let (client, jar) = crate::discover::helpers::anyshare::build_anyshare_client_with_jar()?;
  let link_id = crate::discover::helpers::anyshare::extract_link_id(&share_url)?;
  let base_url = crate::discover::helpers::anyshare::base_url_from_link(&share_url)?;

  crate::discover::helpers::anyshare::check_share_info(&client, &base_url, &link_id).await?;
  let token =
    crate::discover::helpers::anyshare::ensure_link_token(&client, &jar, &share_url, &base_url, &link_id)
      .await?;

  let mut download_info = get_download_url(&client, &base_url, &token, &docid, &file_name).await?;

  // Add the link_token as a Cookie header so the progressive download task
  // can authenticate with the Anyshare server even though it uses the app's
  // shared reqwest client (which doesn't have the cookie jar).
  let cookie_name = format!("link_token:{}", link_id);
  let base_parsed_url = url::Url::parse(&base_url).map_err(|e| crate::error::USTBLError(e.to_string()))?;
  let cookie_header: Option<reqwest::header::HeaderValue> = jar.cookies(&base_parsed_url);
  if let Some(header_val) = cookie_header {
    if let Ok(header_str) = header_val.to_str() {
      if let Some(link_token_value) = crate::discover::helpers::anyshare::extract_token_from_cookie_header(header_str, &cookie_name) {
        download_info.headers.insert("Cookie".to_string(), format!("{}={}", cookie_name, link_token_value));
      }
    }
  }

  Ok(download_info)
}
