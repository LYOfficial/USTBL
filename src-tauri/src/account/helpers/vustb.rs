use crate::account::helpers::authlib_injector::constants::USTB_AUTH_SERVER_URL;
use crate::account::helpers::authlib_injector::info::get_auth_server_info_by_url;
use crate::account::helpers::authlib_injector::oauth::{self, OAuthProfileLogin};
use crate::account::models::{
  AccountError, AccountInfo, AuthServer, OAuthTokens, VustbAccount, VustbCheckinResult,
  VustbFriend, VustbProfile, VustbProgression, VustbSession, VustbTexture, VustbTexturePage,
};
use crate::error::{USTBLError, USTBLResult};
use crate::storage::Storage;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest::{self, RequestBuilder};

const VUSTB_ISSUER: &str = "https://www.ustb.world";
static SESSION_REFRESH_LOCK: LazyLock<tokio::sync::Mutex<()>> =
  LazyLock::new(|| tokio::sync::Mutex::new(()));

#[derive(Deserialize)]
struct LauncherAccountResponse {
  id: u64,
  username: String,
  #[serde(default)]
  display_name: String,
  #[serde(default)]
  avatar_url: String,
  #[serde(default)]
  last_checkin: Option<String>,
  #[serde(default)]
  progression: VustbProgression,
}

#[derive(Deserialize)]
struct UserInfoResponse {
  #[serde(default, alias = "group", alias = "userGroup")]
  user_group: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProfilesResponse {
  Wrapped { profiles: Vec<VustbProfile> },
  Data { data: Vec<VustbProfile> },
  Single(VustbProfile),
  List(Vec<VustbProfile>),
}

impl ProfilesResponse {
  fn into_profiles(self) -> Vec<VustbProfile> {
    match self {
      Self::Wrapped { profiles } | Self::List(profiles) => profiles,
      Self::Data { data } => data,
      Self::Single(profile) => vec![profile],
    }
  }
}

#[derive(Deserialize)]
struct CheckinResponse {
  #[serde(default)]
  message: String,
  #[serde(default)]
  experience_gained: u32,
}

#[derive(Deserialize)]
struct LauncherFriendResponse {
  friendship_id: u64,
  id: u64,
  username: String,
  display_name: String,
  avatar_url: String,
  online: bool,
  instance_name: Option<String>,
  last_seen_at: Option<String>,
}

fn map_status(status: reqwest::StatusCode) -> AccountError {
  if status == reqwest::StatusCode::UNAUTHORIZED {
    AccountError::Expired
  } else if status == reqwest::StatusCode::FORBIDDEN {
    AccountError::Forbidden
  } else {
    AccountError::NetworkError
  }
}

fn response_error(status: reqwest::StatusCode, value: Option<serde_json::Value>) -> USTBLError {
  let detail = value.and_then(|value| match value.get("detail") {
    Some(serde_json::Value::String(detail)) => Some(detail.clone()),
    Some(detail) => Some(detail.to_string()),
    None => value
      .get("error_description")
      .and_then(|detail| detail.as_str())
      .map(str::to_string),
  });
  detail
    .map(USTBLError)
    .unwrap_or_else(|| map_status(status).into())
}

fn absolute_vustb_url(url: String) -> String {
  if url.starts_with("http://") || url.starts_with("https://") {
    url
  } else if url.starts_with('/') {
    format!("{VUSTB_ISSUER}{url}")
  } else if url.is_empty() {
    String::new()
  } else {
    format!("{VUSTB_ISSUER}/{url}")
  }
}

async fn parse_json_response<T: DeserializeOwned>(
  response: reqwest::Response,
  endpoint: &str,
) -> USTBLResult<T> {
  if !response.status().is_success() {
    let status = response.status();
    log::error!(
      "vUSTB account request failed: endpoint={endpoint}, status={}",
      status
    );
    let value = response.json::<serde_json::Value>().await.ok();
    return Err(response_error(status, value));
  }

  let value = response
    .json::<serde_json::Value>()
    .await
    .map_err(|error| {
      log::error!("vUSTB account JSON parse failed: endpoint={endpoint}, error={error}");
      AccountError::ParseError
    })?;
  let shape = match &value {
    serde_json::Value::Object(object) => {
      format!("object keys={:?}", object.keys().collect::<Vec<_>>())
    }
    serde_json::Value::Array(array) => format!("array len={}", array.len()),
    _ => "scalar".to_string(),
  };
  log::debug!("vUSTB account response parsed: endpoint={endpoint}, {shape}");
  serde_json::from_value(value).map_err(|error| {
    log::error!("vUSTB account fields parse failed: endpoint={endpoint}, error={error}");
    AccountError::ParseError.into()
  })
}

async fn get_json_with_token<T: DeserializeOwned>(
  app: &AppHandle,
  endpoint: &str,
  access_token: &str,
) -> USTBLResult<T> {
  let client = app.state::<reqwest::Client>();
  let response = client
    .get(format!("{VUSTB_ISSUER}{endpoint}"))
    .bearer_auth(access_token)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?;
  parse_json_response(response, endpoint).await
}

fn tokens_from_state(state: &AccountInfo) -> USTBLResult<OAuthTokens> {
  if let Some(session) = state
    .vustb_session
    .as_ref()
    .filter(|session| !session.access_token.is_empty())
  {
    return Ok(OAuthTokens {
      access_token: session.access_token.clone(),
      refresh_token: session.refresh_token.clone(),
      id_token: None,
    });
  }

  let account = state.vustb_account.as_ref().ok_or(AccountError::NotFound)?;
  let player = state
    .players
    .iter()
    .find(|player| player.id == account.player_id)
    .ok_or(AccountError::Expired)?;
  Ok(OAuthTokens {
    access_token: player.access_token.clone().ok_or(AccountError::Expired)?,
    refresh_token: player.refresh_token.clone(),
    id_token: None,
  })
}

pub fn stored_tokens(app: &AppHandle) -> USTBLResult<OAuthTokens> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let state = binding.lock()?;
  tokens_from_state(&state)
}

