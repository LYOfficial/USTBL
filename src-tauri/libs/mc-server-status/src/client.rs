// Copyright (c) 2025 pynickle. This is a fork of Original Crate. Original copyright: Copyright (c) 2025 NameOfShadow

use std::io::Cursor;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::{Duration, SystemTime};

use dashmap::DashMap;
use once_cell::sync::Lazy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use trust_dns_resolver::TokioAsyncResolver;
use trust_dns_resolver::config::*;

use crate::error::McError;
use crate::models::*;

static DNS_CACHE: Lazy<DashMap<String, (SocketAddr, SystemTime)>> = Lazy::new(DashMap::new);
static SRV_CACHE: Lazy<DashMap<String, (String, u16, SystemTime)>> = Lazy::new(DashMap::new);
const DNS_CACHE_TTL: u64 = 300; // 5 minutes

#[derive(Clone)]
pub struct McClient {
    timeout: Duration,
    max_parallel: usize,
}

impl Default for McClient {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_parallel: 10,
        }
    }
}

impl McClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_parallel(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel;
        self
    }

    pub async fn ping(
        &self,
        address: &str,
        edition: ServerEdition,
    ) -> Result<ServerStatus, McError> {
        match edition {
            ServerEdition::Java => self.ping_java(address).await,
            ServerEdition::Bedrock => self.ping_bedrock(address).await,
        }
    }

    pub async fn ping_java(&self, address: &str) -> Result<ServerStatus, McError> {
        let start = SystemTime::now();
        let (host, port, explicit_port) = Self::parse_address_with_flag(address, 25565)?;

        // If no explicit port was given, try SRV lookup
        let (final_host, final_port) = if !explicit_port {
            self.resolve_srv(host, port)
                .await
                .unwrap_or((host.to_string(), port))
        } else {
            (host.to_string(), port)
        };

        let resolved = self.resolve_dns(&final_host, final_port).await?;
        let dns_info = self.get_dns_info(&final_host).await.ok(); // DNS info is optional

        let mut stream = timeout(self.timeout, TcpStream::connect(resolved))
            .await
            .map_err(|_| McError::Timeout)?
            .map_err(|e| McError::ConnectionError(e.to_string()))?;

        stream.set_nodelay(true).map_err(McError::IoError)?;

        // Send handshake with the final host and port
        self.send_handshake(&mut stream, &final_host, final_port)
            .await?;

        // Send status request
        self.send_status_request(&mut stream).await?;

        // Read and parse response
        let response = self.read_response(&mut stream).await?;
        let (json, latency) = self.parse_java_response(response, start)?;

        // Build result
        Ok(ServerStatus {
            online: true,
            ip: resolved.ip().to_string(),
            port: resolved.port(),
            hostname: host.to_string(),
            latency,
            dns: dns_info,
            data: ServerData::Java(self.parse_java_json(&json)?),
        })
    }

    pub async fn ping_bedrock(&self, address: &str) -> Result<ServerStatus, McError> {
        let start = SystemTime::now();
        let (host, port) = Self::parse_address(address, 19132)?;
        let resolved = self.resolve_dns(host, port).await?;
        let dns_info = self.get_dns_info(host).await.ok(); // DNS info is optional

        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(McError::IoError)?;

        // Send ping packet
        let ping_packet = self.create_bedrock_ping_packet();
        timeout(self.timeout, socket.send_to(&ping_packet, resolved))
            .await
            .map_err(|_| McError::Timeout)?
            .map_err(|e| McError::IoError(e))?;

        // Receive response
        let mut buf = [0u8; 1024];
        let (len, _) = timeout(self.timeout, socket.recv_from(&mut buf))
            .await
            .map_err(|_| McError::Timeout)?
            .map_err(|e| McError::IoError(e))?;

        if len < 35 {
            return Err(McError::InvalidResponse("Response too short".to_string()));
        }

        let latency = start
            .elapsed()
            .map_err(|_| McError::InvalidResponse("Time error".to_string()))?
            .as_secs_f64()
            * 1000.0;

        let pong_data = String::from_utf8_lossy(&buf[35..len]).to_string();

        Ok(ServerStatus {
            online: true,
            ip: resolved.ip().to_string(),
            port: resolved.port(),
            hostname: host.to_string(),
            latency,
            dns: dns_info,
            data: ServerData::Bedrock(self.parse_bedrock_response(&pong_data)?),
        })
    }

    pub async fn ping_many(
        &self,
        servers: &[ServerInfo],
    ) -> Vec<(ServerInfo, Result<ServerStatus, McError>)> {
        use futures::stream::StreamExt;
        use tokio::sync::Semaphore;

        let semaphore = std::sync::Arc::new(Semaphore::new(self.max_parallel));
        let client = self.clone();

        let futures = servers.iter().map(|server| {
            let server = server.clone();
            let semaphore = semaphore.clone();
            let client = client.clone();

            async move {
                let _permit = semaphore.acquire().await;
                let result = client.ping(&server.address, server.edition).await;
                (server, result)
            }
        });

        futures::stream::iter(futures)
            .buffer_unordered(self.max_parallel)
            .collect()
            .await
    }

    // Helper methods
    fn parse_address(address: &str, default_port: u16) -> Result<(&str, u16), McError> {
        if let Some((host, port_str)) = address.split_once(':') {
            let port = port_str
                .parse::<u16>()
                .map_err(|e| McError::InvalidPort(e.to_string()))?;
            Ok((host, port))
        } else {
            Ok((address, default_port))
        }
    }

    fn parse_address_with_flag(
        address: &str,
        default_port: u16,
    ) -> Result<(&str, u16, bool), McError> {
        if let Some((host, port_str)) = address.split_once(':') {
            let port = port_str
                .parse::<u16>()
                .map_err(|e| McError::InvalidPort(e.to_string()))?;
            Ok((host, port, true)) // explicit port
        } else {
            Ok((address, default_port, false)) // no explicit port
        }
    }

    async fn resolve_srv(&self, host: &str, default_port: u16) -> Result<(String, u16), McError> {
        let cache_key = format!("_minecraft._tcp.{}", host);

        // Check cache with TTL validation
        if let Some(entry) = SRV_CACHE.get(&cache_key) {
            let (srv_host, srv_port, timestamp) = entry.value().clone();
            if timestamp
                .elapsed()
                .map(|d| d.as_secs() < DNS_CACHE_TTL)
                .unwrap_or(false)
            {
                return Ok((srv_host, srv_port));
            }
        }

        // Try SRV lookup
        match self.lookup_srv(host).await {
            Ok((srv_host, srv_port)) => {
                SRV_CACHE.insert(cache_key, (srv_host.clone(), srv_port, SystemTime::now()));
                Ok((srv_host, srv_port))
            }
            Err(_) => {
                // No SRV record found, use default
                Ok((host.to_string(), default_port))
            }
        }
    }

    async fn lookup_srv(&self, host: &str) -> Result<(String, u16), McError> {
        let resolver =
            TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default());

        let srv_name = format!("_minecraft._tcp.{}", host);

        let response = timeout(self.timeout, resolver.srv_lookup(&srv_name))
            .await
            .map_err(|_| McError::Timeout)?
            .map_err(|e| McError::DnsError(format!("SRV lookup failed: {}", e)))?;

        // Get the first SRV record with the highest priority (lowest number)
        let srv = response
            .iter()
            .min_by_key(|r| r.priority())
            .ok_or_else(|| McError::DnsError("No SRV records found".to_string()))?;

        let target = srv.target().to_utf8();
        let port = srv.port();

        // Remove trailing dot from target if present
        let target = target.trim_end_matches('.');

        Ok((target.to_string(), port))
    }

    async fn resolve_dns(&self, host: &str, port: u16) -> Result<SocketAddr, McError> {
        let cache_key = format!("{}:{}", host, port);

        // Check cache with TTL validation
        if let Some(entry) = DNS_CACHE.get(&cache_key) {
            let (addr, timestamp) = *entry.value();
            if timestamp
                .elapsed()
                .map(|d| d.as_secs() < DNS_CACHE_TTL)
                .unwrap_or(false)
            {
                return Ok(addr);
            }
        }

        // Resolve and cache
        let addrs: Vec<SocketAddr> = format!("{}:{}", host, port)
            .to_socket_addrs()
            .map_err(|e| McError::DnsError(e.to_string()))?
            .collect();

        let addr = addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied()
            .ok_or_else(|| McError::DnsError("No addresses resolved".to_string()))?;

        DNS_CACHE.insert(cache_key, (addr, SystemTime::now()));
        Ok(addr)
    }

    async fn get_dns_info(&self, host: &str) -> Result<DnsInfo, McError> {
        // Simple implementation - in production you might want to use a proper DNS library
        let addrs: Vec<SocketAddr> = format!("{}:0", host)
            .to_socket_addrs()
            .map_err(|e| McError::DnsError(e.to_string()))?
            .collect();

        Ok(DnsInfo {
            a_records: addrs.iter().map(|a| a.ip().to_string()).collect(),
            cname: None, // This would require proper DNS queries
            ttl: 300,
        })
    }

    async fn send_handshake(
        &self,
        stream: &mut TcpStream,
        host: &str,
        port: u16,
    ) -> Result<(), McError> {
        let mut handshake = Vec::with_capacity(64);
        write_var_int(&mut handshake, 0x00);
        write_var_int(&mut handshake, 47);
        write_string(&mut handshake, host);
        handshake.extend_from_slice(&port.to_be_bytes());
        write_var_int(&mut handshake, 1);

        let mut packet = Vec::with_capacity(handshake.len() + 5);
        write_var_int(&mut packet, handshake.len() as i32);
        packet.extend_from_slice(&handshake);

        timeout(self.timeout, stream.write_all(&packet))
            .await
            .map_err(|_| McError::Timeout)?
            .map_err(McError::IoError)
    }

    async fn send_status_request(&self, stream: &mut TcpStream) -> Result<(), McError> {
        let mut status_request = Vec::with_capacity(5);
        write_var_int(&mut status_request, 0x00);

        let mut status_packet = Vec::with_capacity(status_request.len() + 5);
        write_var_int(&mut status_packet, status_request.len() as i32);
        status_packet.extend_from_slice(&status_request);

        timeout(self.timeout, stream.write_all(&status_packet))
            .await
            .map_err(|_| McError::Timeout)?
            .map_err(McError::IoError)
    }

    async fn read_response(&self, stream: &mut TcpStream) -> Result<Vec<u8>, McError> {
        let mut response = Vec::with_capacity(1024);
        let mut buf = [0u8; 4096];
        let mut expected_length = None;

        loop {
            let n = timeout(self.timeout, stream.read(&mut buf))
                .await
                .map_err(|_| McError::Timeout)?
                .map_err(McError::IoError)?;

            if n == 0 {
                break;
            }

            response.extend_from_slice(&buf[..n]);

            // Check if we have enough data to determine packet length
            if expected_length.is_none() && response.len() >= 5 {
                let mut cursor = Cursor::new(&response);
                if let Ok(packet_length) = read_var_int(&mut cursor) {
                    expected_length = Some(cursor.position() as usize + packet_length as usize);
                }
            }

            if let Some(expected) = expected_length {
                if response.len() >= expected {
                    break;
                }
            }
        }

        if response.is_empty() {
            return Err(McError::InvalidResponse(
                "No response from server".to_string(),
            ));
        }

        Ok(response)
    }

    fn parse_java_response(
        &self,
        response: Vec<u8>,
        start: SystemTime,
    ) -> Result<(serde_json::Value, f64), McError> {
        let mut cursor = Cursor::new(&response);
        let packet_length = read_var_int(&mut cursor).map_err(|e| {
            McError::InvalidResponse(format!("Failed to read packet length: {}", e))
        })?;

        let total_expected = cursor.position() as usize + packet_length as usize;
        if response.len() < total_expected {
            return Err(McError::InvalidResponse(format!(
                "Incomplete packet: expected {}, got {}",
                total_expected,
                response.len()
            )));
        }

        let packet_id = read_var_int(&mut cursor)
            .map_err(|e| McError::InvalidResponse(format!("Failed to read packet ID: {}", e)))?;

        if packet_id != 0x00 {
            return Err(McError::InvalidResponse(format!(
                "Unexpected packet ID: {}",
                packet_id
            )));
        }

        let json_length = read_var_int(&mut cursor)
            .map_err(|e| McError::InvalidResponse(format!("Failed to read JSON length: {}", e)))?;

        if cursor.position() as usize + json_length as usize > response.len() {
            return Err(McError::InvalidResponse("JSON data truncated".to_string()));
        }

        let json_buf = &response
            [cursor.position() as usize..cursor.position() as usize + json_length as usize];
        let json_str = String::from_utf8(json_buf.to_vec()).map_err(McError::Utf8Error)?;

        let json: serde_json::Value =
            serde_json::from_str(&json_str).map_err(McError::JsonError)?;

        let latency = start
            .elapsed()
            .map_err(|_| McError::InvalidResponse("Time error".to_string()))?
            .as_secs_f64()
            * 1000.0;

        Ok((json, latency))
    }

    fn parse_java_json(&self, json: &serde_json::Value) -> Result<JavaStatus, McError> {
        let version = JavaVersion {
            name: json["version"]["name"]
                .as_str()
                .unwrap_or("Unknown")
                .to_string(),
            protocol: json["version"]["protocol"].as_i64().unwrap_or(0),
        };

        let players = JavaPlayers {
            online: json["players"]["online"].as_i64().unwrap_or(0),
            max: json["players"]["max"].as_i64().unwrap_or(0),
            sample: if let Some(sample) = json["players"]["sample"].as_array() {
                Some(
                    sample
                        .iter()
                        .filter_map(|p| {
                            Some(JavaPlayer {
                                name: p["name"].as_str()?.to_string(),
                                id: p["id"].as_str()?.to_string(),
                            })
                        })
                        .collect(),
                )
            } else {
                None
            },
        };

        let description = if let Some(desc) = json["description"].as_str() {
            desc.to_string()
        } else if let Some(text) = json["description"]["text"].as_str() {
            text.to_string()
        } else {
            "No description".to_string()
        };

        let favicon = json["favicon"].as_str().map(|s| s.to_string());
        let map = json["map"].as_str().map(|s| s.to_string());
        let gamemode = json["gamemode"].as_str().map(|s| s.to_string());
        let software = json["software"].as_str().map(|s| s.to_string());

        let plugins = if let Some(plugins_array) = json["plugins"].as_array() {
            Some(
                plugins_array
                    .iter()
                    .filter_map(|p| {
                        Some(JavaPlugin {
                            name: p["name"].as_str()?.to_string(),
                            version: p["version"].as_str().map(|s| s.to_string()),
                        })
                    })
                    .collect(),
            )
        } else {
            None
        };

        let mods = if let Some(mods_array) = json["mods"].as_array() {
            Some(
                mods_array
                    .iter()
                    .filter_map(|m| {
                        Some(JavaMod {
                            modid: m["modid"].as_str()?.to_string(),
                            version: m["version"].as_str().map(|s| s.to_string()),
                        })
                    })
                    .collect(),
            )
        } else {
            None
        };

        Ok(JavaStatus {
            version,
            players,
            description,
            favicon,
            map,
            gamemode,
            software,
            plugins,
            mods,
            raw_data: json.clone(),
        })
    }

    fn create_bedrock_ping_packet(&self) -> Vec<u8> {
        let mut ping_packet = Vec::with_capacity(35);
        ping_packet.push(0x01);
        ping_packet.extend_from_slice(
            &(SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64)
                .to_be_bytes(),
        );
        ping_packet.extend_from_slice(&[
            0x00, 0xFF, 0xFF, 0x00, 0xFE, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFD, 0xFD, 0x12, 0x34,
            0x56, 0x78,
        ]);
        ping_packet.extend_from_slice(&[0x00; 8]);
        ping_packet
    }

    fn parse_bedrock_response(&self, pong_data: &str) -> Result<BedrockStatus, McError> {
        let parts: Vec<&str> = pong_data.split(';').collect();

        if parts.len() < 6 {
            return Err(McError::InvalidResponse(
                "Invalid Bedrock response".to_string(),
            ));
        }

        Ok(BedrockStatus {
            edition: parts[0].to_string(),
            motd: parts[1].to_string(),
            protocol_version: parts[2].to_string(),
            version: parts[3].to_string(),
            online_players: parts[4].to_string(),
            max_players: parts[5].to_string(),
            server_uid: parts.get(6).map_or("", |s| *s).to_string(),
            motd2: parts.get(7).map_or("", |s| *s).to_string(),
            game_mode: parts.get(8).map_or("", |s| *s).to_string(),
            game_mode_numeric: parts.get(9).map_or("", |s| *s).to_string(),
            port_ipv4: parts.get(10).map_or("", |s| *s).to_string(),
            port_ipv6: parts.get(11).map_or("", |s| *s).to_string(),
            map: parts.get(12).map(|s| s.to_string()),
            software: parts.get(13).map(|s| s.to_string()),
            raw_data: pong_data.to_string(),
        })
    }
}

// Helper functions
fn write_var_int(buffer: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let mut temp = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            temp |= 0x80;
        }
        buffer.push(temp);
        if value == 0 {
            break;
        }
    }
}

fn write_string(buffer: &mut Vec<u8>, s: &str) {
    write_var_int(buffer, s.len() as i32);
    buffer.extend_from_slice(s.as_bytes());
}

fn read_var_int(reader: &mut impl std::io::Read) -> Result<i32, String> {
    let mut result = 0i32;
    let mut shift = 0;
    loop {
        let mut byte = [0u8];
        reader.read_exact(&mut byte).map_err(|e| e.to_string())?;
        let value = byte[0] as i32;
        result |= (value & 0x7F) << shift;
        shift += 7;
        if shift > 35 {
            return Err("VarInt too big".to_string());
        }
        if (value & 0x80) == 0 {
            break;
        }
    }
    Ok(result)
}
