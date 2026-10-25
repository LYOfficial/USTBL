# Rust Minecraft Server Status Library

[![Crates.io](https://img.shields.io/crates/v/mc-server-status)](https://crates.io/crates/mc-server-status)
[![Documentation](https://docs.rs/mc-server-status/badge.svg)](https://docs.rs/mc-server-status)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Fork of [NameOfShadow/rust-mc-status](https://github.com/NameOfShadow/rust-mc-status)**

A high-performance, asynchronous Rust library for querying the status of both Minecraft Java Edition and Bedrock Edition servers.

## Features

*   **Dual Protocol Support**: Ping both Minecraft Java Edition (`25565`) and Bedrock Edition (`19132`) servers.
*   **DNS SRV Record Support (New Feature)**: Automatically resolves DNS SRV records (`_minecraft._tcp`) for Java Edition servers when no port is specified, matching native Minecraft client behavior.
*   **Async/Await**: Built on Tokio for non-blocking operations and high concurrency.
*   **Batch Queries**: Ping multiple servers in parallel with configurable concurrency limits.
*   **DNS Caching (New Feature)**: Automatically caches DNS lookups and SRV records to reduce latency for repeated queries.
*   **Structured Data**: Returns richly structured, serializable data (using `serde`), including version info, player counts, MOTD, map, gamemode, plugins, mods and more.
*   **Favicon Handling**: Easily retrieve and save the server's favicon (Java Edition only).
*   **Robust Error Handling**: Comprehensive error types using `thiserror`.
*   **Extended Information**: Detailed data about plugins, mods, DNS and more.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
mc-server-status = "1.0.0"
tokio = { version = "*", features = ["full"] }
```

## Usage

### Basic Example

```rust
use mc_server_status::{McClient, ServerEdition, ServerInfo};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = McClient::new()
        .with_timeout(Duration::from_secs(5))
        .with_max_parallel(10);

    // Check a single server
    let status = client.ping("mc.hypixel.net", ServerEdition::Java).await?;
    println!("Status: {:?}", status);

    // Batch check servers
    let servers = vec![
        ServerInfo {
            address: "mc.hypixel.net".to_string(),
            edition: ServerEdition::Java,
        },
        ServerInfo {
            address: "geo.hivebedrock.network:19132".to_string(),
            edition: ServerEdition::Bedrock,
        },
    ];

    let results = client.ping_many(&servers).await;

    for (server, result) in results {
        println!("Server: {} - {:?}", server.address, result);
    }

    Ok(())
}
```

### Advanced Example

See [examples/advanced_usage.rs](examples/advanced_usage.rs) for a demonstration of all the new library features.

## Key Structs and Methods

*   `McClient`: The main client for making requests.
    *   `new()`, `with_timeout()`, `with_max_parallel()`
    *   `ping(address, edition)`: Ping a single server.
    *   `ping_many(servers)`: Ping multiple servers in parallel.
*   `ServerStatus`: The result of a successful ping.
    *   `online`: `bool`
    *   `ip`: `String` - Server IP address
    *   `port`: `u16` - Server port
    *   `hostname`: `String` - Hostname
    *   `latency`: `f64` - Latency in ms
    *   `dns`: `Option<DnsInfo>` - DNS information
    *   `data`: `ServerData` (enum containing either `JavaStatus` or `BedrockStatus`)
*   `JavaStatus`: Contains detailed information from a Java server.
    *   `version`: Version information
    *   `players`: Player information
    *   `description`: Server description (MOTD)
    *   `map`: Map name
    *   `gamemode`: Game mode
    *   `software`: Server software
    *   `plugins`: List of plugins
    *   `mods`: List of mods
    *   `save_favicon(filename)`: Saves the server icon to a PNG file.
*   `BedrockStatus`: Contains information from a Bedrock server.
    *   `edition`: Minecraft edition
    *   `motd`: Message of the day
    *   `version`: Server version
    *   `online_players`: Online players count
    *   `max_players`: Maximum players
    *   `map`: Map name
    *   `software`: Server software
    *   `game_mode`: Game mode

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