pub fn store_tokens(app: &AppHandle, tokens: &OAuthTokens) -> USTBLResult<()> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let mut state = binding.lock()?;
  state.vustb_session = Some(VustbSession {
    access_token: tokens.access_token.clone(),
    refresh_token: tokens.refresh_token.clone(),
  });
  for player in state.players.iter_mut().filter(|player| {
    player
      .auth_server_url
      .as_deref()
      .is_some_and(|url| url.trim_end_matches('/') == USTB_AUTH_SERVER_URL.trim_end_matches('/'))
  }) {
    player.access_token = Some(tokens.access_token.clone());
    player.refresh_token = tokens.refresh_token.clone();
  }
  state.save()?;
  Ok(())
}

fn access_token_was_replaced(failed_access_token: Option<&str>, current: &OAuthTokens) -> bool {
  failed_access_token.is_some_and(|token| token != current.access_token)
}

async fn refresh_session_after(
  app: &AppHandle,
  failed_access_token: Option<&str>,
) -> USTBLResult<OAuthTokens> {
  let _refresh_guard = SESSION_REFRESH_LOCK.lock().await;
  let current = stored_tokens(app)?;
  if access_token_was_replaced(failed_access_token, &current) {
    return Ok(current);
  }
  let refresh_token = current
    .refresh_token
    .filter(|token| !token.is_empty())
    .ok_or(AccountError::Expired)?;
  let auth_server = AuthServer::from(get_auth_server_info_by_url(
    app,
    USTB_AUTH_SERVER_URL.to_string(),
  )?);
  let tokens = oauth::refresh_tokens(
    app,
    refresh_token,
    auth_server.client_id,
    auth_server.features.openid_configuration_url,
    auth_server.redirect_uri,
    auth_server.client_secret,
  )
  .await?;
  store_tokens(app, &tokens)?;
  Ok(tokens)
}

pub async fn load_all_profiles(app: &AppHandle) -> USTBLResult<OAuthProfileLogin> {
  let current = stored_tokens(app)?;
  let first_attempt =
    oauth::load_all_profiles(app, USTB_AUTH_SERVER_URL.to_string(), current.clone()).await;
  let login = match first_attempt {
    Ok(login) => login,
    Err(error)
      if current
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.is_empty()) =>
    {
      log::debug!("Refreshing vUSTB session after profile sync failed: {error:?}");
      let refreshed = refresh_session_after(app, Some(&current.access_token)).await?;
      oauth::load_all_profiles(app, USTB_AUTH_SERVER_URL.to_string(), refreshed).await?
    }
    Err(error) => return Err(error),
  };
  store_tokens(app, &login.tokens)?;
  Ok(login)
}

pub async fn send_authenticated<F>(
  app: &AppHandle,
  build_request: F,
) -> USTBLResult<reqwest::Response>
where
  F: Fn(&reqwest::Client, &str) -> RequestBuilder,
{
  let client = app.state::<reqwest::Client>();
  let current = stored_tokens(app)?;
  let response = build_request(&client, &current.access_token)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?;
  if response.status() != reqwest::StatusCode::UNAUTHORIZED {
    return Ok(response);
  }

  let refreshed = refresh_session_after(app, Some(&current.access_token)).await?;
  build_request(&client, &refreshed.access_token)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError.into())
}

