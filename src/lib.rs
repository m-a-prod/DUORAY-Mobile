//! DUORAY for phones: the desktop client's look and duoray-core, with
//! xray-core reading the platform's VPN TUN itself (Android's VpnService
//! now, iOS's packet tunnel later, both through `xray.tun.fd`).

mod app;
mod deeplink;
mod exit;
mod flags;
mod netpolicy;
mod ping;
mod platform;
mod servers;
mod service;
mod settings;
mod themes;
mod tunnel;

slint::include_modules!();

/// A link the platform opened the app with (`duoray://add/...`).
pub fn open_link(link: &str) {
    app::open_link(link);
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
