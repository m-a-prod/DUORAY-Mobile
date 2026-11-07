//! Phone-only settings, in mobile.json next to the shared store.json: the
//! core's Store drops fields it does not know when it saves.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::netpolicy::NetworkPolicy;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MobileSettings {
    pub policy: NetworkPolicy,
    pub apps: AppRules,
    /// Speeds and the exit country in the notification while the app is
    /// closed: xray's counters are read every few seconds (src/status.rs).
    pub background_stats: bool,
    /// Ping threads on mobile data (store.json's count is for Wi-Fi):
    /// operators throttle bursts of new connections.
    pub mobile_ping_threads: u32,
    /// Ping threads on Wi-Fi. Each runs its own xray (~36 MB): the desktop's
    /// 8 at once pushed phones short of memory into stutter.
    pub wifi_ping_threads: u32,
    /// The connection as a Live Update (Android 16+): pinned on top of the
    /// shade with a chip in the status bar. Off: a plain quiet notification.
    pub live_update: bool,
    /// The xray core picked in «Ядро Xray» (a release tag); None: the bundled one.
    pub core: Option<String>,
    /// «Свернуть уведомление»: the minimal channel, folded, no status bar icon.
    pub quiet_notification: bool,
    /// «Держать подключение» (off by default: on a shaky Wi-Fi it moved
    /// people between servers they had not asked for).
    pub keep_connected: bool,
    /// «Держать подключение»: how long the server may stay silent before
    /// the connection moves to another one.
    pub failover_secs: u32,
    /// «Доступ через хотспот»: the phone as a proxy for the Wi-Fi or hotspot.
    pub share: ShareProxy,
}

/// SOCKS5 on `port` and HTTP on `port + 1`, on every interface, so devices
/// on the same Wi-Fi or on the phone's hotspot can go through its tunnel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareProxy {
    pub enabled: bool,
    /// Login and password asked from the devices.
    pub auth: bool,
    pub user: String,
    pub pass: String,
    pub port: u16,
}

impl Default for ShareProxy {
    fn default() -> Self {
        // The password is made when sharing is first turned on.
        Self { enabled: false, auth: true, user: "duoray".into(), pass: String::new(), port: 10808 }
    }
}

/// Twelve letters and digits for the shared proxy.
pub fn new_password() -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..12).map(|_| CHARS[fastrand::usize(..CHARS.len())] as char).collect()
}

impl Default for MobileSettings {
    fn default() -> Self {
        Self { policy: NetworkPolicy::default(), apps: AppRules::default(), background_stats: true, mobile_ping_threads: 1,
            wifi_ping_threads: 4,
            live_update: true,
            core: None,
            quiet_notification: false,
            failover_secs: 15,
            keep_connected: false,
            share: ShareProxy::default(),
        }
    }
}

/// Which apps the VPN carries, the way Android's VpnService can split it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    /// Every app.
    #[default]
    All,
    /// Every app but the listed ones.
    Bypass,
    /// Only the listed apps.
    Only,
}

impl AppMode {
    pub fn code(self) -> i32 {
        match self {
            Self::All => 0,
            Self::Bypass => 1,
            Self::Only => 2,
        }
    }

    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Bypass,
            2 => Self::Only,
            _ => Self::All,
        }
    }
}

/// Each mode keeps its own list: switching "Кроме выбранных" and "Только
/// выбранные" back and forth does not lose either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppRules {
    pub mode: AppMode,
    /// Apps that go around the VPN ("Кроме выбранных"; shown with "Все" too).
    pub bypass: Vec<String>,
    /// The only apps the VPN carries ("Только выбранные").
    pub only: Vec<String>,
    /// The one shared list of 0.1.0, read once into the list of its mode.
    #[serde(skip_serializing)]
    pub packages: Vec<String>,
}

impl AppRules {
    /// The list the current mode uses.
    pub fn list(&self) -> &Vec<String> {
        if self.mode == AppMode::Only { &self.only } else { &self.bypass }
    }

    pub fn list_mut(&mut self) -> &mut Vec<String> {
        if self.mode == AppMode::Only { &mut self.only } else { &mut self.bypass }
    }

    fn migrate(&mut self) {
        let old = std::mem::take(&mut self.packages);
        if !old.is_empty() && self.bypass.is_empty() && self.only.is_empty() {
            *self.list_mut() = old;
        }
    }
}

impl MobileSettings {
    pub fn load(path: &Path) -> Self {
        let mut s: Self = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        s.apps.migrate();
        s
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?).context("mobile.json")?;
        std::fs::rename(&tmp, path).context("mobile.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!("duoray-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mobile.json");
        assert_eq!(MobileSettings::load(&path), MobileSettings::default());
        let mut s = MobileSettings::default();
        s.apps = AppRules { mode: AppMode::Only, only: vec!["org.telegram.messenger".into()], ..Default::default() };
        s.policy.enabled = true;
        s.policy.set_ssid("Home", "k".into());
        s.save(&path).unwrap();
        assert_eq!(MobileSettings::load(&path), s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn modes_keep_their_own_lists_and_old_files_migrate() {
        let old: MobileSettings = serde_json::from_str(r#"{"apps":{"mode":"only","packages":["a","b"]}}"#).unwrap();
        let mut rules = old.apps.clone();
        rules.migrate();
        assert_eq!((rules.only.as_slice(), rules.bypass.len()), (&["a".to_string(), "b".to_string()][..], 0));
        rules.mode = AppMode::Bypass;
        rules.list_mut().push("c".into());
        rules.mode = AppMode::Only;
        assert_eq!(rules.list().len(), 2);
        let saved = serde_json::to_value(&rules).unwrap();
        assert!(saved.get("packages").is_none());
        assert_eq!(new_password().len(), 12);
    }
}
