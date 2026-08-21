use crate::account::models::{AccountError, VustbAccount, VustbProfile};
use crate::error::USTBLResult;
use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

const VUSTB_ISSUER: &str = "https://www.ustb.world";

#[derive(Deserialize)]
struct UserInfoResponse {
  sub: String,
  username: String,
  avatar_url: String,
  #[serde(default)]
  user_group: String,
}

#[derive(Deserialize)]
struct ProfilesResponse {
  profiles: Vec<VustbProfile>,
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
    return Err(map_status(response.status()).into());
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
  let user_info: UserInfoResponse = get_json(app, "/oauth/userinfo", access_token).await?;
  let profiles: ProfilesResponse = get_json(app, "/oauth/profiles", access_token).await?;

  Ok(VustbAccount {
    subject: user_info.sub,
    username: user_info.username,
    avatar_url: user_info.avatar_url,
    user_group: user_info.user_group,
    profiles: profiles.profiles,
    player_id,
  })
}
