use crate::error::USTBLError;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri_plugin_http::reqwest;
use tauri_plugin_http::reqwest::cookie::{CookieStore, Jar};

const DEFAULT_API_TIMEOUT: u64 = 30;
const LINK_TOKEN_COOKIE_PREFIX: &str = "link_token:";

/// Build a dedicated reqwest client with a shared cookie jar for Anyshare operations.
/// The cookie jar is required because the share link sets a `link_token` cookie during
/// the initial page visit (often during redirects), which must be retained for
/// subsequent API calls. We return both the client and the jar so we can extract
/// the token from the jar directly (since `resp.cookies()` misses cookies set during
/// intermediate redirects).
pub fn build_anyshare_client_with_jar() -> Result<(reqwest::Client, Arc<Jar>), USTBLError> {
  let jar = Arc::new(Jar::default());
  let client = reqwest::ClientBuilder::new()
    .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT))
    .cookie_provider(jar.clone())
    .build()
    .map_err(|e| USTBLError(format!("Failed to build Anyshare client: {}", e)))?;
  Ok((client, jar))
}

/// Extract the link ID from a share URL like `https://yunpan.ustb.edu.cn/link/AA96B8...`
pub fn extract_link_id(link_url: &str) -> Result<String, USTBLError> {
  let re = Regex::new(r"/link/([A-Za-z0-9]{30,64})").map_err(|e| USTBLError(e.to_string()))?;
  if let Some(caps) = re.captures(link_url) {
    return Ok(caps[1].to_string());
  }
  Err(USTBLError(
    "Could not find link id in the URL.".to_string(),
  ))
}

/// Extract the base URL (scheme + host) from a share URL.
pub fn base_url_from_link(link_url: &str) -> Result<String, USTBLError> {
  let parsed = url::Url::parse(link_url).map_err(|e| USTBLError(e.to_string()))?;
  if parsed.scheme().is_empty() || parsed.host_str().is_none() {
    return Err(USTBLError(
      "Link must include scheme and host.".to_string(),
    ));
  }
  Ok(format!(
    "{}://{}",
    parsed.scheme(),
    parsed.host_str().unwrap()
  ))
}

/// Parse a `Cookie` header value to extract a specific cookie's value.
/// The header format is: `name1=value1; name2=value2; ...`
pub fn extract_token_from_cookie_header(header_str: &str, cookie_name: &str) -> Option<String> {
  let prefix = format!("{}=", cookie_name);
  for pair in header_str.split(';') {
    let pair = pair.trim();
    if pair.starts_with(&prefix) {
      return Some(pair[prefix.len()..].to_string());
    }
  }
  None
}

/// Visit the share URL to obtain the `link_token` cookie, then return its value.
/// The cookie is set by the server when the share page is visited, often during
/// HTTP redirects. We extract it from the shared cookie jar (not from the response
/// object) because `resp.cookies()` only includes cookies from the final response,
/// missing those set during intermediate redirects.
pub async fn ensure_link_token(
  client: &reqwest::Client,
  jar: &Arc<Jar>,
  share_url: &str,
  base_url: &str,
  link_id: &str,
) -> Result<String, USTBLError> {
  let cookie_name = format!("{}{}", LINK_TOKEN_COOKIE_PREFIX, link_id);
  let base_parsed_url = url::Url::parse(base_url).map_err(|e| USTBLError(e.to_string()))?;

  // First attempt: GET the share URL directly
  let _resp = client
    .get(share_url)
    .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT))
    .send()
    .await
    .map_err(|e| USTBLError(e.to_string()))?;

  // The cookie jar automatically stores cookies from Set-Cookie headers,
  // including those set during intermediate redirects.
  let cookie_header: Option<reqwest::header::HeaderValue> = jar.cookies(&base_parsed_url);
  if let Some(header_val) = cookie_header {
    if let Ok(header_str) = header_val.to_str() {
      if let Some(token) = extract_token_from_cookie_header(header_str, &cookie_name) {
        log::info!("Obtained Anyshare link_token from primary URL");
        return Ok(token);
      }
    }
  }

  // Second attempt: alternative URL format
  let alt_url = format!("{}/anyshare/zh-cn/link/{}", base_url, link_id);
  let _resp = client
    .get(&alt_url)
    .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT))
    .send()
    .await
    .map_err(|e| USTBLError(e.to_string()))?;

  let cookie_header2: Option<reqwest::header::HeaderValue> = jar.cookies(&base_parsed_url);
  if let Some(header_val) = cookie_header2 {
    if let Ok(header_str) = header_val.to_str() {
      if let Some(token) = extract_token_from_cookie_header(header_str, &cookie_name) {
        log::info!("Obtained Anyshare link_token from alternative URL");
        return Ok(token);
      }
    }
  }

  Err(USTBLError(
    "Failed to obtain link token. The link may be invalid or expired.".to_string(),
  ))
}

