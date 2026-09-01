use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Url};
use tauri_plugin_http::reqwest;

use crate::error::USTBLResult;
use crate::instance::models::misc::InstanceError;
use crate::resource::helpers::misc::get_download_api;
use crate::resource::models::{ResourceType, SourceType};
use crate::storage::{load_json_async, save_json_async};
use crate::utils::web::fetch_json_with_fallbacks;

const VANILLA_METADATA_DOWNLOAD_ATTEMPTS: usize = 10;

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct AssetIndex {
  pub objects: HashMap<String, AssetIndexItem>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct AssetIndexItem {
  pub hash: String,
  pub size: i64,
}

pub async fn load_asset_index(
  app: &AppHandle,
  asset_index_path: &Path,
  asset_index_id: &str,
  asset_index_sha1: &str,
  asset_index_url: &str,
  priority: &[SourceType],
) -> USTBLResult<AssetIndex> {
  if asset_index_path.exists() {
    let asset_index = load_json_async::<AssetIndex>(asset_index_path)
      .await
      .map_err(|_| InstanceError::AssetIndexParseError)?;

    Ok(asset_index)
  } else {
    let original_url = Url::parse(asset_index_url).map_err(|_| InstanceError::NetworkError)?;
    let official_url = Url::parse(&format!(
      "https://piston-meta.mojang.com/v1/packages/{asset_index_sha1}/{asset_index_id}.json"
    ))
    .map_err(|_| InstanceError::NetworkError)?;
    let mut sources = Vec::new();
    for source in priority {
      let candidate = match source {
        SourceType::Official => official_url.clone(),
        SourceType::BMCLAPIMirror => get_download_api(*source, ResourceType::Launcher)?
          .join(&format!("assets/indexes/{asset_index_id}.json"))?,
      };
      if !sources.contains(&candidate) {
        sources.push(candidate);
      }
    }
    if sources.is_empty() {
      sources.push(original_url.clone());
    }
    if !sources.contains(&original_url) {
      sources.push(original_url);
    }

    let client = app.state::<reqwest::Client>();
    let asset_index =
      fetch_json_with_fallbacks(client.inner(), &sources, VANILLA_METADATA_DOWNLOAD_ATTEMPTS)
        .await?;

    save_json_async(&asset_index, asset_index_path).await?;

    Ok(asset_index)
  }
}
