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
        .or_else(|| moved(store, key))
}

/// The server `key` was made for after the panel changed its address (a
/// bridge moved to a new IP) or its name: the only server of the same
/// subscription with that name, else the only one at that address.
fn moved<'a>(store: &'a Store, key: &str) -> Option<(&'a Subscription, &'a Server)> {
    let mut parts = key.split('\u{1f}');
    let (id, name, endpoint) = (parts.next()?, parts.next()?, parts.next()?);
    let sub = store.get(id)?;
    let only = |hit: &dyn Fn(&Server) -> bool| {
        let mut found = sub.servers.iter().filter(|s| hit(s));
        match (found.next(), found.next()) {
            (Some(s), None) => Some(s),
            _ => None,
        }
    };
    only(&|s| s.name == name).or_else(|| only(&|s| format!("{}:{}", s.address, s.port) == endpoint)).map(|s| (sub, s))
}

/// Old keys whose servers now go by another one (see [`find`]): old → new.
pub fn moved_keys<'a>(store: &Store, keys: impl IntoIterator<Item = &'a String>) -> std::collections::HashMap<String, String> {
    keys.into_iter()
        .filter_map(|k| find(store, k).map(|(sub, s)| key_of(sub, s)).filter(|new| new != k).map(|new| (k.clone(), new)))
        .collect()
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
    fn errors_read_as_russian() {
        assert_eq!(humanize("GET https://x/sub: timeout: global"), "Сервер подписки не ответил вовремя.");
        assert_eq!(humanize("this subscription is already added"), "Эта подписка уже добавлена.");
        assert_eq!(humanize("odd\nsecond"), "odd");
    }

    #[test]
    fn dualizm_by_host() {
        let sub = Subscription { url: "https://vip.dualizm.space/sub/x".into(), ..Default::default() };
        assert!(is_dualizm(&sub));
        assert_eq!(renew_url(&sub).as_deref(), Some(BUY_URL));
    }

    #[test]
    fn a_server_the_panel_moved_is_still_found() {
        use duoray_core::server::Source;
        let server = |name: &str, address: &str| Server {
            name: name.into(),
            description: None,
            protocol: "VLESS".into(),
            address: address.into(),
            port: 443,
            network: "tcp".into(),
            security: "tls".into(),
            proxies: 1,
            source: Source::Json(serde_json::json!({})),
            override_key: None,
        };
        let sub = |servers| Subscription { id: "s".into(), servers, ..Default::default() };
        let before = sub(vec![server("Мост", "192.0.2.1"), server("Финляндия", "192.0.2.2"), server("LTE 1", "192.0.2.9"), server("LTE 2", "192.0.2.9")]);
        let keys: Vec<String> = before.servers.iter().map(|s| key_of(&before, s)).collect();
        // The bridge got a new address, Финляндия a new name, the LTE pair both.
        let after = sub(vec![server("Мост", "198.51.100.1"), server("Финляндия #1", "192.0.2.2"), server("LTE 1 (1x)", "192.0.2.9"), server("LTE 2 (1x)", "192.0.2.9")]);
        let store = Store { subscriptions: vec![after], ..Default::default() };
        assert_eq!(find(&store, &keys[0]).map(|(_, s)| s.address.as_str()), Some("198.51.100.1"));
        assert_eq!(find(&store, &keys[1]).map(|(_, s)| s.name.as_str()), Some("Финляндия #1"));
        assert!(find(&store, &keys[2]).is_none(), "two servers at that address: no guess");
        assert!(find(&store, "other\u{1f}Мост\u{1f}192.0.2.1:443").is_none(), "only within its subscription");
        let moved = moved_keys(&store, &keys);
        assert_eq!(moved.len(), 2);
        assert_eq!(moved[&keys[0]], key_of(&store.subscriptions[0], &store.subscriptions[0].servers[0]));
    }

    #[test]
    fn sizes() {
        assert_eq!(bytes(512), "512 Б");
        assert_eq!(bytes(1536), "1.5 КБ");
        assert_eq!(bytes(153_223_000_000), "142.7 ГБ");
    }
}

