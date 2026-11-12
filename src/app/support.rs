//! Consent to error reports, "Сообщить о проблеме" and in-app updates: the
//! desktop's support_ui.rs. Updates are looked for on any network. One
//! downloads by itself on Wi-Fi; on mobile data the banner offers it and the
//! user starts the download, unless «Скачивать и по мобильной сети» is on.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use duoray_core::store;
use serde_json::json;
use slint::ComponentHandle;

use super::{App, later, with_app};
use crate::netpolicy::NetKind;
use crate::service::{self, Status};
use crate::update::{self, Found};
use crate::{AppState, diag};

/// Where an update stands; shown as a banner over the server list.
#[derive(Default)]
pub(super) enum UpdateState {
    #[default]
    Idle,
    Checking,
    /// Newer version found on mobile data: downloaded when the user asks.
    Found(Found),
    /// Version, percent, install when done (the banner was tapped).
    Downloading(String, u8, bool),
    /// Verified and ready to install.
    Ready(Found, PathBuf),
}

/// Whether a version just found downloads without being asked for: on Wi-Fi
/// always, elsewhere (mobile data, a tethered phone's Bluetooth) only with
/// «Скачивать и по мобильной сети». An APK is about 35 MB.
fn downloads_ahead(net: NetKind, on_mobile: bool) -> bool {
    net == NetKind::Wifi || on_mobile
}

/// First check a little after start, then every 6 hours.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// A check that failed (no network just then) is tried again sooner.
const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