/// Check that the share link does not require a password.
pub async fn check_share_info(
  client: &reqwest::Client,
  base_url: &str,
  link_id: &str,
) -> Result<(), USTBLError> {
  let url = format!("{}/api/shared-link/v1/links/{}", base_url, link_id);
  let resp = client
    .get(&url)
    .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT))
    .send()
    .await
    .map_err(|e| USTBLError(e.to_string()))?;

  if !resp.status().is_success() {
    return Ok(()); // can't verify, proceed anyway
  }

  let data: Value = resp.json().await.map_err(|e| USTBLError(e.to_string()))?;
  if data
    .get("password_required")
    .and_then(|v| v.as_bool())
    .unwrap_or(false)
  {
    return Err(USTBLError(
      "This share link requires a password and is not supported.".to_string(),
    ));
  }

  Ok(())
}

/// Build the API headers with Bearer token.
fn api_headers(token: &str) -> reqwest::header::HeaderMap {
  let mut headers = reqwest::header::HeaderMap::new();
  if let Ok(val) = format!("Bearer {}", token).parse() {
    headers.insert(reqwest::header::AUTHORIZATION, val);
  }
  if let Ok(val) = "XMLHttpRequest".parse() {
    headers.insert("X-Requested-With", val);
  }
  headers
}

/// GET an API endpoint and return parsed JSON.
async fn api_get_json(
  client: &reqwest::Client,
  base_url: &str,
  token: &str,
  path: &str,
  params: Option<&[(&str, &str)]>,
) -> Result<Value, USTBLError> {
  let url = format!(
    "{}/api/{}",
    base_url,
    path.strip_prefix('/').unwrap_or(path)
  );
  let mut req = client
    .get(&url)
    .headers(api_headers(token))
    .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT));

  if let Some(p) = params {
    req = req.query(p);
  }

  let resp = req.send().await.map_err(|e| USTBLError(e.to_string()))?;

  if !resp.status().is_success() {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    return Err(USTBLError(format!(
      "API GET {} failed: HTTP {} {}",
      path, status, body
    )));
  }

  resp
    .json::<Value>()
    .await
    .map_err(|e| USTBLError(e.to_string()))
}

/// POST JSON to an API endpoint and return parsed response.
async fn api_post_json(
  client: &reqwest::Client,
  base_url: &str,
  token: &str,
  path: &str,
  payload: &Value,
) -> Result<Value, USTBLError> {
  let url = format!(
    "{}/api/{}",
    base_url,
    path.strip_prefix('/').unwrap_or(path)
  );
  let resp = client
    .post(&url)
    .headers(api_headers(token))
    .json(payload)
    .timeout(std::time::Duration::from_secs(DEFAULT_API_TIMEOUT))
    .send()
    .await
    .map_err(|e| USTBLError(e.to_string()))?;

  if !resp.status().is_success() {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    return Err(USTBLError(format!(
      "API POST {} failed: HTTP {} {}",
      path, status, body
    )));
  }

  resp
    .json::<Value>()
    .await
    .map_err(|e| USTBLError(e.to_string()))
}

/// Get the root entry item (folder) from the share link.
async fn get_entry_item(
  client: &reqwest::Client,
  base_url: &str,
  token: &str,
) -> Result<Value, USTBLError> {
  let data = api_get_json(client, base_url, token, "/efast/v1/entry-item", None).await?;
  // The response is an array; return the first entry
  if let Some(arr) = data.as_array() {
    if let Some(first) = arr.first() {
      return Ok(first.clone());
    }
  }
  Err(USTBLError(
    "Entry item not found in share link.".to_string(),
  ))
}

/// Extract the docid from a folder/file item.
fn item_docid(item: &Value) -> Option<String> {
  item
    .get("docid")
    .or_else(|| item.get("id"))
    .and_then(|v| v.as_str())
    .map(|s| s.to_string())
}

/// A single file or directory item returned from the folder listing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnyshareFolderItem {
  pub docid: String,
  pub name: String,
  pub size: Option<i64>,
  pub is_dir: bool,
}

/// Download info returned by the osdownload endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnyshareDownloadInfo {
  pub method: String,
  pub url: String,
  pub headers: HashMap<String, String>,
  pub file_name: String,
}