/// The DUALIZM bot, where DUALIZM subscriptions are bought and renewed.
pub const BUY_URL: &str = "https://t.me/dualizm_bot";

pub fn is_dualizm(sub: &Subscription) -> bool {
    let host = url_host(&sub.url);
    host == "dualizm.space" || host.ends_with(".dualizm.space") || sub.info.title.as_deref().is_some_and(|t| t.to_uppercase().contains("DUALIZM"))
}

fn url_host(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split(['/', ':', '?']).next()).unwrap_or_default().to_lowercase()
}

/// The reminder starts this many days before the end and lasts a week after.
const EXPIRY_AHEAD: u64 = 3 * 86400;
const EXPIRY_AFTER: u64 = 7 * 86400;

/// The subscription closest to its end, unless its reminder was closed.
pub fn expiring(subs: &[Subscription], now: u64) -> Option<usize> {
    subs.iter()
        .enumerate()
        .filter_map(|(i, s)| s.info.expire.filter(|&e| e > 0 && s.expiry_dismissed != Some(e)).map(|e| (i, e)))
        .filter(|&(_, e)| e < now + EXPIRY_AHEAD && e + EXPIRY_AFTER > now)
        .min_by_key(|&(_, e)| e)
        .map(|(i, _)| i)
}

/// "Подписка «DUALIZM» заканчивается завтра в 14:00"
pub fn expiry_text(name: &str, expire: u64, now: u64) -> String {
    use chrono::TimeZone;
    if expire <= now {
        return format!("Подписка «{name}» закончилась");
    }
    let local = |t: u64| chrono::Local.timestamp_opt(t as i64, 0).single();
    let (Some(end), Some(today)) = (local(expire), local(now)) else {
        return format!("Подписка «{name}» заканчивается {}", date(expire));
    };
    let when = match (end.date_naive() - today.date_naive()).num_days() {
        0 => format!("сегодня в {}", end.format("%H:%M")),
        1 => format!("завтра в {}", end.format("%H:%M")),
        n => format!("через {} ({})", plural(n as usize, "день", "дня", "дней"), end.format("%d.%m")),
    };
    format!("Подписка «{name}» заканчивается {when}")
}

/// Where to renew: the DUALIZM bot, otherwise the panel's page or support.
pub fn renew_url(sub: &Subscription) -> Option<String> {
    if is_dualizm(sub) {
        return Some(BUY_URL.to_string());
    }
    sub.info.web_page_url.clone().or_else(|| sub.info.support_url.clone())
}

/// The common English errors from core and its HTTP client, readable.
pub fn humanize(e: &str) -> String {
    let map = [
        ("already added", "Эта подписка уже добавлена."),
        ("not a valid URL", "Это не похоже на ссылку."),
        ("must be http(s)", "Ссылка должна начинаться с http:// или https://."),
        ("HTML page", "Вместо подписки пришла веб-страница. Проверьте ссылку."),
        ("timed out", "Сервер подписки не ответил вовремя."),
        ("timeout", "Сервер подписки не ответил вовремя."),
        ("dns failed", "Не удалось найти сервер подписки. Проверьте интернет."),
        ("status: 404", "Подписка не найдена (404)."),
        ("status: 403", "Доступ к подписке запрещён (403)."),
    ];
    if let Some((_, app)) = e.split_once("the panel redirects to the ") {
        let app = app.split(' ').next().unwrap_or_default();
        return format!("Панель отдала ссылку для приложения {app}, а не подписку. Скопируйте ссылку на подписку (https://…) в личном кабинете.");
    }
    if let Some((_, note)) = e.split_once("a note instead of servers: ") {
        return format!("Серверов нет, панель пишет: «{note}»");
    }
    map.iter().find(|(needle, _)| e.contains(needle)).map_or_else(|| e.lines().next().unwrap_or_default().to_string(), |(_, text)| text.to_string())
}
