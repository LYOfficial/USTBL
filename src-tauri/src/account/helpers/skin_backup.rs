use crate::account::models::{
  AccountError, PlayerInfo, SkinModel, Texture, TextureType, VustbTexture,
};
use crate::error::USTBLResult;
use crate::utils::image::load_image_from_dir;
use crate::APP_DATA_DIR;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const BACKUP_DIR_NAME: &str = "skin-backups";
const BACKUP_INDEX_NAME: &str = "index.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSkinBackup {
  id: String,
  player_id: String,
  player_name: String,
  model: SkinModel,
  created_at: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSkinBackupIndex {
  backups: Vec<LocalSkinBackup>,
}

fn backup_root() -> USTBLResult<PathBuf> {
  APP_DATA_DIR
    .get()
    .map(|path| path.join(BACKUP_DIR_NAME))
    .ok_or_else(|| AccountError::TextureError.into())
}

fn load_index() -> USTBLResult<LocalSkinBackupIndex> {
  let path = backup_root()?.join(BACKUP_INDEX_NAME);
  if !path.exists() {
    return Ok(LocalSkinBackupIndex::default());
  }
  let contents = fs::read_to_string(path).map_err(|_| AccountError::TextureError)?;
  serde_json::from_str(&contents).map_err(|_| AccountError::ParseError.into())
}

fn save_index(index: &LocalSkinBackupIndex) -> USTBLResult<()> {
  let root = backup_root()?;
  fs::create_dir_all(&root).map_err(|_| AccountError::TextureError)?;
  let contents = serde_json::to_string_pretty(index).map_err(|_| AccountError::ParseError)?;
  fs::write(root.join(BACKUP_INDEX_NAME), contents).map_err(|_| AccountError::TextureError)?;
  Ok(())
}

fn backup_id(player: &PlayerInfo, texture: &Texture) -> String {
  let mut hasher = Sha256::new();
  hasher.update(player.id.as_bytes());
  hasher.update(texture.image.image.width().to_le_bytes());
  hasher.update(texture.image.image.height().to_le_bytes());
  hasher.update(texture.image.image.as_raw());
  hex::encode(hasher.finalize())
}

fn texture_hash(texture: &Texture) -> String {
  let image = &texture.image.image;
  let mut hasher = Sha256::new();
  hasher.update(image.width().to_be_bytes());
  hasher.update(image.height().to_be_bytes());
  for x in 0..image.width() {
    for y in 0..image.height() {
      let [mut red, mut green, mut blue, alpha] = image.get_pixel(x, y).0;
      if alpha == 0 {
        red = 0;
        green = 0;
        blue = 0;
      }
      hasher.update([alpha, red, green, blue]);
    }
  }
  hex::encode(hasher.finalize())
}

fn backup_path(id: &str) -> USTBLResult<PathBuf> {
  Ok(backup_root()?.join("textures").join(format!("{id}.png")))
}

pub fn backup_current_skin(player: &PlayerInfo, cloud_hashes: &[String]) -> USTBLResult<()> {
  let Some(texture) = player
    .textures
    .iter()
    .find(|texture| texture.texture_type == TextureType::Skin)
  else {
    return Ok(());
  };

  if texture.source_hash.is_some() || cloud_hashes.contains(&texture_hash(texture)) {
    return Ok(());
  }

  let id = backup_id(player, texture);
  let mut index = load_index()?;
  if index.backups.iter().any(|backup| backup.id == id) {
    return Ok(());
  }

  let path = backup_path(&id)?;
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).map_err(|_| AccountError::TextureError)?;
  }
  texture
    .image
    .image
    .save(&path)
    .map_err(|_| AccountError::TextureError)?;
  index.backups.insert(
    0,
    LocalSkinBackup {
      id,
      player_id: player.id.clone(),
      player_name: player.name.clone(),
      model: texture.model.clone(),
      created_at: chrono::Utc::now().to_rfc3339(),
    },
  );
  save_index(&index)
}

pub fn list_local_skin_backups(cloud_hashes: &HashSet<String>) -> USTBLResult<Vec<VustbTexture>> {
  let index = load_index()?;
  let mut textures = Vec::with_capacity(index.backups.len());
  for backup in index.backups {
    let path = backup_path(&backup.id)?;
    let Some(image) = load_image_from_dir(&path) else {
      continue;
    };
    let texture = Texture {
      texture_type: TextureType::Skin,
      image: image.into(),
      model: backup.model.clone(),
      preset: None,
      source_hash: None,
    };
    if cloud_hashes.contains(&texture_hash(&texture)) {
      continue;
    }
    let Ok(bytes) = fs::read(path) else {
      continue;
    };
    textures.push(VustbTexture {
      hash: format!("local-{}", backup.id),
      texture_type: "skin".to_string(),
      name: backup.player_name,
      model: backup.model.to_string(),
      is_public: false,
      uploader_name: "本地".to_string(),
      created_at: Some(backup.created_at),
      url: format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
      ),
      collected: true,
      local_backup: true,
      local_backup_id: Some(backup.id),
    });
  }
  Ok(textures)
}

#[cfg(test)]
mod tests {
  use super::texture_hash;
  use crate::account::models::{SkinModel, Texture, TextureType};
  use image::{Rgba, RgbaImage};

  #[test]
  fn texture_hash_matches_vustb_pixel_hash() {
    let texture = Texture {
      texture_type: TextureType::Skin,
      image: RgbaImage::from_pixel(1, 1, Rgba([10, 20, 30, 255])).into(),
      model: SkinModel::Default,
      preset: None,
      source_hash: None,
    };

    assert_eq!(
      texture_hash(&texture),
      "c2a3d002fae92b2d184cf6da86964fae2bc2344536aa1fc695e8fa4fbf90105f"
    );
  }
}
