use crate::account::helpers::authlib_injector::constants::{
  PRESET_AUTH_SERVERS, USTB_AUTH_SERVER_URL,
};
use crate::account::helpers::authlib_injector::info::{
  fetch_auth_server_info, fetch_auth_url, get_auth_server_info_by_url,
};
use crate::account::helpers::authlib_injector::jar::check_authlib_jar;
use crate::account::helpers::authlib_injector::{self};
use crate::account::helpers::{microsoft, misc, offline, skin_backup, vustb, vustb_presence};
use crate::account::models::{
  AccountError, AccountInfo, AuthServer, DeviceAuthResponseInfo, Player, PlayerInfo, PlayerType,
  PresetRole, SkinModel, Texture, TextureType, VustbAccount, VustbCheckinResult, VustbFriend,
  VustbSession, VustbTexture, VustbTexturePage,
};
use crate::error::USTBLResult;
use crate::launcher_config::models::LauncherConfig;
use crate::storage::Storage;
use crate::utils::fs::get_app_resource_filepath;
use crate::utils::web::normalize_url;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use url::Url;

#[tauri::command]
pub fn retrieve_player_list(app: AppHandle) -> USTBLResult<Vec<Player>> {
  let account_binding = app.state::<Mutex<AccountInfo>>();
  let account_state = account_binding.lock()?;

  let player_list: Vec<Player> = account_state
    .clone()
    .players
    .into_iter()
    .map(Player::from)
    .collect();

  // ensure a player is selected when player_list is not empty
  if !player_list.is_empty() {
    let config_binding = app.state::<Mutex<LauncherConfig>>();
    let mut config_state = config_binding.lock()?;
    if !player_list
      .iter()
      .any(|player| player.id == config_state.states.shared.selected_player_id)
    {
      config_state.partial_update(
        &app,
        "states.shared.selected_player_id",
        &serde_json::to_string(&player_list[0].id).unwrap_or_default(),
      )?;
      config_state.save()?;
    }
  }

  Ok(player_list)
}

#[tauri::command]
pub async fn add_player_offline(app: AppHandle, username: String, uuid: String) -> USTBLResult<()> {
  let new_player = offline::login(&app, username, uuid).await?;

  let account_binding = app.state::<Mutex<AccountInfo>>();
  let mut account_state = account_binding.lock()?;

  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let mut config_state = config_binding.lock()?;

  if account_state
    .players
    .iter()
    .any(|player| player.id == new_player.id)
  {
    return Err(AccountError::Duplicate.into());
  }

  config_state.partial_update(
    &app,
    "states.shared.selected_player_id",
    &serde_json::to_string(&new_player.id).unwrap_or_default(),
  )?;
  config_state.save()?;

  account_state.players.push(new_player);
  account_state.save()?;
  Ok(())
}

#[tauri::command]
pub async fn fetch_oauth_code(
  app: AppHandle,
  server_type: PlayerType,
  auth_server_url: String,
) -> USTBLResult<DeviceAuthResponseInfo> {
  if server_type == PlayerType::ThirdParty {
    let auth_server = AuthServer::from(get_auth_server_info_by_url(&app, auth_server_url)?);

    authlib_injector::oauth::device_authorization(
      &app,
      auth_server.features.openid_configuration_url,
      auth_server.client_id,
      auth_server.redirect_uri,
      auth_server.client_secret,
    )
    .await
  } else if server_type == PlayerType::Microsoft {
    microsoft::oauth::device_authorization(&app).await
  } else {
    Err(AccountError::Invalid.into())
  }
}

