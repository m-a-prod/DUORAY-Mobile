//! The connection, apart from the UI: Android keeps the VPN service (and
//! this process) when the window is closed, and the connection still has to
//! follow network changes with no window at all.
//!
//! Flow: [`connect`] writes xray's config, asks the platform for the VPN,
//! the platform opens the TUN and reports it with [`on_tun_up`], and xray
//! starts on that descriptor. Switching servers (by hand or because the
//! network changed) restarts xray on the same TUN.

use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use duoray_core::balancer::{self, Balancer};
use duoray_core::device::Device;
use duoray_core::geo::GeoDir;
use duoray_core::routing::{self, GeoData};
use duoray_core::runtime;
use duoray_core::store::Store;
use serde_json::{Value, json};

use crate::netpolicy::{NetworkEnv, NetworkPolicy};
use crate::platform::Platform;
use crate::servers;
use crate::settings::{AppRules, MobileSettings};
use crate::tunnel::{self, Xray};

/// MTU of the TUN, on both sides: the platform's interface and xray's stack.
pub const TUN_MTU: u32 = 1500;

/// What the platform shell knows about the app and the device.
#[derive(Debug, Clone)]
pub struct Host {
    /// App-private storage.
    pub data: PathBuf,
    /// The xray executable.
    pub xray: PathBuf,
    /// geoip.dat / geosite.dat.
    pub assets: PathBuf,
    /// Stable per install (Android's ANDROID_ID); hashed before it is sent.
    pub device_id: String,
    pub os: &'static str,
    pub os_version: String,
    pub model: String,
}

impl Host {
    pub fn store(&self) -> PathBuf {
        self.data.join("store.json")
    }

    pub fn settings(&self) -> PathBuf {
        self.data.join("mobile.json")
    }

    pub fn xray_log(&self) -> PathBuf {
        self.run_dir().join("xray.log")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.data.join("run")
    }

    /// Routing databases kept fresh in `<data>/geo` (as on the desktop).
    pub fn geo(&self) -> GeoDir {
        GeoDir::new(&self.data)
    }

    /// geoip/geosite for xray: the updated ones once they exist, else the
    /// ones shipped in the APK.
    pub fn geo_assets(&self) -> PathBuf {
        self.geo().assets().map_or_else(|| self.assets.clone(), Path::to_path_buf)
    }

    fn paths(&self) -> tunnel::Paths {
        let run = self.run_dir();
        tunnel::Paths {
            xray: self.xray.clone(),
            assets: self.geo_assets(),
            config: run.join("xray.json"),
            log: run.join("xray.log"),
            pid: run.join("xray.pid"),
        }
    }

    /// Who the app says it is to subscription panels (x-hwid & co.).
    pub fn device(&self) -> Device {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let half = |salt: &str| {
            let mut h = DefaultHasher::new();
            ("duoray-hwid", salt, &self.device_id).hash(&mut h);
            h.finish()
        };
        let (a, b) = (half("a"), half("b"));
        Device {
            hwid: format!(
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                a >> 32,
                (a >> 16) & 0xffff,
                a & 0xffff,
                b >> 48,
                b & 0xffff_ffff_ffff
            ),
            os: self.os,
            os_version: Some(self.os_version.clone()).filter(|v| !v.is_empty()),
            model: Some(self.model.clone()).filter(|v| !v.is_empty()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Off,
    Connecting,
    On,
    Failed(String),
}

/// How the app itself reaches a running xray: its SOCKS inbound (the app is
/// outside its own VPN) and its API.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub socks: SocketAddr,
    pub user: String,
    pub pass: String,
    pub api: SocketAddr,
    /// The balancer that carries the traffic, if the config has one.
    pub balancer: Option<Balancer>,
}

impl Link {
    /// For ureq: socks5h, so names resolve on the far side.
    pub fn proxy_url(&self) -> String {
        format!("socks5h://{}:{}@{}", self.user, self.pass, self.socks)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub status: Status,
    /// Key of the server in use (or being connected to).
    pub server: Option<String>,
    pub net: NetworkEnv,
    /// Set while connected.
    pub link: Option<Link>,
}

pub type Listener = Box<dyn Fn(Snapshot) + Send>;

#[derive(Default)]
struct Service {
    host: Option<Arc<Host>>,
    platform: Option<Arc<dyn Platform>>,
    listener: Option<Listener>,
    policy: NetworkPolicy,
    apps: AppRules,
    /// The running (or starting) xray's link.
    link: Option<Link>,
    status: Status,
    server: Option<String>,
    net: NetworkEnv,
    tun: Option<RawFd>,
    xray: Option<Xray>,
    /// Bumped by every connect and disconnect: late results of an older
    /// attempt are dropped.
    attempt: u64,
}

static SERVICE: LazyLock<Mutex<Service>> = LazyLock::new(Default::default);

fn lock() -> MutexGuard<'static, Service> {
    SERVICE.lock().unwrap_or_else(|e| e.into_inner())
}

impl Service {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            status: self.status.clone(),
            server: self.server.clone(),
            net: self.net.clone(),
            link: (self.status == Status::On).then(|| self.link.clone()).flatten(),
        }
    }

