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
use duoray_core::identity;
use duoray_core::server::{Server, display_name};
use duoray_core::store::{self, PingMode, ProxyPingMode, Store, Subscription};
use duoray_core::subscription;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::exit::{self, Traffic};
use crate::netpolicy::{NetKind, NetworkPolicy};
use crate::platform::AppInfo;
use crate::service::{self, Host, Snapshot, Status};
use crate::settings::{AppMode, MobileSettings};
use crate::{Actions, AppRow, AppState, AppWindow, ServerRow, SsidRow, SubItem, flags, ping, servers, themes, tunnel};

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
    ticks: u32,
    refreshing: HashSet<String>,
    conn: Snapshot,
    traffic: Option<Traffic>,
    exit: Option<Trace>,
    /// When the exit country was last asked for (retried until known).
    traced: Option<Instant>,
    polling: bool,
    apps: Vec<AppInfo>,
    apps_filter: String,
    toast_timer: slint::Timer,
    poll_timer: slint::Timer,
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

/// Runs `f` on the UI thread with the app, from any thread.
fn later(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(f));
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
        ticks: 0,
        refreshing: HashSet::new(),
        conn: service::snapshot(),
        traffic: None,
        exit: None,
        traced: None,
        polling: false,
        apps: Vec::new(),
        apps_filter: String::new(),
        toast_timer: slint::Timer::default(),
        poll_timer: slint::Timer::default(),
    };
    APP.with(|a| *a.borrow_mut() = Some(Rc::new(RefCell::new(app))));
    wire(&ui);
    ui.global::<AppState>().set_version(env!("CARGO_PKG_VERSION").into());
    with_app(|app| {
        app.apply_appearance();
        app.render_all();
        app.refresh_stale();
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
    let result = ui.run();
    service::set_listener(None);
    APP.with(|a| *a.borrow_mut() = None);
    result
}

fn wire(ui: &AppWindow) {
    let a = ui.global::<Actions>();
    a.on_select_sub(|i| with_app(|app| app.select_sub(i)));
    a.on_select_server(|row| with_app(|app| app.select_server(row as usize)));
    a.on_remove_server(|row| with_app(|app| app.remove_server(row as usize)));
    a.on_toggle_connection(|| with_app(App::toggle));
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
            app.edit_apps(|r| match r.packages.iter().position(|p| *p == package) {
                Some(i) => drop(r.packages.remove(i)),
                None => r.packages.push(package),
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
            app.store.settings.ping.threads = v.clamp(1, 32) as u32;
            app.save_store();
            app.render_settings();
        })
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
            app.store.settings.keep_connected = on;
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
        })
    });
    a.on_connect_fastest(|| with_app(App::connect_fastest));
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
        if let Err(e) = self.store.save(&self.host.store()) {
            self.toast(format!("Не удалось сохранить: {e:#}"));
        }
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
        if self.sort_by_ping {
            // The connected server stays on top; unmeasured and failed go last.
            order.sort_by_key(|&i| {
                let key = servers::key_of(sub, &sub.servers[i]);
                (connected != Some(key.as_str()), ping::sort_key(self.pings.get(&key)))
            });
        }
        let selected = self.store.settings.last_server.as_deref();
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
        let rule = self.settings.policy.server_for(&self.conn.net).map(String::from);
        let chosen = rule.clone().or_else(|| self.store.settings.last_server.clone());
        let server = self.conn.server.clone().or_else(|| chosen.clone());
        let (state, title, detail) = match &self.conn.status {
            Status::Off => (
                "idle",
                "Не подключено".to_string(),
                match &rule {
                    Some(k) => format!("{}: {}", self.conn.net.describe(), self.server_name(Some(k))),
                    None => self.server_name(chosen.as_deref()),
                },
            ),
            Status::Connecting => ("connecting", "Подключение…".into(), self.server_name(server.as_deref())),
            Status::On => ("connected", "Подключено".into(), self.server_name(server.as_deref())),
            Status::Failed(e) => ("failed", "Ошибка подключения".into(), e.clone()),
        };
        st.set_conn_state(state.into());
        st.set_conn_title(title.into());
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
        st.set_conn_ping_level(ping.map_or(0, |p| p.1));
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
        st.set_current_ssid(self.conn.net.ssid.clone().filter(|_| wifi).unwrap_or_default().into());
        st.set_wifi_server(label(p.wifi.as_deref()).into());
        st.set_mobile_server(label(p.mobile.as_deref()).into());
        let rules: Vec<SsidRow> = p.ssids.iter().map(|r| SsidRow { ssid: r.ssid.clone().into(), server: label(Some(&r.server)).into() }).collect();
        st.set_ssid_rules(ModelRc::new(VecModel::from(rules)));
        st.set_app_mode(self.settings.apps.mode.code());
        st.set_app_count(self.settings.apps.packages.len() as i32);
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
        st.set_ping_threads(p.threads as i32);
        st.set_ping_url(p.url.clone().into());
        st.set_ping_proxy_mode(match p.proxy_mode {
            ProxyPingMode::Default => 0,
            ProxyPingMode::Double => 1,
            ProxyPingMode::KeepAlive => 2,
        });
        st.set_ping_timeout(p.timeout_ms as i32);
        st.set_switch_on_select(self.store.settings.switch_on_select);
        st.set_autoconnect(self.store.settings.autoconnect);
        st.set_keep_connected(self.store.settings.keep_connected);
    }

    fn render_apps(&self) {
        let Some(ui) = self.ui() else { return };
        let filter = self.apps_filter.to_lowercase();
        let chosen = &self.settings.apps.packages;
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
        if self.ticks % 5 == 0 {
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
    /// "Держать подключение" moves on after LIVE_FAILURES misses in a row.
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
                    app.failover_tried.clear();
                } else {
                    app.live_failures += 1;
                }
                app.render_servers();
                app.render_connection();
                if app.live_failures >= LIVE_FAILURES && app.store.settings.keep_connected {
                    app.live_failures = 0;
                    app.failover(&key);
                }
            });
        });
    }

    /// Moves the connection from the silent server `key` to the fastest other
    /// server of its subscription by the last ping (unmeasured next, failed
    /// and already tried ones never), as the desktop app does.
    fn failover(&mut self, key: &str) {
        self.failover_tried.insert(key.to_string());
        let Some(sub) = self.store.subscriptions.iter().find(|s| s.servers.iter().any(|x| servers::key_of(s, x) == key)) else { return };
        let pick = sub
            .servers
            .iter()
            .enumerate()
            .map(|(i, s)| (i, servers::key_of(sub, s)))
            .filter(|(_, k)| k != key && !self.failover_tried.contains(k) && !matches!(self.pings.get(k), Some(Err(_))))
            .min_by_key(|(i, k)| (ping::sort_key(self.pings.get(k)), *i));
        let from = self.server_name(Some(key));
        match pick {
            None => {
                self.failover_tried.clear();
                self.toast("Серверы подписки не отвечают. Проверьте интернет".into());
            }
            Some((i, next)) => {
                let to = display_name(&sub.servers[i].name).1;
                self.store.settings.last_server = Some(next.clone());
                self.save_store();
                self.toast(format!("«{from}» не отвечает, переключаемся на «{to}»"));
                service::switch_to(&next);
            }
        }
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

    fn toggle(&mut self) {
        match self.conn.status {
            Status::Off | Status::Failed(_) => service::connect(self.store.settings.last_server.clone()),
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
        let headers = identity::subscription_headers(&settings.happ, &self.host.device(), settings.send_device_info, &sub.identity);
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
                let ok = result.is_ok();
                app.store.apply(&id, result);
                app.save_store();
                app.forget_dead_rules();
                let _ = ok;
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

    fn ping_current(&mut self) {
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

    fn ping(&mut self, jobs: Vec<(String, Server)>) {
        if self.pinging {
            return;
        }
        if jobs.is_empty() {
            return;
        }
        self.measuring.extend(jobs.iter().map(|(k, _)| k.clone()));
        self.pinging = true;
        self.render_servers();
        self.render_status();
        ping::run_all(
            jobs,
            self.store.settings.ping.clone(),
            self.host.clone(),
            |key, result| {
                later(move |app| {
                    app.measuring.remove(&key);
                    app.pings.insert(key, result);
                    app.render_servers();
                })
            },
            || {
                later(|app| {
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

/// Live pings a connected server may miss before "Держать подключение"
/// moves away from it (15 s).
const LIVE_FAILURES: u32 = 3;

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
