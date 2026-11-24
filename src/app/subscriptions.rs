use super::*;
use std::sync::{Mutex, MutexGuard};

pub(super) fn fetch(url: &str, fallback: Option<&str>, headers: &[(String, String)], proxy: Option<&str>) -> Result<subscription::Fetched, String> {
    let direct = || subscription::fetch(url, fallback, headers);
    match proxy {
        Some(proxy) => subscription::fetch_with_proxy(url, fallback, headers, Some(proxy)).or_else(|_| direct()),
        None => direct(),
    }
    .map_err(|e| format!("{e:#}"))
}

// The Android job commits to disk even with the activity suspended. Replay the
// same update on the UI store before it can save an older list over that commit.
pub(crate) struct Update {
    id: String,
    url: String,
    previous: Option<u64>,
    updated: Option<u64>,
    result: subscription::Fetched,
}

static UPDATES: Mutex<Vec<Update>> = Mutex::new(Vec::new());

pub(crate) fn pending() -> MutexGuard<'static, Vec<Update>> {
    UPDATES.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn merge(store: &mut Store, pending: &mut Vec<Update>) -> bool {
    let mut changed = false;
    for update in pending.drain(..) {
        if store.get(&update.id).is_some_and(|s| s.url == update.url && s.updated_at == update.previous) {
            store.apply(&update.id, Ok(update.result));
            if let Some(sub) = store.subscriptions.iter_mut().find(|s| s.id == update.id) {
                sub.updated_at = update.updated;
            }
            changed = true;
        }
    }
    changed
}

#[cfg(target_os = "android")]
pub fn background_refresh(active: impl Fn() -> bool) -> anyhow::Result<()> {
    let Some(host) = service::host() else { return Ok(()) };
    if crate::status::window_open() || !host.store().exists() { return Ok(()) }
    let store = Store::load(&host.store())?;
    let settings = MobileSettings::load(&host.settings());
    let due = servers::due_for_update(&store.subscriptions, &HashMap::new(), &settings.subscription_hours, store::now());
    for id in due {
        if !active() { break }
        let Some(sub) = store.get(&id) else { continue };
        let headers = mobile_headers(identity::subscription_headers(&store.settings.happ, &host.device(), store.settings.send_device_info, &sub.identity));
        let proxy = service::snapshot().link.map(|link| link.proxy_url());
        let Ok(fetched) = fetch(&sub.url, sub.info.fallback_url.as_deref(), &headers, proxy.as_deref()) else { continue };
        let mut pending = pending();
        if !active() { break }
        let mut current = Store::load(&host.store())?;
        if !current.get(&id).is_some_and(|s| s.updated_at == sub.updated_at && s.url == sub.url && s.identity == sub.identity) {
            continue;
        }
        let hours = MobileSettings::load(&host.settings()).subscription_hours;
        if !servers::due_for_update(std::slice::from_ref(sub), &HashMap::new(), &hours, store::now()).contains(&id) {
            continue;
        }
        let copy = subscription::Fetched { info: fetched.info.clone(), servers: fetched.servers.clone(), skipped: fetched.skipped.clone(), json: fetched.json };
        current.apply(&id, Ok(fetched));
        current.save(&host.store())?;
        crate::widgets::write(&host.data, &current);
        let updated = current.get(&id).and_then(|s| s.updated_at);
        // Keep at most the latest result per subscription while the UI sleeps.
        let older = pending.iter().position(|u| u.id == id).map(|i| pending.swap_remove(i));
        let (url, previous) = older.map_or_else(|| (sub.url.clone(), sub.updated_at), |u| (u.url, u.previous));
        pending.push(Update { id, url, previous, updated, result: copy });
    }
    Ok(())
}