    fn notify(&self) {
        crate::status::track(&self.status);
        if let Some(listener) = &self.listener {
            listener(self.snapshot());
        }
        // No window to publish the full line: the shade gets a plain one.
        if !crate::status::window_open() {
            crate::status::background(self.snapshot(), self.host.clone());
        }
    }

    fn active(&self) -> bool {
        matches!(self.status, Status::On | Status::Connecting)
    }

    /// The server the current network asks for, when that is not the one in use.
    fn wanted_switch(&self) -> Option<String> {
        let target = self.policy.server_for(&self.net)?;
        (self.active() && self.tun.is_some() && self.server.as_deref() != Some(target)).then(|| target.to_string())
    }

    /// Records the failure, reports it (`kind`, with consent) and stops
    /// xray; the caller then closes the VPN.
    fn fail(&mut self, kind: &str, message: String) -> Option<Arc<dyn Platform>> {
        log::warn!("connection failed: {message}");
        crate::diag::auto(kind, &message);
        self.attempt += 1;
        self.status = Status::Failed(message);
        if let Some(x) = self.xray.take() {
            x.stop();
        }
        self.notify();
        self.tun.is_some().then(|| self.platform.clone()).flatten()
    }
}

/// Called by the shell before anything else. Only the first call in a
/// process cleans up after a previous one; later calls (a re-created
/// window) just refresh what the shell reports.
pub fn init(host: Host) {
    let mut s = lock();
    if s.host.is_none() {
        crate::diag::init(&host.data, &format!("{} {}", host.os, host.os_version), &host.model);
        let paths = host.paths();
        let _ = std::fs::create_dir_all(host.run_dir());
        tunnel::kill_orphan(&paths.pid);
        let settings = MobileSettings::load(&host.settings());
        crate::status::set_background_stats(settings.background_stats);
        s.policy = settings.policy;
        s.apps = settings.apps;
        // Daily geoip (ru, ru-whitelist), weekly whitelist and game lists.
        let (geo, bundled) = (host.geo(), host.assets.clone());
        std::thread::spawn(move || {
            if let Err(e) = geo.update(Some(&bundled), false) {
                log::warn!("geo update: {e:#}");
            }
        });
    }
    s.host = Some(Arc::new(host));
}

pub fn host() -> Option<Arc<Host>> {
    lock().host.clone()
}

pub fn set_platform(platform: Arc<dyn Platform>) {
    lock().platform = Some(platform);
}

pub fn platform() -> Option<Arc<dyn Platform>> {
    lock().platform.clone()
}

/// The UI's view of the connection; `None` when the window goes away.
pub fn set_listener(listener: Option<Listener>) {
    let mut s = lock();
    s.listener = listener;
    s.notify();
}

pub fn snapshot() -> Snapshot {
    lock().snapshot()
}

pub fn set_policy(policy: NetworkPolicy) {
    let switch = {
        let mut s = lock();
        s.policy = policy;
        s.wanted_switch()
    };
    if let Some(key) = switch {
        start(key);
    }
}

/// New per-app rules; an open VPN is rebuilt so they apply now.
pub fn set_apps(apps: AppRules) {
    let platform = {
        let mut s = lock();
        if s.apps == apps {
            return;
        }
        s.apps = apps;
        s.tun.is_some().then(|| s.platform.clone()).flatten()
    };
    if let Some(p) = platform {
        p.rebuild_vpn();
    }
}

