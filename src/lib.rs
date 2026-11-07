//! DUORAY for phones: the desktop client's look and duoray-core, with
//! xray-core reading the platform's VPN TUN itself (Android's VpnService
//! now, iOS's packet tunnel later, both through `xray.tun.fd`).

mod app;
mod core;
mod deeplink;
mod diag;
mod exit;
mod flags;
mod netpolicy;
mod ping;
mod platform;
mod qr;
mod servers;
mod service;
mod settings;
mod share;
mod status;
#[cfg(test)]
mod testing;
mod themes;
mod tunnel;
mod update;

slint::include_modules!();

/// A link the platform opened the app with (`duoray://add/...`).
pub fn open_link(link: &str) {
    app::open_link(link);
}

/// The system Back, while the window takes it (see Platform::set_back_enabled).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn back() {
    app::back();
}

/// Connects to the last server, or disconnects: the Quick Settings tile and
/// the launcher shortcut, with or without the window.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn toggle() {
    let snapshot = service::snapshot();
    if matches!(snapshot.status, service::Status::On | service::Status::Connecting) {
        service::disconnect();
        return;
    }
    let last = service::host()
        .and_then(|h| duoray_core::store::Store::load(&h.store()).ok())
        .and_then(|s| s.settings.last_server);
    service::connect(last);
}

/// What the platform's QR scanner read.
pub fn scanned(text: &str) {
    app::scanned(text);
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    platform::android::main(app);
}

/// The phone UI in a desktop window (`cargo run --bin duoray-preview`).
#[cfg(not(target_os = "android"))]
pub fn preview() -> anyhow::Result<()> {
    platform::preview::main()
}
