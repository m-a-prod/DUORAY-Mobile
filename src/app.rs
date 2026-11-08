//! The window: shows the store, the connection and the routing rules the
//! way the desktop app does, and turns taps into store changes and
//! [`service`] calls. Lives on the UI thread; worker threads come back
//! through `invoke_from_event_loop`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use duoray_core::balancer::{self, Trace};
use duoray_core::identity::{self, SubIdentityMode};
use duoray_core::server::{Server, display_name};
use duoray_core::store::{self, PingMode, ProxyPingMode, Store, Subscription};
use duoray_core::subscription;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use self::support::UpdateState;
use crate::exit::{self, Traffic};
use crate::netpolicy::{NetKind, NetworkPolicy};
use crate::platform::AppInfo;
use crate::service::{self, Host, Snapshot, Status};
use crate::settings::{AppMode, MobileSettings};
use crate::{Actions, AppRow, AppState, AppWindow, ServerRow, SsidRow, SubItem, flags, ping, servers, themes, tunnel};

mod cores;
mod support;

type PingResult = Result<Duration, String>;

struct App {
    ui: slint::Weak<AppWindow>,
    host: Arc<Host>,
    store: Store,
    settings: MobileSettings,
    /// Index of the subscription on screen.
    current: Option<usize>,
    /// Row → server index of the shown subscription (sorted by ping or not).
    order: Vec<usize>,
    sort_by_ping: bool,
    pings: HashMap<String, PingResult>,
    /// Servers being pinged now (a spinner in their row).
    measuring: HashSet<String>,
    pinging: bool,
    live_busy: bool,
    /// Live pings missed in a row by the connected server.
    live_failures: u32,
    /// Servers "Держать подключение" already moved away from.
    failover_tried: HashSet<String>,
    /// Servers being checked for «Держать подключение» right now.
    failover_busy: bool,
    /// Live pings answered so far: a check that finds the connected server
    /// answering again meanwhile changes nothing.
    live_answers: u64,
    /// After a round where no server answered (the internet is gone), the
    /// next waits: each round starts throwaway xrays.
    failover_quiet_until: Option<Instant>,
    ticks: u32,
    /// Stops the running ping check.
    ping_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// A list redraw is scheduled (render_servers_soon).
    render_pending: bool,
    /// The server open in the editor: (subscription id, server index).
    editing: Option<(String, usize)>,
    refreshing: HashSet<String>,
    conn: Snapshot,
    traffic: Option<Traffic>,
    exit: Option<Trace>,
    /// When the exit country was last asked for (retried until known).
    traced: Option<Instant>,
    polling: bool,
    apps: Vec<AppInfo>,
    apps_filter: String,
    update: UpdateState,
    cores: cores::Cores,
    toast_timer: slint::Timer,
    poll_timer: slint::Timer,
    update_timer: slint::Timer,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
    static ICONS: RefCell<HashMap<String, Option<slint::Image>>> = RefCell::default();
}

fn with_app(f: impl FnOnce(&mut App)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&mut app.borrow_mut());
    }
}

/// Servers «Держать подключение» checks in one round before staying put.
const FAILOVER_CHECKS: usize = 5;

/// Runs `f` on the UI thread with the app, from any thread.
/// Whether Android leaves DUORAY alone in the background (Android reports it
/// on every return to the app).
static BATTERY_UNRESTRICTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Android 17's local network permission, for the hotspot proxy.
static LOCAL_NETWORK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The caption bar a floating window puts over its top and the bars over
/// its bottom, in pixels.
static CAPTION_TOP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static BARS_BOTTOM: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_window_insets(top: i32, bottom: i32) {
    log::info!("window insets: caption {top}px, bars {bottom}px");
    CAPTION_TOP.store(top.max(0), std::sync::atomic::Ordering::Relaxed);
    BARS_BOTTOM.store(bottom.max(0), std::sync::atomic::Ordering::Relaxed);
    let _ = slint::invoke_from_event_loop(|| with_app(App::apply_caption));
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_local_network(granted: bool) {
    LOCAL_NETWORK.store(granted, std::sync::atomic::Ordering::Relaxed);
    let _ = slint::invoke_from_event_loop(|| with_app(|app| app.render_settings()));
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_battery_unrestricted(on: bool) {
    BATTERY_UNRESTRICTED.store(on, std::sync::atomic::Ordering::Relaxed);
    let _ = slint::invoke_from_event_loop(|| with_app(|app| app.render_settings()));
}

/// The system Back (Android's OnBackInvokedCallback): closes what is open.
/// Not a key event: keys go to whatever has the focus (a text field in a
/// sheet), which knows nothing of Back.
pub fn back() {
    let _ = slint::invoke_from_event_loop(|| {
        with_app(|app| {
            if let Some(ui) = app.ui() {
                ui.invoke_go_back();
            }
        })
    });
}

/// Bumped by every window: when Android replaces the activity, the old
/// window's teardown may run after the new one started and must not undo it.
static WINDOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn later(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(f));
}

/// A server chosen outside the window (a home screen widget): the open
/// window keeps it as its selection, so its next save does not undo it.
pub fn adopt_selection(key: String) {
    later(move |app| {
        app.store.settings.last_server = Some(key);
        app.render_servers();
        app.render_connection();
    });
}

/// Shows the window until the platform closes it. `setup` runs once the
/// window is built (the preview uses it for screenshots).
pub fn run(setup: impl FnOnce(&AppWindow)) -> Result<(), slint::PlatformError> {
    let host = service::host().ok_or_else(|| slint::PlatformError::Other("service::init was not called".into()))?;
    let ui = AppWindow::new()?;
    let store = Store::load(&host.store()).unwrap_or_else(|e| {
        log::error!("store.json: {e:#}");
        Store::default()
    });
    let settings = MobileSettings::load(&host.settings());
    let current = store
        .settings
        .last_server
        .as_deref()
        .and_then(|k| servers::find(&store, k))
        .and_then(|(sub, _)| store.subscriptions.iter().position(|s| s.id == sub.id))
        .or_else(|| store.subscriptions.iter().position(|s| !s.is_manual()))
        .or_else(|| (!store.subscriptions.is_empty()).then_some(0));
    let autoconnect = store.settings.autoconnect;
    let app = App {
        ui: ui.as_weak(),
        host: host.clone(),
        store,
        settings,
        current,
        order: Vec::new(),
        sort_by_ping: false,
        pings: load_pings(&host),
        measuring: HashSet::new(),
        pinging: false,
        live_busy: false,
        live_failures: 0,
        failover_tried: HashSet::new(),
        failover_busy: false,
        live_answers: 0,
        failover_quiet_until: None,
        ticks: 0,
        editing: None,
        ping_cancel: None,
        render_pending: false,
        refreshing: HashSet::new(),
        conn: service::snapshot(),
        traffic: None,
        exit: None,
        traced: None,
        polling: false,
        apps: Vec::new(),
        apps_filter: String::new(),
        update: UpdateState::default(),
        cores: cores::Cores::default(),
        toast_timer: slint::Timer::default(),
        poll_timer: slint::Timer::default(),
        update_timer: slint::Timer::default(),
    };
    crate::widgets::write(&host.data, &app.store);
    APP.with(|a| *a.borrow_mut() = Some(Rc::new(RefCell::new(app))));
    wire(&ui);
    ui.global::<AppState>().set_version(env!("CARGO_PKG_VERSION").into());
    with_app(|app| {
        app.apply_caption();
        app.apply_appearance();
        app.render_all();
        app.refresh_stale();
        app.start_support();
    });
    // "Подключаться при запуске": the last server, a moment after start.
    if autoconnect && service::snapshot().status == Status::Off {
        slint::Timer::single_shot(Duration::from_millis(1500), || {
            with_app(|app| {
                if matches!(app.conn.status, Status::Off | Status::Failed(_)) {
                    service::connect(app.store.settings.last_server.clone());
                }
            })
        });
    }
    service::set_listener(Some(Box::new(|snapshot| later(move |app| app.on_connection(snapshot)))));
    let poll = ui.as_weak();
    with_app(|app| {
        app.poll_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
            if poll.upgrade().is_some() {
                with_app(App::poll);
            }
        })
    });
    setup(&ui);
    with_app(App::take_link);
    if let Some(p) = service::platform() {
        p.set_back_enabled(ui.get_can_go_back());
        p.set_light_bars(ui.get_light_theme());
    }
    let window = WINDOW.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    crate::status::set_window_open(true);
    let result = ui.run();
    if WINDOW.load(std::sync::atomic::Ordering::SeqCst) == window {
        crate::status::set_window_open(false);
        service::set_listener(None);
    }
    APP.with(|a| *a.borrow_mut() = None);
    result
}