#[tauri::command]
pub async fn add_player_oauth(
  app: AppHandle,
  server_type: PlayerType,
  auth_info: DeviceAuthResponseInfo,
  auth_server_url: String,
) -> USTBLResult<()> {
  let new_player = match server_type {
    PlayerType::ThirdParty => {
      let _ = check_authlib_jar(&app).await; // ignore the error when logging in

      let auth_server =
        AuthServer::from(get_auth_server_info_by_url(&app, auth_server_url.clone())?);

      authlib_injector::oauth::login(
        &app,
        auth_server_url,
        auth_server.features.openid_configuration_url,
        auth_server.client_id,
        auth_info,
        auth_server.redirect_uri,
        auth_server.client_secret,
      )
      .await?
    }

    PlayerType::Microsoft => microsoft::oauth::login(&app, auth_info).await?,

    PlayerType::Offline => {
      return Err(AccountError::Invalid.into());
    }
  };

  {
    let account_binding = app.state::<Mutex<AccountInfo>>();
    let mut account_state = account_binding.lock()?;

    let config_binding = app.state::<Mutex<LauncherConfig>>();
    let mut config_state = config_binding.lock()?;

    if account_state
      .players
      .iter()
      .any(|player| player.id == new_player.id)
    {
      return Err(AccountError::Duplicate.into());
    }

    config_state.partial_update(
      &app,
      "states.shared.selected_player_id",
      &serde_json::to_string(&new_player.id).unwrap_or_default(),
    )?;
    config_state.save()?;

    account_state.players.push(new_player);
    account_state.save()?;
  }

  misc::check_full_login_availability(&app).await
}

/// 完成像素北科设备流登录，同时保存其网站账户资料和可用于启动游戏的角色。
#[tauri::command]
pub async fn login_vustb_account(
  app: AppHandle,
  auth_info: DeviceAuthResponseInfo,
) -> USTBLResult<VustbAccount> {
  let _ = check_authlib_jar(&app).await;
  let auth_server = AuthServer::from(get_auth_server_info_by_url(
    &app,
    USTB_AUTH_SERVER_URL.to_string(),
  )?);
  let login = authlib_injector::oauth::login_all(
    &app,
    USTB_AUTH_SERVER_URL.to_string(),
    auth_server.features.openid_configuration_url,
    auth_server.client_id,
    auth_info,
    auth_server.redirect_uri,
    auth_server.client_secret,
  )
  .await?;

  let account = vustb::fetch_account(
    &app,
    &login.tokens.access_token,
    login.selected_player_id.clone(),
  )
  .await?;

  {
    let account_binding = app.state::<Mutex<AccountInfo>>();
    let mut account_state = account_binding.lock()?;
    account_state.players.retain(|player| {
      player
        .auth_server_url
        .as_deref()
        .is_none_or(|url| normalize_url(url) != normalize_url(USTB_AUTH_SERVER_URL))
    });
    account_state.players.extend(login.players);
    account_state.vustb_account = Some(account.clone());
    account_state.vustb_session = Some(VustbSession {
      access_token: login.tokens.access_token,
      refresh_token: login.tokens.refresh_token,
    });
    account_state.save()?;

    if !login.selected_player_id.is_empty() {
      let config_binding = app.state::<Mutex<LauncherConfig>>();
      let mut config_state = config_binding.lock()?;
      config_state.partial_update(
        &app,
        "states.shared.selected_player_id",
        &serde_json::to_string(&login.selected_player_id).unwrap_or_default(),
      )?;
      config_state.save()?;
    }
  }

  misc::check_full_login_availability(&app).await?;
  let sync_app = app.clone();
  tauri::async_runtime::spawn(async move {
    let _ = crate::launch::helpers::playtime_sync::flush_playtime_queue(&sync_app).await;
    let _ = vustb_presence::sync(&sync_app).await;
  });
  Ok(account)
}

#[tauri::command]
pub fn retrieve_vustb_account(app: AppHandle) -> USTBLResult<Option<VustbAccount>> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let account = binding.lock()?.vustb_account.clone();
  Ok(account)
}