/// Connects to the server the current network asks for, else to `chosen`.
pub fn connect(chosen: Option<String>) {
    let key = {
        let s = lock();
        s.policy.server_for(&s.net).map(String::from).or(chosen)
    };
    match key {
        Some(key) => start(key),
        None => {
            let mut s = lock();
            s.status = Status::Failed("Выберите сервер".into());
            s.notify();
        }
    }
}

/// A server picked by hand while connected: move there, unless the network
/// rule decides which server this network uses.
pub fn switch_if_connected(key: &str) {
    let go = {
        let s = lock();
        s.active() && s.policy.server_for(&s.net).is_none() && s.server.as_deref() != Some(key)
    };
    if go {
        start(key.to_string());
    }
}

/// Restarts an active connection with a fresh config (the routing changed).
pub fn reconnect() {
    let key = {
        let s = lock();
        if !s.active() {
            return;
        }
        s.server.clone()
    };
    if let Some(key) = key {
        start(key);
    }
}

/// Moves an active connection to `key`, whatever the network rule says
/// ("Держать подключение" leaving a silent server).
pub fn switch_to(key: &str) {
    let go = {
        let s = lock();
        s.active() && s.server.as_deref() != Some(key)
    };
    if go {
        start(key.to_string());
    }
}

pub fn disconnect() {
    let platform = {
        let mut s = lock();
        s.attempt += 1;
        s.status = Status::Off;
        if let Some(x) = s.xray.take() {
            x.stop();
        }
        s.notify();
        s.platform.clone()
    };
    if let Some(p) = platform {
        p.stop_vpn();
    }
}

fn start(key: String) {
    let (attempt, host) = {
        let mut s = lock();
        let Some(host) = s.host.clone() else { return };
        s.attempt += 1;
        s.status = Status::Connecting;
        s.server = Some(key.clone());
        s.notify();
        (s.attempt, host)
    };
    std::thread::spawn(move || {
        let prepared = write_config(&host, &key);
        let mut s = lock();
        if s.attempt != attempt {
            return;
        }
        let prepared = prepared.map(|link| s.link = Some(link));
        if let Err(e) = prepared {
            let platform = s.fail("connect_failed", format!("{e:#}"));
            drop(s);
            if let Some(p) = platform {
                p.stop_vpn();
            }
            return;
        }
        if s.tun.is_some() {
            drop(s);
            launch(attempt);
        } else {
            let platform = s.platform.clone();
            drop(s);
            if let Some(p) = platform {
                p.start_vpn();
            }
        }
    });
}

/// (Re)starts xray on the open TUN. Runs on a worker thread: stopping the
/// old xray waits for it to let go of the TUN.
fn launch(attempt: u64) {
    let (old, host, tun) = {
        let mut s = lock();
        if s.attempt != attempt {
            return;
        }
        (s.xray.take(), s.host.clone(), s.tun)
    };
    if let Some(old) = old {
        old.stop_wait(Duration::from_millis(1500));
    }
    let (Some(host), Some(fd)) = (host, tun) else { return };
    // The desktop preview has no TUN: it only pretends to connect.
    let result = if fd < 0 { Ok(None) } else { tunnel::spawn(&host.paths(), fd, move |exit| on_xray_exit(attempt, exit)).map(Some) };
    let mut s = lock();
    if s.attempt != attempt {
        if let Ok(Some(x)) = result {
            x.stop();
        }
        return;
    }
    match result {
        Ok(x) => {
            crate::diag::log("connected");
            s.xray = x;
            s.status = Status::On;
            s.notify();
        }
        Err(e) => {
            let platform = s.fail("connect_failed", format!("{e:#}"));
            drop(s);
            if let Some(p) = platform {
                p.stop_vpn();
            }
        }
    }
}

fn on_xray_exit(attempt: u64, exit: tunnel::Exit) {
    let platform = {
        let mut s = lock();
        if s.attempt != attempt {
            return;
        }
        s.xray = None;
        let mut message = format!("xray остановился через {} с", exit.ran_for.as_secs());
        if !exit.log_tail.is_empty() {
            message.push_str(":\n");
            message.push_str(&exit.log_tail);
        }
        s.fail("tunnel_stopped", message)
    };
    if let Some(p) = platform {
        p.stop_vpn();
    }
}

