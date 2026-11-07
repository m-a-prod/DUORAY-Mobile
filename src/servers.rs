//! Server keys and the wording every screen shares (sizes, dates, plurals).

use duoray_core::server::{Server, display_name};
use duoray_core::store::{Store, Subscription};

/// The desktop app's key: it survives subscription updates as long as the
/// server keeps its name and address.
pub fn key_of(sub: &Subscription, s: &Server) -> String {
    format!("{}\u{1f}{}\u{1f}{}:{}", sub.id, s.name, s.address, s.port)
}

pub fn find<'a>(store: &'a Store, key: &str) -> Option<(&'a Subscription, &'a Server)> {
    store
        .subscriptions
        .iter()
        .find_map(|sub| sub.servers.iter().find(|s| key_of(sub, s) == key).map(|s| (sub, s)))
}

/// The name a server is shown under (its flag goes to an image instead).
pub fn name(key: &str, store: &Store) -> Option<String> {
    find(store, key).map(|(_, s)| display_name(&s.name).1)
}

/// The header line, as on the desktop: "29.10.2026 00:31 · Автообновление — 12 ч · 62 сервера".
pub fn header_meta(sub: &Subscription) -> String {
    let servers = plural(sub.servers.len(), "сервер", "сервера", "серверов");
    if sub.is_manual() {
        return format!("{servers} · добавлены вручную");
    }
    let mut parts = vec![sub.updated_at.map_or_else(|| "ещё не обновлялась".into(), local_time)];
    parts.push(format!("Автообновление — {} ч", update_interval_secs(sub) / 3600));
    parts.push(servers);
    if sub.skipped > 0 {
        parts.push(format!("пропущено {}", sub.skipped));
    }
    parts.join(" · ")
}

/// The short header line: "обновлено 05:01 · 78 серверов".
pub fn header_short(sub: &Subscription) -> String {
    let servers = plural(sub.servers.len(), "сервер", "сервера", "серверов");
    if sub.is_manual() {
        return servers;
    }
    match sub.updated_at {
        Some(t) => {
            use chrono::TimeZone;
            let when = chrono::Local.timestamp_opt(t as i64, 0).single();
            let today = chrono::Local::now().date_naive();
            let label = when.map_or_else(String::new, |w| {
                if w.date_naive() == today { w.format("%H:%M").to_string() } else { w.format("%d.%m %H:%M").to_string() }
            });
            format!("обновлено {label} · {servers}")
        }
        None => format!("не обновлялась · {servers}"),
    }
}

/// The drawer card line, as in the desktop sidebar: "62 сервера · 142.7 ГБ · безлимит".
pub fn card_meta(sub: &Subscription) -> String {
    let servers = plural(sub.servers.len(), "сервер", "сервера", "серверов");
    let used = bytes(sub.info.download.unwrap_or(0) + sub.info.upload.unwrap_or(0));
    let limit = match sub.info.total {
        Some(total) if total > 0 => format!("из {}", bytes(total)),
        _ => "безлимит".into(),
    };
    match sub.info.expire.filter(|e| *e > 0) {
        Some(expire) => format!("{servers} · {used} {limit} · до {}", date(expire)),
        None => format!("{servers} · {used} · {limit}"),
    }
}

/// "86.0 ГБ / ∞" or "12.4 ГБ / 100.0 ГБ" and the used share (negative when
/// the plan is unlimited).
pub fn usage(sub: &Subscription) -> (String, f32) {
    let used = sub.info.download.unwrap_or(0) + sub.info.upload.unwrap_or(0);
    match sub.info.total {
        Some(total) if total > 0 => (format!("{} / {}", bytes(used), bytes(total)), used as f32 / total as f32),
        _ => (format!("{} / ∞", bytes(used)), -1.0),
    }
}

/// The panel's `profile-update-interval`, or 12 h.
pub fn update_interval_secs(sub: &Subscription) -> u64 {
    u64::from(sub.info.update_interval_hours.filter(|h| *h > 0).unwrap_or(12)) * 3600
}

pub fn plural(n: usize, one: &str, few: &str, many: &str) -> String {
    let word = match (n % 10, n % 100) {
        (1, r) if r != 11 => one,
        (2..=4, r) if !(12..=14).contains(&r) => few,
        _ => many,
    };
    format!("{n} {word}")
}

pub fn bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{b} Б") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn local_time(unix: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix as i64, 0)
        .single()
        .map_or_else(|| date(unix), |t| t.format("%d.%m.%Y %H:%M").to_string())
}

fn date(unix: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix as i64, 0)
        .single()
        .map_or_else(String::new, |t| t.format("%d.%m.%Y").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "сервер", "сервера", "серверов"), "1 сервер");
        assert_eq!(plural(3, "сервер", "сервера", "серверов"), "3 сервера");
        assert_eq!(plural(11, "сервер", "сервера", "серверов"), "11 серверов");
        assert_eq!(plural(62, "сервер", "сервера", "серверов"), "62 сервера");
    }

    #[test]
    fn sizes() {
        assert_eq!(bytes(512), "512 Б");
        assert_eq!(bytes(1536), "1.5 КБ");
        assert_eq!(bytes(153_223_000_000), "142.7 ГБ");
    }
}
