use serde::{Deserialize, Serialize};
use std::{io, path::Path};

#[cfg(windows)]
use winapi::um::wininet::{
  InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
#[cfg(windows)]
use winreg::{
  enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE},
  RegKey,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SystemProxySnapshot {
  proxy_enable: Option<u32>,
  proxy_server: Option<String>,
  proxy_override: Option<String>,
  owned_server: String,
}

#[cfg(windows)]
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

pub fn enable_local_proxy(
  host: &str,
  port: u16,
  recovery: &Path,
) -> io::Result<SystemProxySnapshot> {
  #[cfg(windows)]
  {
    recover_proxy(recovery)?;
    let key = RegKey::predef(HKEY_CURRENT_USER)
      .open_subkey_with_flags(INTERNET_SETTINGS, KEY_READ | KEY_WRITE)?;
    let snapshot = SystemProxySnapshot {
      proxy_enable: key.get_value("ProxyEnable").ok(),
      proxy_server: key.get_value("ProxyServer").ok(),
      proxy_override: key.get_value("ProxyOverride").ok(),
      owned_server: format!("{host}:{port}"),
    };
    if let Some(parent) = recovery.parent() {
      std::fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let temporary = recovery.with_extension("pending");
    let mut record = std::fs::File::create(&temporary)?;
    record.write_all(&serde_json::to_vec(&snapshot)?)?;
    record.sync_all()?;
    drop(record);
    std::fs::rename(temporary, recovery)?;
    let changed = (|| -> io::Result<()> {
      key.set_value("ProxyServer", &snapshot.owned_server)?;
      key.set_value("ProxyOverride", &"<local>")?;
      key.set_value("ProxyEnable", &1u32)
    })();
    if let Err(error) = changed {
      if write_snapshot(&key, &snapshot).is_ok() {
        let _ = std::fs::remove_file(recovery);
      }
      notify_windows_proxy_changed();
      return Err(error);
    }
    notify_windows_proxy_changed();
    Ok(snapshot)
  }
  #[cfg(not(windows))]
  {
    let _ = (host, port, recovery);
    Err(io::Error::other("Windows system proxy is unavailable"))
  }
}

fn owns_settings(
  snapshot: &SystemProxySnapshot,
  enabled: Option<u32>,
  server: Option<&str>,
) -> bool {
  enabled == Some(1) && server == Some(snapshot.owned_server.as_str())
}

pub fn restore_proxy(snapshot: &SystemProxySnapshot) -> io::Result<()> {
  #[cfg(windows)]
  {
    let key = RegKey::predef(HKEY_CURRENT_USER)
      .open_subkey_with_flags(INTERNET_SETTINGS, KEY_READ | KEY_WRITE)?;
    let server: Option<String> = key.get_value("ProxyServer").ok();
    if owns_settings(
      snapshot,
      key.get_value("ProxyEnable").ok(),
      server.as_deref(),
    ) {
      write_snapshot(&key, snapshot)?;
      notify_windows_proxy_changed();
    }
    Ok(())
  }
  #[cfg(not(windows))]
  {
    let _ = snapshot;
    Ok(())
  }
}

pub fn recover_proxy(path: &Path) -> io::Result<()> {
  let data = match std::fs::read(path) {
    Ok(data) => data,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error),
  };
  let snapshot: SystemProxySnapshot = serde_json::from_slice(&data)?;
  if snapshot.owned_server
    != format!(
      "{}:{}",
      super::browser_proxy::LOCAL_PROXY_HOST,
      super::browser_proxy::LOCAL_PROXY_PORT
    )
  {
    return Err(io::Error::other("Invalid USTBL proxy recovery record"));
  }
  restore_proxy(&snapshot)?;
  std::fs::remove_file(path)
}

#[cfg(windows)]
fn write_snapshot(key: &RegKey, snapshot: &SystemProxySnapshot) -> io::Result<()> {
  for (name, value) in [
    ("ProxyServer", &snapshot.proxy_server),
    ("ProxyOverride", &snapshot.proxy_override),
  ] {
    match value {
      Some(value) => key.set_value(name, value)?,
      None => match key.delete_value(name) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
      },
    }
  }
  match snapshot.proxy_enable {
    Some(value) => key.set_value("ProxyEnable", &value),
    None => match key.delete_value("ProxyEnable") {
      Ok(()) => Ok(()),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
      Err(error) => Err(error),
    },
  }
}

#[cfg(windows)]
fn notify_windows_proxy_changed() {
  unsafe {
    InternetSetOptionW(
      std::ptr::null_mut(),
      INTERNET_OPTION_SETTINGS_CHANGED,
      std::ptr::null_mut(),
      0,
    );
    InternetSetOptionW(
      std::ptr::null_mut(),
      INTERNET_OPTION_REFRESH,
      std::ptr::null_mut(),
      0,
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recovery_never_overwrites_proxy_changes_from_another_program() {
    let snapshot = SystemProxySnapshot {
      owned_server: "127.0.0.1:17880".into(),
      ..Default::default()
    };
    assert!(owns_settings(&snapshot, Some(1), Some("127.0.0.1:17880")));
    assert!(!owns_settings(&snapshot, Some(1), Some("127.0.0.1:7890")));
    assert!(!owns_settings(&snapshot, Some(0), Some("127.0.0.1:17880")));
  }

  #[cfg(windows)]
  #[test]
  fn registry_snapshot_restores_absent_and_existing_values() {
    let name = format!(r"Software\USTBL-Test-{}", uuid::Uuid::new_v4());
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(&name).unwrap();
    key.set_value("ProxyEnable", &1u32).unwrap();
    key.set_value("ProxyServer", &"owned").unwrap();
    key.set_value("ProxyOverride", &"<local>").unwrap();
    let snapshot = SystemProxySnapshot {
      proxy_enable: Some(0),
      proxy_server: Some("original:7890".into()),
      ..Default::default()
    };
    write_snapshot(&key, &snapshot).unwrap();
    assert_eq!(key.get_value::<u32, _>("ProxyEnable").unwrap(), 0);
    assert_eq!(
      key.get_value::<String, _>("ProxyServer").unwrap(),
      "original:7890"
    );
    assert!(key.get_value::<String, _>("ProxyOverride").is_err());
    drop(key);
    hkcu.delete_subkey_all(name).unwrap();
  }
}
