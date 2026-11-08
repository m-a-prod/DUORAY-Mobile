//! Server delay, measured the way the desktop app does (its ping.rs, the
//! same settings from store.json): by default an HTTP request through the
//! server itself, so a server the network blocks (mobile whitelists) shows
//! no answer instead of a TCP time that means nothing. The app is outside
//! its own VPN, so the checks go straight to the servers even while
//! connected.

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use duoray_core::runtime;
use duoray_core::server::Server;
use duoray_core::store::{PingMode, PingSettings, ProxyPingMode};
use serde_json::Value;

use crate::service::{self, Host};
use crate::tunnel;

/// Pings `jobs` with `settings.threads` workers; `on_result` runs on worker
/// threads as results arrive, `on_done` once at the end.
pub fn run_all(
    jobs: Vec<(String, Server)>,
    settings: PingSettings,
    host: Arc<Host>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    on_start: impl Fn(String) + Send + Sync + 'static,
    on_result: impl Fn(String, Result<Duration, String>) + Send + Sync + 'static,
    on_done: impl FnOnce() + Send + 'static,
) {
    let threads = settings.threads.clamp(1, 32) as usize;
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let on_result = Arc::new(on_result);
    let on_start = Arc::new(on_start);
    let settings = Arc::new(settings);
    std::thread::spawn(move || {
        let workers: Vec<_> = (0..threads)
            .map(|n| {
                let (queue, on_start, on_result, settings, host, cancel) =
                    (queue.clone(), on_start.clone(), on_result.clone(), settings.clone(), host.clone(), cancel.clone());
                std::thread::spawn(move || {
                    loop {
                        // Stopped: the servers under way finish, no new ones start.
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        let Some((key, server)) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { return };
                        on_start(key.clone());
                        let result = ping(&server, &settings, &host, n).map_err(|e| format!("{e:#}"));
                        on_result(key, result);
                    }
                })
            })
            .collect();
        for w in workers {
            let _ = w.join();
        }
        on_done();
    });
}

fn ping(server: &Server, s: &PingSettings, host: &Host, worker: usize) -> Result<Duration> {
    let timeout = Duration::from_millis(s.timeout_ms.clamp(500, 30_000));
    match s.mode {
        PingMode::Tcp => tcp(&server.address, server.port, timeout),
        PingMode::Icmp => icmp(&server.address, timeout),
        PingMode::HttpGet | PingMode::HttpHead => through_server(server, host, &format!("ping-{worker}"), |socks, user, pass| {
            via_socks_mode(socks, user, pass, &s.url, s.mode == PingMode::HttpHead, timeout, s.proxy_mode)
        }),
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    (host, port)
        .to_socket_addrs()
        .with_context(|| format!("не удалось найти {host}"))?
        .find(SocketAddr::is_ipv4)
        .with_context(|| format!("у {host} нет IPv4-адреса"))
}

fn tcp(host: &str, port: u16, timeout: Duration) -> Result<Duration> {
    let addr = resolve(host, port)?;
    let start = Instant::now();
    TcpStream::connect_timeout(&addr, timeout).map_err(|e| anyhow::anyhow!("нет соединения: {e}"))?;
    Ok(start.elapsed())
}

/// Android's own `ping`.
fn icmp(host: &str, timeout: Duration) -> Result<Duration> {
    let out = Command::new("ping")
        .args(["-c", "1", "-W", &timeout.as_secs().max(1).to_string(), host])
        .stdin(Stdio::null())
        .output()
        .context("не удалось запустить ping")?;
    parse_ping_time(&String::from_utf8_lossy(&out.stdout)).context("нет ответа")
}

/// Finds "time=12.3 ms" in ping output.
fn parse_ping_time(text: &str) -> Option<Duration> {
    let rest = &text[text.find("time=")? + 5..];
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let ms: f64 = num.parse().ok()?;
    Some(Duration::from_micros((ms * 1000.0) as u64))
}

/// Runs `job` against a throwaway xray with `server`'s own config; `job`
/// gets xray's SOCKS address and credentials. `name` names its config file.
pub fn through_server<T>(server: &Server, host: &Host, name: &str, job: impl FnOnce(SocketAddr, &str, &str) -> Result<T>) -> Result<T> {
    let rt = runtime::build(server, "", None)?;
    let mut config = rt.config;
    service::strip_interface(&mut config);
    strip_geo(&mut config);
    let dir = host.run_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec(&config)?)?;
    let mut child = tunnel::spawn_plain(&host.xray, &host.assets, &path)?;
    let result = (|| {
        wait_listening(rt.socks, &mut child, Duration::from_secs(5))?;
        job(rt.socks, &rt.user, &rt.pass)
    })();
    child.kill();
    let _ = std::fs::remove_file(&path);
    result
}

