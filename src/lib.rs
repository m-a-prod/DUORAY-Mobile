//! DUORAY for phones: the desktop client's look and duoray-core, with
//! xray-core reading the platform's VPN TUN itself (Android's VpnService
//! now, iOS's packet tunnel later, both through `xray.tun.fd`).

mod app;
mod core;
mod deeplink;
mod diag;
mod exit;
mod flags;
mod grpc;
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
mod widgets;

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

/// A pinned server tapped in a home screen widget: chosen as by hand in
/// the list (over the network's rule) and connected to, or switched to.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn connect_to(key: String) {
    let stored = service::host().and_then(|host| duoray_core::store::Store::load(&host.store()).ok().map(|store| (host, store)));
    // A widget drawn before a subscription update may still hold a server
    // that has left the subscription. Nothing to connect to: it used to fail
    // with «Сервер больше не найден в подписках» and leave the dead server
    // as the selection. The widgets are drawn again from what is there now.
    if stored.as_ref().is_some_and(|(_, store)| servers::find(store, &key).is_none()) {
        diag::log("widget: its server is no longer in a subscription");
        if let Some(p) = service::platform() {
            p.refresh_widgets();
        }
        app::say("Этого сервера больше нет в подписке".into());
        return;
    }
    if let Some((host, mut store)) = stored
        && store.settings.last_server.as_deref() != Some(key.as_str())
    {
        store.settings.last_server = Some(key.clone());
        if let Err(e) = store.save(&host.store()) {
            log::warn!("store.json: {e:#}");
        }
        widgets::write(&host.data, &store);
    }
    app::adopt_selection(key.clone());
    service::pick(&key);
    let snapshot = service::snapshot();
    if matches!(snapshot.status, service::Status::On | service::Status::Connecting) {
        service::switch_to(&key);
    } else {
        service::connect(Some(key));
    }
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