#[tauri::command]
pub async fn sync_vustb_account(app: AppHandle) -> USTBLResult<VustbAccount> {
  let player_id = {
    let binding = app.state::<Mutex<AccountInfo>>();
    let account_state = binding.lock()?;
    let vustb_account = account_state
      .vustb_account
      .as_ref()
      .ok_or(AccountError::NotFound)?;
    vustb_account.player_id.clone()
  };
  let login = vustb::load_all_profiles(&app).await?;
  let refreshed = vustb::fetch_current_account(
    &app,
    if login.selected_player_id.is_empty() {
      player_id
    } else {
      login.selected_player_id.clone()
    },
  )
  .await?;
  let binding = app.state::<Mutex<AccountInfo>>();
  let mut account_state = binding.lock()?;
  account_state.players.retain(|player| {
    player
      .auth_server_url
      .as_deref()
      .is_none_or(|url| normalize_url(url) != normalize_url(USTB_AUTH_SERVER_URL))
  });
  account_state.players.extend(login.players);
  account_state.vustb_account = Some(refreshed.clone());
  account_state.save()?;

  if !login.selected_player_id.is_empty() {
    let config_binding = app.state::<Mutex<LauncherConfig>>();
    let mut config_state = config_binding.lock()?;
    config_state.partial_update(
      &app,
      "states.shared.selected_player_id",
      &serde_json::to_string(&login.selected_player_id).unwrap_or_default(),
    )?;
    config_state.save()?;
  }
  let sync_app = app.clone();
  tauri::async_runtime::spawn(async move {
    let _ = crate::launch::helpers::playtime_sync::flush_playtime_queue(&sync_app).await;
  });
  Ok(refreshed)
}

#[tauri::command]
pub async fn checkin_vustb_account(app: AppHandle) -> USTBLResult<VustbCheckinResult> {
  let player_id = {
    let binding = app.state::<Mutex<AccountInfo>>();
    let state = binding.lock()?;
    state
      .vustb_account
      .as_ref()
      .ok_or(AccountError::NotFound)?
      .player_id
      .clone()
  };
  let result = vustb::checkin(&app, player_id).await?;
  let binding = app.state::<Mutex<AccountInfo>>();
  let mut state = binding.lock()?;
  state.vustb_account = Some(result.account.clone());
  state.save()?;
  Ok(result)
}

#[tauri::command]
pub async fn retrieve_vustb_friends(app: AppHandle) -> USTBLResult<Vec<VustbFriend>> {
  vustb::fetch_friends(&app).await
}

#[tauri::command]
pub async fn retrieve_vustb_skin_library(
  app: AppHandle,
  page: u32,
  limit: u32,
  texture_type: Option<String>,
) -> USTBLResult<VustbTexturePage> {
  vustb::fetch_skin_library(&app, page, limit, texture_type).await
}

#[tauri::command]
pub async fn retrieve_vustb_wardrobe(
  app: AppHandle,
  texture_type: Option<String>,
) -> USTBLResult<Vec<VustbTexture>> {
  let include_local_skins = texture_type
    .as_deref()
    .is_none_or(|texture_type| texture_type.eq_ignore_ascii_case("skin"));
  let mut wardrobe = vustb::fetch_wardrobe(&app, texture_type).await?;
  if include_local_skins {
    let cloud_hashes = wardrobe
      .iter()
      .map(|texture| texture.hash.clone())
      .collect::<HashSet<_>>();
    let mut local_backups = skin_backup::list_local_skin_backups(&cloud_hashes)?;
    local_backups.append(&mut wardrobe);
    Ok(local_backups)
  } else {
    Ok(wardrobe)
  }
}

#[tauri::command]
pub async fn collect_vustb_texture(app: AppHandle, hash: String) -> USTBLResult<()> {
  vustb::collect_texture(&app, &hash).await
}

fn replace_player_texture(
  app: &AppHandle,
  player_id: &str,
  texture_type: TextureType,
  texture: Option<Texture>,
) -> USTBLResult<()> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let mut state = binding.lock()?;
  let player = state
    .get_player_by_id_mut(player_id.to_string())
    .ok_or(AccountError::NotFound)?;
  player
    .textures
    .retain(|item| item.texture_type != texture_type);
  if let Some(texture) = texture {
    player.textures.push(texture);
  } else if texture_type == TextureType::Skin {
    player
      .textures
      .extend(offline::load_preset_skin(app, PresetRole::Steve)?);
  }
  state.save()?;
  Ok(())
}

