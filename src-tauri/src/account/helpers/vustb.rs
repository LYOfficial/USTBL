use crate::account::models::{AccountError, VustbAccount, VustbProfile};
use crate::error::USTBLResult;
use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

const VUSTB_ISSUER: &str = "https://www.ustb.world";

#[derive(Deserialize)]
struct UserInfoResponse {
  #[serde(default, alias = "id")]
  sub: Option<String>,
  #[serde(default, alias = "preferred_username", alias = "name")]
  username: Option<String>,
  #[serde(default, alias = "picture", alias = "avatarUrl", alias = "avatar")]
  avatar_url: Option<String>,
  #[serde(default, alias = "group", alias = "userGroup")]
  user_group: Option<String>,
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

fn map_status(status: reqwest::StatusCode) -> AccountError {
  if status == reqwest::StatusCode::UNAUTHORIZED {
    AccountError::Expired
  } else if status == reqwest::StatusCode::FORBIDDEN {
    AccountError::Forbidden
  } else {
    AccountError::NetworkError
  }
}

async fn get_json<T: for<'de> Deserialize<'de>>(
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

pub async fn fetch_account(
  app: &AppHandle,
  access_token: &str,
  player_id: String,
) -> USTBLResult<VustbAccount> {
  let user_info: UserInfoResponse = get_json(app, "/oauth/userinfo", access_token).await?;
  let profiles: ProfilesResponse = get_json(app, "/oauth/profiles", access_token).await?;

  Ok(VustbAccount {
    subject: user_info.sub.unwrap_or_default(),
    username: user_info.username.unwrap_or_default(),
    avatar_url: user_info.avatar_url.unwrap_or_default(),
    user_group: user_info.user_group.unwrap_or_default(),
    profiles: profiles.into_profiles(),
    player_id,
  })
}