/// The platform opened the TUN. Returns false when there is nothing to run
/// on it (an always-on VPN started before any connection was made).
pub fn on_tun_up(fd: RawFd) -> bool {
    let attempt = {
        let mut s = lock();
        let Some(host) = s.host.clone() else { return false };
        if fd >= 0 && !host.paths().config.exists() {
            // Always-on VPN before any connection was made here: the last
            // chosen server, if there is one.
            let last = Store::load(&host.store()).ok().and_then(|st| st.settings.last_server);
            match last.map(|key| (write_config(&host, &key), key)) {
                Some((Ok(link), key)) => {
                    s.link = Some(link);
                    s.server = Some(key);
                }
                _ => return false,
            }
        }
        s.tun = Some(fd);
        if !s.active() {
            // Started by the system (always-on VPN): run the last config.
            s.attempt += 1;
            s.status = Status::Connecting;
            s.notify();
        }
        s.attempt
    };
    std::thread::spawn(move || launch(attempt));
    true
}

/// The platform closed the TUN (disconnect, another VPN took over, ...).
pub fn on_tun_down() {
    let mut s = lock();
    s.tun = None;
    s.attempt += 1;
    if let Some(x) = s.xray.take() {
        x.stop();
    }
    if !matches!(s.status, Status::Failed(_)) {
        s.status = Status::Off;
    }
    s.notify();
}

/// What lets the app read Wi-Fi names (Android): 0 no location access,
/// 1 approximate only, 2 precise but location off, 3 readable.
static WIFI_ACCESS: AtomicU8 = AtomicU8::new(3);

