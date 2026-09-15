use crate::account::helpers::authlib_injector::common::{
  parse_profile_with_policy, retrieve_profile,
};
use crate::account::helpers::authlib_injector::constants::{SCOPE, USTB_AUTH_SERVER_URL};
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
use url::Url;

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
    .map_err(|error| {
      log::error!("OpenID configuration JSON parse failed: {error}");
      AccountError::ParseError
    })?;

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
    .map_err(|error| {
      log::error!("JWKS JSON parse failed: {error}");
      AccountError::ParseError
    })?;

  Ok(res)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProfileListResponse {
  Wrapped { profiles: Vec<MinecraftProfile> },
  Data { data: Vec<MinecraftProfile> },
  Single(MinecraftProfile),
  List(Vec<MinecraftProfile>),
}

#[derive(Debug)]
pub struct OAuthProfileLogin {
  pub players: Vec<PlayerInfo>,
  pub selected_player_id: String,
  pub tokens: OAuthTokens,
}

async fn fetch_profiles(
  app: &AppHandle,
  auth_server_url: &str,
  access_token: &str,
) -> USTBLResult<Vec<MinecraftProfile>> {
  let mut endpoint = Url::parse(auth_server_url).map_err(|_| AccountError::ParseError)?;
  endpoint.set_path("/oauth/profiles");
  endpoint.set_query(None);

  let client = app.state::<reqwest::Client>();
  let response = client
    .get(endpoint)
    .bearer_auth(access_token)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?;
  log::debug!("OAuth profiles response status={}", response.status());
  if !response.status().is_success() {
    return Err(AccountError::ParseError.into());
  }

  let profiles = match response
    .json::<ProfileListResponse>()
    .await
    .map_err(|error| {
      log::error!("OAuth profiles JSON parse failed: {error}");
      AccountError::ParseError
    })? {
    ProfileListResponse::Wrapped { profiles } | ProfileListResponse::List(profiles) => profiles,
    ProfileListResponse::Data { data } => data,
    ProfileListResponse::Single(profile) => vec![profile],
  };
  Ok(profiles)
}

fn preferred_profile(profiles: &[MinecraftProfile]) -> Option<&MinecraftProfile> {
  profiles
    .iter()
    .find(|profile| profile.selected)
    .or_else(|| profiles.first())
}

