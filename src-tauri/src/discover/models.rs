use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewsSourceInfo {
  pub name: String,
  pub full_name: String,
  pub endpoint_url: String,
  pub icon_src: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewsPostSummary {
  pub title: String,
  #[serde(rename = "abstract")]
  pub abstracts: String,
  pub keywords: String,
  pub image_src: (String, u64, u64),
  pub source: NewsSourceInfo,
  pub create_at: String, // ISO Datetime String
  pub link: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsPostRequest {
  pub url: String,
  pub cursor: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsPostResponse {
  pub posts: Vec<NewsPostSummary>,
  pub next: Option<u64>,
  pub cursors: Option<HashMap<String, u64>>,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct McServerMotdSegment {
  #[serde(default)]
  pub text: String,
  #[serde(default)]
  pub color: Option<String>,
  #[serde(default)]
  pub bold: bool,
  #[serde(default)]
  pub italic: bool,
  #[serde(default)]
  pub underlined: bool,
  #[serde(default)]
  pub strikethrough: bool,
  #[serde(default)]
  pub obfuscated: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct McServerStatus {
  pub id: u64,
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub address: Option<String>,
  #[serde(default)]
  pub description: Option<String>,
  #[serde(default)]
  pub icon_url: Option<String>,
  #[serde(default)]
  pub banner_urls: Vec<String>,
  #[serde(default)]
  pub version_hint: Option<String>,
  #[serde(default)]
  pub theme: Option<String>,
  #[serde(default)]
  pub parent_id: Option<u64>,
  #[serde(default)]
  pub expose_ip: bool,
  #[serde(default)]
  pub motd_segments: Vec<McServerMotdSegment>,
  #[serde(default)]
  pub connect_ms: Option<u64>,
  #[serde(default)]
  pub protocol: Option<i64>,
  #[serde(default)]
  pub players_online: Option<u64>,
  #[serde(default)]
  pub players_max: Option<u64>,
  #[serde(default)]
  pub last_update: Option<String>,
  #[serde(default)]
  pub server_status: String,
  #[serde(default)]
  pub r#type: Option<String>,
  #[serde(default)]
  pub version: Option<String>,
  #[serde(default)]
  pub icon: Option<String>,
}

#[cfg(test)]
mod tests {
  use super::McServerStatus;

  #[test]
  fn server_status_deserializes_api_shape_and_serializes_frontend_shape() {
    let server: McServerStatus = serde_json::from_str(
      r#"{
        "id": 2,
        "name": "主服",
        "address": "mc.ustb.world",
        "banner_urls": [],
        "expose_ip": true,
        "motd_segments": [{"text": "USTB"}],
        "players_online": 1,
        "players_max": 20,
        "server_status": "online",
        "type": "java"
      }"#,
    )
    .unwrap();
    let value = serde_json::to_value(server).unwrap();

    assert_eq!(value["playersOnline"], 1);
    assert_eq!(value["serverStatus"], "online");
    assert_eq!(value["type"], "java");
    assert!(value.get("players_online").is_none());
  }
}
