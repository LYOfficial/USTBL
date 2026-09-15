use crate::account::helpers::authlib_injector::constants::USTB_AUTH_SERVER_URL;
use crate::account::helpers::authlib_injector::info::get_auth_server_info_by_url;
use crate::account::helpers::authlib_injector::oauth::{self, OAuthProfileLogin};
use crate::account::models::{
  AccountError, AccountInfo, AuthServer, OAuthTokens, VustbAccount, VustbCheckinResult,
  VustbProfile, VustbProgression, VustbSession,
};
use crate::error::{USTBLError, USTBLResult};
use crate::storage::Storage;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest::{self, RequestBuilder};

const VUSTB_ISSUER: &str = "https://www.ustb.world";

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

fn map_status(status: reqwest::StatusCode) -> AccountError {
  if status == reqwest::StatusCode::UNAUTHORIZED {
    AccountError::Expired
  } else if status == reqwest::StatusCode::FORBIDDEN {
    AccountError::Forbidden
  } else {
    AccountError::NetworkError
  }
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
    log::error!(
      "vUSTB account request failed: endpoint={endpoint}, status={}",
      response.status()
    );
    return Err(map_status(response.status()).into());
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

pub async fn refresh_session(app: &AppHandle) -> USTBLResult<OAuthTokens> {
  let current = stored_tokens(app)?;
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
      let refreshed = refresh_session(app).await?;
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

  let refreshed = refresh_session(app).await?;
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
  if !response.status().is_success() {
    let status = response.status();
    let detail = response
      .json::<serde_json::Value>()
      .await
      .ok()
      .and_then(|value| {
        value
          .get("detail")
          .and_then(|value| value.as_str())
          .map(str::to_string)
      })
      .unwrap_or_else(|| format!("像素北科 API 返回 HTTP {status}"));
    return Err(USTBLError(detail));
  }
  response
    .json::<T>()
    .await
    .map_err(|_| AccountError::ParseError.into())
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

#[cfg(test)]
mod tests {
  use super::tokens_from_state;
  use crate::account::helpers::authlib_injector::constants::USTB_AUTH_SERVER_URL;
  use crate::account::models::{
    AccountInfo, PlayerInfo, PlayerType, VustbAccount, VustbProgression, VustbSession,
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
}
