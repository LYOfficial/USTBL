//! The download addresses of a local Modrinth modpack that Modrinth does not
//! approve, listed before the pack is imported so the player can confirm them.

use crate::error::{USTBLError, USTBLResult};
use crate::instance::helpers::modpack::misc::ModpackManifest;
use crate::instance::helpers::modpack::modrinth::{ModrinthFile, ModrinthManifest};
use serde::{Deserialize, Serialize};

const ALLOWED_HOSTS: [&str; 4] = [
  "cdn.modrinth.com",
  "github.com",
  "raw.githubusercontent.com",
  "gitlab.com",
];

/// Files whose download addresses are outside the allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnofficialDownloads {
  /// Number of files with at least one address outside the allowlist.
  pub file_count: usize,
  /// The addresses outside the allowlist, without repeats, in manifest order.
  pub addresses: Vec<String>,
}

/// Whether `address` is an `https` URL whose host is exactly one of the
/// allowed hosts. Subdomains, other schemes and unparsable text do not count.
pub fn is_allowed_address(address: &str) -> bool {
  let Ok(url) = url::Url::parse(address) else {
    return false;
  };
  url.scheme() == "https"
    && url.host_str().is_some_and(|host| {
      ALLOWED_HOSTS
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
    })
}

/// The files of a Modrinth manifest the launcher would download on a client,
/// with any address (fallbacks included) outside the allowlist.
pub fn unofficial_downloads(files: &[ModrinthFile]) -> UnofficialDownloads {
  let mut result = UnofficialDownloads::default();
  for file in files {
    // The launcher never installs files the format marks unsupported on the
    // client, so their addresses are never fetched.
    if file
      .env
      .as_ref()
      .is_some_and(|env| env.client == "unsupported")
    {
      continue;
    }
    let mut flagged = false;
    for address in &file.downloads {
      if is_allowed_address(address) {
        continue;
      }
      flagged = true;
      if !result.addresses.contains(address) {
        result.addresses.push(address.clone());
      }
    }
    if flagged {
      result.file_count += 1;
    }
  }
  result
}

/// List the unofficial download addresses of an archive the user picked from
/// disk. Only a Modrinth pack has any: CurseForge addresses are built by the
/// launcher and a MultiMC pack has no download list. An archive that cannot be
/// read yields nothing, because the import itself refuses it.
#[tauri::command]
pub async fn list_local_modpack_unofficial_downloads(
  path: String,
) -> USTBLResult<UnofficialDownloads> {
  let manifest = tokio::task::spawn_blocking(move || {
    let file = std::fs::File::open(&path)?;
    ModrinthManifest::from_archive(&file)
  })
  .await
  .map_err(|e| USTBLError(e.to_string()))?;
  Ok(
    manifest
      .map(|manifest| unofficial_downloads(&manifest.files))
      .unwrap_or_default(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  fn file(env_client: Option<&str>, downloads: &[&str]) -> ModrinthFile {
    let mut value = serde_json::json!({
      "path": "mods/a.jar",
      "hashes": { "sha1": "a", "sha512": "b" },
      "downloads": downloads,
      "fileSize": 1,
    });
    if let Some(client) = env_client {
      value["env"] = serde_json::json!({ "client": client, "server": "required" });
    }
    serde_json::from_value(value).unwrap()
  }

  #[test]
  fn the_four_hosts_over_https_are_allowed() {
    for address in [
      "https://cdn.modrinth.com/data/AAAA/versions/v1/a.jar",
      "https://github.com/o/r/releases/download/v1/a.jar",
      "https://raw.githubusercontent.com/o/r/main/a.jar",
      "https://gitlab.com/o/r/-/raw/main/a.jar",
    ] {
      assert!(is_allowed_address(address), "{address}");
    }
  }

  #[test]
  fn the_host_is_compared_without_case_and_the_port_is_ignored() {
    assert!(is_allowed_address("https://CDN.Modrinth.COM/a.jar"));
    assert!(is_allowed_address("https://github.com:443/a.jar"));
    assert!(is_allowed_address("https://github.com:8443/a.jar"));
  }

  #[test]
  fn subdomains_lookalikes_and_other_hosts_are_not_allowed() {
    for address in [
      "https://api.github.com/a.jar",
      "https://evil.cdn.modrinth.com/a.jar",
      "https://cdn.modrinth.com.evil.test/a.jar",
      "https://notgithub.com/a.jar",
      "https://github.com.evil.test/a.jar",
      "https://user@evil.test/github.com",
      "https://evil.test/cdn.modrinth.com/a.jar",
      "https://127.0.0.1/a.jar",
      "https://[::1]/a.jar",
      "https://github.com./a.jar",
    ] {
      assert!(!is_allowed_address(address), "{address}");
    }
  }

  #[test]
  fn other_schemes_and_unparsable_text_are_not_allowed() {
    for address in [
      "http://cdn.modrinth.com/a.jar",
      "ftp://github.com/a.jar",
      "file:///c:/a.jar",
      "cdn.modrinth.com/a.jar",
      "not a url",
      "",
    ] {
      assert!(!is_allowed_address(address), "{address}");
    }
  }

  #[test]
  fn a_pack_of_approved_addresses_needs_no_confirmation() {
    let files = [
      file(None, &["https://cdn.modrinth.com/a.jar"]),
      file(
        Some("required"),
        &["https://github.com/a.jar", "https://gitlab.com/a.jar"],
      ),
    ];
    assert_eq!(unofficial_downloads(&files), UnofficialDownloads::default());
  }

  #[test]
  fn a_fallback_address_outside_the_list_counts_the_file() {
    let files = [file(
      None,
      &[
        "https://cdn.modrinth.com/a.jar",
        "https://mirror.example/a.jar",
      ],
    )];
    let found = unofficial_downloads(&files);
    assert_eq!(found.file_count, 1);
    assert_eq!(found.addresses, ["https://mirror.example/a.jar"]);
  }

  #[test]
  fn each_file_counts_once_and_addresses_are_listed_once() {
    let files = [
      file(
        None,
        &["https://a.example/1.jar", "http://cdn.modrinth.com/1.jar"],
      ),
      file(None, &["https://a.example/1.jar"]),
      file(None, &["https://b.example/2.jar"]),
      file(None, &["https://cdn.modrinth.com/3.jar"]),
    ];
    let found = unofficial_downloads(&files);
    assert_eq!(found.file_count, 3);
    assert_eq!(
      found.addresses,
      [
        "https://a.example/1.jar",
        "http://cdn.modrinth.com/1.jar",
        "https://b.example/2.jar"
      ]
    );
  }

  #[test]
  fn files_unsupported_on_the_client_are_not_checked() {
    let files = [
      file(Some("unsupported"), &["https://evil.example/a.jar"]),
      file(Some("optional"), &["https://evil.example/b.jar"]),
    ];
    let found = unofficial_downloads(&files);
    assert_eq!(found.file_count, 1);
    assert_eq!(found.addresses, ["https://evil.example/b.jar"]);
  }
}
