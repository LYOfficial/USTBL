use std::io;

#[cfg(windows)]
use winapi::um::wininet::{
  InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
};
#[cfg(windows)]
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
#[cfg(windows)]
use winreg::RegKey;

#[derive(Clone, Debug, Default)]
pub struct SystemProxySnapshot {
  #[cfg(windows)]
  proxy_enable: Option<u32>,
  #[cfg(windows)]
  proxy_server: Option<String>,
  #[cfg(windows)]
  proxy_override: Option<String>,
}

#[cfg(windows)]
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

pub fn enable_local_proxy(host: &str, port: u16) -> io::Result<SystemProxySnapshot> {
  #[cfg(windows)]
  {
    let key = RegKey::predef(HKEY_CURRENT_USER)
      .open_subkey_with_flags(INTERNET_SETTINGS, KEY_READ | KEY_WRITE)?;
    let snapshot = SystemProxySnapshot {
      proxy_enable: key.get_value("ProxyEnable").ok(),
      proxy_server: key.get_value("ProxyServer").ok(),
      proxy_override: key.get_value("ProxyOverride").ok(),
    };
    key.set_value("ProxyEnable", &1u32)?;
    key.set_value("ProxyServer", &format!("{host}:{port}"))?;
    key.set_value("ProxyOverride", &"<local>")?;
    notify_windows_proxy_changed();
    Ok(snapshot)
  }
  #[cfg(not(windows))]
  {
    let _ = (host, port);
    Err(io::Error::other("Windows system proxy is unavailable"))
  }
}

pub fn restore_proxy(snapshot: SystemProxySnapshot) -> io::Result<()> {
  #[cfg(windows)]
  {
    let key =
      RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(INTERNET_SETTINGS, KEY_WRITE)?;
    match snapshot.proxy_enable {
      Some(value) => key.set_value("ProxyEnable", &value)?,
      None => {
        let _ = key.delete_value("ProxyEnable");
      }
    };
    match snapshot.proxy_server {
      Some(value) => key.set_value("ProxyServer", &value)?,
      None => {
        let _ = key.delete_value("ProxyServer");
      }
    };
    match snapshot.proxy_override {
      Some(value) => key.set_value("ProxyOverride", &value)?,
      None => {
        let _ = key.delete_value("ProxyOverride");
      }
    };
    notify_windows_proxy_changed();
    Ok(())
  }
  #[cfg(not(windows))]
  {
    let _ = snapshot;
    Err(io::Error::other("Windows system proxy is unavailable"))
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
