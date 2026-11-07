//! Where a live connection comes out and how much goes through it, as the
//! desktop app shows it (its exit.rs): the node a balancer picked and the
//! traffic come from xray's API, the exit country from Cloudflare's trace
//! fetched through xray's SOCKS inbound.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use duoray_core::balancer::{self, Trace};

use crate::servers::bytes;
use crate::service::Link;
use crate::tunnel;

const TRACE_URL: &str = "https://1.1.1.1/cdn-cgi/trace";

/// Cloudflare's view of the connection.
pub fn trace(link: &Link) -> Result<Trace> {
    let proxy = ureq::Proxy::new(&format!("socks5://{}:{}@{}", link.user, link.pass, link.socks))?;
    let agent: ureq::Agent = ureq::Agent::config_builder().proxy(Some(proxy)).timeout_global(Some(Duration::from_secs(6))).build().into();
    let body = agent.get(TRACE_URL).call()?.body_mut().read_to_string()?;
    balancer::parse_trace(&body).context("no loc in the trace")
}

/// Bytes through the TUN inbound since xray started: (uplink, downlink).
pub fn counters(xray: &Path, link: &Link) -> Result<(u64, u64)> {
    let out = tunnel::api(xray, &["statsquery", &format!("--server={}", link.api), "-t", "2", "-pattern", "inbound>>>socks>>>traffic"])?;
    Ok(balancer::parse_inbound_traffic(&out, "socks").unwrap_or((0, 0)))
}

/// Traffic of the session, from successive counter readings.
#[derive(Debug, Clone)]
pub struct Traffic {
    at: Instant,
    counters: (u64, u64),
    session: (u64, u64),
    rate: (f64, f64),
}

impl Traffic {
    pub fn read(prev: Option<&Traffic>, now: Instant, (up, down): (u64, u64)) -> Traffic {
        let Some(p) = prev else {
            return Traffic { at: now, counters: (up, down), session: (up, down), rate: (0.0, 0.0) };
        };
        // A counter that went down means xray started again: count from zero.
        let delta = |now: u64, before: u64| now.checked_sub(before).unwrap_or(now);
        let (du, dd) = (delta(up, p.counters.0), delta(down, p.counters.1));
        let secs = now.duration_since(p.at).as_secs_f64().max(0.001);
        Traffic { at: now, counters: (up, down), session: (p.session.0 + du, p.session.1 + dd), rate: (du as f64 / secs, dd as f64 / secs) }
    }

    /// "↓ 2.4 МБ/с  ↑ 120.0 КБ/с": just the speeds, for the narrow card.
    pub fn speeds(&self) -> String {
        format!("↓ {}/с   ↑ {}/с", bytes(self.rate.1 as u64), bytes(self.rate.0 as u64))
    }

    /// "↓ 2.4 МБ/с · ↑ 120.0 КБ/с · 1.2 ГБ"
    pub fn label(&self) -> String {
        let rate = |b: f64| format!("{}/с", bytes(b as u64));
        format!("↓ {} · ↑ {} · {}", rate(self.rate.1), rate(self.rate.0), bytes(self.session.0 + self.session.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_counts_across_restarts() {
        let t0 = Instant::now();
        let a = Traffic::read(None, t0, (100, 1000));
        let b = Traffic::read(Some(&a), t0 + Duration::from_secs(1), (300, 3000));
        assert_eq!(b.session, (300, 3000));
        assert_eq!(b.rate, (200.0, 2000.0));
        // xray restarted: counters start over.
        let c = Traffic::read(Some(&b), t0 + Duration::from_secs(2), (50, 500));
        assert_eq!(c.session, (350, 3500));
        assert!(c.label().starts_with("↓ 500 Б/с · ↑ 50 Б/с"));
    }
}