pub fn wifi_access() -> u8 {
    WIFI_ACCESS.load(Ordering::Relaxed)
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn set_wifi_access(code: u8) {
    if WIFI_ACCESS.swap(code, Ordering::Relaxed) != code {
        lock().notify();
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn on_vpn_denied() {
    let mut s = lock();
    s.attempt += 1;
    s.status = Status::Failed("Нет разрешения на VPN".into());
    s.notify();
}

pub fn on_network(net: NetworkEnv) {
    let switch = {
        let mut s = lock();
        if s.net == net {
            return;
        }
        log::info!("network: {}", net.describe());
        // The kind only: a Wi-Fi name says where the user is.
        crate::diag::log(format!("network: {:?}", net.kind));
        s.net = net;
        s.notify();
        s.wanted_switch()
    };
    if let Some(key) = switch {
        start(key);
    }
}

fn write_config(host: &Host, key: &str) -> Result<Link> {
    let store = Store::load(&host.store()).context("store.json")?;
    let (_, server) = servers::find(&store, key).context("Сервер больше не найден в подписках")?;
    let geo = GeoData::load(Some(&host.geo_assets()), Some(&host.geo().lists()));
    let routing = runtime::Routing { settings: &store.settings.routing, geo: &geo };
    let rt = runtime::build(server, "", Some(routing))?;
    for warning in &rt.warnings {
        log::warn!("routing: {warning}");
    }
    let mut config = mobile_config(rt.config);
    let api = SocketAddr::from(([127, 0, 0, 1], runtime::free_port()?));
    balancer::add_api(&mut config, api);
    let link = Link { socks: rt.socks, user: rt.user, pass: rt.pass, api, balancer: balancer::main_balancer(&config) };
    let paths = host.paths();
    std::fs::create_dir_all(host.run_dir())?;
    std::fs::write(&paths.config, serde_json::to_vec_pretty(&config)?).context("xray.json")?;
    Ok(link)
}

/// Inbound the app's own requests (subscription updates, exit check) use.
pub const APP_INBOUND: &str = "duoray-app";

/// Drops the empty interface binding `runtime::build` puts on outbounds:
/// the app is outside its own VPN, its sockets need no binding.
pub fn strip_interface(config: &mut Value) {
    if let Some(outbounds) = config["outbounds"].as_array_mut() {
        for ob in outbounds {
            if let Some(sockopt) = ob.pointer_mut("/streamSettings/sockopt").and_then(Value::as_object_mut)
                && sockopt.get("interface").is_some_and(|i| i == "")
            {
                sockopt.remove("interface");
            }
        }
    }
}

/// The desktop runtime config made for the phone: xray reads packets from
/// the TUN itself, tagged like the panel's own inbound so its rules apply;
/// the runtime's password SOCKS inbound stays for the app's own requests.
pub fn mobile_config(mut config: Value) -> Value {
    let mut app = config["inbounds"][0].clone();
    let sniffing = app
        .get("sniffing")
        .cloned()
        .unwrap_or_else(|| json!({ "enabled": true, "destOverride": ["http", "tls", "quic"], "routeOnly": false }));
    app["tag"] = json!(APP_INBOUND);
    config["inbounds"] = json!([{
        "tag": "socks",
        "port": 0,
        "protocol": "tun",
        "settings": { "name": "tun0", "MTU": TUN_MTU },
        "sniffing": sniffing
    }, app]);
    // DNS from the phone goes through the tunnel as plain UDP, like any
    // app's traffic, ahead of a panel's port-53 rule: its DoH servers behind
    // a balancer timed out on mobile whitelists, leaving nothing resolving.
    let target = routing::proxy_target(&config, config["routing"]["rules"].as_array().map(Vec::as_slice).unwrap_or_default());
    let mut dns_rule = json!({ "type": "field", "inboundTag": ["socks"], "port": "53", "network": "udp" });
    match target {
        routing::Target::Outbound(tag) => dns_rule["outboundTag"] = json!(tag),
        routing::Target::Balancer(tag) => dns_rule["balancerTag"] = json!(tag),
    }
    if let Some(rules) = config["routing"]["rules"].as_array_mut() {
        rules.insert(0, dns_rule);
    } else {
        config["routing"]["rules"] = json!([dns_rule]);
    }
    strip_interface(&mut config);
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tun_inbound_replaces_socks_and_keeps_sniffing() {
        let config = json!({
            "inbounds": [
                { "tag": "socks", "protocol": "socks", "port": 4242, "sniffing": { "enabled": true, "destOverride": ["tls"] } },
                { "tag": "direct-in", "protocol": "socks", "port": 4343 }
            ],
            "outbounds": [
                { "tag": "proxy", "protocol": "vless", "streamSettings": { "sockopt": { "interface": "", "mark": 1 } } },
                { "tag": "direct", "protocol": "freedom", "streamSettings": { "sockopt": { "interface": "" } } },
                { "tag": "block", "protocol": "blackhole" }
            ]
        });
        let c = mobile_config(config);
        let inbounds = c["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[1]["tag"], APP_INBOUND);
        assert_eq!(inbounds[1]["port"], 4242);
        assert_eq!(inbounds[0]["protocol"], "tun");
        assert_eq!(inbounds[0]["tag"], "socks");
        assert_eq!(inbounds[0]["settings"]["MTU"], TUN_MTU);
        assert_eq!(inbounds[0]["sniffing"]["destOverride"], json!(["tls"]));
        assert_eq!(c["outbounds"][0]["streamSettings"]["sockopt"], json!({ "mark": 1 }));
        assert_eq!(c["outbounds"][1]["streamSettings"]["sockopt"], json!({}));
        assert!(c["outbounds"][2].get("streamSettings").is_none());
        // DNS from the TUN goes where the panel sends traffic by default.
        assert_eq!(c["routing"]["rules"][0]["port"], "53");
        assert_eq!(c["routing"]["rules"][0]["outboundTag"], "proxy");
    }

    #[test]
    fn hwid_is_stable_and_shaped_like_desktop() {
        let host = Host {
            data: "/x".into(),
            xray: "/x/xray".into(),
            assets: "/x".into(),
            device_id: "abc".into(),
            os: "Android",
            os_version: "16".into(),
            model: "Pixel".into(),
        };
        let (a, b) = (host.device(), host.device());
        assert_eq!(a.hwid, b.hwid);
        assert_eq!(a.hwid.len(), 36);
        assert_eq!(a.os, "Android");
        assert_eq!(a.model.as_deref(), Some("Pixel"));
    }
}