async fn get_authenticated<T: DeserializeOwned>(app: &AppHandle, endpoint: &str) -> USTBLResult<T> {
  let response = send_authenticated(app, |client, access_token| {
    client
      .get(format!("{VUSTB_ISSUER}{endpoint}"))
      .bearer_auth(access_token)
  })
  .await?;
  parse_json_response(response, endpoint).await
}

pub async fn post_authenticated<B: Serialize, T: DeserializeOwned>(
  app: &AppHandle,
  endpoint: &str,
  body: &B,
) -> USTBLResult<T> {
  let response = send_authenticated(app, |client, access_token| {
    client
      .post(format!("{VUSTB_ISSUER}{endpoint}"))
      .bearer_auth(access_token)
      .json(body)
  })
  .await?;
  parse_json_response(response, endpoint).await
}

pub async fn put_authenticated<B: Serialize, T: DeserializeOwned>(
  app: &AppHandle,
  endpoint: &str,
  body: &B,
) -> USTBLResult<T> {
  let response = send_authenticated(app, |client, access_token| {
    client
      .put(format!("{VUSTB_ISSUER}{endpoint}"))
      .bearer_auth(access_token)
      .json(body)
  })
  .await?;
  parse_json_response(response, endpoint).await
}

pub async fn delete_authenticated<T: DeserializeOwned>(
  app: &AppHandle,
  endpoint: &str,
) -> USTBLResult<T> {
  let response = send_authenticated(app, |client, access_token| {
    client
      .delete(format!("{VUSTB_ISSUER}{endpoint}"))
      .bearer_auth(access_token)
  })
  .await?;
  parse_json_response(response, endpoint).await
}

pub async fn fetch_account(
  app: &AppHandle,
  access_token: &str,
  player_id: String,
) -> USTBLResult<VustbAccount> {
  let user_info: LauncherAccountResponse =
    get_json_with_token(app, "/api/launcher/account", access_token).await?;
  let permission: UserInfoResponse =
    get_json_with_token(app, "/oauth/userinfo", access_token).await?;
  let profiles: ProfilesResponse =
    get_json_with_token(app, "/oauth/profiles", access_token).await?;
  Ok(VustbAccount {
    subject: user_info.id.to_string(),
    username: if user_info.display_name.is_empty() {
      user_info.username
    } else {
      user_info.display_name
    },
    avatar_url: absolute_vustb_url(user_info.avatar_url),
    user_group: permission.user_group,
    profiles: profiles.into_profiles(),
    progression: user_info.progression,
    last_checkin: user_info.last_checkin,
    player_id,
  })
}

pub async fn fetch_current_account(
  app: &AppHandle,
  player_id: String,
) -> USTBLResult<VustbAccount> {
  let user_info: LauncherAccountResponse = get_authenticated(app, "/api/launcher/account").await?;
  let permission: UserInfoResponse = get_authenticated(app, "/oauth/userinfo").await?;
  let profiles: ProfilesResponse = get_authenticated(app, "/oauth/profiles").await?;
  Ok(VustbAccount {
    subject: user_info.id.to_string(),
    username: if user_info.display_name.is_empty() {
      user_info.username
    } else {
      user_info.display_name
    },
    avatar_url: absolute_vustb_url(user_info.avatar_url),
    user_group: permission.user_group,
    profiles: profiles.into_profiles(),
    progression: user_info.progression,
    last_checkin: user_info.last_checkin,
    player_id,
  })
}

pub async fn checkin(app: &AppHandle, player_id: String) -> USTBLResult<VustbCheckinResult> {
  let response: CheckinResponse =
    post_authenticated(app, "/api/launcher/checkin", &serde_json::json!({})).await?;
  let account = fetch_current_account(app, player_id).await?;
  Ok(VustbCheckinResult {
    message: response.message,
    experience_gained: response.experience_gained,
    account,
  })
}

pub async fn fetch_friends(app: &AppHandle) -> USTBLResult<Vec<VustbFriend>> {
  let friends: Vec<LauncherFriendResponse> =
    get_authenticated(app, "/api/community/launcher/friends").await?;
  Ok(
    friends
      .into_iter()
      .map(|friend| VustbFriend {
        friendship_id: friend.friendship_id,
        id: friend.id,
        username: friend.username,
        display_name: friend.display_name,
        avatar_url: absolute_vustb_url(friend.avatar_url),
        online: friend.online,
        instance_name: friend.instance_name,
        last_seen_at: friend.last_seen_at,
      })
      .collect(),
  )
}

fn normalize_texture_urls(items: &mut [VustbTexture]) {
  for item in items {
    item.url = absolute_vustb_url(std::mem::take(&mut item.url));
  }
}

