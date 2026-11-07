//! `duoray://` links, as on the desktop (its deeplink.rs): a subscription to
//! add in one click from a Telegram bot or a website. Never added silently:
//! the add sheet opens with the link filled in.
//!
//!   duoray://add/<subscription url>          (raw or percent-encoded)
//!   duoray://add?url=<percent-encoded url>   (also `import` for `add`)

pub const SCHEME: &str = "duoray://";

/// The subscription URL in a `duoray://` link; `None` for anything else.
pub fn subscription(link: &str) -> Option<String> {
    let rest = link.trim().strip_prefix(SCHEME)?;
    let (action, rest) = rest.split_once(['/', '?']).unwrap_or((rest, ""));
    if !matches!(action, "add" | "import") {
        return None;
    }
    let raw = match rest.strip_prefix("url=") {
        Some(q) => q,
        None => rest.split_once("?url=").map_or(rest, |(_, q)| q),
    };
    let decoded = percent_decode(raw);
    let url = url::Url::parse(decoded.trim()).ok()?;
    matches!(url.scheme(), "https" | "http").then(|| url.to_string())
}

fn percent_decode(s: &str) -> String {
    // Already a plain URL (raw form): leave its own escapes alone.
    if s.starts_with("http://") || s.starts_with("https://") {
        return s.to_string();
    }
    url::form_urlencoded::parse(format!("u={s}").as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::subscription;

    #[test]
    fn reads_the_subscription_url() {
        let sub = "https://sub.example.com/abc?x=1";
        assert_eq!(subscription("duoray://add/https://sub.example.com/abc?x=1").as_deref(), Some(sub));
        assert_eq!(subscription("duoray://add/https%3A%2F%2Fsub.example.com%2Fabc%3Fx%3D1").as_deref(), Some(sub));
        assert_eq!(subscription("duoray://add?url=https%3A%2F%2Fsub.example.com%2Fabc%3Fx%3D1").as_deref(), Some(sub));
        assert_eq!(subscription("duoray://open/whatever"), None);
        assert_eq!(subscription("duoray://add/ftp://x"), None);
    }
}
