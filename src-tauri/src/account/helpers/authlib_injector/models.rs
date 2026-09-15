use std::collections::HashMap;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MinecraftProfileProperty {
  pub name: String,
  pub value: String,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct MinecraftProfile {
  #[serde(alias = "uuid")]
  pub id: String,
  #[serde(alias = "username")]
  pub name: String,
  #[serde(default)]
  pub properties: Option<Vec<MinecraftProfileProperty>>,
  #[serde(default)]
  pub selected: bool,
}

structstruck::strike! {
  #[strikethrough[derive(serde::Deserialize, serde::Serialize)]]
  pub struct TextureInfo {
    pub textures: HashMap<String, pub struct {
      pub url: String,
      pub metadata: Option<HashMap<String, String>>,
    }>
  }
}
