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
    /// «Живой пинг»: the connected server's delay every 5 s while the window
    /// is open. Off: no pings at all (some operators throttle them), and
    /// «Держать подключение» has nothing to go by.
    pub live_ping: bool,
    /// «Держать подключение» (off by default: on a shaky Wi-Fi it moved
    /// people between servers they had not asked for).
    pub keep_connected: bool,
    /// Kill switch: when the connection breaks the VPN stays up with nothing
    /// behind it, so apps lose the network instead of going around it.
    pub kill_switch: bool,
    /// «Держать подключение»: how long the server may stay silent before
    /// the connection moves to another one.
    pub failover_secs: u32,
    /// «Резерв»: servers (by key) a silent one is replaced with, the fastest
    /// by the last ping. Empty: any server of its subscription.
    pub reserve: Vec<String>,
    /// «Подключаться без Wi-Fi»: leaving Wi-Fi for mobile data connects.
    pub connect_off_wifi: bool,
    /// «Отключать на Wi-Fi» (with connect_off_wifi): back on Wi-Fi, a
    /// connection made on mobile data ends. Off by default; saved under a
    /// new name because 0.1.5 had it on and wrote that into mobile.json.
    #[serde(rename = "disconnect_on_wifi_v2")]
    pub disconnect_on_wifi: bool,
    /// «Режим работы»: VPN, VPN with a proxy next to it, or the proxy alone.
    pub tunnel: TunnelMode,
    /// The proxy of «VPN + прокси» and «Прокси»; «Доступ через хотспот»
    /// opens it to the Wi-Fi or hotspot.
    pub share: ShareProxy,
}

/// What the connection gives the phone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelMode {
    /// Every app's traffic through the VPN.
    #[default]
    Vpn,
    /// The VPN, plus SOCKS5/HTTP for apps that want a proxy and, with
    /// «Доступ через хотспот», for other devices.
    VpnProxy,
    /// SOCKS5/HTTP only, no VPN: it runs next to another VPN (Tailscale, a
    /// work one), and only the apps set to the proxy use it.
    Proxy,
}

impl TunnelMode {
    pub fn code(self) -> i32 {
        match self {
            Self::Vpn => 0,
            Self::VpnProxy => 1,
            Self::Proxy => 2,
        }
    }

    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::VpnProxy,
            2 => Self::Proxy,
            _ => Self::Vpn,
        }
    }

    /// SOCKS5/HTTP listeners in the config.
    pub fn proxy(self) -> bool {
        self != Self::Vpn
    }

    /// A VpnService TUN.
    pub fn vpn(self) -> bool {
        self != Self::Proxy
    }
}

/// SOCKS5 on `port` and HTTP on `port + 1`: on 127.0.0.1 for the phone's
/// apps, or on every interface so devices on the same Wi-Fi or on the
/// phone's hotspot can go through its tunnel too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareProxy {
    /// «Доступ через хотспот»: every interface, not only 127.0.0.1.
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
            reserve: Vec::new(),
            connect_off_wifi: false,
            disconnect_on_wifi: false,
            keep_connected: false,
            live_ping: true,
            kill_switch: false,
            tunnel: TunnelMode::Vpn,
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
        let raw: Option<serde_json::Value> = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok());
        let mut s: Self = raw.as_ref().and_then(|v| Self::deserialize(v).ok()).unwrap_or_default();
        s.apps.migrate();
        // Before «Режим работы» the hotspot proxy ran next to the VPN.
        if s.share.enabled && raw.is_some_and(|v| v.get("tunnel").is_none()) {
            s.tunnel = TunnelMode::VpnProxy;
        }
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

    #[test]
    fn disconnect_on_wifi_is_off_unless_turned_on_again() {
        let old: MobileSettings = serde_json::from_str(r#"{"disconnect_on_wifi": true}"#).unwrap();
        assert!(!old.disconnect_on_wifi);
        let on = MobileSettings { disconnect_on_wifi: true, ..Default::default() };
        let back: MobileSettings = serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(back.disconnect_on_wifi);
    }

    #[test]
    fn hotspot_sharing_of_old_files_becomes_vpn_with_proxy() {
        let dir = std::env::temp_dir().join(format!("duoray-tunnel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mobile.json");
        std::fs::write(&path, r#"{"share":{"enabled":true}}"#).unwrap();
        assert_eq!(MobileSettings::load(&path).tunnel, TunnelMode::VpnProxy);
        std::fs::write(&path, r#"{"share":{"enabled":true},"tunnel":"vpn"}"#).unwrap();
        assert_eq!(MobileSettings::load(&path).tunnel, TunnelMode::Vpn);
        std::fs::write(&path, r#"{"tunnel":"proxy"}"#).unwrap();
        let s = MobileSettings::load(&path);
        assert!(s.tunnel.proxy() && !s.tunnel.vpn());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