/// What a ping does not need and xray pays for at every start: rules and
/// DNS that name geoip/geosite make it read those files (megabytes) once
/// per server checked. The check goes through the proxy anyway.
fn strip_geo(config: &mut Value) {
    fn geo(v: &Value) -> bool {
        v.as_array().is_some_and(|l| l.iter().any(|x| x.as_str().is_some_and(|x| x.starts_with("geosite:") || x.starts_with("geoip:") || x.starts_with("ext:"))))
    }
    if let Some(rules) = config["routing"]["rules"].as_array_mut() {
        rules.retain(|r| !(geo(&r["domain"]) || geo(&r["ip"]) || geo(&r["source"])));
    }
    if let Some(obj) = config.as_object_mut() {
        obj.remove("dns");
    }
}

fn wait_listening(addr: SocketAddr, child: &mut tunnel::Plain, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            bail!("xray завершился при старте (код {status})");
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("xray не открыл порт за {}с", timeout.as_secs())
}

/// HTTP requests through a SOCKS5 proxy. Default: one request on a fresh
/// connection, so the time includes the handshake (the "real delay").
/// Double: two fresh connections. KeepAlive: the second request reuses the
/// first one's connection. Both keep the best time.
pub fn via_socks_mode(socks: SocketAddr, user: &str, pass: &str, url: &str, head: bool, timeout: Duration, mode: ProxyPingMode) -> Result<Duration> {
    let proxy = ureq::Proxy::new(&format!("socks5://{user}:{pass}@{socks}"))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(Some(proxy))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_idle_connections(if mode == ProxyPingMode::KeepAlive { 1 } else { 0 })
        .build()
        .into();
    let attempts = if mode == ProxyPingMode::Default { 1 } else { 2 };
    let mut best: Option<Duration> = None;
    for attempt in 1..=attempts {
        let drain = mode == ProxyPingMode::KeepAlive && attempt < attempts;
        // A server that failed once is not asked again.
        let elapsed = proxy_request(&agent, url, head, drain)?;
        best = Some(best.map_or(elapsed, |previous| previous.min(elapsed)));
    }
    best.ok_or_else(|| anyhow::anyhow!("нет ответа"))
}

fn proxy_request(agent: &ureq::Agent, url: &str, head: bool, drain: bool) -> Result<Duration> {
    let start = Instant::now();
    let mut resp = if head { agent.head(url).call() } else { agent.get(url).call() }.map_err(|e| anyhow::anyhow!("нет ответа: {e}"))?;
    let elapsed = start.elapsed();
    if resp.status().is_server_error() {
        bail!("HTTP {}", resp.status());
    }
    if drain {
        let _ = resp.body_mut().with_config().limit(1024 * 1024).read_to_vec();
    }
    Ok(elapsed)
}

/// Whether `server` carries traffic at all: the ping URL through its own
/// throwaway xray, whatever the list's ping mode (a TCP or ICMP answer says
/// nothing about the proxy). "Держать подключение" checks a server with it
/// before moving to it.
pub fn check(server: &Server, s: &PingSettings, host: &Host) -> Result<Duration> {
    through_server(server, host, "failover", |socks, user, pass| {
        via_socks_mode(socks, user, pass, &s.url, s.mode == PingMode::HttpHead, Duration::from_secs(5), s.proxy_mode)
    })
}

/// Label and level of a ping chip, as on the desktop: 1 fast, 2 medium,
/// 3 slow, 4 failed.
pub fn label(result: &Result<Duration, String>) -> (String, i32) {
    match result {
        Ok(d) => {
            let ms = d.as_millis().max(1);
            (format!("{ms} мс"), if ms < 150 { 1 } else if ms < 400 { 2 } else { 3 })
        }
        Err(_) => ("—".into(), 4),
    }
}

/// Sort key: failed last.
pub fn sort_key(result: Option<&Result<Duration, String>>) -> u128 {
    match result {
        Some(Ok(d)) => d.as_millis(),
        _ => u128::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ping_output() {
        let line = "64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=12.3 ms";
        assert_eq!(parse_ping_time(line), Some(Duration::from_micros(12300)));
        assert_eq!(parse_ping_time("Request timeout"), None);
    }

    #[test]
    fn tcp_to_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp("127.0.0.1", port, Duration::from_secs(1)).is_ok());
        drop(listener);
        assert!(tcp("127.0.0.1", port, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn labels_like_desktop() {
        assert_eq!(label(&Ok(Duration::from_millis(53))), ("53 мс".into(), 1));
        assert_eq!(label(&Ok(Duration::from_millis(200))).1, 2);
        assert_eq!(label(&Err("x".into())), ("—".into(), 4));
    }
}

#[cfg(test)]
mod geo_tests {
    use super::*;

    #[test]
    fn pings_skip_geo_rules_and_dns() {
        let mut c = serde_json::json!({
            "dns": { "servers": ["1.1.1.1"] },
            "routing": { "rules": [
                { "ip": ["geoip:private"], "outboundTag": "direct" },
                { "domain": ["geosite:category-ads"], "outboundTag": "block" },
                { "inboundTag": ["socks"], "balancerTag": "b" },
                { "ip": ["10.0.0.0/8"], "outboundTag": "direct" }
            ] }
        });
        strip_geo(&mut c);
        assert!(c.get("dns").is_none());
        let rules = c["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["balancerTag"], "b");
    }
}
