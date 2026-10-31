use crate::error::{USTBLError, USTBLResult};
use base64::{engine::general_purpose::STANDARD, Engine};
use p256::{elliptic_curve::rand_core::OsRng, pkcs8::EncodePublicKey, SecretKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

const API: &str = "https://api.cloudflareclient.com/v0a4471";

fn encode_private_key(key: &SecretKey) -> USTBLResult<String> {
  use p256::pkcs8::der::Encode;
  let der = key.to_sec1_der()?;
  let mut encoded = sec1::EcPrivateKey::try_from(der.as_slice())?;
  encoded.parameters = Some(sec1::EcParameters::NamedCurve(
    p256::pkcs8::ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7"),
  ));
  Ok(STANDARD.encode(encoded.to_der()?))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarpDevice {
  pub private_key: String,
  pub peer_public_key: String,
  pub ipv4: String,
  pub ipv6: String,
}

impl WarpDevice {
  pub fn validate(&self) -> USTBLResult<()> {
    SecretKey::from_sec1_der(&STANDARD.decode(&self.private_key)?)?;
    use p256::pkcs8::DecodePublicKey;
    p256::PublicKey::from_public_key_der(&STANDARD.decode(&self.peer_public_key)?)?;
    self.ipv4.parse::<std::net::Ipv4Addr>()?;
    self.ipv6.parse::<std::net::Ipv6Addr>()?;
    Ok(())
  }

  #[cfg(test)]
  pub fn test_device() -> Self {
    let key = SecretKey::random(&mut OsRng);
    Self {
      private_key: encode_private_key(&key).unwrap(),
      peer_public_key: STANDARD.encode(key.public_key().to_public_key_der().unwrap().as_bytes()),
      ipv4: "172.16.0.2".into(),
      ipv6: "2606:4700:110::1234".into(),
    }
  }
}

pub async fn register() -> USTBLResult<WarpDevice> {
  let client = reqwest::Client::builder()
    .no_gzip()
    .no_proxy()
    .timeout(Duration::from_secs(20))
    .build()?;
  let mut bytes = [0u8; 32];
  rand::rng().fill_bytes(&mut bytes);
  let account = request(
    &client,
    reqwest::Method::POST,
    "/reg",
    None,
    json!({
      "key": STANDARD.encode(bytes), "install_id": "", "fcm_token": "",
      "tos": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
      "model": "PC", "serial_number": uuid::Uuid::new_v4().simple().to_string(),
      "os_version": "", "key_type": "curve25519", "tunnel_type": "wireguard", "locale": "en-US"
    }),
  )
  .await?;
  let id = account["id"]
    .as_str()
    .filter(|id| id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
    .ok_or_else(|| USTBLError("WARP 注册未返回有效设备 ID".into()))?;
  let token = account["token"]
    .as_str()
    .filter(|token| !token.is_empty())
    .ok_or_else(|| USTBLError("WARP 注册未返回令牌".into()))?;
  let key = SecretKey::random(&mut OsRng);
  let enrolled = request(
    &client,
    reqwest::Method::PATCH,
    &format!("/reg/{id}"),
    Some(token),
    json!({
      "key": STANDARD.encode(key.public_key().to_public_key_der()?.as_bytes()),
      "key_type": "secp256r1", "tunnel_type": "masque", "name": "USTBL"
    }),
  )
  .await?;
  let peer = enrolled["config"]["peers"][0]["public_key"]
    .as_str()
    .unwrap_or_default()
    .replace("-----BEGIN PUBLIC KEY-----", "")
    .replace("-----END PUBLIC KEY-----", "")
    .split_whitespace()
    .collect::<String>();
  let device = WarpDevice {
    private_key: encode_private_key(&key)?,
    peer_public_key: peer,
    ipv4: enrolled["config"]["interface"]["addresses"]["v4"]
      .as_str()
      .unwrap_or_default()
      .into(),
    ipv6: enrolled["config"]["interface"]["addresses"]["v6"]
      .as_str()
      .unwrap_or_default()
      .into(),
  };
  device.validate()?;
  Ok(device)
}

async fn request(
  client: &reqwest::Client,
  method: reqwest::Method,
  path: &str,
  token: Option<&str>,
  body: Value,
) -> USTBLResult<Value> {
  let mut request = client
    .request(method, format!("{API}{path}"))
    .header("User-Agent", "WARP for Android")
    .header("CF-Client-Version", "a-6.35-4471")
    .json(&body);
  if let Some(token) = token {
    request = request.bearer_auth(token);
  }
  let response = request.send().await?.error_for_status()?;
  let body = response.bytes().await?;
  if body.len() > 1024 * 1024 {
    return Err(USTBLError("WARP 响应过大".into()));
  }
  Ok(serde_json::from_slice(&body)?)
}
