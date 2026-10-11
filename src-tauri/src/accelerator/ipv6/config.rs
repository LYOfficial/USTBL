use super::warp::WarpDevice;
use crate::error::{USTBLError, USTBLResult};
use serde_json::{json, Value};
use std::net::IpAddr;

pub const GROUP: &str = "USTBL-WARP";
pub const AUTO: &str = "Auto";
pub const SERVERS: [&str; 4] = [
  "2606:4700:103::1",
  "2606:4700:103::2",
  "2606:4700:104::1",
  "2606:4700:104::2",
];
pub const PORTS: [u16; 7] = [443, 500, 1701, 4500, 4443, 8443, 8095];

pub fn node_names() -> Vec<String> {
  SERVERS
    .iter()
    .flat_map(|server| PORTS.map(|port| format!("{server}:{port}")))
    .collect()
}

pub fn normalize_bypass(values: &[String]) -> USTBLResult<Vec<String>> {
  if values.len() > 128 {
    return Err(USTBLError("绕过规则最多 128 条".into()));
  }
  let mut result = Vec::new();
  for value in values {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
      continue;
    }
    let valid = if let Some((address, prefix)) = value.split_once('/') {
      match (address.parse::<IpAddr>(), prefix.parse::<u8>()) {
        (Ok(IpAddr::V4(_)), Ok(prefix)) => prefix <= 32,
        (Ok(IpAddr::V6(_)), Ok(prefix)) => prefix <= 128,
        _ => false,
      }
    } else {
      value.len() <= 253
        && value.contains('.')
        && value.split('.').all(|label| {
          !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
              .bytes()
              .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    };
    if !valid {
      return Err(USTBLError(format!("无效的绕过域名或 CIDR: {value}")));
    }
    if !result.contains(&value) {
      result.push(value);
    }
  }
  Ok(result)
}

pub fn render(
  device: &WarpDevice,
  bypass: &[String],
  controller: u16,
  mixed: u16,
  secret: &str,
  tun: bool,
) -> USTBLResult<Value> {
  device.validate()?;
  let bypass = normalize_bypass(bypass)?;
  let proxies: Vec<Value> = SERVERS
    .iter()
    .flat_map(|server| {
      PORTS.map(|port| {
        json!({
          "name": format!("{server}:{port}"), "type": "masque", "server": server, "port": port,
          "private-key": device.private_key, "public-key": device.peer_public_key,
          "ip": device.ipv4, "ipv6": device.ipv6, "mtu": 1280, "udp": true,
          "remote-dns-resolve": true, "dns": ["1.1.1.1", "2606:4700:4700::1111"]
        })
      })
    })
    .collect();
  let mut rules: Vec<String> = bypass
    .iter()
    .map(|value| {
      if value.contains('/') {
        format!(
          "{},{value},DIRECT,no-resolve",
          if value.contains(':') {
            "IP-CIDR6"
          } else {
            "IP-CIDR"
          }
        )
      } else {
        format!("DOMAIN-SUFFIX,{value},DIRECT")
      }
    })
    .collect();
  for cidr in [
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
  ] {
    rules.push(format!("IP-CIDR,{cidr},DIRECT,no-resolve"));
  }
  for cidr in ["::1/128", "fc00::/7", "fe80::/10"] {
    rules.push(format!("IP-CIDR6,{cidr},DIRECT,no-resolve"));
  }
  rules.push(format!("MATCH,{GROUP}"));
  let mut choices = vec![AUTO.to_string()];
  choices.extend(node_names());
  Ok(json!({
    "mixed-port": mixed, "bind-address": "127.0.0.1", "allow-lan": false,
    "external-controller": format!("127.0.0.1:{controller}"), "secret": secret,
    "log-level": "warning", "mode": "rule", "ipv6": true,
    "tun": {"enable": tun, "device": "USTBL-FreeV6", "stack": "mixed", "auto-route": true,
      "auto-detect-interface": true, "strict-route": true, "dns-hijack": ["any:53", "tcp://any:53"]},
    "dns": {"enable": true, "ipv6": true, "enhanced-mode": "fake-ip", "fake-ip-range": "198.18.0.1/16",
      "fake-ip-filter": ["+.lan", "+.local", "*.msftconnecttest.com", "*.msftncsi.com"],
      "default-nameserver": ["2606:4700:4700::1111", "2400:3200::1"],
      "nameserver": ["https://[2606:4700:4700::1111]/dns-query", "https://[2400:3200::1]/dns-query"]},
    "proxies": proxies,
    "proxy-groups": [
      {"name": GROUP, "type": "select", "proxies": choices},
      {"name": AUTO, "type": "url-test", "proxies": node_names(), "url": "https://www.gstatic.com/generate_204",
        "interval": 300, "tolerance": 50, "lazy": false}
    ],
    "rules": rules
  }))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bypass_rejects_injection_and_invalid_networks() {
    for value in [
      "bad\nMATCH,DIRECT",
      "https://ustb.edu.cn",
      "a..cn",
      "1.2.3.4/33",
      "::/129",
      "-bad.cn",
    ] {
      assert!(normalize_bypass(&[value.into()]).is_err(), "{value}");
    }
    assert_eq!(
      normalize_bypass(&[
        " USTB.EDU.CN ".into(),
        "ustb.edu.cn".into(),
        "202.204.0.0/16".into()
      ])
      .unwrap(),
      ["ustb.edu.cn", "202.204.0.0/16"]
    );
  }

  #[test]
  fn all_transport_endpoints_are_ipv6_and_bypass_precedes_warp() {
    let device = WarpDevice::test_device();
    let config = render(
      &device,
      &["ustb.edu.cn".into()],
      19090,
      17890,
      "secret",
      true,
    )
    .unwrap();
    let proxies = config["proxies"].as_array().unwrap();
    assert_eq!(proxies.len(), 28);
    for proxy in proxies {
      assert!(proxy["server"]
        .as_str()
        .unwrap()
        .parse::<std::net::Ipv6Addr>()
        .is_ok());
    }
    assert_eq!(config["rules"][0], "DOMAIN-SUFFIX,ustb.edu.cn,DIRECT");
    assert_eq!(
      config["rules"].as_array().unwrap().last().unwrap(),
      &json!("MATCH,USTBL-WARP")
    );
    assert_eq!(config["secret"], "secret");
    assert_eq!(config["allow-lan"], false);
    assert!(!config.to_string().contains("rule-providers"));
  }
}