impl App {
    /// At start: consent (asked once), queued reports, update checks.
    pub(super) fn start_support(&mut self) {
        let s = &self.store.settings;
        diag::configure(s.telemetry == Some(true), &s.install_id);
        diag::flush();
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_consent_open(s.telemetry.is_none());
        }
        self.update_diag_context();
        self.show_update();
        // The first check is the timer's, not check_update_if_due's.
        self.update_checked_at = store::now();
        slint::Timer::single_shot(FIRST_CHECK, || with_app(|app| app.check_update(false)));
        self.update_timer.start(slint::TimerMode::Repeated, CHECK_EVERY, || {
            with_app(|app| app.check_update(false));
            // Reports queued while offline get another chance as well.
            diag::flush();
        });
    }

    /// Records the answer; the install id exists only while reports are on,
    /// and opting out drops reports that were still waiting to be sent.
    pub(super) fn set_consent(&mut self, agree: bool) {
        let s = &mut self.store.settings;
        if agree && s.install_id.is_empty() {
            s.install_id = format!("{:016x}", fastrand::u64(..));
        }
        if !agree {
            s.install_id.clear();
            diag::drop_queue();
        }
        s.telemetry = Some(agree);
        diag::configure(agree, &s.install_id);
        diag::log(format!("reports {}", if agree { "on" } else { "off" }));
        self.save_store();
        self.render_settings();
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_consent_open(false);
        }
    }

    /// What reports carry besides the error: the kind of connection and
    /// settings, never names, addresses or links.
    pub(super) fn update_diag_context(&self) {
        let s = &self.store.settings;
        let server = s.last_server.as_deref().and_then(|k| crate::servers::find(&self.store, k)).map(|(_, v)| {
            json!({
                "protocol": v.protocol, "network": v.network, "security": v.security, "proxies": v.proxies,
                "json": matches!(v.source, duoray_core::server::Source::Json(_)), "edited": v.override_key.is_some(),
            })
        });
        let connection = match &self.conn.status {
            Status::Off => "off",
            Status::Connecting => "connecting",
            Status::On => "on",
            Status::Failed(_) => "failed",
        };
        diag::set_context(json!({
            "connection": connection,
            "network": format!("{:?}", self.conn.net.kind),
            "server": server,
            "subscriptions": self.store.subscriptions.iter().filter(|x| !x.is_manual()).count(),
            "settings": {
                "ping": format!("{:?}/{:?}/{}", s.ping.mode, s.ping.proxy_mode, s.ping.threads),
                "ru_direct": s.routing.simple.ru_direct,
                "whitelist_direct": s.routing.simple.whitelist_direct,
                "lan_direct": s.routing.simple.lan_direct,
                "apps_mode": self.settings.apps.mode.code(),
                "apps": self.settings.apps.list().len(),
                "network_rules": self.settings.policy.ssids.len(),
                "keep_connected": self.settings.keep_connected,
                "failover_secs": self.settings.failover_secs,
                "live_ping": self.settings.live_ping,
                "kill_switch": self.settings.kill_switch,
                "share": self.settings.share.enabled,
                "tunnel": format!("{:?}", self.settings.tunnel),
                "core": self.settings.core.is_some(),
                "autoconnect": s.autoconnect,
                "happ": s.happ.enabled,
            },
        }));
    }

    // ── "Сообщить о проблеме" ──────────────────────────────────

    pub(super) fn open_report(&mut self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        st.set_report_preview("".into());
        st.set_report_code("".into());
        st.set_report_error("".into());
        st.set_report_busy(false);
        st.set_report_open(true);
    }

    pub(super) fn preview_report(&mut self, note: &str) {
        let Some(ui) = self.ui() else { return };
        self.update_diag_context();
        let report = diag::manual_report(note);
        ui.global::<AppState>().set_report_preview(serde_json::to_string_pretty(&report).unwrap_or_default().into());
    }

    pub(super) fn send_report(&mut self, note: &str) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        if st.get_report_busy() {
            return;
        }
        self.update_diag_context();
        let report = diag::manual_report(note);
        st.set_report_busy(true);
        st.set_report_error("".into());
        std::thread::spawn(move || {
            let result = diag::send_now(&report).map_err(|e| format!("{e:#}"));
            later(move |app| {
                let Some(ui) = app.ui() else { return };
                let st = ui.global::<AppState>();
                st.set_report_busy(false);
                match result {
                    Ok(code) => {
                        diag::log(format!("report sent: {code}"));
                        st.set_report_code(code.into());
                    }
                    Err(e) => st.set_report_error(format!("Не удалось отправить: {e}").into()),
                }
            });
        });
    }

    // ── Updates ────────────────────────────────────────────────

    /// The automatic check once 6 hours have passed by the clock on the
    /// wall, asked for while the window is on screen. The timer of
    /// start_support counts only the time the phone is awake: with the app
    /// left open for days its 6 hours took far longer, and a new version
    /// was not offered to people who had it open in front of them.
    pub(super) fn check_update_if_due(&mut self) {
        let now = store::now();
        // The clock was set back: count from here.
        self.update_checked_at = self.update_checked_at.min(now);
        if now - self.update_checked_at >= CHECK_EVERY.as_secs() {
            self.update_checked_at = now;
            self.check_update(false);
        }
    }

    /// Asks the hub for a newer version. `manual`: the user pressed
    /// "Проверить обновления" (runs with automatic checks off, downloads on
    /// mobile data too).
    pub(super) fn check_update(&mut self, manual: bool) {
        let busy = matches!(self.update, UpdateState::Checking | UpdateState::Downloading(..));
        if busy || (!manual && !self.store.settings.auto_update) || matches!(self.update, UpdateState::Ready(..)) {
            return;
        }
        self.update_checked_at = store::now();
        self.update = UpdateState::Checking;
        self.set_update_info("Проверка обновлений…");
        self.show_update();
        let proxy = self.conn.link.as_ref().map(service::Link::proxy_url);
        std::thread::spawn(move || {
            let found = update::check(proxy.as_deref()).map_err(|e| format!("{e:#}"));
            later(move |app| app.update_checked(found, manual));
        });
    }

    fn update_checked(&mut self, found: Result<Option<Found>, String>, manual: bool) {
        match found {
            Ok(None) => {
                self.update = UpdateState::Idle;
                self.set_update_info(&format!("Установлена последняя версия ({}, {}).", env!("CARGO_PKG_VERSION"), update::BUILD));
            }
            Ok(Some(f)) => {
                diag::log(format!("update {} found", f.version));
                self.set_update_info(&format!("Доступна версия {}.", f.version));
                // On Wi-Fi it downloads ahead, on mobile data only if the
                // user said so; the banner installs on a tap.
                if manual || downloads_ahead(self.conn.net.kind, self.settings.update_on_mobile) {
                    self.download_update(f, false);
                    return;
                }
                self.update = UpdateState::Found(f);
            }
            Err(e) => {
                self.update = UpdateState::Idle;
                // Offline just then, most likely: not another 6 hours.
                self.update_checked_at = store::now().saturating_sub((CHECK_EVERY - RETRY_AFTER).as_secs());
                diag::log(format!("update check failed: {e}"));
                self.set_update_info(&format!("Не удалось проверить обновления: {e}"));
            }
        }
        self.show_update();
    }

    /// «Скачивать и по мобильной сети». Turned on with a version already
    /// offered by the banner, it starts that download.
    pub(super) fn set_update_on_mobile(&mut self, on: bool) {
        self.settings.update_on_mobile = on;
        self.save_settings();
        self.render_settings();
        if on
            && matches!(self.update, UpdateState::Found(_))
            && let UpdateState::Found(found) = std::mem::take(&mut self.update)
        {
            self.download_update(found, false);
        }
    }

    /// `install`: the user tapped the banner, so the installer opens as soon
    /// as the file is in and verified.
    fn download_update(&mut self, found: Found, install: bool) {
        let version = found.version.clone();
        self.set_update_info(&format!("Загружается версия {version}…"));
        self.update = UpdateState::Downloading(version, 0, install);
        self.show_update();
        let dir = self.host.data.join("updates");
        let proxy = self.conn.link.as_ref().map(service::Link::proxy_url);
        std::thread::spawn(move || {
            let last = AtomicU64::new(0);
            let result = update::download(&found.asset, &found.hub, &dir, proxy.as_deref(), |done, total| {
                // A few progress updates, not one per chunk.
                let pct = done * 100 / total.max(1);
                if pct >= last.load(Ordering::Relaxed) + 2 {
                    last.store(pct, Ordering::Relaxed);
                    later(move |app| {
                        if let UpdateState::Downloading(_, p, _) = &mut app.update {
                            *p = pct as u8;
                        }
                        app.show_update();
                    });
                }
            });
            let result = result.map_err(|e| format!("{e:#}"));
            later(move |app| {
                let install = matches!(app.update, UpdateState::Downloading(_, _, true));
                match result {
                    Ok(file) => {
                        diag::log(format!("update {} downloaded and verified", found.version));
                        let notes = if found.notes.is_empty() { String::new() } else { format!(" Что нового: {}", found.notes) };
                        app.set_update_info(&format!("Версия {} загружена и проверена.{notes}", found.version));
                        app.update = UpdateState::Ready(found, file);
                        if install {
                            app.update_clicked();
                        }
                    }
                    Err(e) => {
                        diag::log(format!("update download failed: {e}"));
                        app.set_update_info(&format!("Не удалось загрузить обновление: {e}"));
                        app.update = UpdateState::Found(found);
                        app.toast(format!("Не удалось загрузить обновление: {e}"));
                    }
                }
                app.show_update();
            });
        });
    }

    /// A tap on the banner, as in MaterialXray: download if needed, then the
    /// system installer.
    pub(super) fn update_clicked(&mut self) {
        match std::mem::take(&mut self.update) {
            UpdateState::Found(f) => self.download_update(f, true),
            UpdateState::Downloading(v, p, _) => self.update = UpdateState::Downloading(v, p, true),
            UpdateState::Ready(f, file) => {
                diag::log(format!("installing update {}", f.version));
                if let Some(p) = service::platform() {
                    p.install_apk(&file.to_string_lossy());
                }
                // Stays ready: the user may cancel the system prompt.
                self.update = UpdateState::Ready(f, file);
            }
            other => self.update = other,
        }
        self.show_update();
    }

    fn set_update_info(&self, text: &str) {
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_update_info(text.into());
        }
    }

    /// The banner over the server list: title, version line, and a progress
    /// bar while downloading (-1: none).
    fn show_update(&self) {
        let Some(ui) = self.ui() else { return };
        let (detail, progress) = match &self.update {
            UpdateState::Idle | UpdateState::Checking => (String::new(), -1.0),
            UpdateState::Found(f) => (format!("DUORAY {} · нажмите, чтобы установить", f.version), -1.0),
            UpdateState::Downloading(v, p, _) => (format!("DUORAY {v} · загрузка {p}%"), f32::from(*p) / 100.0),
            UpdateState::Ready(f, _) => (format!("DUORAY {} загружено · нажмите, чтобы установить", f.version), -1.0),
        };
        let st = ui.global::<AppState>();
        st.set_update_banner(if detail.is_empty() { "".into() } else { "Доступно обновление".into() });
        st.set_update_detail(detail.into());
        st.set_update_progress(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_downloads_by_itself_on_wifi_and_elsewhere_only_when_allowed() {
        assert!(downloads_ahead(NetKind::Wifi, false));
        assert!(!downloads_ahead(NetKind::Mobile, false));
        assert!(!downloads_ahead(NetKind::Other, false));
        assert!(downloads_ahead(NetKind::Mobile, true));
    }
}