fn wire(ui: &AppWindow) {
    let a = ui.global::<Actions>();
    a.on_select_sub(|i| with_app(|app| app.select_sub(i)));
    a.on_select_server(|row| with_app(|app| app.select_server(row as usize)));
    a.on_remove_server(|row| with_app(|app| app.remove_server(row as usize)));
    a.on_toggle_connection(|| with_app(App::toggle));
    a.on_consent(|agree| with_app(|app| app.set_consent(agree)));
    a.on_set_telemetry(|on| with_app(|app| app.set_consent(on)));
    a.on_set_auto_update(|on| with_app(|app| app.edit_settings(|s| s.auto_update = on)));
    a.on_check_update(|| with_app(|app| app.check_update(true)));
    a.on_update_clicked(|| with_app(App::update_clicked));
    a.on_report_problem(|| with_app(App::open_report));
    a.on_report_preview(|note| with_app(|app| app.preview_report(&note)));
    a.on_report_send(|note| with_app(|app| app.send_report(&note)));
    a.on_refresh(|| {
        with_app(|app| {
            if let Some(id) = app.sub().map(|s| s.id.clone()) {
                app.refresh(id);
            }
        })
    });
    a.on_ping_all(|| with_app(App::ping_current));
    a.on_ping_server(|row| with_app(|app| app.ping_row(row as usize)));
    a.on_toggle_sort(|| {
        with_app(|app| {
            app.sort_by_ping = !app.sort_by_ping;
            app.render_servers();
        })
    });
    a.on_toggle_announce(|| {
        with_app(|app| {
            if let Some(i) = app.current {
                app.store.subscriptions[i].announce_hidden = !app.store.subscriptions[i].announce_hidden;
                app.save_store();
                app.render_header();
            }
        })
    });
    a.on_open_web_page(|| with_app(|app| app.open(|s| s.info.web_page_url.clone())));
    a.on_open_support(|| with_app(|app| app.open(|s| s.info.support_url.clone())));
    a.on_add_subscription(|text| with_app(|app| app.add(&text)));
    a.on_add_links(|text| with_app(|app| app.add_links(&text)));
    a.on_remove_subscription(|| with_app(App::remove_current));
    a.on_set_policy_enabled(|on| with_app(|app| app.edit_policy(|p| p.enabled = on)));
    a.on_pick_for(|target, row| with_app(|app| app.pick_for(target, row)));
    a.on_remove_ssid(|i| {
        with_app(|app| {
            app.edit_policy(|p| {
                if (i as usize) < p.ssids.len() {
                    p.ssids.remove(i as usize);
                }
            })
        })
    });
    a.on_ask_wifi_names(|| {
        if let Some(p) = service::platform() {
            p.request_wifi_names();
        }
    });
    a.on_load_apps(|| with_app(App::load_apps));
    a.on_set_app_mode(|mode| with_app(|app| app.edit_apps(|r| r.mode = AppMode::from_code(mode))));
    a.on_toggle_app(|package| {
        with_app(|app| {
            let package = package.to_string();
            app.edit_apps(|r| {
                let list = r.list_mut();
                match list.iter().position(|p| *p == package) {
                    Some(i) => drop(list.remove(i)),
                    None => list.push(package),
                }
            })
        })
    });
    a.on_filter_apps(|text| {
        with_app(|app| {
            app.apps_filter = text.to_string();
            app.render_apps();
        })
    });
    a.on_set_theme(|i| {
        with_app(|app| {
            app.store.settings.theme = themes::id_at(i as usize).to_string();
            app.save_store();
            app.apply_appearance();
        })
    });
    a.on_set_text_scale(|v| {
        with_app(|app| {
            app.store.settings.text_scale = v.clamp(80, 150) as u32;
            app.save_store();
            app.apply_appearance();
        })
    });
    a.on_set_ping_mode(|m| {
        with_app(|app| {
            app.store.settings.ping.mode = match m {
                1 => PingMode::HttpHead,
                2 => PingMode::Tcp,
                3 => PingMode::Icmp,
                _ => PingMode::HttpGet,
            };
            app.save_store();
            app.render_settings();
        })
    });
    a.on_set_ping_threads(|v| {
        with_app(|app| {
            app.settings.wifi_ping_threads = v.clamp(1, 16) as u32;
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_set_mobile_ping_threads(|v| {
        with_app(|app| {
            app.settings.mobile_ping_threads = v.clamp(1, 32) as u32;
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_open_cores(|| with_app(App::open_cores));
    a.on_install_core(|tag| with_app(|app| app.install_core(&tag)));
    a.on_select_core(|tag| with_app(|app| app.select_core(&tag)));
    a.on_remove_core(|tag| with_app(|app| app.remove_core(&tag)));
    a.on_set_live_ping(|on| {
        with_app(|app| {
            app.settings.live_ping = on;
            app.live_failures = 0;
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_set_connect_off_wifi(|on| {
        with_app(|app| {
            app.settings.connect_off_wifi = on;
            service::set_connect_off_wifi(on, app.settings.disconnect_on_wifi);
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_set_disconnect_on_wifi(|on| {
        with_app(|app| {
            app.settings.disconnect_on_wifi = on;
            service::set_connect_off_wifi(app.settings.connect_off_wifi, on);
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_pin_widget(|kind| {
        if let Some(p) = service::platform() {
            p.pin_widget(&kind);
        }
    });
    a.on_set_server_pinned(|on| with_app(|app| app.set_server_pinned(on)));
    a.on_set_server_reserve(|on| with_app(|app| app.set_server_reserve(on)));
    a.on_set_server_no_failover(|on| with_app(|app| app.set_server_no_failover(on)));
    a.on_set_kill_switch(|on| {
        with_app(|app| {
            app.settings.kill_switch = on;
            service::set_kill_switch(on);
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_set_quiet_notification(|on| {
        with_app(|app| {
            app.settings.quiet_notification = on;
            crate::status::set_quiet(on);
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_filter_picker(|query| {
        with_app(|app| {
            let Some(ui) = app.ui() else { return };
            let st = ui.global::<AppState>();
            let query = query.trim().to_lowercase();
            let hidden: Vec<bool> = st
                .get_servers()
                .iter()
                .map(|r| !query.is_empty() && ![r.name.as_str(), r.description.as_str(), r.badge.as_str()].iter().any(|t| t.to_lowercase().contains(&query)))
                .collect();
            st.set_picker_hidden(ModelRc::new(VecModel::from(hidden)));
        })
    });
    a.on_request_local_network(|| {
        if let Some(p) = service::platform() {
            p.request_local_network();
        }
    });
    a.on_request_background(|| {
        if let Some(p) = service::platform() {
            p.request_background();
        }
    });
    a.on_set_failover_secs(|v| {
        with_app(|app| {
            app.settings.failover_secs = v.clamp(5, 120) as u32;
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_set_tunnel_mode(|code| with_app(|app| app.set_tunnel_mode(crate::settings::TunnelMode::from_code(code))));
    a.on_set_share(|on| with_app(|app| app.edit_share(|s| s.enabled = on)));
    a.on_set_share_auth(|on| with_app(|app| app.edit_share(|s| s.auth = on)));
    a.on_new_share_password(|| with_app(|app| app.edit_share(|s| s.pass = crate::settings::new_password())));
    a.on_set_share_port(|text| {
        with_app(|app| match text.trim().parse::<u16>() {
            Ok(port) if (1024..=65534).contains(&port) => app.edit_share(|s| s.port = port),
            _ => {
                app.toast("Порт — число от 1024 до 65534".to_string());
                app.render_settings();
            }
        })
    });
    a.on_set_live_update(|on| {
        with_app(|app| {
            app.settings.live_update = on;
            crate::status::set_live_update(on);
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_set_background_stats(|on| {
        with_app(|app| {
            app.settings.background_stats = on;
            crate::status::set_background_stats(on);
            app.save_settings();
            app.render_settings();
        })
    });
    a.on_light_bars(|light| {
        if let Some(p) = service::platform() {
            p.set_light_bars(light);
        }
    });
    a.on_open_url(|url| {
        if let Some(p) = service::platform() {
            p.open_url(&url);
        }
    });
    a.on_back_state(|on| {
        if let Some(p) = service::platform() {
            p.set_back_enabled(on);
        }
    });
    a.on_open_settings(|which| {
        if let Some(p) = service::platform() {
            p.open_settings(&which);
        }
    });
    a.on_set_ping_timeout(|v| {
        with_app(|app| {
            app.store.settings.ping.timeout_ms = v.clamp(1000, 30_000) as u64;
            app.save_store();
            app.render_settings();
        })
    });
    a.on_set_ping_url(|url| {
        with_app(|app| {
            let url = url.trim();
            app.store.settings.ping.url = if url.is_empty() { store::DEFAULT_PING_URL.into() } else { url.into() };
            app.save_store();
        })
    });
    a.on_set_ping_proxy_mode(|m| {
        with_app(|app| {
            app.store.settings.ping.proxy_mode = match m {
                1 => ProxyPingMode::Double,
                2 => ProxyPingMode::KeepAlive,
                _ => ProxyPingMode::Default,
            };
            app.save_store();
            app.render_settings();
        })
    });
    a.on_set_send_device_info(|on| with_app(|app| app.edit_settings(|s| s.send_device_info = on)));
    a.on_set_happ(|on| with_app(|app| app.edit_settings(|s| s.happ.enabled = on)));
    a.on_set_happ_os(|i| with_app(|app| app.edit_settings(|s| s.happ.os = HAPP_OS[(i as usize).min(3)].into())));
    a.on_happ_new_ids(|| with_app(|app| app.edit_settings(|s| s.happ.randomize_ids())));
    a.on_happ_new_device(|| with_app(|app| app.edit_settings(|s| s.happ.randomize_device())));
    a.on_set_sub_id_mode(|m| {
        with_app(|app| {
            let global = app.store.settings.happ.clone();
            app.edit_identity(|id| {
                id.mode = match m {
                    1 => SubIdentityMode::Duoray,
                    2 => SubIdentityMode::Happ,
                    _ => SubIdentityMode::Settings,
                };
                if id.mode == SubIdentityMode::Happ && id.happ.is_none() {
                    let mut own = global;
                    own.enabled = true;
                    own.randomize_ids();
                    id.happ = Some(own);
                }
            })
        })
    });
    a.on_set_sub_hwid(|hwid| with_app(|app| app.edit_identity(|id| id.hwid = hwid.trim().to_string())));
    a.on_sub_random_hwid(|| with_app(|app| app.edit_identity(|id| id.hwid = identity::random_device_hwid())));
    a.on_sub_new_happ(|| {
        with_app(|app| {
            app.edit_identity(|id| {
                if let Some(h) = id.happ.as_mut() {
                    h.randomize_device();
                }
            })
        })
    });
    a.on_set_switch_on_select(|on| {
        with_app(|app| {
            app.store.settings.switch_on_select = on;
            app.save_store();
        })
    });
    a.on_set_autoconnect(|on| {
        with_app(|app| {
            app.store.settings.autoconnect = on;
            app.save_store();
        })
    });
    a.on_set_keep_connected(|on| {
        with_app(|app| {
            app.settings.keep_connected = on;
            app.save_store();
        })
    });
    a.on_set_show_server_type(|on| {
        with_app(|app| {
            app.store.settings.show_server_type = on;
            app.save_store();
            app.apply_appearance();
        })
    });
    a.on_load_log(|| with_app(App::load_log));
    a.on_set_direct(|which, on| {
        with_app(|app| {
            let simple = &mut app.store.settings.routing.simple;
            match which {
                0 => simple.ru_direct = on,
                1 => simple.whitelist_direct = on,
                _ => simple.lan_direct = on,
            }
            app.save_store();
            app.render_networks();
            service::reconnect();
            if which == 2 {
                // The LAN leaves or joins the VPN's routes.
                service::rebuild_tun();
            }
        })
    });
    a.on_connect_fastest(|| with_app(App::connect_fastest));
    a.on_open_server(|row| with_app(|app| app.open_server(row as usize)));
    a.on_save_server(|text| with_app(|app| app.save_server(&text)));
    a.on_reset_server(|| with_app(App::reset_server));
    a.on_copy_text(|text| {
        with_app(|app| {
            if let Some(p) = service::platform() {
                p.copy_text(&text);
                app.toast("Скопировано".into());
            }
        })
    });
    a.on_show_qr(|| {
        with_app(|app| {
            let Some(ui) = app.ui() else { return };
            let url = app.sub().map(|s| s.url.clone()).filter(|u| !u.is_empty());
            match url.as_deref().and_then(crate::qr::image) {
                Some(img) => {
                    ui.global::<AppState>().set_qr_image(img);
                    ui.invoke_open_qr();
                }
                None => app.toast("Ссылка слишком длинная для QR-кода".into()),
            }
        })
    });
    a.on_scan_qr(|| {
        if let Some(p) = service::platform() {
            p.scan_qr();
        }
    });
    a.on_copy_link(|| {
        with_app(|app| {
            if let (Some(url), Some(p)) = (app.sub().map(|s| s.url.clone()).filter(|u| !u.is_empty()), service::platform()) {
                p.copy_text(&url);
                app.toast("Ссылка на подписку скопирована".into());
            }
        })
    });
    a.on_renew(|| {
        with_app(|app| {
            let now = store::now();
            let url = servers::expiring(&app.store.subscriptions, now).and_then(|i| servers::renew_url(&app.store.subscriptions[i]));
            if let (Some(url), Some(p)) = (url, service::platform()) {
                p.open_url(&url);
            }
        })
    });
    a.on_dismiss_expiry(|| {
        with_app(|app| {
            if let Some(i) = servers::expiring(&app.store.subscriptions, store::now()) {
                app.store.subscriptions[i].expiry_dismissed = app.store.subscriptions[i].info.expire;
                app.save_store();
                app.render_expiry();
            }
        })
    });
    a.on_buy(|| {
        if let Some(p) = service::platform() {
            p.open_url(servers::BUY_URL);
        }
    });
}

impl App {
    fn ui(&self) -> Option<AppWindow> {
        self.ui.upgrade()
    }

    fn sub(&self) -> Option<&Subscription> {
        self.current.and_then(|i| self.store.subscriptions.get(i))
    }

    /// Store indexes of the subscriptions with a URL, in drawer order.
    fn url_subs(&self) -> Vec<usize> {
        self.store.subscriptions.iter().enumerate().filter(|(_, s)| !s.is_manual()).map(|(i, _)| i).collect()
    }

    fn save_store(&mut self) {
        // Pins, the theme and the last server show in the home screen widgets.
        crate::widgets::write(&self.host.data, &self.store);
        if let Err(e) = self.store.save(&self.host.store()) {
            self.toast(format!("Не удалось сохранить: {e:#}"));
        }
    }

    /// Changes the app-wide store settings and shows the result.
    fn edit_settings(&mut self, edit: impl FnOnce(&mut store::Settings)) {
        edit(&mut self.store.settings);
        self.save_store();
        self.render_settings();
    }

    /// Changes the shown subscription's own identity (its next update uses it).
    fn edit_identity(&mut self, edit: impl FnOnce(&mut identity::SubIdentity)) {
        let Some(i) = self.current else { return };
        edit(&mut self.store.subscriptions[i].identity);
        self.save_store();
        self.render_header();
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.host.settings()) {
            self.toast(format!("Не удалось сохранить: {e:#}"));
        }
    }

    fn toast(&mut self, message: String) {
        let Some(ui) = self.ui() else { return };
        ui.global::<AppState>().set_toast(message.into());
        let weak = self.ui.clone();
        self.toast_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(3500), move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<AppState>().set_toast("".into());
            }
        });
    }

    fn open(&self, url: impl Fn(&Subscription) -> Option<String>) {
        if let (Some(url), Some(p)) = (self.sub().and_then(url), service::platform()) {
            p.open_url(&url);
        }
    }

    // ── Rendering ──────────────────────────────────────────────

    fn apply_caption(&mut self) {
        let Some(ui) = self.ui() else { return };
        let scale = ui.window().scale_factor();
        let px = |v: &std::sync::atomic::AtomicI32| v.load(std::sync::atomic::Ordering::Relaxed) as f32 / scale;
        ui.global::<AppState>().set_caption_top(px(&CAPTION_TOP));
        ui.global::<AppState>().set_bars_bottom(px(&BARS_BOTTOM));
    }

    fn apply_appearance(&self) {
        let Some(ui) = self.ui() else { return };
        let s = &self.store.settings;
        themes::apply(&ui, &s.theme, s.text_scale);
        let st = ui.global::<AppState>();
        st.set_show_server_type(s.show_server_type);
        st.set_themes(themes::swatches(&s.theme));
        st.set_text_scale(s.text_scale.clamp(80, 150) as i32);
        self.render_settings();
    }

    fn render_all(&mut self) {
        self.render_subs();
        self.render_header();
        self.render_servers();
        self.render_connection();
        self.render_networks();
        self.render_status();
    }

    fn render_subs(&self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        let items: Vec<SubItem> = self
            .url_subs()
            .into_iter()
            .map(|i| {
                let s = &self.store.subscriptions[i];
                SubItem { name: s.display_name().into(), meta: servers::card_meta(s).into(), active: self.current == Some(i) }
            })
            .collect();
        st.set_subs(ModelRc::new(VecModel::from(items)));
        let manual = self.store.subscriptions.iter().find(|s| s.is_manual()).map_or(0, |s| s.servers.len());
        st.set_manual_meta(servers::plural(manual, "сервер", "сервера", "серверов").into());
        st.set_has_manual(manual > 0 || self.sub().is_some_and(Subscription::is_manual));
        st.set_manual_selected(self.sub().is_some_and(Subscription::is_manual));
    }

    fn render_header(&self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        st.set_has_sub(self.sub().is_some());
        let Some(sub) = self.sub() else { return };
        let (usage_text, fraction) = servers::usage(sub);
        st.set_header_title(sub.display_name().into());
        st.set_header_meta(servers::header_meta(sub).into());
        st.set_header_short(servers::header_short(sub).into());
        st.set_show_usage_row(!sub.is_manual());
        st.set_usage_text(usage_text.into());
        st.set_usage_fraction(fraction);
        st.set_announce(sub.info.announce.clone().unwrap_or_default().into());
        st.set_announce_hidden(sub.announce_hidden);
        st.set_has_web_page(sub.info.web_page_url.is_some());
        st.set_has_support(sub.info.support_url.is_some());
        st.set_sub_error(sub.last_error.as_deref().map(servers::humanize).unwrap_or_default().into());
        st.set_busy(self.refreshing.contains(&sub.id));
        st.set_sub_id_mode(match sub.identity.mode {
            SubIdentityMode::Settings => 0,
            SubIdentityMode::Duoray => 1,
            SubIdentityMode::Happ => 2,
        });
        st.set_sub_hwid(sub.identity.hwid.clone().into());
        st.set_sub_happ_ua(sub.identity.happ.as_ref().map(|h| format!("{}\nHWID {}", h.user_agent(), h.hwid)).unwrap_or_default().into());
        self.render_expiry();
    }

    /// The reminder before (and a week after) a subscription ends.
    fn render_expiry(&self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        let now = store::now();
        let sub = servers::expiring(&self.store.subscriptions, now).map(|i| &self.store.subscriptions[i]);
        let text = sub.and_then(|s| Some(servers::expiry_text(&s.display_name(), s.info.expire?, now)));
        st.set_expiry_text(text.unwrap_or_default().into());
        st.set_expiry_can_renew(sub.and_then(servers::renew_url).is_some());
    }

    fn render_servers(&mut self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        let Some(sub) = self.sub() else {
            self.order.clear();
            update_model(st.get_servers(), Vec::new(), |m| st.set_servers(m));
            return;
        };
        let connected = (self.conn.status == Status::On).then_some(self.conn.server.as_deref()).flatten();
        let mut order: Vec<usize> = (0..sub.servers.len()).collect();
        let favorites = &self.store.settings.favorites;
        // Pinned servers first, in the order pinned.
        let pinned = |key: &str| favorites.iter().position(|f| f == key).unwrap_or(usize::MAX);
        if self.sort_by_ping {
            // Then the connected server; unmeasured and failed go last.
            order.sort_by_key(|&i| {
                let key = servers::key_of(sub, &sub.servers[i]);
                (pinned(&key), connected != Some(key.as_str()), ping::sort_key(self.pings.get(&key)))
            });
        } else {
            order.sort_by_key(|&i| pinned(&servers::key_of(sub, &sub.servers[i])));
        }
        // The server in use while connected (a network rule may have picked
        // it), else the one the next connect will use.
        let rule = service::network_target();
        let selected = match self.conn.status {
            Status::On | Status::Connecting => self.conn.server.as_deref(),
            _ => rule.as_deref().or(self.store.settings.last_server.as_deref()),
        };
        let live = (self.conn.status == Status::On).then_some(self.conn.server.as_deref()).flatten();
        let rows: Vec<ServerRow> = order
            .iter()
            .map(|&i| {
                let s = &sub.servers[i];
                let key = servers::key_of(sub, s);
                let (flag, name) = display_name(&s.name);
                let image = flag.as_deref().and_then(flags::image);
                let (ping_text, ping_level) = match self.pings.get(&key) {
                    _ if self.measuring.contains(&key) => (String::new(), 5),
                    Some(r) => ping::label(r),
                    None => (String::new(), 0),
                };
                ServerRow {
                    has_flag: image.is_some(),
                    flag: image.unwrap_or_default(),
                    badge: flag.unwrap_or_else(|| "··".into()).into(),
                    name: name.into(),
                    description: s.description.clone().unwrap_or_else(|| format!("{}:{}", s.address, s.port)).into(),
                    protocol: s.protocol.clone().into(),
                    transport: s.network.clone().into(),
                    proxies: s.proxies as i32,
                    edited: s.override_key.is_some(),
                    ping_text: ping_text.into(),
                    ping_level,
                    selected: selected == Some(key.as_str()),
                    live: live == Some(key.as_str()),
                    pinned: favorites.contains(&key),
                    reserve: self.settings.reserve.contains(&key),
                }
            })
            .collect();
        let has_pings = sub.servers.iter().any(|s| self.pings.contains_key(&servers::key_of(sub, s)));
        self.order = order;
        update_model(st.get_servers(), rows, |m| st.set_servers(m));
        st.set_has_ping_results(has_pings);
        st.set_sort_by_ping(self.sort_by_ping);
        st.set_pinging(self.pinging);
    }

    fn server_name(&self, key: Option<&str>) -> String {
        key.and_then(|k| servers::name(k, &self.store)).unwrap_or_default()
    }

    fn render_connection(&self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        let rule = service::network_target();
        let chosen = rule.clone().or_else(|| self.store.settings.last_server.clone());
        let server = self.conn.server.clone().or_else(|| chosen.clone());
        let (state, title, detail) = match &self.conn.status {
            Status::Off => (
                "idle",
                "Не подключено".to_string(),
                match (&rule, &chosen) {
                    (Some(k), _) => format!("{}: {}", self.conn.net.describe(), self.server_name(Some(k))),
                    (None, Some(k)) => self.server_name(Some(k)),
                    (None, None) => "Выберите сервер в списке".into(),
                },
            ),
            Status::Connecting => ("connecting", "Подключение…".into(), self.server_name(server.as_deref())),
            Status::On => ("connected", "Подключено".into(), self.server_name(server.as_deref())),
            // Kill switch: the VPN holds the network closed.
            Status::Failed(e) if self.conn.tun => ("blocked", "Интернет закрыт".into(), format!("Kill switch, переподключение… {}", e.lines().next().unwrap_or_default())),
            Status::Failed(e) => ("failed", "Ошибка подключения".into(), e.clone()),
        };
        st.set_conn_state(state.into());
        st.set_conn_title(title.clone().into());
        st.set_conn_detail(detail.into());
        st.set_can_connect(chosen.is_some());
        st.set_conn_server(self.server_name(server.as_deref()).into());
        st.set_conn_node("".into());
        let connected = self.conn.status == Status::On;
        match self.exit.as_ref().filter(|_| connected) {
            Some(t) => {
                st.set_conn_exit_text(balancer::country_name(&t.loc).unwrap_or(&t.loc).into());
                st.set_conn_exit_flag(flags::image(&t.loc).unwrap_or_default());
            }
            None => st.set_conn_exit_text("".into()),
        }
        st.set_conn_traffic(self.traffic.as_ref().filter(|_| connected).map(Traffic::speeds).unwrap_or_default().into());
        let ping = server.as_deref().and_then(|k| self.pings.get(k)).filter(|_| connected).map(ping::label);
        st.set_conn_ping_text(ping.as_ref().map(|p| p.0.clone()).unwrap_or_default().into());
        st.set_conn_ping_level(ping.as_ref().map_or(0, |p| p.1));

        // The same line for the notification and the Quick Settings tile.
        let exit = self.exit.as_ref().filter(|_| connected);
        let server_name = self.server_name(server.as_deref());
        let exit_line = exit.map(|t| crate::status::exit_line(&t.loc));
        crate::status::publish(crate::status::Notice {
            state: crate::status::state_code(&self.conn.status),
            title: match &exit_line {
                Some((line, _)) => line.clone(),
                None => title,
            },
            text: match (&self.conn.status, &ping) {
                (Status::Failed(e), _) => e.lines().next().unwrap_or_default().to_string(),
                (_, Some((ms, level))) if *level != 4 => format!("{server_name} · {ms}"),
                _ => server_name,
            },
            speeds: self.traffic.as_ref().filter(|_| connected).map(Traffic::speeds).unwrap_or_default(),
            chip: exit_line.map(|(_, chip)| chip).unwrap_or_default(),
            since_ms: crate::status::since_ms(),
            key: server.clone().unwrap_or_default(),
        });
    }

    fn render_status(&self) {
        let Some(ui) = self.ui() else { return };
        let status = if !self.refreshing.is_empty() {
            format!("Обновление подписок… ({})", self.refreshing.len())
        } else if self.pinging {
            "Проверка задержки…".into()
        } else {
            String::new()
        };
        ui.global::<AppState>().set_status(status.into());
    }

    fn render_networks(&self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        let p = &self.settings.policy;
        let wifi = self.conn.net.kind == NetKind::Wifi;
        let label = |key: Option<&str>| match key {
            Some(k) => servers::name(k, &self.store).unwrap_or_else(|| "сервер удалён".into()),
            None => "Не выбран: сервер, выбранный вручную".into(),
        };
        st.set_policy_enabled(p.enabled);
        st.set_network_text(self.conn.net.describe().into());
        st.set_on_wifi(wifi);
        st.set_wifi_access(service::wifi_access() as i32);
        st.set_current_ssid(self.conn.net.ssid.clone().filter(|_| wifi).unwrap_or_default().into());
        st.set_wifi_server(label(p.wifi.as_deref()).into());
        st.set_mobile_server(label(p.mobile.as_deref()).into());
        let rules: Vec<SsidRow> = p.ssids.iter().map(|r| SsidRow { ssid: r.ssid.clone().into(), server: label(Some(&r.server)).into() }).collect();
        st.set_ssid_rules(ModelRc::new(VecModel::from(rules)));
        st.set_app_mode(self.settings.apps.mode.code());
        st.set_app_count(self.settings.apps.list().len() as i32);
        let simple = &self.store.settings.routing.simple;
        st.set_ru_direct(simple.ru_direct);
        st.set_whitelist_direct(simple.whitelist_direct);
        st.set_lan_direct(simple.lan_direct);
    }

    fn render_settings(&self) {
        let Some(ui) = self.ui() else { return };
        let st = ui.global::<AppState>();
        let p = &self.store.settings.ping;
        st.set_ping_mode(match p.mode {
            PingMode::HttpGet => 0,
            PingMode::HttpHead => 1,
            PingMode::Tcp => 2,
            PingMode::Icmp => 3,
        });
        st.set_ping_threads(self.settings.wifi_ping_threads as i32);
        st.set_mobile_ping_threads(self.settings.mobile_ping_threads as i32);
        st.set_background_stats(self.settings.background_stats);
        st.set_live_update(self.settings.live_update);
        st.set_quiet_notification(self.settings.quiet_notification);
        st.set_kill_switch(self.settings.kill_switch);
        st.set_connect_off_wifi(self.settings.connect_off_wifi);
        st.set_live_ping(self.settings.live_ping);
        st.set_disconnect_on_wifi(self.settings.disconnect_on_wifi);
        st.set_reserve_count(self.reserve_keys().len() as i32);
        st.set_failover_secs(self.settings.failover_secs as i32);
        st.set_battery_unrestricted(BATTERY_UNRESTRICTED.load(std::sync::atomic::Ordering::Relaxed));
        let share = &self.settings.share;
        st.set_tunnel_mode(self.settings.tunnel.code());
        st.set_share_enabled(share.enabled);
        st.set_share_auth(share.auth);
        st.set_share_user(share.user.as_str().into());
        st.set_share_pass(share.pass.as_str().into());
        st.set_share_port(share.port.to_string().into());
        st.set_local_network(LOCAL_NETWORK.load(std::sync::atomic::Ordering::Relaxed));
        st.set_share_addresses(if self.settings.tunnel.proxy() { crate::share::describe(share).into() } else { "".into() });
        st.set_core_label(self.core_label().into());
        st.set_ping_url(p.url.clone().into());
        st.set_ping_proxy_mode(match p.proxy_mode {
            ProxyPingMode::Default => 0,
            ProxyPingMode::Double => 1,
            ProxyPingMode::KeepAlive => 2,
        });
        st.set_ping_timeout(p.timeout_ms as i32);
        st.set_switch_on_select(self.store.settings.switch_on_select);
        let happ = &self.store.settings.happ;
        st.set_send_device_info(self.store.settings.send_device_info);
        st.set_happ_enabled(happ.enabled);
        st.set_happ_ua(happ.user_agent().into());
        st.set_happ_hwid(happ.hwid.clone().into());
        st.set_happ_os(HAPP_OS.iter().position(|o| *o == happ.os).unwrap_or(0) as i32);
        st.set_autoconnect(self.store.settings.autoconnect);
        st.set_keep_connected(self.settings.keep_connected);
        st.set_telemetry(self.store.settings.telemetry == Some(true));
        st.set_auto_update(self.store.settings.auto_update);
    }

    fn render_apps(&self) {
        let Some(ui) = self.ui() else { return };
        let filter = self.apps_filter.to_lowercase();
        let chosen = self.settings.apps.list();
        let mut apps: Vec<&AppInfo> = self
            .apps
            .iter()
            .filter(|a| filter.is_empty() || a.label.to_lowercase().contains(&filter) || a.package.contains(&filter))
            .collect();
        apps.sort_by_key(|a| (!chosen.contains(&a.package), a.label.to_lowercase()));
        let rows: Vec<AppRow> = apps
            .into_iter()
            .map(|a| {
                let icon = icon(&a.package, &a.icon);
                AppRow {
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                    label: a.label.clone().into(),
                    package: a.package.clone().into(),
                    checked: chosen.contains(&a.package),
                }
            })
            .collect();
        let st = ui.global::<AppState>();
        st.set_apps(ModelRc::new(VecModel::from(rows)));
        st.set_app_mode(self.settings.apps.mode.code());
        st.set_app_count(chosen.len() as i32);
    }

    // ── Connection ─────────────────────────────────────────────

    fn on_connection(&mut self, snapshot: Snapshot) {
        let new_link = snapshot.link != self.conn.link;
        let server_changed = snapshot.server != self.conn.server;
        if new_link {
            crate::diag::set_proxy(snapshot.link.as_ref().map(service::Link::proxy_url));
        }
        self.conn = snapshot;
        if new_link || server_changed {
            self.traffic = None;
            self.exit = None;
            self.traced = None;
            self.trace_exit();
        }
        self.render_servers();
        self.render_connection();
        self.render_networks();
        self.update_diag_context();
    }

    /// The exit country through the tunnel; a balancer that has not picked a
    /// node yet fails it, so it is asked again every few seconds until known.
    fn trace_exit(&mut self) {
        let Some(link) = self.conn.link.clone() else { return };
        if self.exit.is_some() || self.traced.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        self.traced = Some(Instant::now());
        std::thread::spawn(move || {
            let trace = exit::trace(&link);
            later(move |app| {
                if app.conn.link.as_ref() == Some(&link)
                    && let Ok(t) = trace
                {
                    app.exit = Some(t);
                    app.render_connection();
                }
            });
        });
    }

    /// Once a second while connected: the traffic counters (and the exit
    /// country until it is known).
    fn poll(&mut self) {
        self.trace_exit();
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % 5 == 0 && self.settings.live_ping {
            self.live_ping();
        }
        let Some(link) = self.conn.link.clone() else { return };
        if self.polling {
            return;
        }
        self.polling = true;
        let xray = self.host.xray.clone();
        std::thread::spawn(move || {
            let counters = exit::counters(&xray, &link);
            later(move |app| {
                app.polling = false;
                if let (Ok(c), true) = (counters, app.conn.link.as_ref() == Some(&link)) {
                    app.traffic = Some(Traffic::read(app.traffic.as_ref(), Instant::now(), c));
                    app.render_connection();
                }
            });
        });
    }

    /// The connected server's delay through the tunnel itself, every 5 s, the
    /// same request as the list's ping; its result lands in the server's row.
    /// "Держать подключение" moves on once it has been silent for
    /// MobileSettings::failover_secs.
    fn live_ping(&mut self) {
        let (Some(link), Some(key)) = (self.conn.link.clone(), self.conn.server.clone()) else { return };
        if self.live_busy {
            return;
        }
        self.live_busy = true;
        let ping = self.store.settings.ping.clone();
        std::thread::spawn(move || {
            let head = ping.mode == PingMode::HttpHead;
            let result = ping::via_socks_mode(link.socks, &link.user, &link.pass, &ping.url, head, Duration::from_secs(5), ping.proxy_mode)
                .map_err(|e| format!("{e:#}"));
            later(move |app| {
                app.live_busy = false;
                if app.conn.link.as_ref() != Some(&link) {
                    return;
                }
                let answered = result.is_ok();
                app.pings.insert(key.clone(), result);
                app.save_pings();
                if answered {
                    app.live_failures = 0;
                    app.live_answers += 1;
                    app.failover_tried.clear();
                    app.failover_quiet_until = None;
                } else {
                    app.live_failures += 1;
                }
                app.render_servers();
                app.render_connection();
                // Pings go every 5 s: the setting in seconds, in misses.
                let allowed = app.settings.failover_secs.div_ceil(5).max(1);
                let quiet = app.failover_quiet_until.is_some_and(|t| Instant::now() < t);
                if app.live_failures >= allowed && app.settings.keep_connected && !app.failover_busy && !quiet {
                    app.live_failures = 0;
                    app.failover(&key);
                }
            });
        });
    }

    /// «Закрепить вверху» on the server page: pinned servers lead the list
    /// (the desktop's favorites, in the order starred).
    fn set_server_pinned(&mut self, on: bool) {
        let Some(key) = self.editing_key() else { return };
        let favorites = &mut self.store.settings.favorites;
        favorites.retain(|k| *k != key);
        if on {
            favorites.push(key);
        }
        self.save_store();
        self.render_servers();
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_edit_pinned(on);
        }
    }

    /// «В резерве» on the server page. A server in the reserve is not
    /// excluded from switching.
    fn set_server_reserve(&mut self, on: bool) {
        let Some(key) = self.editing_key() else { return };
        self.settings.reserve.retain(|k| *k != key);
        if on {
            self.settings.no_failover.retain(|k| *k != key);
            self.settings.reserve.push(key);
        }
        self.save_settings();
        self.render_servers();
        self.render_settings();
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_edit_reserve(on);
            if on {
                ui.global::<AppState>().set_edit_no_failover(false);
            }
        }
    }

    /// «Исключить из переключения» on the server page: «Держать
    /// подключение» never moves to it (it leaves the reserve too).
    fn set_server_no_failover(&mut self, on: bool) {
        let Some(key) = self.editing_key() else { return };
        self.settings.no_failover.retain(|k| *k != key);
        if on {
            self.settings.reserve.retain(|k| *k != key);
            self.settings.no_failover.push(key);
        }
        self.save_settings();
        self.render_servers();
        self.render_settings();
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_edit_no_failover(on);
            if on {
                ui.global::<AppState>().set_edit_reserve(false);
            }
        }
    }

    fn editing_key(&self) -> Option<String> {
        let (id, index) = self.editing.clone()?;
        self.server_key(&id, index)
    }

    /// The reserve's servers that are still in a subscription.
    fn reserve_keys(&self) -> Vec<String> {
        self.settings.reserve.iter().filter(|k| servers::find(&self.store, k).is_some()).cloned().collect()
    }

    /// Where «Держать подключение» may move from the silent server `key`,
    /// best first: the reserve if there is one; else the server this network
    /// is set to (a hand pick or the rule's auto-select), then the others of
    /// its subscription by the last ping (unmeasured next; on Wi-Fi the ones
    /// made for mobile whitelists, "LTE", last). Never the excluded ones,
    /// the ones already tried or whose last ping failed.
    fn failover_candidates(&self, key: &str) -> Vec<String> {
        let usable = |k: &String| {
            k != key && !self.failover_tried.contains(k) && !self.settings.no_failover.contains(k) && !matches!(self.pings.get(k), Some(Err(_)))
        };
        let by_ping = |list: Vec<(usize, String, bool)>| {
            let mut list: Vec<_> = list.into_iter().filter(|(_, k, _)| usable(k)).collect();
            list.sort_by_key(|(i, k, last)| (*last, ping::sort_key(self.pings.get(k)), *i));
            list.into_iter().map(|(_, k, _)| k).collect::<Vec<_>>()
        };
        let reserve: Vec<String> = self.reserve_keys().into_iter().filter(|k| k != key).collect();
        if !reserve.is_empty() {
            return by_ping(reserve.into_iter().enumerate().map(|(i, k)| (i, k, false)).collect());
        }
        let mut out: Vec<String> = service::network_target().filter(|t| usable(t)).into_iter().collect();
        let Some(sub) = self.store.subscriptions.iter().find(|s| s.servers.iter().any(|x| servers::key_of(s, x) == key)) else { return out };
        let wifi = self.conn.net.kind == NetKind::Wifi;
        let rest = sub
            .servers
            .iter()
            .enumerate()
            .map(|(i, s)| (i, servers::key_of(sub, s), wifi && s.name.to_lowercase().contains("lte")))
            .collect();
        for k in by_ping(rest) {
            if !out.contains(&k) {
                out.push(k);
            }
        }
        out
    }

    /// The connected server `key` stopped answering: the candidates are
    /// checked one by one through their own xray (the last pings can be
    /// hours old) and the connection moves to the first that answers. None
    /// answering means the internet is gone, not the server: the connection
    /// stays where it is, and the next round waits a minute.
    fn failover(&mut self, key: &str) {
        self.failover_tried.insert(key.to_string());
        let from = self.server_name(Some(key));
        let in_reserve = !self.reserve_keys().iter().all(|k| k == key);
        let jobs: Vec<(String, Server)> = self
            .failover_candidates(key)
            .into_iter()
            .filter_map(|k| servers::find(&self.store, &k).map(|(_, s)| (k, s.clone())))
            .take(FAILOVER_CHECKS)
            .collect();
        if jobs.is_empty() {
            self.failover_tried.clear();
            return self.toast(format!("«{from}» не отвечает, а переключиться не на что"));
        }
        crate::diag::log(format!("keep connected: checking {} servers", jobs.len()));
        self.failover_busy = true;
        let answers = self.live_answers;
        let (host, ping, key) = (self.host.clone(), self.store.settings.ping.clone(), key.to_string());
        std::thread::spawn(move || {
            let mut found = None;
            let mut failed = Vec::new();
            for (k, server) in jobs {
                match ping::check(&server, &ping, &host) {
                    Ok(d) => {
                        found = Some((k, d));
                        break;
                    }
                    Err(e) => failed.push((k, format!("{e:#}"))),
                }
            }
            later(move |app| {
                app.failover_busy = false;
                // With nobody answering it is the internet that failed, not
                // the servers: their rows keep the pings they had.
                if let Some((k, d)) = &found {
                    for (k, e) in failed {
                        app.failover_tried.insert(k.clone());
                        app.pings.insert(k, Err(e));
                    }
                    app.pings.insert(k.clone(), Ok(*d));
                    app.save_pings();
                    app.render_servers();
                }
                // Answering again, switched or disconnected meanwhile.
                if app.live_answers != answers || app.conn.server.as_deref() != Some(key.as_str()) {
                    return;
                }
                match found {
                    Some((next, _)) => {
                        let to = app.server_name(Some(&next));
                        crate::diag::log("keep connected: switching");
                        app.store.settings.last_server = Some(next.clone());
                        app.save_store();
                        let reserve = if in_reserve { " из резерва" } else { "" };
                        app.toast(format!("«{from}» не отвечает, переключаемся на «{to}»{reserve}"));
                        service::switch_to(&next);
                    }
                    None => {
                        crate::diag::log("keep connected: no server answers, staying");
                        app.failover_tried.clear();
                        app.failover_quiet_until = Some(Instant::now() + Duration::from_secs(60));
                        app.toast(format!("«{from}» не отвечает, другие серверы тоже: похоже, пропал интернет. Остаёмся на «{from}»"));
                    }
                }
            });
        });
    }

    fn save_pings(&self) {
        let live: HashSet<String> = self.store.subscriptions.iter().flat_map(|sub| sub.servers.iter().map(|s| servers::key_of(sub, s))).collect();
        let saved: std::collections::BTreeMap<&String, Option<u64>> = self
            .pings
            .iter()
            .filter(|(k, _)| live.contains(*k))
            .map(|(k, r)| (k, r.as_ref().ok().map(|d| d.as_millis() as u64)))
            .collect();
        let path = self.host.data.join("pings.json");
        let tmp = path.with_extension("json.tmp");
        if serde_json::to_vec(&saved).is_ok_and(|b| std::fs::write(&tmp, b).is_ok()) {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// The fastest answering server of the shown subscription by the last
    /// ping: selected and connected to (switched to, while connected).
    fn connect_fastest(&mut self) {
        let Some(sub) = self.sub() else { return };
        let best = sub
            .servers
            .iter()
            .filter_map(|s| {
                let key = servers::key_of(sub, s);
                let d = self.pings.get(&key)?.as_ref().ok()?;
                Some((d.as_millis(), key, display_name(&s.name).1))
            })
            .min_by_key(|(ms, ..)| *ms);
        let Some((ms, key, name)) = best else {
            return self.toast("Ни один сервер подписки не ответил. Сначала проверьте задержку".into());
        };
        self.store.settings.last_server = Some(key.clone());
        self.save_store();
        self.render_servers();
        self.toast(format!("Самый быстрый: «{name}», {ms} мс"));
        match self.conn.status {
            Status::On | Status::Connecting => service::switch_to(&key),
            Status::Off | Status::Failed(_) => service::connect(Some(key)),
        }
    }

    fn take_link(&mut self) {
        let Some(url) = PENDING_LINK.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_add_prefill(url.into());
            ui.invoke_open_add();
        }
    }

    /// Opens the editor on the row's server.
    fn open_server(&mut self, row: usize) {
        let (Some(sub), Some(&index)) = (self.sub(), self.order.get(row)) else { return };
        let Some(ui) = self.ui() else { return };
        let s = &sub.servers[index];
        let st = ui.global::<AppState>();
        st.set_edit_name(display_name(&s.name).1.into());
        st.set_edit_meta(format!("{} · {}:{} · {}", s.protocol, s.address, s.port, s.network).into());
        st.set_edit_json(serde_json::to_string_pretty(&s.config_json()).unwrap_or_default().into());
        st.set_edit_error("".into());
        st.set_edit_edited(s.override_key.is_some());
        let key = servers::key_of(sub, s);
        st.set_edit_pinned(self.store.settings.favorites.contains(&key));
        st.set_edit_reserve(self.settings.reserve.contains(&key));
        st.set_edit_no_failover(self.settings.no_failover.contains(&key));
        self.editing = Some((sub.id.clone(), index));
        ui.invoke_open_server_page();
    }

    fn save_server(&mut self, text: &str) {
        let Some((id, index)) = self.editing.clone() else { return };
        let Some(ui) = self.ui() else { return };
        if text == ui.global::<AppState>().get_edit_json().as_str() {
            ui.invoke_close_server_page();
            return;
        }
        let mut edited = match Server::from_json_text(text) {
            Ok(s) => s,
            Err(e) => {
                ui.global::<AppState>().set_edit_error(format!("Строка {}, столбец {}: {}", e.line, e.column, e.message).into());
                return;
            }
        };
        let Some(old_key) = self.server_key(&id, index) else { return };
        if edited.name.trim().is_empty() {
            edited.name = self.store.get(&id).map(|sub| sub.servers[index].name.clone()).unwrap_or_default();
        }
        if self.store.edit_server(&id, index, edited) {
            self.server_changed(&id, index, old_key, "Сервер изменён");
        }
    }

    fn reset_server(&mut self) {
        let Some((id, index)) = self.editing.clone() else { return };
        let Some(old_key) = self.server_key(&id, index) else { return };
        if self.store.reset_server(&id, index) {
            self.server_changed(&id, index, old_key, "Вернули версию подписки");
        }
    }

    fn server_key(&self, id: &str, index: usize) -> Option<String> {
        let sub = self.store.get(id)?;
        sub.servers.get(index).map(|s| servers::key_of(sub, s))
    }

    /// After an edit or a reset: the remembered server follows the edited one
    /// (its key may have changed), its stale ping goes, a live tunnel restarts.
    fn server_changed(&mut self, id: &str, index: usize, old_key: String, msg: &str) {
        let Some(new_key) = self.server_key(id, index) else { return };
        if self.store.settings.last_server.as_deref() == Some(old_key.as_str()) {
            self.store.settings.last_server = Some(new_key.clone());
        }
        // Pinned, in the reserve and excluded under its new key.
        for k in self.store.settings.favorites.iter_mut().chain(self.settings.reserve.iter_mut()).chain(self.settings.no_failover.iter_mut()) {
            if *k == old_key {
                k.clone_from(&new_key);
            }
        }
        self.save_settings();
        self.pings.remove(&old_key);
        self.editing = None;
        self.save_store();
        self.render_all();
        if let Some(ui) = self.ui() {
            ui.invoke_close_server_page();
        }
        self.toast(msg.into());
        if self.conn.server.as_deref() == Some(old_key.as_str()) {
            if new_key == old_key {
                service::reconnect();
            } else {
                service::switch_to(&new_key);
            }
        }
    }

    fn toggle(&mut self) {
        match self.conn.status {
            // Kill switch holding the network: the button lets it go.
            Status::Failed(_) if self.conn.tun => service::disconnect(),
            Status::Off | Status::Failed(_) => {
                // Connecting: the check has done its job.
                if self.pinging {
                    self.stop_ping();
                }
                service::connect(self.store.settings.last_server.clone())
            }
            Status::Connecting | Status::On => service::disconnect(),
        }
    }

    // ── Subscriptions and servers ──────────────────────────────

    fn select_sub(&mut self, i: i32) {
        let index = if i < 0 {
            Some(self.store.ensure_manual_group())
        } else {
            self.url_subs().get(i as usize).copied()
        };
        if index.is_none() || index == self.current {
            return;
        }
        self.current = index;
        self.render_subs();
        self.render_header();
        self.render_servers();
    }

    fn row_key(&self, row: usize) -> Option<String> {
        let sub = self.sub()?;
        let &i = self.order.get(row)?;
        sub.servers.get(i).map(|s| servers::key_of(sub, s))
    }

    fn select_server(&mut self, row: usize) {
        let Some(key) = self.row_key(row) else { return };
        self.store.settings.last_server = Some(key.clone());
        self.save_store();
        if service::pick(&key) {
            self.toast(format!("Выбран вручную. Правило для сети «{}» вернётся при смене сети", self.conn.net.describe()));
        }
        self.render_servers();
        self.render_connection();
        if self.store.settings.switch_on_select {
            service::switch_if_connected(&key);
        }
    }

    fn remove_server(&mut self, row: usize) {
        let (Some(sub), Some(&index)) = (self.sub(), self.order.get(row)) else { return };
        let id = sub.id.clone();
        if self.store.remove_manual_server(&id, index) {
            self.save_store();
            self.forget_dead_rules();
            self.render_all();
        }
    }

    fn refresh(&mut self, id: String) {
        let Some(sub) = self.store.get(&id) else { return };
        if sub.is_manual() || !self.refreshing.insert(id.clone()) {
            return;
        }
        let (url, fallback) = (sub.url.clone(), sub.info.fallback_url.clone());
        let settings = &self.store.settings;
        let headers = mobile_headers(identity::subscription_headers(&settings.happ, &self.host.device(), settings.send_device_info, &sub.identity));
        // Connected: through the tunnel first (the direct path may be what
        // is blocked, as with mobile whitelists), then directly.
        let proxy = self.conn.link.as_ref().map(service::Link::proxy_url);
        self.render_header();
        self.render_status();
        std::thread::spawn(move || {
            let direct = || subscription::fetch(&url, fallback.as_deref(), &headers);
            let result = match proxy.as_deref() {
                Some(proxy) => subscription::fetch_with_proxy(&url, fallback.as_deref(), &headers, Some(proxy)).or_else(|_| direct()),
                None => direct(),
            }
            .map_err(|e| format!("{e:#}"));
            later(move |app| {
                app.refreshing.remove(&id);
                if let Err(e) = &result {
                    crate::diag::auto_online("subscription", e);
                }
                app.store.apply(&id, result);
                app.save_store();
                app.forget_dead_rules();
                app.render_all();
            });
        });
    }

    /// Subscriptions whose update interval has passed.
    fn refresh_stale(&mut self) {
        let now = store::now();
        let stale: Vec<String> = self
            .store
            .subscriptions
            .iter()
            .filter(|s| !s.is_manual() && s.updated_at.is_none_or(|t| now.saturating_sub(t) >= servers::update_interval_secs(s)))
            .map(|s| s.id.clone())
            .collect();
        for id in stale {
            self.refresh(id);
        }
    }

    /// The ping button: starts the check, or stops one under way.
    fn ping_current(&mut self) {
        if self.pinging {
            return self.stop_ping();
        }
        let Some(sub) = self.sub() else { return };
        let jobs: Vec<(String, Server)> = sub.servers.iter().map(|s| (servers::key_of(sub, s), s.clone())).collect();
        self.ping(jobs);
    }

    /// A held row: ping just that server, with a tick of the vibration motor.
    fn ping_row(&mut self, row: usize) {
        if let Some(p) = service::platform() {
            p.haptic();
        }
        let (Some(sub), Some(&i)) = (self.sub(), self.order.get(row)) else { return };
        let job = (servers::key_of(sub, &sub.servers[i]), sub.servers[i].clone());
        self.ping(vec![job]);
    }

    /// Results come in bursts: the list redraws at most every 150 ms.
    fn render_servers_soon(&mut self) {
        if self.render_pending {
            return;
        }
        self.render_pending = true;
        slint::Timer::single_shot(Duration::from_millis(150), || {
            with_app(|app| {
                app.render_pending = false;
                app.render_servers();
            })
        });
    }

    /// Stops a running check: servers already being pinged finish, the rest
    /// keep their last result.
    fn stop_ping(&mut self) {
        if let Some(cancel) = self.ping_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.pinging = false;
        self.measuring.clear();
        self.render_servers();
        self.render_status();
    }

    fn ping(&mut self, jobs: Vec<(String, Server)>) {
        if self.pinging {
            return;
        }
        if jobs.is_empty() {
            return;
        }
        // Rows get their spinner when a worker takes them (at most a few at
        // once): 78 spinners turning together made scrolling stutter.
        self.pinging = true;
        self.render_servers();
        self.render_status();
        let mut settings = self.store.settings.ping.clone();
        settings.threads = if self.conn.net.kind == NetKind::Mobile {
            self.settings.mobile_ping_threads
        } else {
            self.settings.wifi_ping_threads
        };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.ping_cancel = Some(cancel.clone());
        ping::run_all(
            jobs,
            settings,
            self.host.clone(),
            cancel.clone(),
            |key| {
                later(move |app| {
                    if app.pinging {
                        app.measuring.insert(key);
                        app.render_servers_soon();
                    }
                })
            },
            |key, result| {
                later(move |app| {
                    app.measuring.remove(&key);
                    app.pings.insert(key, result);
                    app.render_servers_soon();
                })
            },
            move || {
                later(move |app| {
                    // A check stopped and another started since: not ours.
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) && app.pinging {
                        return;
                    }
                    app.ping_cancel = None;
                    app.pinging = false;
                    app.measuring.clear();
                    app.save_pings();
                    app.render_servers();
                    app.render_connection();
                    app.render_status();
                })
            },
        );
    }

    fn add(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if store::looks_like_links(text) {
            return self.add_links(text);
        }
        match self.store.add(text, "") {
            Ok(id) => {
                self.save_store();
                self.show(&id);
                self.refresh(id);
            }
            Err(e) => self.toast(servers::humanize(&format!("{e:#}"))),
        }
    }

    fn add_links(&mut self, text: &str) {
        match self.store.add_links(text.trim()) {
            Ok((id, added)) => {
                self.save_store();
                self.show(&id);
                self.toast(match added {
                    0 => "Эти серверы уже добавлены".into(),
                    n => format!("Добавлено: {}", servers::plural(n, "сервер", "сервера", "серверов")),
                });
            }
            Err(e) => self.toast(format!("Не удалось добавить: {e:#}")),
        }
    }

    /// Puts subscription `id` on screen.
    fn show(&mut self, id: &str) {
        self.current = self.store.subscriptions.iter().position(|s| s.id == id);
        self.render_all();
    }

    fn remove_current(&mut self) {
        let Some(id) = self.sub().filter(|s| !s.is_manual()).map(|s| s.id.clone()) else { return };
        self.store.remove(&id);
        if self.store.settings.last_server.as_deref().is_some_and(|k| servers::find(&self.store, k).is_none()) {
            self.store.settings.last_server = None;
        }
        self.current = self.url_subs().first().copied().or_else(|| (!self.store.subscriptions.is_empty()).then_some(0));
        self.save_store();
        self.forget_dead_rules();
        self.render_all();
    }

    // ── Routing ────────────────────────────────────────────────

    fn forget_dead_rules(&mut self) {
        let store = &self.store;
        let mut policy = self.settings.policy.clone();
        policy.forget_missing(|k| servers::find(store, k).is_some());
        if policy != self.settings.policy {
            self.edit_policy(|p| *p = policy);
        }
    }

    fn edit_policy(&mut self, edit: impl FnOnce(&mut NetworkPolicy)) {
        edit(&mut self.settings.policy);
        self.save_settings();
        service::set_policy(self.settings.policy.clone());
        self.render_networks();
        self.render_connection();
    }

    fn pick_for(&mut self, target: i32, row: i32) {
        let key = usize::try_from(row).ok().and_then(|r| self.row_key(r));
        let ssid = self.conn.net.ssid.clone();
        self.edit_policy(|p| match target {
            0 => p.wifi = key,
            1 => p.mobile = key,
            _ => {
                if let (Some(ssid), Some(key)) = (ssid, key) {
                    p.set_ssid(&ssid, key);
                }
            }
        });
    }

    fn load_apps(&mut self) {
        let Some(ui) = self.ui() else { return };
        if !self.apps.is_empty() {
            return self.render_apps();
        }
        ui.global::<AppState>().set_apps_loading(true);
        let Some(platform) = service::platform() else { return };
        std::thread::spawn(move || {
            let apps = platform.list_apps();
            later(move |app| {
                app.apps = apps;
                if let Some(ui) = app.ui() {
                    ui.global::<AppState>().set_apps_loading(false);
                }
                app.render_apps();
            });
        });
    }

    /// «Режим работы»: VPN, VPN with the proxy, or the proxy alone. A live
    /// connection moves to the new mode.
    fn set_tunnel_mode(&mut self, mode: crate::settings::TunnelMode) {
        let was = self.settings.tunnel;
        if was == mode {
            return;
        }
        self.settings.tunnel = mode;
        if self.settings.share.pass.is_empty() {
            self.settings.share.pass = crate::settings::new_password();
        }
        // Saved first: the service reads mobile.json for the TUN and its routes.
        self.save_settings();
        self.render_settings();
        service::set_tunnel_mode(was.vpn() != mode.vpn());
    }

    /// «Доступ через хотспот» changed: a live connection restarts with the
    /// new listeners. A password is made the first time it is needed.
    fn edit_share(&mut self, edit: impl FnOnce(&mut crate::settings::ShareProxy)) {
        let was = self.settings.share.enabled;
        edit(&mut self.settings.share);
        if self.settings.share.pass.is_empty() {
            self.settings.share.pass = crate::settings::new_password();
        }
        // Saved first: the TUN reads mobile.json for its routes.
        self.save_settings();
        self.render_settings();
        if was != self.settings.share.enabled {
            service::rebuild_tun();
        }
        if self.settings.share.enabled
            && let Some(p) = service::platform()
        {
            p.request_local_network();
        }
        if matches!(self.conn.status, Status::On | Status::Connecting) {
            service::reconnect();
        }
    }

    fn edit_apps(&mut self, edit: impl FnOnce(&mut crate::settings::AppRules)) {
        edit(&mut self.settings.apps);
        self.save_settings();
        service::set_apps(self.settings.apps.clone());
        self.render_apps();
        self.render_networks();
    }

    fn load_log(&mut self) {
        let Some(ui) = self.ui() else { return };
        let tail = tunnel::log_tail(&self.host.xray_log(), 80);
        ui.global::<AppState>().set_xray_log(if tail.is_empty() { "Журнал пуст".into() } else { tail.into() });
    }
}

/// A subscription from a `duoray://` link, waiting for the window.
static PENDING_LINK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Opens the add sheet with the subscription from a `duoray://` link, now or
/// as soon as the window is up (the link may come before it).
pub fn open_link(link: &str) {
    let Some(url) = crate::deeplink::subscription(link) else { return };
    prefill(url);
}

/// What the QR scanner read: a duoray:// link, a subscription URL or keys,
/// into the add sheet.
pub fn scanned(text: &str) {
    let text = text.trim();
    prefill(crate::deeplink::subscription(text).unwrap_or_else(|| text.to_string()));
}

fn prefill(text: String) {
    *PENDING_LINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
    let _ = slint::invoke_from_event_loop(|| with_app(App::take_link));
}

/// The panel sees this app's version: duoray-core names its own (the
/// desktop's). A User-Agent typed by hand or Happ's stays as it is.
const USER_AGENT: &str = concat!("Duoray/", env!("CARGO_PKG_VERSION"));

fn mobile_headers(mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
    for (key, value) in &mut headers {
        if key == "User-Agent" && value == identity::DUORAY_USER_AGENT {
            *value = USER_AGENT.to_string();
        }
    }
    headers
}

/// Platforms Happ Spoof can claim, in the picker's order.
const HAPP_OS: [&str; 4] = ["Android", "iOS", "Windows", "macOS"];

/// Last ping results, kept in pings.json so they show after a restart
/// (desktop format: server key -> milliseconds, or null for no answer).
fn load_pings(host: &Host) -> HashMap<String, PingResult> {
    let Ok(text) = std::fs::read_to_string(host.data.join("pings.json")) else { return HashMap::new() };
    let saved: HashMap<String, Option<u64>> = serde_json::from_str(&text).unwrap_or_default();
    saved.into_iter().map(|(k, ms)| (k, ms.map(Duration::from_millis).ok_or_else(|| "нет ответа".to_string()))).collect()
}

/// An app icon from the PNG the platform saved, decoded once.
fn icon(package: &str, path: &str) -> Option<slint::Image> {
    if path.is_empty() {
        return None;
    }
    ICONS.with(|cache| {
        cache
            .borrow_mut()
            .entry(package.to_string())
            .or_insert_with(|| slint::Image::load_from_path(std::path::Path::new(path)).ok())
            .clone()
    })
}

/// Puts `rows` into the list model in place, touching only rows that
/// changed: the list keeps its scroll position through pings and updates.
fn update_model<T: Clone + PartialEq + 'static>(model: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    let Some(vec) = model.as_any().downcast_ref::<VecModel<T>>() else {
        return set(ModelRc::new(VecModel::from(rows)));
    };
    if vec.row_count() != rows.len() {
        return vec.set_vec(rows);
    }
    for (i, row) in rows.into_iter().enumerate() {
        if vec.row_data(i).as_ref() != Some(&row) {
            vec.set_row_data(i, row);
        }
    }
}