async fn fetch_selected_profile(
  app: &AppHandle,
  auth_server_url: &str,
  access_token: &str,
) -> USTBLResult<MinecraftProfile> {
  let profiles = fetch_profiles(app, auth_server_url, access_token).await?;
  preferred_profile(&profiles)
    .cloned()
    .ok_or_else(|| AccountError::NoMinecraftProfile.into())
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
    (
      "client_id".to_string(),
      client_id.clone().unwrap_or_default(),
    ),
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
  preferred_profile_id: Option<&str>,
) -> USTBLResult<PlayerInfo> {
  let mut selected_profile = None;
  if let Some(token) = tokens.id_token.as_deref() {
    if let Some(keys) = jwks["keys"].as_array() {
      if let Ok(header) = jsonwebtoken::decode_header(token) {
        if let Some(key) = header
          .kid
          .as_deref()
          .and_then(|kid| keys.iter().find(|key| key["kid"].as_str() == Some(kid)))
          .or_else(|| keys.first())
        {
          let e = key["e"].as_str().unwrap_or_default();
          let n = key["n"].as_str().unwrap_or_default();
          if let Ok(decoding_key) = DecodingKey::from_rsa_components(n, e) {
            let mut validation = Validation::new(Algorithm::RS256);
            validation.set_audience(&[client_id.unwrap_or_default().to_string()]);
            if let Ok(token_data) =
              decode::<Value>(token, &decoding_key, &validation).or_else(|_| {
                // Some OAuth deployments issue a valid token with an audience that is
                // not identical to the shared client id advertised in metadata.
                validation.validate_aud = false;
                decode::<Value>(token, &decoding_key, &validation)
              })
            {
              selected_profile = token_data
                .claims
                .get("selectedProfile")
                .or_else(|| token_data.claims.get("selected_profile"))
                .cloned()
                .and_then(|value| serde_json::from_value::<MinecraftProfile>(value).ok());
            }
          }
        }
      }
    }
  }

  if let Some(preferred_profile_id) = preferred_profile_id {
    let preferred_profile_id = preferred_profile_id.replace('-', "");
    let selected_matches = selected_profile.as_ref().is_some_and(|profile| {
      profile
        .id
        .replace('-', "")
        .eq_ignore_ascii_case(&preferred_profile_id)
    });
    if !selected_matches {
      selected_profile = fetch_profiles(
        app,
        auth_server_url.as_deref().unwrap_or_default(),
        tokens.access_token.as_str(),
      )
      .await?
      .into_iter()
      .find(|profile| {
        profile
          .id
          .replace('-', "")
          .eq_ignore_ascii_case(&preferred_profile_id)
      });
    }
  }

  if selected_profile.is_none() {
    selected_profile = Some(
      fetch_selected_profile(
        app,
        auth_server_url.as_deref().unwrap_or_default(),
        tokens.access_token.as_str(),
      )
      .await?,
    );
  }
  let mut selected_profile = selected_profile.ok_or(AccountError::ParseError)?;
  log::debug!(
    "OAuth selected profile received; id_len={}, name_len={}",
    selected_profile.id.len(),
    selected_profile.name.len()
  );

  if selected_profile.properties.is_none() {
    if let Ok(profile) = retrieve_profile(
      app,
      auth_server_url.clone().unwrap_or_default(),
      selected_profile.id.clone(),
    )
    .await
    {
      selected_profile = profile;
    }
  }

  let require_platform_skin = auth_server_url
    .as_deref()
    .is_some_and(|url| url.trim_end_matches('/') == USTB_AUTH_SERVER_URL.trim_end_matches('/'));
  let result = parse_profile_with_policy(
    app,
    &selected_profile,
    Some(tokens.access_token.clone()),
    tokens.refresh_token.clone(),
    auth_server_url,
    Some(selected_profile.name.clone()),
    require_platform_skin,
  )
  .await;
  if let Err(error) = &result {
    log::error!("OAuth profile parsing failed: {error:?}");
  }
  result
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
    (
      "client_id".to_string(),
      client_id.clone().unwrap_or_default(),
    ),
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
  parse_token(app, jwks, &tokens, Some(auth_server_url), client_id, None).await
}