#[tauri::command]
pub async fn apply_vustb_texture_to_player(
  app: AppHandle,
  player_id: String,
  texture: VustbTexture,
) -> USTBLResult<()> {
  let player = {
    let binding = app.state::<Mutex<AccountInfo>>();
    let player = binding
      .lock()?
      .players
      .iter()
      .find(|item| item.id == player_id)
      .cloned()
      .ok_or(AccountError::NotFound)?;
    player
  };
  let texture_type = if texture.texture_type.eq_ignore_ascii_case("cape") {
    TextureType::Cape
  } else {
    TextureType::Skin
  };
  let local_texture = texture
    .local_backup_id
    .as_deref()
    .map(skin_backup::load_local_skin_backup)
    .transpose()?;
  let model = local_texture
    .as_ref()
    .map(|texture| texture.model.clone())
    .unwrap_or_else(|| texture.model.parse().unwrap_or_default());

  if texture_type == TextureType::Skin
    && matches!(
      player.player_type,
      PlayerType::Offline | PlayerType::Microsoft
    )
  {
    let cloud_hashes = vustb::fetch_wardrobe(&app, Some("skin".to_string()))
      .await
      .map(|textures| {
        textures
          .into_iter()
          .map(|texture| texture.hash)
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    skin_backup::backup_current_skin(&player, &cloud_hashes)?;
  }

  match player.player_type {
    PlayerType::Offline => {}
    PlayerType::ThirdParty
      if player
        .auth_server_url
        .as_deref()
        .is_some_and(|url| normalize_url(url) == normalize_url(USTB_AUTH_SERVER_URL)) =>
    {
      vustb::select_profile_texture(
        &app,
        &player.uuid.simple().to_string(),
        &texture.texture_type,
        Some(&texture.hash),
      )
      .await?;
    }
    PlayerType::Microsoft if texture_type == TextureType::Skin => {
      if let Some(local_backup_id) = texture.local_backup_id.as_deref() {
        microsoft::oauth::set_skin_from_bytes(
          &app,
          &player,
          skin_backup::local_skin_backup_bytes(local_backup_id)?,
          model.clone(),
        )
        .await?;
      } else {
        microsoft::oauth::set_skin_from_url(&app, &player, &texture.url, model.clone()).await?;
      }
    }
    _ => return Err(AccountError::Invalid.into()),
  }

  let image = match local_texture {
    Some(texture) => texture.image,
    None => misc::fetch_image(&app, texture.url).await?,
  };
  replace_player_texture(
    &app,
    &player_id,
    texture_type.clone(),
    Some(Texture {
      texture_type,
      image,
      model,
      preset: None,
      source_hash: (!texture.local_backup).then_some(texture.hash),
    }),
  )
}

#[tauri::command]
pub async fn clear_player_texture(
  app: AppHandle,
  player_id: String,
  texture_type: TextureType,
) -> USTBLResult<()> {
  let player = {
    let binding = app.state::<Mutex<AccountInfo>>();
    let player = binding
      .lock()?
      .players
      .iter()
      .find(|item| item.id == player_id)
      .cloned()
      .ok_or(AccountError::NotFound)?;
    player
  };
  if texture_type == TextureType::Skin
    && matches!(
      player.player_type,
      PlayerType::Offline | PlayerType::Microsoft
    )
  {
    let cloud_hashes = vustb::fetch_wardrobe(&app, Some("skin".to_string()))
      .await
      .map(|textures| {
        textures
          .into_iter()
          .map(|texture| texture.hash)
          .collect::<Vec<_>>()
      })
      .unwrap_or_default();
    skin_backup::backup_current_skin(&player, &cloud_hashes)?;
  }
  match player.player_type {
    PlayerType::Offline => {}
    PlayerType::ThirdParty
      if player
        .auth_server_url
        .as_deref()
        .is_some_and(|url| normalize_url(url) == normalize_url(USTB_AUTH_SERVER_URL)) =>
    {
      vustb::select_profile_texture(
        &app,
        &player.uuid.simple().to_string(),
        &texture_type.to_string().to_lowercase(),
        None,
      )
      .await?;
    }
    PlayerType::Microsoft if texture_type == TextureType::Skin => {
      microsoft::oauth::clear_skin(&app, &player).await?;
    }
    _ => return Err(AccountError::Invalid.into()),
  }
  replace_player_texture(&app, &player_id, texture_type, None)
}

#[tauri::command]
pub async fn logout_vustb_account(app: AppHandle) -> USTBLResult<()> {
  let _ = vustb_presence::clear(&app).await;
  {
    let binding = app.state::<Mutex<AccountInfo>>();
    let mut account_state = binding.lock()?;
    let removed_player_ids: Vec<String> = account_state
      .players
      .iter()
      .filter(|player| {
        player
          .auth_server_url
          .as_deref()
          .is_some_and(|url| normalize_url(url) == normalize_url(USTB_AUTH_SERVER_URL))
      })
      .map(|player| player.id.clone())
      .collect();
    account_state
      .players
      .retain(|player| !removed_player_ids.contains(&player.id));
    account_state.vustb_account = None;
    account_state.vustb_session = None;
    account_state.save()?;

    let config_binding = app.state::<Mutex<LauncherConfig>>();
    let mut config_state = config_binding.lock()?;
    if removed_player_ids.contains(&config_state.states.shared.selected_player_id) {
      config_state.partial_update(
        &app,
        "states.shared.selected_player_id",
        &serde_json::to_string(
          &account_state
            .players
            .first()
            .map_or("".to_string(), |player| player.id.clone()),
        )
        .unwrap_or_default(),
      )?;
      config_state.save()?;
    }
  }
  misc::check_full_login_availability(&app).await
}

#[tauri::command]
pub async fn relogin_player_oauth(
  app: AppHandle,
  player_id: String,
  auth_info: DeviceAuthResponseInfo,
) -> USTBLResult<()> {
  let account_binding = app.state::<Mutex<AccountInfo>>();

  let cloned_account_state = account_binding.lock()?.clone();

  let old_player = cloned_account_state
    .players
    .iter()
    .find(|player| player.id == player_id)
    .ok_or(AccountError::NotFound)?;

  let new_player = match old_player.player_type {
    PlayerType::ThirdParty => {
      let auth_server = AuthServer::from(get_auth_server_info_by_url(
        &app,
        old_player.auth_server_url.clone().unwrap_or_default(),
      )?);

      authlib_injector::oauth::login(
        &app,
        old_player.auth_server_url.clone().unwrap_or_default(),
        auth_server.features.openid_configuration_url,
        auth_server.client_id,
        auth_info,
        auth_server.redirect_uri,
        auth_server.client_secret,
      )
      .await?
    }

    PlayerType::Microsoft => microsoft::oauth::login(&app, auth_info).await?,

    PlayerType::Offline => {
      return Err(AccountError::Invalid.into());
    }
  };

  {
    let mut account_state = account_binding.lock()?;

    if let Some(player) = account_state
      .players
      .iter_mut()
      .find(|player| player.id == player_id)
    {
      *player = new_player;
      account_state.save()?;
    }
  }

  misc::check_full_login_availability(&app).await
}

#[tauri::command]
pub fn cancel_oauth(app: AppHandle) -> USTBLResult<()> {
  let account_binding = app.state::<Mutex<AccountInfo>>();
  let mut account_state = account_binding.lock()?;
  account_state.is_oauth_processing = false;

  Ok(())
}

#[tauri::command]
pub async fn add_player_3rdparty_password(
  app: AppHandle,
  auth_server_url: String,
  username: String,
  password: String,
) -> USTBLResult<Vec<Player>> {
  let _ = check_authlib_jar(&app).await; // ignore the error when logging in

  let (mut new_players, is_token_binded) =
    authlib_injector::password::login(&app, auth_server_url, username, password).await?;

  if new_players.is_empty() {
    return Err(AccountError::NotFound.into());
  }

  {
    let account_binding = app.state::<Mutex<AccountInfo>>();
    let account_state = account_binding.lock()?;
    new_players.retain_mut(|new_player| {
      account_state
        .players
        .iter()
        .all(|player| new_player.id != player.id)
    });
  }

  if new_players.is_empty() {
    Err(AccountError::Duplicate.into())
  } else if new_players.len() == 1 {
    // if only one player will be added, save it and return **an empty vector** to inform the frontend not to trigger selector.
    if !is_token_binded {
      // if the token is not binded, refresh it to bind the token.
      new_players[0] = authlib_injector::password::refresh(&app, &new_players[0], true).await?;
    }
    {
      let account_binding = app.state::<Mutex<AccountInfo>>();
      let mut account_state = account_binding.lock()?;
      let config_binding = app.state::<Mutex<LauncherConfig>>();
      let mut config_state = config_binding.lock()?;

      config_state.partial_update(
        &app,
        "states.shared.selected_player_id",
        &serde_json::to_string(&new_players[0].id).unwrap_or_default(),
      )?;
      account_state.players.push(new_players[0].clone());

      account_state.save()?;
      config_state.save()?;
    }

    Ok(vec![])
  } else {
    // if more than one player will be added, return the players to inform the frontend to trigger selector.
    let players = new_players
      .iter()
      .map(|player| Player::from(player.clone()))
      .collect::<Vec<Player>>();

    Ok(players)
  }
}

#[tauri::command]
pub async fn relogin_player_3rdparty_password(
  app: AppHandle,
  player_id: String,
  password: String,
) -> USTBLResult<()> {
  let account_binding = app.state::<Mutex<AccountInfo>>();

  let cloned_account_state = account_binding.lock()?.clone();

  let old_player = cloned_account_state
    .players
    .iter()
    .find(|player| player.id == player_id)
    .ok_or(AccountError::NotFound)?;

  if old_player.player_type != PlayerType::ThirdParty {
    return Err(AccountError::Invalid.into());
  }

  let (player_list, is_token_binded) = authlib_injector::password::login(
    &app,
    old_player.auth_server_url.clone().unwrap_or_default(),
    old_player.auth_account.clone().unwrap_or_default(),
    password,
  )
  .await?;

  let mut new_player = player_list
    .into_iter()
    .find(|player| player.uuid == old_player.uuid)
    .ok_or(AccountError::NotFound)?;

  if !is_token_binded {
    new_player = authlib_injector::password::refresh(&app, &new_player, true).await?;
  }

  {
    let mut account_state = account_binding.lock()?;

    if let Some(player) = account_state
      .players
      .iter_mut()
      .find(|player| player.id == player_id)
    {
      *player = new_player;
      account_state.save()?;
    }
  }

  misc::check_full_login_availability(&app).await
}

#[tauri::command]
pub async fn add_player_from_selection(app: AppHandle, player: Player) -> USTBLResult<()> {
  let player_info: PlayerInfo = player.into();
  let refreshed_player = authlib_injector::password::refresh(&app, &player_info, true).await?;

  {
    let account_binding = app.state::<Mutex<AccountInfo>>();
    let mut account_state = account_binding.lock()?;

    let config_binding = app.state::<Mutex<LauncherConfig>>();
    let mut config_state = config_binding.lock()?;

    if account_state
      .players
      .iter()
      .any(|x| x.id == refreshed_player.id)
    {
      return Err(AccountError::Duplicate.into());
    }

    config_state.partial_update(
      &app,
      "states.shared.selected_player_id",
      &serde_json::to_string(&refreshed_player.id).unwrap_or_default(),
    )?;
    account_state.players.push(refreshed_player);

    account_state.save()?;
    config_state.save()?;
  }

  misc::check_full_login_availability(&app).await
}

#[tauri::command]
pub fn update_player_skin_offline_preset(
  app: AppHandle,
  player_id: String,
  preset_role: PresetRole,
) -> USTBLResult<()> {
  let account_binding = app.state::<Mutex<AccountInfo>>();
  let mut account_state = account_binding.lock()?;

  let current_player = account_state
    .players
    .iter()
    .find(|player| player.id == player_id)
    .cloned()
    .ok_or(AccountError::NotFound)?;
  if current_player.player_type != PlayerType::Offline {
    return Err(AccountError::Invalid.into());
  }
  skin_backup::backup_current_skin(&current_player, &[])?;

  let player = account_state
    .get_player_by_id_mut(player_id.clone())
    .ok_or(AccountError::NotFound)?;

  player.textures = offline::load_preset_skin(&app, preset_role)?;
  account_state.save()?;
  Ok(())
}

#[tauri::command]
pub fn update_player_skin_offline_local(
  app: AppHandle,
  player_id: String,
  image_path: String,
  texture_type: TextureType,
  skin_model: SkinModel,
) -> USTBLResult<()> {
  let image_path = if image_path == "dummy" {
    // this is an Easter Egg :)
    get_app_resource_filepath(&app, "assets/skins/dummy.png")
      .map_err(|_| AccountError::TextureError)?
  } else {
    Path::new(&image_path).to_path_buf()
  };
  let texture_img =
    crate::utils::image::load_image_from_dir(&image_path).ok_or(AccountError::TextureError)?;

  let account_binding = app.state::<Mutex<AccountInfo>>();
  let mut account_state = account_binding.lock()?;

  let current_player = account_state
    .players
    .iter()
    .find(|player| player.id == player_id)
    .cloned()
    .ok_or(AccountError::NotFound)?;
  if current_player.player_type != PlayerType::Offline {
    return Err(AccountError::Invalid.into());
  }
  if texture_type == TextureType::Skin {
    skin_backup::backup_current_skin(&current_player, &[])?;
  }

  let player = account_state
    .get_player_by_id_mut(player_id.clone())
    .ok_or(AccountError::NotFound)?;

  // remove existing texture of the same type
  player
    .textures
    .retain(|texture| texture.texture_type != texture_type);

  // add the new texture
  player.textures.push(crate::account::models::Texture {
    texture_type: texture_type.clone(),
    image: texture_img.into(),
    model: skin_model.clone(),
    preset: None,
    source_hash: None,
  });

  account_state.save()?;
  Ok(())
}

#[tauri::command]
pub async fn delete_player(app: AppHandle, player_id: String) -> USTBLResult<()> {
  {
    let account_binding = app.state::<Mutex<AccountInfo>>();
    let mut account_state = account_binding.lock()?;

    let config_binding = app.state::<Mutex<LauncherConfig>>();
    let mut config_state = config_binding.lock()?;

    let initial_len = account_state.players.len();
    account_state.players.retain(|s| s.id != player_id);
    if account_state.players.len() == initial_len {
      return Err(AccountError::NotFound.into());
    }

    let deleted_linked_vustb_player = account_state
      .vustb_account
      .as_ref()
      .is_some_and(|account| account.player_id == player_id);
    if deleted_linked_vustb_player {
      if account_state.vustb_session.is_some() {
        let replacement_player_id = account_state
          .players
          .iter()
          .find(|player| {
            player
              .auth_server_url
              .as_deref()
              .is_some_and(|url| normalize_url(url) == normalize_url(USTB_AUTH_SERVER_URL))
          })
          .map(|player| player.id.clone())
          .unwrap_or_default();
        if let Some(account) = account_state.vustb_account.as_mut() {
          account.player_id = replacement_player_id;
        }
      } else {
        account_state.vustb_account = None;
      }
    }

    if config_state.states.shared.selected_player_id == player_id {
      config_state.partial_update(
        &app,
        "states.shared.selected_player_id",
        &serde_json::to_string(
          &account_state
            .players
            .first()
            .map_or("".to_string(), |player| player.id.clone()),
        )
        .unwrap_or_default(),
      )?;
      config_state.save()?;
    }

    account_state.save()?;
  }

  misc::check_full_login_availability(&app).await
}

#[tauri::command]
pub async fn refresh_player(app: AppHandle, player_id: String) -> USTBLResult<()> {
  let account_binding = app.state::<Mutex<AccountInfo>>();

  let cloned_account_state = account_binding.lock()?.clone();

  let player = cloned_account_state
    .players
    .iter()
    .find(|player| player.id == player_id)
    .ok_or(AccountError::NotFound)?;

  let refreshed_player = match player.player_type {
    PlayerType::ThirdParty => {
      let auth_server = AuthServer::from(get_auth_server_info_by_url(
        &app,
        player.auth_server_url.clone().unwrap_or_default(),
      )?);

      authlib_injector::common::refresh(&app, player, &auth_server).await?
    }

    PlayerType::Microsoft => microsoft::oauth::refresh(&app, player).await?,

    PlayerType::Offline => {
      return Err(AccountError::Invalid.into());
    }
  };

  let mut account_state = account_binding.lock()?;

  if let Some(player) = account_state
    .players
    .iter_mut()
    .find(|player| player.id == player_id)
  {
    *player = refreshed_player;
    account_state.save()?;
  }

  Ok(())
}

#[tauri::command]
pub fn retrieve_auth_server_list(app: AppHandle) -> USTBLResult<Vec<AuthServer>> {
  let binding = app.state::<Mutex<AccountInfo>>();
  let state = binding.lock()?;
  let auth_servers = state
    .auth_servers
    .iter()
    .map(|server| AuthServer::from(server.clone()))
    .collect();
  Ok(auth_servers)
}

#[tauri::command]
pub async fn fetch_auth_server(app: AppHandle, url: String) -> USTBLResult<AuthServer> {
  // check the url integrity following the standard
  // https://github.com/yushijinhun/authlib-injector/wiki/%E5%90%AF%E5%8A%A8%E5%99%A8%E6%8A%80%E6%9C%AF%E8%A7%84%E8%8C%83#%E5%9C%A8%E5%90%AF%E5%8A%A8%E5%99%A8%E4%B8%AD%E8%BE%93%E5%85%A5%E5%9C%B0%E5%9D%80
  let parsed_url = Url::parse(&url)
    .or(Url::parse(&format!("https://{}", url)))
    .map_err(|_| AccountError::Invalid)?;

  let auth_url = fetch_auth_url(&app, parsed_url).await?;

  if get_auth_server_info_by_url(&app, auth_url.clone()).is_ok() {
    return Err(AccountError::Duplicate.into());
  }

  Ok(AuthServer::from(
    fetch_auth_server_info(&app, auth_url).await?,
  ))
}

#[tauri::command]
pub async fn add_auth_server(app: AppHandle, auth_url: String) -> USTBLResult<()> {
  if get_auth_server_info_by_url(&app, auth_url.clone()).is_ok() {
    return Err(AccountError::Duplicate.into());
  }

  let server = fetch_auth_server_info(&app, auth_url).await?;

  let binding = app.state::<Mutex<AccountInfo>>();
  let mut state = binding.lock()?;
  state.auth_servers.push(server);
  state.save()?;
  Ok(())
}

#[tauri::command]
pub fn delete_auth_server(app: AppHandle, url: String) -> USTBLResult<()> {
  // prevent deletion of preset auth servers
  if PRESET_AUTH_SERVERS
    .iter()
    .any(|s| normalize_url(s) == normalize_url(&url))
  {
    return Err(AccountError::Invalid.into());
  }

  let account_binding = app.state::<Mutex<AccountInfo>>();
  let mut account_state = account_binding.lock()?;

  let config_binding = app.state::<Mutex<LauncherConfig>>();
  let mut config_state = config_binding.lock()?;

  let initial_len = account_state.auth_servers.len();

  // try to remove the server from the storage
  account_state
    .auth_servers
    .retain(|server| server.auth_url != url);
  if account_state.auth_servers.len() == initial_len {
    return Err(AccountError::NotFound.into());
  }

  // remove all players using this server & check if the selected player needs reset
  let mut need_reset = false;
  let selected_id = config_state.states.shared.selected_player_id.clone();

  account_state.players.retain(|player| {
    let should_remove = player.auth_server_url == Some(url.clone());
    if should_remove && player.id == selected_id {
      need_reset = true;
    }
    !should_remove
  });

  if need_reset {
    config_state.partial_update(
      &app,
      "states.shared.selected_player_id",
      &serde_json::to_string(
        &(if let Some(first_player) = account_state.players.first() {
          first_player.id.clone()
        } else {
          "".to_string()
        }),
      )
      .unwrap_or_default(),
    )?;
  }

  account_state.save()?;
  config_state.save()?;
  Ok(())
}
