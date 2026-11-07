//! Country flags for server rows (Twemoji SVGs from assets/flags, built in).

use std::cell::RefCell;
use std::collections::HashMap;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/flags.rs"));
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Option<slint::Image>>> = RefCell::default();
}

/// The decoded flag for an ISO code ("SE"); `None` when there is none.
pub fn image(code: &str) -> Option<slint::Image> {
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(code.to_string())
            .or_insert_with(|| embedded::flag_svg(code).and_then(|svg| slint::Image::load_from_svg_data(svg).ok()))
            .clone()
    })
}
