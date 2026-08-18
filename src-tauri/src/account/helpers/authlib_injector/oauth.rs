use crate::account::helpers::authlib_injector::common::{parse_profile, retrieve_profile};
use crate::account::helpers::authlib_injector::constants::SCOPE;
use crate::account::helpers::authlib_injector::models::MinecraftProfile;
use crate::account::helpers::misc::oauth_polling;
use crate::account::models::{
  AccountError, DeviceAuthResponse, DeviceAuthResponseInfo, OAuthTokens, PlayerInfo,
};
use crate::error::USTBLResult;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_http::reqwest;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct OpenIDConfig {
  device_authorization_endpoint: String,
  token_endpoint: String,
  jwks_uri: String,
}

async fn fetch_openid_configuration(
  app: &AppHandle,
  openid_configuration_url: String,
) -> USTBLResult<OpenIDConfig> {
  let client = app.state::<reqwest::Client>();

  let res = client
    .get(&openid_configuration_url)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?
    .json::<OpenIDConfig>()
    .await
    .map_err(|_| AccountError::ParseError)?;

  Ok(res)
}

async fn fetch_jwks(app: &AppHandle, jwks_uri: String) -> USTBLResult<Value> {
  let client = app.state::<reqwest::Client>();

  let res = client
    .get(&jwks_uri)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?
    .json::<Value>()
    .await
    .map_err(|_| AccountError::ParseError)?;

  Ok(res)
}

pub async fn device_authorization(
  app: &AppHandle,
  openid_configuration_url: String,
  client_id: Option<String>,
  redirect_uri: Option<String>,
  client_secret: Option<String>,
) -> USTBLResult<DeviceAuthResponseInfo> {
  let client = app.state::<reqwest::Client>();

  let openid_configuration = fetch_openid_configuration(app, openid_configuration_url).await?;

  let mut form_params = vec![
    ("client_id".to_string(), client_id.clone().unwrap_or_default()),
    ("scope".to_string(), SCOPE.to_string()),
  ];
  if let Some(uri) = &redirect_uri {
    form_params.push(("redirect_uri".to_string(), uri.clone()));
  }
  if let Some(secret) = &client_secret {
    form_params.push(("client_secret".to_string(), secret.clone()));
  }

  let response = client
    .post(openid_configuration.device_authorization_endpoint)
    .form(&form_params)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?
    .json::<DeviceAuthResponse>()
    .await
    .map_err(|_| AccountError::ParseError)?;

  let device_code = response.device_code;
  let user_code = response.user_code;
  // if redirect_uri is provided, use it to override the verification URL
  // (some servers return a non-functional verification_uri, e.g. USTB)
  let verification_uri = if let Some(ref uri) = redirect_uri {
    format!("{}?user_code={}", uri, user_code)
  } else {
    response
      .verification_uri_complete
      .unwrap_or(response.verification_uri)
  };
  let interval = response.interval;
  let expires_in = response.expires_in;

  app.clipboard().write_text(user_code.clone())?;

  Ok(DeviceAuthResponseInfo {
    device_code,
    user_code,
    verification_uri,
    interval,
    expires_in,
  })
}

