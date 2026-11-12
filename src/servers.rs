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

/// "86 ГБ / ∞" or "12.4 / 100 ГБ" and the used share (negative when the
/// plan is unlimited). Short, since the bar shares its row with the
/// subscription's end: whole numbers go without ".0", and a unit both
/// numbers have is written once.
pub fn usage(sub: &Subscription) -> (String, f32) {
    let short = |b: u64| bytes(b).replace(".0 ", " ");
    let used = short(sub.info.download.unwrap_or(0) + sub.info.upload.unwrap_or(0));
    let Some(total) = sub.info.total.filter(|t| *t > 0) else {
        return (format!("{used} / ∞"), -1.0);
    };
    let share = (sub.info.download.unwrap_or(0) + sub.info.upload.unwrap_or(0)) as f32 / total as f32;
    let total = short(total);
    match (used.rsplit_once(' '), total.rsplit_once(' ')) {
        (Some((number, unit)), Some((_, same))) if unit == same => (format!("{number} / {total}"), share),
        _ => (format!("{used} / {total}"), share),
    }
}

/// How close the end of a subscription is: the colour of its words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    Fine,
    /// The days of the reminder (EXPIRY_AHEAD).
    Soon,
    Ended,
}

impl Term {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// What stands to the right of the traffic bar, where Happ has «Expires:
/// 09.12.2026»: what is left of the subscription and the day it ends,
/// ("Осталось 27 дней", "до 09.12.2026"). People looked for it next to the
/// traffic and did not find the drawer's card. Days are the calendar's, as
/// the reminder counts them; more than a year away, the day alone. A panel
/// that names no end (`expire=0`) gets nothing, as in Happ.
pub fn term(sub: &Subscription, now: u64) -> Option<(String, String, Term)> {
    use chrono::TimeZone;
    let end = sub.info.expire.filter(|e| *e > 0)?;
    if end <= now {
        return Some(("Истекла".into(), date(end), Term::Ended));
    }
    let until = format!("до {}", date(end));
    let local = |t: u64| chrono::Local.timestamp_opt(t as i64, 0).single();
    let (Some(ends), Some(today)) = (local(end), local(now)) else {
        return Some((String::new(), until, Term::Fine));
    };
    Some(match (ends.date_naive() - today.date_naive()).num_days() {
        ..=0 => ("Истекает сегодня".into(), format!("в {}", ends.format("%H:%M")), Term::Soon),
        days @ 1..=365 => {
            let n = days as usize;
            // "Остался 21 день", "Осталось 22 дня".
            let left = if n % 10 == 1 && n % 100 != 11 { "Остался" } else { "Осталось" };
            let state = if days as u64 * 86400 <= EXPIRY_AHEAD { Term::Soon } else { Term::Fine };
            (format!("{left} {}", plural(n, "день", "дня", "дней")), until, state)
        }
        _ => (String::new(), until, Term::Fine),
    })
}

/// The panel's `profile-update-interval`, or 12 h.
pub fn update_interval_secs(sub: &Subscription) -> u64 {
    u64::from(sub.info.update_interval_hours.filter(|h| *h > 0).unwrap_or(12)) * 3600
}

/// An update that failed is not asked for again sooner than this.
const UPDATE_RETRY: u64 = 10 * 60;

/// The subscriptions to update now without being asked: the panel's
/// interval has passed since the last update, and the last try (`tried`,
/// unix seconds by id) is not a moment ago: a failed one keeps its old time
/// of update and would be fetched in a loop.
pub fn due_for_update(subs: &[Subscription], tried: &std::collections::HashMap<String, u64>, now: u64) -> Vec<String> {
    subs.iter()
        .filter(|s| !s.is_manual() && s.updated_at.is_none_or(|t| now.saturating_sub(t) >= update_interval_secs(s)))
        .filter(|s| tried.get(&s.id).is_none_or(|t| now.saturating_sub(*t) >= UPDATE_RETRY))
        .map(|s| s.id.clone())
        .collect()
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
    fn the_time_left_stands_next_to_the_traffic() {
        use chrono::TimeZone;
        use duoray_core::subscription::SubInfo;
        let sub = |expire| Subscription { info: SubInfo { download: Some(3 << 30), expire, ..Default::default() }, ..Default::default() };
        // Noon of the phone's own day: the days are the calendar's.
        let noon = |d: u32| chrono::Local.with_ymd_and_hms(2026, 11, d, 12, 0, 0).unwrap().timestamp() as u64;
        let (now, end) = (noon(2), noon(29));
        let said = |expire, now| term(&sub(expire), now).map(|(left, until, state)| (format!("{left} | {until}"), state));
        assert_eq!(said(Some(end), now), Some(("Осталось 27 дней | до 29.11.2026".into(), Term::Fine)));
        assert_eq!(said(Some(end), noon(8)), Some(("Остался 21 день | до 29.11.2026".into(), Term::Fine)));
        assert_eq!(said(Some(end), noon(7)), Some(("Осталось 22 дня | до 29.11.2026".into(), Term::Fine)));
        assert_eq!(said(Some(end), noon(26)), Some(("Осталось 3 дня | до 29.11.2026".into(), Term::Soon)));
        // Tomorrow morning is a day away, though not 24 hours.
        assert_eq!(said(Some(end - 5 * 3600), noon(28)), Some(("Остался 1 день | до 29.11.2026".into(), Term::Soon)));
        assert_eq!(said(Some(end + 3600), end), Some(("Истекает сегодня | в 13:00".into(), Term::Soon)));
        assert_eq!(said(Some(end), end), Some(("Истекла | 29.11.2026".into(), Term::Ended)));
        // Years ahead: the day says it, a count of days would not.
        assert_eq!(said(Some(end + 400 * 86400), now), Some((format!(" | до {}", date(end + 400 * 86400)), Term::Fine)));
        // The panel's «no end», and a plain list of links.
        assert_eq!(term(&sub(Some(0)), now), None);
        assert_eq!(term(&Subscription::default(), now), None);
    }

    #[test]
    fn traffic_in_the_bar() {
        use duoray_core::subscription::SubInfo;
        let sub = |upload, download, total| Subscription { info: SubInfo { upload, download, total, ..Default::default() }, ..Default::default() };
        assert_eq!(usage(&sub(None, Some(214_405_837_087), None)), ("199.7 ГБ / ∞".into(), -1.0));
        assert_eq!(usage(&sub(Some(1 << 30), Some(5 << 30), Some(300 << 30))), ("6 / 300 ГБ".into(), 0.02));
        assert_eq!(usage(&sub(None, Some(5_798_205_849), Some(300 << 30))).0, "5.4 / 300 ГБ");
        assert_eq!(usage(&sub(None, Some(512 << 20), Some(300 << 30))).0, "512 МБ / 300 ГБ");
        assert_eq!(usage(&Subscription::default()).0, "0 Б / ∞");
    }

    #[test]
    fn stale_subscriptions_are_updated_and_failed_ones_not_in_a_loop() {
        use duoray_core::subscription::SubInfo;
        let now = 1_800_000_000;
        let sub = |id: &str, updated_at, hours| Subscription {
            id: id.into(),
            url: "https://sub.example.com/x".into(),
            updated_at,
            info: SubInfo { update_interval_hours: hours, ..Default::default() },
            ..Default::default()
        };
        let subs = [sub("hourly", Some(now - 3700), Some(1)), sub("fresh", Some(now - 600), Some(1)), sub("new", None, None), sub("daily", Some(now - 3700), None)];
        let mut tried = std::collections::HashMap::new();
        assert_eq!(due_for_update(&subs, &tried, now), ["hourly", "new"]);
        // The hourly one failed just now: its time of update did not move.
        tried.insert("hourly".to_string(), now - 60);
        assert_eq!(due_for_update(&subs, &tried, now), ["new"]);
        tried.insert("hourly".to_string(), now - UPDATE_RETRY);
        assert_eq!(due_for_update(&subs, &tried, now), ["hourly", "new"]);
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
