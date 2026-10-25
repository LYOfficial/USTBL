// Copyright (c) 2025 pynickle. This is a fork of Original Crate. Original copyright: Copyright (c) 2025 NameOfShadow

use std::fmt;
use std::fs::File;
use std::io::Write;

use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::McError;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServerStatus {
    pub online: bool,
    pub ip: String,
    pub port: u16,
    pub hostname: String,
    pub latency: f64,
    pub dns: Option<DnsInfo>,
    pub data: ServerData,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum ServerData {
    Java(JavaStatus),
    Bedrock(BedrockStatus),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DnsInfo {
    pub a_records: Vec<String>,
    pub cname: Option<String>,
    pub ttl: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct JavaStatus {
    pub version: JavaVersion,
    pub players: JavaPlayers,
    pub description: String,
    #[serde(skip_serializing)]
    pub favicon: Option<String>,
    pub map: Option<String>,
    pub gamemode: Option<String>,
    pub software: Option<String>,
    pub plugins: Option<Vec<JavaPlugin>>,
    pub mods: Option<Vec<JavaMod>>,
    #[serde(skip)]
    pub raw_data: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaVersion {
    pub name: String,
    pub protocol: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaPlayers {
    pub online: i64,
    pub max: i64,
    pub sample: Option<Vec<JavaPlayer>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaPlayer {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaPlugin {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JavaMod {
    pub modid: String,
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BedrockStatus {
    pub edition: String,
    pub motd: String,
    pub protocol_version: String,
    pub version: String,
    pub online_players: String,
    pub max_players: String,
    pub server_uid: String,
    pub motd2: String,
    pub game_mode: String,
    pub game_mode_numeric: String,
    pub port_ipv4: String,
    pub port_ipv6: String,
    pub map: Option<String>,
    pub software: Option<String>,
    pub raw_data: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServerInfo {
    pub address: String,
    pub edition: ServerEdition,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum ServerEdition {
    Java,
    Bedrock,
}

impl fmt::Debug for JavaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JavaStatus")
            .field("version", &self.version)
            .field("players", &self.players)
            .field("description", &self.description)
            .field("map", &self.map)
            .field("gamemode", &self.gamemode)
            .field("software", &self.software)
            .field("plugins", &self.plugins.as_ref().map(|p| p.len()))
            .field("mods", &self.mods.as_ref().map(|m| m.len()))
            .field("favicon", &self.favicon.as_ref().map(|_| "[Favicon data]"))
            .field("raw_data", &"[Value]")
            .finish()
    }
}

impl JavaStatus {
    pub fn save_favicon(&self, filename: &str) -> Result<(), McError> {
        if let Some(favicon) = &self.favicon {
            let data = favicon.split(',').nth(1).unwrap_or(favicon);
            let bytes = general_purpose::STANDARD
                .decode(data)
                .map_err(McError::Base64Error)?;

            let mut file = File::create(filename).map_err(McError::IoError)?;
            file.write_all(&bytes).map_err(McError::IoError)?;

            Ok(())
        } else {
            Err(McError::InvalidResponse("No favicon available".to_string()))
        }
    }
}

impl fmt::Debug for BedrockStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BedrockStatus")
            .field("edition", &self.edition)
            .field("motd", &self.motd)
            .field("protocol_version", &self.protocol_version)
            .field("version", &self.version)
            .field("online_players", &self.online_players)
            .field("max_players", &self.max_players)
            .field("server_uid", &self.server_uid)
            .field("motd2", &self.motd2)
            .field("game_mode", &self.game_mode)
            .field("game_mode_numeric", &self.game_mode_numeric)
            .field("port_ipv4", &self.port_ipv4)
            .field("port_ipv6", &self.port_ipv6)
            .field("map", &self.map)
            .field("software", &self.software)
            .field("raw_data", &self.raw_data.len())
            .finish()
    }
}

impl std::str::FromStr for ServerEdition {
    type Err = McError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "java" => Ok(ServerEdition::Java),
            "bedrock" => Ok(ServerEdition::Bedrock),
            _ => Err(McError::InvalidEdition(s.to_string())),
        }
    }
}
