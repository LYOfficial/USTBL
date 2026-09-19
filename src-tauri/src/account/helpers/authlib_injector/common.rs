use crate::account::helpers::authlib_injector::models::{MinecraftProfile, TextureInfo};
use crate::account::helpers::authlib_injector::{oauth, password};
use crate::account::helpers::misc::fetch_image;
use crate::account::helpers::offline::load_preset_skin;
use crate::account::models::{
  AccountError, AuthServer, PlayerInfo, PlayerType, PresetRole, SkinModel, Texture, TextureType,
};
use crate::error::USTBLResult;
use base64::engine::general_purpose;
use base64::Engine;
use serde_json::json;
use std::str::FromStr;
use strum::IntoEnumIterator;
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;
use url::Url;
use uuid::Uuid;

fn normalize_texture_url(auth_server_url: Option<&str>, texture_url: &str) -> String {
  let Some(auth_server_url) = auth_server_url else {
    return texture_url.to_string();
  };
  let (Ok(auth_server), Ok(mut texture)) = (Url::parse(auth_server_url), Url::parse(texture_url))
  else {
    return texture_url.to_string();
  };

  if auth_server.host_str() == Some("www.ustb.world")
    && texture.host_str() == Some("www.ustb.world")
    && texture.path().starts_with("/skinapi/static/textures/")
  {
    texture.set_path(
      &texture
        .path()
        .replacen("/skinapi/static/textures/", "/static/textures/", 1),
    );
    log::warn!("Corrected legacy vUSTB texture URL path");
  }

  texture.to_string()
}

pub async fn retrieve_profile(
  app: &AppHandle,
  auth_server_url: String,
  id: String,
) -> USTBLResult<MinecraftProfile> {
  let client = app.state::<reqwest::Client>();
  let endpoint = format!(
    "{}/sessionserver/session/minecraft/profile/{}",
    auth_server_url.trim_end_matches('/'),
    id
  );
  let response = client
    .get(endpoint)
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?;
  if !response.status().is_success() {
    return Err(AccountError::NetworkError.into());
  }
  response
    .json::<MinecraftProfile>()
    .await
    .map_err(|_| AccountError::ParseError.into())
}

pub async fn parse_profile(
  app: &AppHandle,
  profile: &MinecraftProfile,
  access_token: Option<String>,
  refresh_token: Option<String>,
  auth_server_url: Option<String>,
  auth_account: Option<String>,
) -> USTBLResult<PlayerInfo> {
  let uuid = if let Ok(uuid) = Uuid::parse_str(&profile.id) {
    uuid
  } else if profile.id.trim().len() == 32 {
    let compact = profile.id.trim();
    let formatted = format!(
      "{}-{}-{}-{}-{}",
      &compact[0..8],
      &compact[8..12],
      &compact[12..16],
      &compact[16..20],
      &compact[20..32]
    );
    Uuid::parse_str(&formatted).map_err(|_| AccountError::ParseError)?
  } else {
    return Err(AccountError::ParseError.into());
  };
  let name = profile.name.clone();
  let mut textures: Vec<Texture> = vec![];

  if let Some(texture_info_base64) = profile
    .properties
    .as_ref()
    .and_then(|props| props.iter().find(|property| property.name == "textures"))
  {
    let decoded = general_purpose::STANDARD
      .decode(&texture_info_base64.value)
      .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(&texture_info_base64.value))
      .or_else(|_| general_purpose::URL_SAFE.decode(&texture_info_base64.value))
      .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(&texture_info_base64.value));

    match decoded
      .ok()
      .and_then(|bytes| String::from_utf8(bytes).ok())
      .and_then(|json| serde_json::from_str::<TextureInfo>(&json).ok())
    {
      Some(texture_info) => {
        for texture_type in TextureType::iter() {
          if let Some(skin) = texture_info.textures.get(&texture_type.to_string()) {
            let texture_url = normalize_texture_url(auth_server_url.as_deref(), &skin.url);
            match fetch_image(app, texture_url).await {
              Ok(image) => textures.push(Texture {
                image,
                texture_type,
                model: skin
                  .metadata
                  .as_ref()
                  .and_then(|metadata| metadata.get("model").cloned())
                  .map(|model_str| SkinModel::from_str(&model_str).unwrap_or(SkinModel::Default))
                  .unwrap_or_default(),
                preset: None,
              }),
              Err(error) => log::warn!(
                "Failed to load OAuth profile texture; profile_id={}, type={texture_type}, error={error:?}; using preset skin when needed",
                profile.id
              ),
            }
          }
        }
      }
      None => log::warn!(
        "OAuth profile texture property is invalid; profile_id={}; using preset skin",
        profile.id
      ),
    }
  }

  let has_skin = textures
    .iter()
    .any(|texture| texture.texture_type == TextureType::Skin);
  if !has_skin {
    log::warn!(
      "OAuth profile has no available skin; profile_id={}; using preset skin",
      profile.id
    );
    let mut fallback_textures = load_preset_skin(app, PresetRole::Steve)?;
    fallback_textures.extend(textures);
    textures = fallback_textures;
  }

  Ok(
    PlayerInfo {
      id: "".to_string(),
      uuid,
      name: name.to_string(),
      player_type: PlayerType::ThirdParty,
      auth_account,
      access_token,
      access_token_expires: None,
      refresh_token,
      textures,
      auth_server_url,
    }
    .with_generated_id(),
  )
}

pub async fn validate(app: &AppHandle, player: &PlayerInfo) -> USTBLResult<bool> {
  let client = app.state::<reqwest::Client>();

  let response = client
    .post(format!(
      "{}/authserver/validate",
      player.auth_server_url.clone().unwrap_or_default()
    ))
    .json(&json!({
      "accessToken": player.access_token.clone()
    }))
    .send()
    .await
    .map_err(|_| AccountError::NetworkError)?;

  Ok(response.status().is_success())
}

pub async fn refresh(
  app: &AppHandle,
  player: &PlayerInfo,
  auth_server: &AuthServer,
) -> USTBLResult<PlayerInfo> {
  if player.refresh_token.is_none() || Some("") == player.refresh_token.as_deref() {
    // to be compatible with legacy version of account config
    password::refresh(app, player, false).await
  } else {
    oauth::refresh(
      app,
      player,
      auth_server.client_id.clone(),
      auth_server.features.openid_configuration_url.clone(),
      auth_server.redirect_uri.clone(),
      auth_server.client_secret.clone(),
    )
    .await
  }
}
