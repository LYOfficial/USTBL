use crate::account::constants::ACCOUNTS_FILE_NAME;
use crate::account::helpers::authlib_injector::constants::{
  PRESET_AUTH_SERVERS, USTB_AUTH_SERVER_URL, USTB_CLIENT_SECRET, USTB_REDIRECT_URI,
};
use crate::account::helpers::skin::draw_avatar;
use crate::storage::Storage;
use crate::utils::image::ImageWrapper;
use crate::APP_DATA_DIR;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use strum_macros::{Display, EnumIter, EnumString};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub enum PlayerType {
  #[serde(rename = "offline")]
  Offline,
  #[serde(rename = "3rdparty")]
  ThirdParty,
  #[serde(rename = "microsoft")]
  Microsoft,
}
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, Display, Default, EnumIter)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum PresetRole {
  #[default]
  Steve,
  Alex,
}

#[derive(
  Debug, PartialEq, Eq, Clone, Serialize, Deserialize, Display, Default, EnumIter, EnumString,
)]
#[serde(rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE")]
pub enum TextureType {
  #[default]
  Skin,
  Cape,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Display, Default, EnumIter, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum SkinModel {
  #[default]
  Default,
  Slim,
}

impl<'de> Deserialize<'de> for SkinModel {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    let s = String::deserialize(deserializer)?;
    match s.to_lowercase().as_str() {
      "default" | "classic" => Ok(SkinModel::Default),
      "slim" => Ok(SkinModel::Slim),
      _ => Err(serde::de::Error::unknown_variant(
        &s,
        &["default", "classic", "slim"],
      )),
    }
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Texture {
  pub texture_type: TextureType,
  pub image: ImageWrapper,
  pub model: SkinModel,
  pub preset: Option<PresetRole>,
  #[serde(default)]
  pub source_hash: Option<String>,
}

// only for the client
#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
  pub id: String,
  pub name: String,
  pub uuid: Uuid,
  pub avatar: Vec<ImageWrapper>, // [face, hat]
  pub player_type: PlayerType,
  pub auth_account: Option<String>,
  pub auth_server: Option<AuthServer>,
  pub access_token: Option<String>,
  pub access_token_expires: Option<chrono::DateTime<chrono::Utc>>,
  pub refresh_token: Option<String>,
  pub textures: Vec<Texture>,
}

impl Player {
  pub fn from_player_info(
    player_info: PlayerInfo,
    auth_servers: Option<&[AuthServerInfo]>,
  ) -> Self {
    let owned_auth_servers;
    let auth_servers = match auth_servers {
      Some(list) => list,
      None => {
        let state: AccountInfo = Storage::load().unwrap_or_default();
        owned_auth_servers = state.auth_servers.into_iter().collect::<Vec<_>>();
        &owned_auth_servers
      }
    };

    let auth_server = player_info.auth_server_url.clone().map(|auth_server_url| {
      AuthServer::from(
        auth_servers
          .iter()
          .find(|server| server.auth_url == auth_server_url)
          .cloned()
          .unwrap_or_default(),
      )
    });

    Player {
      id: player_info.id,
      name: player_info.name,
      uuid: player_info.uuid,
      avatar: draw_avatar(36, &player_info.textures[0].image.image),
      player_type: player_info.player_type,
      auth_account: player_info.auth_account,
      access_token: player_info.access_token,
      access_token_expires: player_info.access_token_expires,
      refresh_token: player_info.refresh_token,
      auth_server,
      textures: player_info.textures,
    }
  }
}

impl From<PlayerInfo> for Player {
  fn from(player_info: PlayerInfo) -> Self {
    Player::from_player_info(player_info, None)
  }
}

// for backend storage, without saving the whole auth server info
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerInfo {
  pub id: String,
  pub name: String,
  pub uuid: Uuid,
  pub player_type: PlayerType,
  pub auth_account: Option<String>,
  pub auth_server_url: Option<String>,
  pub access_token: Option<String>,
  pub access_token_expires: Option<chrono::DateTime<chrono::Utc>>,
  pub refresh_token: Option<String>,
  pub textures: Vec<Texture>,
}

impl PlayerInfo {
  /// Generate ID from existing fields and return updated struct
  pub fn with_generated_id(mut self) -> Self {
    let server_identity = match self.player_type {
      PlayerType::Offline => "OFFLINE".to_string(),
      PlayerType::Microsoft => "MICROSOFT".to_string(),
      _ => self.auth_server_url.clone().unwrap_or_default(),
    };
    self.id = format!("{}:{}:{}", self.name, server_identity, self.uuid);
    self
  }
}

impl From<Player> for PlayerInfo {
  fn from(player: Player) -> Self {
    PlayerInfo {
      id: player.id,
      name: player.name,
      uuid: player.uuid,
      player_type: player.player_type,
      auth_account: player.auth_account,
      textures: player.textures,
      access_token: player.access_token,
      access_token_expires: player.access_token_expires,
      refresh_token: player.refresh_token,
      auth_server_url: player
        .auth_server
        .as_ref()
        .map(|server| server.auth_url.clone()),
    }
  }
}

impl PartialEq for PlayerInfo {
  fn eq(&self, another: &PlayerInfo) -> bool {
    self.name == another.name && self.auth_server_url == another.auth_server_url
  }
}

impl Eq for PlayerInfo {}

#[derive(Deserialize)]
// received from auth server, do not need camel case
pub struct DeviceAuthResponse {
  pub device_code: String,
  pub user_code: String,
  pub verification_uri: String,
  pub verification_uri_complete: Option<String>,
  pub interval: Option<u64>,
  pub expires_in: u64,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
// communicate with the client
pub struct DeviceAuthResponseInfo {
  pub device_code: String,
  pub user_code: String,
  pub verification_uri: String,
  pub interval: Option<u64>,
  pub expires_in: u64,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
pub struct OAuthTokens {
  #[serde(alias = "accessToken")]
  pub access_token: String,
  #[serde(default)]
  #[serde(alias = "refreshToken")]
  pub refresh_token: Option<String>,
  #[serde(default, alias = "idToken")]
  pub id_token: Option<String>,
}

/// 像素北科网站账户与 Minecraft 启动角色是两层独立概念：前者用于启动器
/// 功能授权，后者仍是 authlib-injector 使用的游戏角色。
#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbProfile {
  #[serde(default, alias = "uuid")]
  pub id: String,
  #[serde(default, alias = "username")]
  pub name: String,
  #[serde(default)]
  pub selected: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VustbProgression {
  pub experience: u32,
  pub level: u8,
  #[serde(alias = "max_level")]
  pub max_level: u8,
  #[serde(alias = "max_experience")]
  pub max_experience: u32,
  #[serde(alias = "level_start_experience")]
  pub level_start_experience: u32,
  #[serde(alias = "next_level_experience")]
  pub next_level_experience: u32,
  #[serde(alias = "experience_into_level")]
  pub experience_into_level: u32,
  #[serde(alias = "experience_for_next_level")]
  pub experience_for_next_level: u32,
  #[serde(alias = "is_max_level")]
  pub is_max_level: bool,
  #[serde(alias = "checkin_days")]
  pub checkin_days: u32,
  #[serde(alias = "play_time_seconds")]
  pub play_time_seconds: u64,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbAccount {
  pub subject: String,
  pub username: String,
  pub avatar_url: String,
  pub user_group: String,
  pub profiles: Vec<VustbProfile>,
  #[serde(default)]
  pub progression: VustbProgression,
  #[serde(default)]
  pub last_checkin: Option<String>,
  /// 兼容旧版存储中承载网站会话令牌的 Minecraft 角色。
  #[serde(default)]
  pub player_id: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbSession {
  #[serde(alias = "access_token")]
  pub access_token: String,
  #[serde(alias = "refresh_token")]
  pub refresh_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbCheckinResult {
  pub message: String,
  pub experience_gained: u32,
  pub account: VustbAccount,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbFriend {
  pub friendship_id: u64,
  pub id: u64,
  pub username: String,
  pub display_name: String,
  pub avatar_url: String,
  pub online: bool,
  pub instance_name: Option<String>,
  pub last_seen_at: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbTexture {
  pub hash: String,
  #[serde(rename = "type")]
  pub texture_type: String,
  pub name: String,
  pub model: String,
  #[serde(alias = "is_public")]
  pub is_public: bool,
  #[serde(default, alias = "uploader_name")]
  pub uploader_name: String,
  #[serde(alias = "created_at")]
  pub created_at: Option<String>,
  pub url: String,
  #[serde(default)]
  pub collected: bool,
  #[serde(default)]
  pub local_backup: bool,
  #[serde(default)]
  pub local_backup_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VustbTexturePage {
  pub total: u64,
  pub items: Vec<VustbTexture>,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
pub struct OAuthErrorResponse {
  pub error: String,
  pub error_description: Option<String>,
  pub error_uri: Option<String>,
}

structstruck::strike! {
  #[strikethrough[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]]
  #[strikethrough[serde(rename_all = "camelCase")]]
  pub struct AuthServer {
    pub name: String,
    pub auth_url: String,
    pub homepage_url: String,
    pub register_url: String,
    pub features: struct {
      pub non_email_login: bool,
      pub openid_configuration_url: String,
    },
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub client_secret: Option<String>,
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthServerInfo {
  pub auth_url: String,
  pub client_id: Option<String>,
  pub metadata: Value,
  pub timestamp: u64,
}

impl From<AuthServerInfo> for AuthServer {
  fn from(info: AuthServerInfo) -> Self {
    let is_ustb = info.auth_url == USTB_AUTH_SERVER_URL;
    let name = if is_ustb {
      "像素北科".to_string()
    } else {
      info.metadata["meta"]["serverName"]
        .as_str()
        .unwrap_or_default()
        .to_string()
    };

    AuthServer {
      name,
      auth_url: info.auth_url,
      homepage_url: info.metadata["meta"]["links"]["homepage"]
        .as_str()
        .unwrap_or_default()
        .to_string(),
      register_url: info.metadata["meta"]["links"]["register"]
        .as_str()
        .unwrap_or_default()
        .to_string(),
      features: Features {
        non_email_login: info.metadata["meta"]["feature.non_email_login"]
          .as_bool()
          .unwrap_or(false),
        openid_configuration_url: info.metadata["meta"]["feature.openid_configuration_url"]
          .as_str()
          .unwrap_or_default()
          .to_string(),
      },
      client_id: info.client_id,
      redirect_uri: if is_ustb {
        Some(USTB_REDIRECT_URI.to_string())
      } else {
        None
      },
      client_secret: if is_ustb {
        Some(USTB_CLIENT_SECRET.to_string())
      } else {
        None
      },
    }
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountInfo {
  pub players: Vec<PlayerInfo>,
  pub auth_servers: Vec<AuthServerInfo>,
  #[serde(default)]
  pub vustb_account: Option<VustbAccount>,
  #[serde(default)]
  pub vustb_session: Option<VustbSession>,
  pub is_oauth_processing: bool,
}

impl Default for AccountInfo {
  fn default() -> Self {
    AccountInfo {
      players: vec![],
      auth_servers: PRESET_AUTH_SERVERS
        .iter()
        .map(|url| AuthServerInfo {
          auth_url: url.to_string(),
          client_id: None,
          metadata: Value::Null,
          timestamp: 0,
        })
        .collect(),
      vustb_account: None,
      vustb_session: None,
      is_oauth_processing: false,
    }
  }
}

impl AccountInfo {
  pub fn get_player_by_id_mut(&mut self, id: String) -> Option<&mut PlayerInfo> {
    self.players.iter_mut().find(|player| player.id == id)
  }
}

impl Storage for AccountInfo {
  fn file_path() -> PathBuf {
    APP_DATA_DIR.get().unwrap().join(ACCOUNTS_FILE_NAME)
  }
}

#[cfg(test)]
mod tests {
  use super::{AccountInfo, VustbProgression, VustbSession, VustbTexturePage};

  #[test]
  fn legacy_account_storage_without_vustb_session_still_loads() {
    let state: AccountInfo = serde_json::from_str(
      r#"{
        "players": [],
        "authServers": [],
        "vustbAccount": null,
        "isOauthProcessing": false
      }"#,
    )
    .unwrap();

    assert!(state.vustb_session.is_none());
  }

  #[test]
  fn vustb_session_accepts_legacy_snake_case_and_saves_camel_case() {
    let session: VustbSession =
      serde_json::from_str(r#"{"access_token":"access","refresh_token":"refresh"}"#).unwrap();
    let value = serde_json::to_value(session).unwrap();

    assert_eq!(value["accessToken"], "access");
    assert_eq!(value["refreshToken"], "refresh");
  }

  #[test]
  fn progression_accepts_launcher_api_snake_case_and_saves_camel_case() {
    let progression: VustbProgression = serde_json::from_str(
      r#"{
        "experience": 3,
        "level": 0,
        "max_level": 40,
        "max_experience": 2920,
        "level_start_experience": 0,
        "next_level_experience": 7,
        "experience_into_level": 3,
        "experience_for_next_level": 7,
        "is_max_level": false,
        "checkin_days": 1,
        "play_time_seconds": 600
      }"#,
    )
    .unwrap();
    let value = serde_json::to_value(progression).unwrap();

    assert_eq!(value["nextLevelExperience"], 7);
    assert_eq!(value["checkinDays"], 1);
    assert_eq!(value["playTimeSeconds"], 600);
  }

  #[test]
  fn texture_page_accepts_launcher_api_snake_case() {
    let page: VustbTexturePage = serde_json::from_str(
      r#"{
        "total": 1,
        "items": [{
          "hash": "texture-hash",
          "type": "skin",
          "name": "Example",
          "model": "slim",
          "is_public": true,
          "uploader_name": "Uploader",
          "created_at": "2026-09-19T00:00:00Z",
          "url": "/static/textures/texture-hash.png",
          "collected": false
        }]
      }"#,
    )
    .unwrap();

    assert_eq!(page.total, 1);
    assert!(page.items[0].is_public);
    assert_eq!(page.items[0].uploader_name, "Uploader");
  }
}

#[derive(Debug, Display)]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
#[allow(dead_code)]
pub enum AccountError {
  Duplicate,
  Expired,
  FullLoginUnavailable,
  Invalid,
  NotFound,
  TextureError,
  NetworkError,
  ServiceUnavailable,
  TooManyRequests,
  Forbidden,
  UnknownProfile,
  CannotAddSelf,
  DuplicatedProfiles,
  ParseError,
  Cancelled,
  NoDownloadApi,
  SaveError,
  NoMinecraftProfile,
  XstsBanned,
  XstsParentalRestriction,
  XstsNoXboxAccount,
  XstsTermsNotAccepted,
  XstsRegionBanned,
  XstsChildAccount,
  XstsUnknownError,
}

impl std::error::Error for AccountError {}