async fn parse_token(
  app: &AppHandle,
  jwks: Value,
  tokens: &OAuthTokens,
  auth_server_url: Option<String>,
  client_id: Option<String>,
) -> USTBLResult<PlayerInfo> {
  let key = &jwks["keys"].as_array().ok_or(AccountError::ParseError)?[0];

  let e = key["e"].as_str().unwrap_or_default();
  let n = key["n"].as_str().unwrap_or_default();

  let decoding_key =
    DecodingKey::from_rsa_components(n, e).map_err(|_| AccountError::ParseError)?;

  let mut validation = Validation::new(Algorithm::RS256);
  validation.set_audience(&[client_id.unwrap_or_default().to_string()]);

  let token_data = decode::<Value>(
    tokens.id_token.clone().unwrap_or_default().as_str(),
    &decoding_key,
    &validation,
  )
  .map_err(|_| AccountError::ParseError)?;

  let mut selected_profile =
    serde_json::from_value::<MinecraftProfile>(token_data.claims["selectedProfile"].clone())
      .map_err(|_| AccountError::ParseError)?;

  if selected_profile.properties.is_none() {
    selected_profile = retrieve_profile(
      app,
      auth_server_url.clone().unwrap_or_default(),
      selected_profile.id.clone(),
    )
    .await?;
  }

  parse_profile(
    app,
    &selected_profile,
    Some(tokens.access_token.clone()),
    tokens.refresh_token.clone(),
    auth_server_url,
    Some(selected_profile.name.clone()),
  )
  .await
}

pub async fn login(
  app: &AppHandle,
  auth_server_url: String,
  openid_configuration_url: String,
  client_id: Option<String>,
  auth_info: DeviceAuthResponseInfo,
  redirect_uri: Option<String>,
  client_secret: Option<String>,
) -> USTBLResult<PlayerInfo> {
  let client = app.state::<reqwest::Client>();
  let openid_configuration = fetch_openid_configuration(app, openid_configuration_url).await?;
  let jwks = fetch_jwks(app, openid_configuration.jwks_uri).await?;

  let mut form_params = vec![
    ("client_id".to_string(), client_id.clone().unwrap_or_default()),
    ("device_code".to_string(), auth_info.device_code.clone()),
    (
      "grant_type".to_string(),
      "urn:ietf:params:oauth:grant-type:device_code".to_string(),
    ),
  ];
  if let Some(uri) = &redirect_uri {
    form_params.push(("redirect_uri".to_string(), uri.clone()));
  }
  if let Some(secret) = &client_secret {
    form_params.push(("client_secret".to_string(), secret.clone()));
  }

  let sender = client
    .post(&openid_configuration.token_endpoint)
    .form(&form_params);
  let tokens = oauth_polling(app, sender, auth_info).await?;
  parse_token(app, jwks, &tokens, Some(auth_server_url), client_id).await
}

pub async fn refresh(
  app: &AppHandle,
  player: &PlayerInfo,
  client_id: Option<String>,
  openid_configuration_url: String,
  redirect_uri: Option<String>,
  client_secret: Option<String>,
) -> USTBLResult<PlayerInfo> {
  let openid_configuration = fetch_openid_configuration(app, openid_configuration_url).await?;
  let jwks = fetch_jwks(app, openid_configuration.jwks_uri).await?;

  let client = app.state::<reqwest::Client>();

  let mut form_params = vec![
    (
      "client_id".to_string(),
      client_id.clone().unwrap_or_default(),
    ),
    (
      "refresh_token".to_string(),
      player.refresh_token.clone().unwrap_or_default(),
    ),
    ("grant_type".to_string(), "refresh_token".to_string()),
    ("scope".to_string(), SCOPE.to_string()),
  ];
  if let Some(uri) = &redirect_uri {
    form_params.push(("redirect_uri".to_string(), uri.clone()));
  }
  if let Some(secret) = &client_secret {
    form_params.push(("client_secret".to_string(), secret.clone()));
  }

  let token_response = client
    .post(&openid_configuration.token_endpoint)
    .form(&form_params)
    .send()
    .await?;

  if !token_response.status().is_success() {
    return Err(AccountError::Expired)?;
  }

  let mut tokens: OAuthTokens = token_response
    .json()
    .await
    .map_err(|_| AccountError::ParseError)?;

  if tokens
    .refresh_token
    .as_deref()
    .unwrap_or_default()
    .is_empty()
  {
    tokens.refresh_token = player.refresh_token.clone();
  }

  parse_token(
    app,
    jwks,
    &tokens,
    player.auth_server_url.clone(),
    client_id,
  )
  .await
}