/// List all files and subdirectories in a folder (handles pagination).
async fn list_folder(
  client: &reqwest::Client,
  base_url: &str,
  token: &str,
  folder_docid: &str,
) -> Result<(Vec<AnyshareFolderItem>, Vec<AnyshareFolderItem>), USTBLError> {
  let mut dirs = Vec::new();
  let mut files = Vec::new();
  let mut marker = String::new();

  loop {
    let encoded_docid = urlencoding::encode(folder_docid);
    let path = format!("/efast/v1/folders/{}/sub_objects", encoded_docid);

    let mut params: Vec<(&str, &str)> = vec![
      ("limit", "100"),
      ("sort", "name"),
      ("direction", "asc"),
      ("permission_attributes_required", "false"),
    ];
    if !marker.is_empty() {
      params.push(("marker", &marker));
    }

    let data =
      api_get_json(client, base_url, token, &path, Some(&params)).await?;

    // Process directories
    if let Some(dir_arr) = data.get("dirs").and_then(|v| v.as_array()) {
      for dir_item in dir_arr {
        if let Some(docid) = item_docid(dir_item) {
          let name = dir_item["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
          dirs.push(AnyshareFolderItem {
            docid,
            name,
            size: None,
            is_dir: true,
          });
        }
      }
    }

    // Process files
    if let Some(file_arr) = data.get("files").and_then(|v| v.as_array()) {
      for file_item in file_arr {
        if let Some(docid) = item_docid(file_item) {
          let name = file_item["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
          let size = file_item["size"].as_i64();
          files.push(AnyshareFolderItem {
            docid,
            name,
            size,
            is_dir: false,
          });
        }
      }
    }

    // Check for next page
    marker = data
      .get("next_marker")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();

    if marker.is_empty() {
      break;
    }
  }

  Ok((dirs, files))
}

/// Get the download URL for a file by calling the osdownload endpoint.
pub async fn get_download_url(
  client: &reqwest::Client,
  base_url: &str,
  token: &str,
  docid: &str,
  savename: &str,
) -> Result<AnyshareDownloadInfo, USTBLError> {
  let payload = serde_json::json!({
    "docid": docid,
    "authtype": "1",
    "savename": savename,
    "usehttps": true,
  });

  let data =
    api_post_json(client, base_url, token, "/efast/v1/file/osdownload", &payload).await?;

  let authrequest = data.get("authrequest").ok_or_else(|| {
    USTBLError("Unexpected download authrequest format: missing authrequest.".to_string())
  })?;

  let arr = authrequest
    .as_array()
    .ok_or_else(|| USTBLError("Unexpected download authrequest format: not an array.".to_string()))?;

  if arr.len() < 2 {
    return Err(USTBLError(
      "Unexpected download authrequest format: too short.".to_string(),
    ));
  }

  let method = arr[0].as_str().unwrap_or("GET").to_string();
  let url = arr[1].as_str().unwrap_or_default().to_string();

  // Parse extra headers from authrequest[2..]
  let mut headers = HashMap::new();
  if arr.len() > 2 {
    for entry in &arr[2..] {
      if let Some(entry_str) = entry.as_str() {
        // Try ": " separator first, then ":"
        if let Some((key, value)) = entry_str.split_once(": ") {
          headers.insert(key.trim().to_string(), value.trim().to_string());
        } else if let Some((key, value)) = entry_str.split_once(':') {
          headers.insert(key.trim().to_string(), value.trim().to_string());
        }
      }
    }
  }

  Ok(AnyshareDownloadInfo {
    method,
    url,
    headers,
    file_name: savename.to_string(),
  })
}

/// High-level function: list all files in the share link's root folder.
/// Returns both directories and files.
pub async fn list_share_folder(
  share_url: &str,
) -> Result<Vec<AnyshareFolderItem>, USTBLError> {
  let (client, jar) = build_anyshare_client_with_jar()?;
  let link_id = extract_link_id(share_url)?;
  let base_url = base_url_from_link(share_url)?;

  check_share_info(&client, &base_url, &link_id).await?;
  let token = ensure_link_token(&client, &jar, share_url, &base_url, &link_id).await?;
  let entry = get_entry_item(&client, &base_url, &token).await?;

  let root_docid = item_docid(&entry).ok_or_else(|| {
    USTBLError("Root folder docid not found.".to_string())
  })?;

  let (dirs, files) = list_folder(&client, &base_url, &token, &root_docid).await?;

  // Combine dirs and files into a single list
  let mut items = Vec::new();
  items.extend(dirs);
  items.extend(files);

  Ok(items)
}