pub async fn login_all(
  app: &AppHandle,
  auth_server_url: String,
  openid_configuration_url: String,
  client_id: Option<String>,
  auth_info: DeviceAuthResponseInfo,
  redirect_uri: Option<String>,
  client_secret: Option<String>,
) -> USTBLResult<OAuthProfileLogin> {
  let client = app.state::<reqwest::Client>();
  let openid_configuration = fetch_openid_configuration(app, openid_configuration_url).await?;
  let mut form_params = vec![
    (
      "client_id".to_string(),
      client_id.clone().unwrap_or_default(),
    ),
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
  load_all_profiles(app, auth_server_url, tokens).await
}

pub async fn load_all_profiles(
  app: &AppHandle,
  auth_server_url: String,
  tokens: OAuthTokens,
) -> USTBLResult<OAuthProfileLogin> {
  let profiles = fetch_profiles(app, &auth_server_url, &tokens.access_token).await?;
  let selected_profile_id = preferred_profile(&profiles).map(|profile| profile.id.replace('-', ""));
  let mut players = Vec::with_capacity(profiles.len());

  for listed_profile in profiles {
    let full_profile = retrieve_profile(app, auth_server_url.clone(), listed_profile.id).await?;
    let player = parse_profile_with_policy(
      app,
      &full_profile,
      Some(tokens.access_token.clone()),
      tokens.refresh_token.clone(),
      Some(auth_server_url.clone()),
      Some(full_profile.name.clone()),
      true,
    )
    .await?;
    players.push(player);
  }

  let selected_player_id = selected_profile_id
    .as_deref()
    .and_then(|selected_profile_id| {
      players.iter().find(|player| {
        player
          .uuid
          .simple()
          .to_string()
          .eq_ignore_ascii_case(selected_profile_id)
      })
    })
    .map(|player| player.id.clone())
    .unwrap_or_default();
  log::info!("Imported all OAuth profiles; count={}", players.len());
  Ok(OAuthProfileLogin {
    players,
    selected_player_id,
    tokens,
  })
}

pub async fn refresh_tokens(
  app: &AppHandle,
  refresh_token: String,
  client_id: Option<String>,
  openid_configuration_url: String,
  redirect_uri: Option<String>,
  client_secret: Option<String>,
) -> USTBLResult<OAuthTokens> {
  let openid_configuration = fetch_openid_configuration(app, openid_configuration_url).await?;
  let client = app.state::<reqwest::Client>();

  let mut form_params = vec![
    (
      "client_id".to_string(),
      client_id.clone().unwrap_or_default(),
    ),
    ("refresh_token".to_string(), refresh_token.clone()),
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

  preserve_refresh_token(&mut tokens, refresh_token);

  Ok(tokens)
}

fn preserve_refresh_token(tokens: &mut OAuthTokens, previous_refresh_token: String) {
  if tokens
    .refresh_token
    .as_deref()
    .unwrap_or_default()
    .is_empty()
  {
    tokens.refresh_token = Some(previous_refresh_token);
  }
}

pub async fn refresh(
  app: &AppHandle,
  player: &PlayerInfo,
  client_id: Option<String>,
  openid_configuration_url: String,
  redirect_uri: Option<String>,
  client_secret: Option<String>,
) -> USTBLResult<PlayerInfo> {
  let jwks_uri = fetch_openid_configuration(app, openid_configuration_url.clone())
    .await?
    .jwks_uri;
  let jwks = fetch_jwks(app, jwks_uri).await?;
  let tokens = refresh_tokens(
    app,
    player.refresh_token.clone().unwrap_or_default(),
    client_id.clone(),
    openid_configuration_url,
    redirect_uri,
    client_secret,
  )
  .await?;

  let preferred_profile_id = player.uuid.simple().to_string();
  parse_token(
    app,
    jwks,
    &tokens,
    player.auth_server_url.clone(),
    client_id,
    Some(&preferred_profile_id),
  )
  .await
}

#[cfg(test)]
mod tests {
  use super::{preferred_profile, preserve_refresh_token};
  use crate::account::helpers::authlib_injector::models::MinecraftProfile;
  use crate::account::models::OAuthTokens;

  fn profile(id: &str, selected: bool) -> MinecraftProfile {
    MinecraftProfile {
      id: id.to_string(),
      name: id.to_string(),
      properties: None,
      selected,
    }
  }

  #[test]
  fn empty_profile_list_is_valid_for_website_only_accounts() {
    assert!(preferred_profile(&[]).is_none());
  }

  #[test]
  fn explicitly_selected_profile_takes_precedence() {
    let profiles = vec![profile("first", false), profile("selected", true)];
    assert_eq!(preferred_profile(&profiles).unwrap().id, "selected");
  }

  #[test]
  fn refresh_keeps_previous_refresh_token_when_server_omits_one() {
    let mut tokens = OAuthTokens {
      access_token: "access".to_string(),
      refresh_token: None,
      id_token: None,
    };
    preserve_refresh_token(&mut tokens, "previous".to_string());
    assert_eq!(tokens.refresh_token.as_deref(), Some("previous"));
  }

  #[test]
  fn refresh_keeps_rotated_refresh_token() {
    let mut tokens = OAuthTokens {
      access_token: "access".to_string(),
      refresh_token: Some("rotated".to_string()),
      id_token: None,
    };
    preserve_refresh_token(&mut tokens, "previous".to_string());
    assert_eq!(tokens.refresh_token.as_deref(), Some("rotated"));
  }
}
