use crate::account::helpers::authlib_injector::constants::{
  CLIENT_IDS, USTB_AUTH_SERVER_URL, USTB_HOMEPAGE_URL, USTB_OPENID_CONFIGURATION_URL,
};
use crate::account::models::{AccountError, AccountInfo, AuthServerInfo};
use crate::error::SJMCLResult;
use crate::utils::web::normalize_url;
use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use url::Url;

fn is_ustb_auth_server(auth_url: &str) -> bool {
  normalize_url(auth_url) == normalize_url(USTB_AUTH_SERVER_URL)
}

fn apply_ustb_oauth_fallback(metadata: &mut Value) {
  if metadata.is_null() {
    *metadata = json!({});
  }

  if metadata.get("meta").is_none() || !metadata["meta"].is_object() {
    metadata["meta"] = json!({});
  }

  if metadata["meta"].get("serverName").is_none() {
    metadata["meta"]["serverName"] = json!("USTB Servers");
  }

  if metadata["meta"].get("links").is_none() || !metadata["meta"]["links"].is_object() {
    metadata["meta"]["links"] = json!({});
  }

  if metadata["meta"]["links"].get("homepage").is_none() {
    metadata["meta"]["links"]["homepage"] = json!(USTB_HOMEPAGE_URL);
  }

  if metadata["meta"]["links"].get("register").is_none() {
    metadata["meta"]["links"]["register"] = json!(USTB_HOMEPAGE_URL);
  }

  if metadata["meta"].get("feature.non_email_login").is_none() {
    metadata["meta"]["feature.non_email_login"] = json!(true);
  }

  if metadata["meta"].get("feature.openid_configuration_url").is_none() {
    metadata["meta"]["feature.openid_configuration_url"] = json!(USTB_OPENID_CONFIGURATION_URL);
  }
}

pub async fn fetch_auth_server_info(
  app: &AppHandle,
  auth_url: String,
) -> SJMCLResult<AuthServerInfo> {
  let client = app.state::<reqwest::Client>();
  match client.get(&auth_url).send().await {
    Ok(response) => {
      let mut metadata: Value = response.json().await.map_err(|_| AccountError::Invalid)?;

      if is_ustb_auth_server(&auth_url) {
        apply_ustb_oauth_fallback(&mut metadata);
      }

      let mut client_id = None;

      let openid_configuration_url = metadata["meta"]["feature.openid_configuration_url"]
        .as_str()
        .unwrap_or_default()
        .to_string();

      if !openid_configuration_url.is_empty() {
        let url = Url::parse(&auth_url).map_err(|_| AccountError::Invalid)?;

        if let Some(domain) = url.domain() {
          client_id = get_client_id(domain.to_string());
        }

        if client_id.is_none() {
          let response = client.get(&openid_configuration_url).send().await?;
          let data: serde_json::Value = response.json().await.map_err(|_| AccountError::Invalid)?;
          client_id = data["shared_client_id"].as_str().map(|s| s.to_string());
        }
      }

      Ok(AuthServerInfo {
        auth_url: auth_url.clone(),
        client_id,
        metadata,
        timestamp: chrono::Utc::now().timestamp_millis() as u64,
      })
    }
    Err(_) => Err(AccountError::Invalid.into()),
  }
}

pub fn get_client_id(domain: String) -> Option<String> {
  CLIENT_IDS
    .iter()
    .find(|(first, _)| first == &domain)
    .map(|(_, id)| id.to_string())
}

pub async fn fetch_auth_url(app: &AppHandle, root: Url) -> SJMCLResult<String> {
  let client = app.state::<reqwest::Client>();
  let response = client
    .get(root.clone())
    .send()
    .await
    .map_err(|_| AccountError::Invalid)?;

  if let Some(auth_url) = response.headers().get("X-Authlib-Injector-API-Location") {
    let auth_url_str = auth_url.to_str().unwrap_or_default();
    // try to parse auth_url_str as a relative URL and append it to the base URL or return it as is
    let full_url = root
      .join(auth_url_str)
      .map(|url| url.to_string())
      .unwrap_or_else(|_| auth_url_str.to_string());

    Ok(full_url)
  } else {
    Ok(root.to_string())
  }
}

pub async fn refresh_and_update_auth_servers(app: &AppHandle) -> SJMCLResult<()> {
  let account_binding = app.state::<Mutex<AccountInfo>>();
  let cloned_account_state = account_binding.lock()?.clone();

  let mut refreshed_auth_server_info_list =
    futures::future::join_all(cloned_account_state.auth_servers.iter().map(|info| async {
      if let Ok(refreshed_info) = fetch_auth_server_info(app, info.auth_url.clone()).await {
        refreshed_info
      } else {
        info.clone()
      }
    }))
    .await;

  refreshed_auth_server_info_list.retain(|info| !info.metadata.is_null()); // remove invalid servers

  let mut account_state = account_binding.lock()?;
  account_state.auth_servers = refreshed_auth_server_info_list;

  Ok(())
}

pub fn get_auth_server_info_by_url(
  app: &AppHandle,
  auth_url: String,
) -> SJMCLResult<AuthServerInfo> {
  let target = normalize_url(&auth_url);

  let account_binding = app.state::<Mutex<AccountInfo>>();
  let account_state = account_binding.lock()?;

  account_state
    .auth_servers
    .iter()
    .find(|server| normalize_url(&server.auth_url) == target)
    .cloned()
    .ok_or(AccountError::NotFound.into())
}
