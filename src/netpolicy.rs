//! A server per network: one for Wi-Fi, one for mobile data, and one per
//! named Wi-Fi network on top (home, work, ...). When the phone changes
//! networks while connected, the connection moves to that network's server.

use serde::{Deserialize, Serialize};

// Only the Android shell reports mobile and other networks.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetKind {
    #[default]
    Offline,
    Wifi,
    Mobile,
    /// Ethernet, Bluetooth tethering and the like: no rule applies.
    Other,
}

impl NetKind {
    /// The code the platform shells report networks with.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Wifi,
            2 => Self::Mobile,
            3 => Self::Other,
            _ => Self::Offline,
        }
    }
}

/// The network the phone uses now (not the VPN itself).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkEnv {
    pub kind: NetKind,
    /// Wi-Fi name, when the system lets us read it.
    pub ssid: Option<String>,
}

impl NetworkEnv {
    pub fn describe(&self) -> String {
        match (self.kind, &self.ssid) {
            (NetKind::Wifi, Some(ssid)) => format!("Wi-Fi «{ssid}»"),
            (NetKind::Wifi, None) => "Wi-Fi".into(),
            (NetKind::Mobile, _) => "Мобильная сеть".into(),
            (NetKind::Other, _) => "Другая сеть".into(),
            (NetKind::Offline, _) => "Нет сети".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsidRule {
    pub ssid: String,
    /// Server key (see [`crate::servers::key_of`]).
    pub server: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkPolicy {
    pub enabled: bool,
    pub wifi: Option<String>,
    pub mobile: Option<String>,
    pub ssids: Vec<SsidRule>,
}

impl NetworkPolicy {
    /// The server this network asks for; `None` keeps the one chosen by hand.
    pub fn server_for(&self, net: &NetworkEnv) -> Option<&str> {
        if !self.enabled {
            return None;
        }
        match net.kind {
            NetKind::Wifi => net
                .ssid
                .as_deref()
                .and_then(|ssid| self.ssids.iter().find(|r| r.ssid == ssid))
                .map(|r| r.server.as_str())
                .or(self.wifi.as_deref()),
            NetKind::Mobile => self.mobile.as_deref(),
            NetKind::Other | NetKind::Offline => None,
        }
    }

    pub fn set_ssid(&mut self, ssid: &str, server: String) {
        match self.ssids.iter_mut().find(|r| r.ssid == ssid) {
            Some(rule) => rule.server = server,
            None => self.ssids.push(SsidRule { ssid: ssid.to_string(), server }),
        }
    }

    /// Drops rules whose server is gone (a subscription removed it).
    pub fn forget_missing(&mut self, exists: impl Fn(&str) -> bool) {
        if self.wifi.as_deref().is_some_and(|k| !exists(k)) {
            self.wifi = None;
        }
        if self.mobile.as_deref().is_some_and(|k| !exists(k)) {
            self.mobile = None;
        }
        self.ssids.retain(|r| exists(&r.server));
    }
}

/// Android quotes ordinary SSIDs and reports "<unknown ssid>" when the app
/// may not see the name. Spaces and letter case are part of the name.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn normalize_ssid(raw: &str) -> Option<String> {
    let s = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(raw);
    (!s.is_empty() && s != "<unknown ssid>" && s != "0x").then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi(ssid: Option<&str>) -> NetworkEnv {
        NetworkEnv { kind: NetKind::Wifi, ssid: ssid.map(String::from) }
    }

    fn policy() -> NetworkPolicy {
        NetworkPolicy {
            enabled: true,
            wifi: Some("w".into()),
            mobile: Some("m".into()),
            ssids: vec![SsidRule { ssid: "Home".into(), server: "h".into() }],
        }
    }

    #[test]
    fn named_wifi_wins_over_any_wifi() {
        let p = policy();
        assert_eq!(p.server_for(&wifi(Some("Home"))), Some("h"));
        assert_eq!(p.server_for(&wifi(Some("Cafe"))), Some("w"));
        assert_eq!(p.server_for(&wifi(None)), Some("w"));
    }

    #[test]
    fn mobile_and_others() {
        let p = policy();
        assert_eq!(p.server_for(&NetworkEnv { kind: NetKind::Mobile, ssid: None }), Some("m"));
        assert_eq!(p.server_for(&NetworkEnv { kind: NetKind::Other, ssid: None }), None);
        assert_eq!(p.server_for(&NetworkEnv::default()), None);
    }

    #[test]
    fn disabled_policy_never_picks() {
        let p = NetworkPolicy { enabled: false, ..policy() };
        assert_eq!(p.server_for(&wifi(Some("Home"))), None);
    }

    #[test]
    fn unset_wifi_server_keeps_hand_choice() {
        let p = NetworkPolicy { wifi: None, ..policy() };
        assert_eq!(p.server_for(&wifi(Some("Cafe"))), None);
        assert_eq!(p.server_for(&wifi(Some("Home"))), Some("h"));
    }

    #[test]
    fn set_ssid_replaces_existing_rule() {
        let mut p = policy();
        p.set_ssid("Home", "x".into());
        p.set_ssid("Work", "y".into());
        assert_eq!(p.ssids.len(), 2);
        assert_eq!(p.server_for(&wifi(Some("Home"))), Some("x"));
        assert_eq!(p.server_for(&wifi(Some("Work"))), Some("y"));
    }

    #[test]
    fn forget_missing_drops_dead_servers() {
        let mut p = policy();
        p.forget_missing(|k| k == "m");
        assert_eq!(p.wifi, None);
        assert_eq!(p.mobile.as_deref(), Some("m"));
        assert!(p.ssids.is_empty());
    }

    #[test]
    fn ssid_normalization() {
        assert_eq!(normalize_ssid("\"Home Net\"").as_deref(), Some("Home Net"));
        assert_eq!(normalize_ssid("Plain").as_deref(), Some("Plain"));
        assert_eq!(normalize_ssid("<unknown ssid>"), None);
        assert_eq!(normalize_ssid("\"\""), None);
        assert_eq!(normalize_ssid("0x"), None);
    }
}