pub async fn fetch_skin_library(
  app: &AppHandle,
  page: u32,
  limit: u32,
  texture_type: Option<String>,
) -> USTBLResult<VustbTexturePage> {
  let mut endpoint = format!("/api/launcher/skins/library?page={page}&limit={limit}");
  if let Some(texture_type) = texture_type.filter(|value| !value.is_empty()) {
    endpoint.push_str("&texture_type=");
    endpoint.push_str(&texture_type);
  }
  let mut result: VustbTexturePage = get_authenticated(app, &endpoint).await?;
  normalize_texture_urls(&mut result.items);
  Ok(result)
}

pub async fn fetch_wardrobe(
  app: &AppHandle,
  texture_type: Option<String>,
) -> USTBLResult<Vec<VustbTexture>> {
  let endpoint = texture_type
    .filter(|value| !value.is_empty())
    .map(|value| format!("/api/launcher/skins/wardrobe?texture_type={value}"))
    .unwrap_or_else(|| "/api/launcher/skins/wardrobe".to_string());
  let mut result: Vec<VustbTexture> = get_authenticated(app, &endpoint).await?;
  normalize_texture_urls(&mut result);
  Ok(result)
}

pub async fn collect_texture(app: &AppHandle, hash: &str) -> USTBLResult<()> {
  let _: serde_json::Value = post_authenticated(
    app,
    &format!("/api/launcher/skins/library/{hash}/collect"),
    &serde_json::json!({}),
  )
  .await?;
  Ok(())
}

pub async fn select_profile_texture(
  app: &AppHandle,
  profile_uuid: &str,
  texture_type: &str,
  hash: Option<&str>,
) -> USTBLResult<()> {
  let _: serde_json::Value = put_authenticated(
    app,
    &format!("/api/launcher/skins/profiles/{profile_uuid}/{texture_type}"),
    &serde_json::json!({ "hash": hash }),
  )
  .await?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::reqwest;
  use super::{access_token_was_replaced, response_error, tokens_from_state};
  use crate::account::helpers::authlib_injector::constants::USTB_AUTH_SERVER_URL;
  use crate::account::models::{
    AccountInfo, OAuthTokens, PlayerInfo, PlayerType, VustbAccount, VustbProgression, VustbSession,
  };
  use uuid::Uuid;

  fn account(player_id: &str) -> VustbAccount {
    VustbAccount {
      subject: "42".to_string(),
      username: "user".to_string(),
      avatar_url: String::new(),
      user_group: "user".to_string(),
      profiles: vec![],
      progression: VustbProgression::default(),
      last_checkin: None,
      player_id: player_id.to_string(),
    }
  }

  fn player(id: &str) -> PlayerInfo {
    PlayerInfo {
      id: id.to_string(),
      name: "player".to_string(),
      uuid: Uuid::nil(),
      player_type: PlayerType::ThirdParty,
      auth_account: None,
      auth_server_url: Some(USTB_AUTH_SERVER_URL.to_string()),
      access_token: Some("legacy-access".to_string()),
      access_token_expires: None,
      refresh_token: Some("legacy-refresh".to_string()),
      textures: vec![],
    }
  }

  #[test]
  fn independent_session_tokens_take_precedence() {
    let state = AccountInfo {
      players: vec![player("player-id")],
      auth_servers: vec![],
      vustb_account: Some(account("player-id")),
      vustb_session: Some(VustbSession {
        access_token: "session-access".to_string(),
        refresh_token: Some("session-refresh".to_string()),
      }),
      is_oauth_processing: false,
    };

    let tokens = tokens_from_state(&state).unwrap();
    assert_eq!(tokens.access_token, "session-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("session-refresh"));
  }

  #[test]
  fn legacy_player_tokens_are_used_during_migration() {
    let state = AccountInfo {
      players: vec![player("player-id")],
      auth_servers: vec![],
      vustb_account: Some(account("player-id")),
      vustb_session: None,
      is_oauth_processing: false,
    };

    let tokens = tokens_from_state(&state).unwrap();
    assert_eq!(tokens.access_token, "legacy-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("legacy-refresh"));
  }

  #[test]
  fn concurrent_refresh_reuses_tokens_already_replaced_by_another_request() {
    let current = OAuthTokens {
      access_token: "new-access".to_string(),
      refresh_token: Some("new-refresh".to_string()),
      id_token: None,
    };

    assert!(access_token_was_replaced(Some("old-access"), &current));
    assert!(!access_token_was_replaced(Some("new-access"), &current));
    assert!(!access_token_was_replaced(None, &current));
  }

  #[test]
  fn api_error_detail_is_preserved_for_reauthentication_prompt() {
    let error = response_error(
      reqwest::StatusCode::FORBIDDEN,
      Some(serde_json::json!({
        "detail": "当前启动器登录授权版本过旧，请退出像素北科账户后重新登录"
      })),
    );

    assert_eq!(
      error.0,
      "当前启动器登录授权版本过旧，请退出像素北科账户后重新登录"
    );
  }
}
