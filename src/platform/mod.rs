//! What the app needs from the OS shell around it: Android today (a Kotlin
//! VpnService), iOS later (a packet tunnel extension), and a desktop window
//! for working on the screens.

#[cfg(target_os = "android")]
pub mod android;
#[cfg(not(target_os = "android"))]
pub mod preview;

/// Calls may come from any thread and must not block: answers come back
/// through `service::on_tun_up` and friends.
pub trait Platform: Send + Sync {
    /// Asks the user for the VPN if needed, then opens the TUN.
    fn start_vpn(&self);
    fn stop_vpn(&self);
    /// Re-opens the TUN with the current per-app rules.
    fn rebuild_vpn(&self) {}
    /// Asks for what lets the app read Wi-Fi names (location on Android).
    fn request_wifi_names(&self) {}
    fn open_url(&self, _url: &str) {}
    fn copy_text(&self, _text: &str) {}
    /// Opens the camera to read a subscription QR code (answer: `crate::scanned`).
    fn scan_qr(&self) {}
    /// A short tick of the vibration motor (long press feedback).
    fn haptic(&self) {}
    /// Apps the user can route, for the per-app list. May block.
    fn list_apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AppInfo {
    pub package: String,
    pub label: String,
    /// A PNG of its icon, when there is one.
    #[serde(default)]
    pub icon: String,
}
