//! What the notification and the Quick Settings tile show about the
//! connection. While the window is open the app publishes the full line (it
//! knows the exit country, the ping and the speeds already); with the window
//! closed a poller here keeps the speeds and the exit country fresh.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use duoray_core::balancer;
use duoray_core::store::Store;

use crate::exit::{self, Traffic};
use crate::service::{self, Host, Link, Snapshot, Status};
use crate::servers;

/// One state of the connection, as the shade shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notice {
    /// 0 off, 1 connecting, 2 on, 3 failed.
    pub state: u8,
    /// "Финляндия", "Подключение…".
    pub title: String,
    /// The server, and its ping when known.
    pub text: String,
    /// "↓ 1.2 МБ/с   ↑ 80 КБ/с".
    pub speeds: String,
    /// The status bar chip of a Live Update: the exit country's code.
    pub chip: String,
    /// When the connection came up, Unix milliseconds; 0 unknown.
    pub since_ms: i64,
    /// The server's key: the favorites widget marks the one in use. Packed
    /// last: it has U+001F in it.
    pub key: String,
}

impl Notice {
    /// One string over JNI: fields joined with U+001F.
    pub fn pack(&self) -> String {
        let live = if LIVE_UPDATE.load(Ordering::Relaxed) { "1" } else { "0" };
        let quiet = if QUIET.load(Ordering::Relaxed) { "1" } else { "0" };
        [
            self.state.to_string(),
            self.title.clone(),
            self.text.clone(),
            self.speeds.clone(),
            self.chip.clone(),
            self.since_ms.to_string(),
            live.to_string(),
            quiet.to_string(),
            self.key.clone(),
        ]
        .join("\u{1f}")
    }
}

/// The flag emoji of a two-letter country code ("FI" → 🇫🇮): Android draws
/// it in colour in the notification and the status bar chip.
pub fn flag_emoji(code: &str) -> String {
    let code = code.trim().to_ascii_uppercase();
    if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
        return String::new();
    }
    code.bytes().filter_map(|b| char::from_u32(0x1F1E6 + u32::from(b - b'A'))).collect()
}

/// "🇫🇮 Финляндия" for the title, the flag for the chip.
pub fn exit_line(loc: &str) -> (String, String) {
    let name = balancer::country_name(loc).unwrap_or(loc);
    let flag = flag_emoji(loc);
    let title = if flag.is_empty() { name.to_string() } else { format!("{flag} {name}") };
    (title, if flag.is_empty() { loc.to_string() } else { flag })
}

pub fn state_code(status: &Status) -> u8 {
    match status {
        Status::Off => 0,
        Status::Connecting => 1,
        Status::On => 2,
        Status::Failed(_) => 3,
    }
}

static WINDOW_OPEN: AtomicBool = AtomicBool::new(false);
/// MobileSettings::background_stats.
static BACKGROUND_STATS: AtomicBool = AtomicBool::new(true);
/// MobileSettings::live_update.
static LIVE_UPDATE: AtomicBool = AtomicBool::new(true);
/// MobileSettings::quiet_notification.
static QUIET: AtomicBool = AtomicBool::new(false);
static SINCE: AtomicI64 = AtomicI64::new(0);
static LAST: Mutex<Option<Notice>> = Mutex::new(None);
/// The link the background poller watches: one poller per connection.
static POLLED: Mutex<Option<Link>> = Mutex::new(None);

/// The app's window opened or closed: who publishes changes hands.
pub fn set_window_open(open: bool) {
    WINDOW_OPEN.store(open, Ordering::Relaxed);
}

pub fn set_background_stats(on: bool) {
    BACKGROUND_STATS.store(on, Ordering::Relaxed);
}

/// Live Update on or off; the notification changes right away.
pub fn set_live_update(on: bool) {
    if LIVE_UPDATE.swap(on, Ordering::Relaxed) != on {
        republish();
    }
}

/// «Свернуть уведомление» on or off; the notification changes right away.
pub fn set_quiet(on: bool) {
    if QUIET.swap(on, Ordering::Relaxed) != on {
        republish();
    }
}

fn republish() {
    let last = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Its own thread: the caller may hold the service lock (service::init).
    if let Some(notice) = last {
        std::thread::spawn(move || {
            if let Some(p) = service::platform() {
                p.show_status(&notice.pack());
            }
        });
    }
}

