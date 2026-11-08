//! What the home screen widgets (android/.../widget/) show besides the
//! connection itself: the app's theme and the pinned servers, written to
//! files/widgets.json whenever either changes. The connection state comes
//! with the notification's (status::Notice).

use std::path::Path;
use std::sync::Mutex;

use duoray_core::server::display_name;
use duoray_core::store::Store;
use serde_json::{Value, json};

use crate::{servers, status};

/// Pinned servers a widget can show at most.
const FAVORITES: usize = 6;

/// What was written last: most store saves change nothing the widgets show.
static LAST: Mutex<String> = Mutex::new(String::new());

pub fn write(data_dir: &Path, store: &Store) {
    let entry = |key: &str| {
        let (_, server) = servers::find(store, key)?;
        let (flag, name) = display_name(&server.name);
        Some(json!({ "key": key, "name": name, "flag": flag.as_deref().map(status::flag_emoji).unwrap_or_default() }))
    };
    let favorites: Vec<Value> = store.settings.favorites.iter().filter_map(|k| entry(k)).take(FAVORITES).collect();
    let last = store.settings.last_server.as_deref().and_then(entry);
    let data = json!({
        "theme": crate::themes::widget_colors(&store.settings.theme),
        "favorites": favorites,
        "last": last,
    });
    let text = data.to_string();
    let path = data_dir.join("widgets.json");
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == text && path.exists() {
            return;
        }
        last.clone_from(&text);
    }
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        log::warn!("widgets.json: {e}");
    }
    if let Some(p) = crate::service::platform() {
        p.refresh_widgets();
    }
}
