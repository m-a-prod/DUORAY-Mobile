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
    /// The connection as a Live Update (Android 16+): pinned on top of the
    /// shade with a chip in the status bar. Off: a plain quiet notification.
    pub live_update: bool,
}

impl Default for MobileSettings {
    fn default() -> Self {
        Self { policy: NetworkPolicy::default(), apps: AppRules::default(), background_stats: true, mobile_ping_threads: 1, live_update: true }
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppRules {
    pub mode: AppMode,
    /// Package names; kept across mode changes.
    pub packages: Vec<String>,
}

impl MobileSettings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
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
        s.apps = AppRules { mode: AppMode::Only, packages: vec!["org.telegram.messenger".into()] };
        s.policy.enabled = true;
        s.policy.set_ssid("Home", "k".into());
        s.save(&path).unwrap();
        assert_eq!(MobileSettings::load(&path), s);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