pub fn window_open() -> bool {
    WINDOW_OPEN.load(Ordering::Relaxed)
}

/// When the current connection came up (Unix ms), 0 when not connected.
pub fn since_ms() -> i64 {
    SINCE.load(Ordering::Relaxed)
}

/// Keeps the connected-since time in step with the status.
pub fn track(status: &Status) {
    if *status == Status::On {
        let _ = SINCE.compare_exchange(0, chrono::Utc::now().timestamp_millis(), Ordering::Relaxed, Ordering::Relaxed);
    } else {
        SINCE.store(0, Ordering::Relaxed);
    }
}

/// Hands a changed notice to the platform (the shade, the tile).
pub fn publish(notice: Notice) {
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&notice) {
            return;
        }
        *last = Some(notice.clone());
    }
    if let Some(p) = service::platform() {
        p.show_status(&notice.pack());
    }
}

/// The status changed with the window closed: a plain notice now, and the
/// poller for speeds and the exit country while connected. Called with the
/// service locked: everything runs on its own thread.
pub fn background(snapshot: Snapshot, host: Option<Arc<Host>>) {
    std::thread::spawn(move || {
        let name = |key: &str| {
            host.as_ref()
                .and_then(|h| Store::load(&h.store()).ok())
                .and_then(|store| servers::name(key, &store))
                .unwrap_or_default()
        };
        let server = snapshot.server.as_deref().map(name).unwrap_or_default();
        let (title, text) = match &snapshot.status {
            Status::Off => ("Не подключено".to_string(), server.clone()),
            Status::Connecting => ("Подключение…".to_string(), server.clone()),
            Status::On => ("Подключено".to_string(), server.clone()),
            Status::Failed(e) => ("Ошибка подключения".to_string(), e.lines().next().unwrap_or_default().to_string()),
        };
        let key = snapshot.server.clone().unwrap_or_default();
        publish(Notice { state: state_code(&snapshot.status), title, text, since_ms: since_ms(), key, ..Default::default() });
        if let (Status::On, Some(link), Some(host)) = (&snapshot.status, snapshot.link, host)
            && BACKGROUND_STATS.load(Ordering::Relaxed)
        {
            {
                let mut polled = POLLED.lock().unwrap_or_else(|e| e.into_inner());
                if polled.as_ref() == Some(&link) {
                    return;
                }
                *polled = Some(link.clone());
            }
            poll(host, link.clone(), server, snapshot.server.unwrap_or_default());
            let mut polled = POLLED.lock().unwrap_or_else(|e| e.into_inner());
            if polled.as_ref() == Some(&link) {
                *polled = None;
            }
        }
    });
}

/// While this link is up and the window closed: speeds every few seconds,
/// the exit country until it is known.
fn poll(host: Arc<Host>, link: Link, server: String, key: String) {
    let mut traffic: Option<Traffic> = None;
    let mut exit: Option<(String, String)> = None;
    let mut traced: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let now = service::snapshot();
        if now.status != Status::On || now.link.as_ref() != Some(&link) {
            return;
        }
        if !BACKGROUND_STATS.load(Ordering::Relaxed) {
            return;
        }
        if window_open() {
            // The app publishes now; look again in case the window closes.
            traffic = None;
            continue;
        }
        if let Ok(counts) = exit::counters(&host.xray, &link) {
            traffic = Some(Traffic::read(traffic.as_ref(), Instant::now(), counts));
        }
        if exit.is_none() && traced.is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) {
            traced = Some(Instant::now());
            if let Ok(t) = exit::trace(&link) {
                exit = Some(exit_line(&t.loc));
            }
        }
        let (title, chip) = exit.clone().unwrap_or_else(|| ("Подключено".into(), String::new()));
        publish(Notice {
            state: 2,
            title,
            text: server.clone(),
            speeds: traffic.as_ref().map(Traffic::speeds).unwrap_or_default(),
            chip,
            since_ms: since_ms(),
            key: key.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_from_country_codes() {
        assert_eq!(flag_emoji("FI"), "🇫🇮");
        assert_eq!(flag_emoji("ru"), "🇷🇺");
        assert_eq!(flag_emoji("XYZ"), "");
        assert_eq!(flag_emoji("1A"), "");
        assert_eq!(exit_line("FI").1, "🇫🇮");
    }
}
