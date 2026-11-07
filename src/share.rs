//! «Доступ через хотспот»: xray also listens on every interface, SOCKS5 on
//! the port and HTTP on the next one, so devices on the same Wi-Fi or on the
//! phone's hotspot can set the phone as their proxy and go through its
//! tunnel. Android does not route tethered devices through a VPN by itself.

use std::net::Ipv4Addr;

use serde_json::{Value, json};

use crate::settings::ShareProxy;

pub const SOCKS_TAG: &str = "share-socks";
pub const HTTP_TAG: &str = "share-http";

/// Adds the shared inbounds to a mobile config: they take the TUN's
/// sniffing and every rule that names the TUN inbound ("socks"), so they
/// route the same as the phone's own traffic.
pub fn add_inbounds(config: &mut Value, share: &ShareProxy) {
    if !share.enabled {
        return;
    }
    let sniffing = config["inbounds"]
        .as_array()
        .and_then(|list| list.iter().find(|i| i["tag"] == "socks"))
        .and_then(|i| i.get("sniffing").cloned())
        .unwrap_or_else(|| json!({ "enabled": true, "destOverride": ["http", "tls", "quic"] }));
    let account = json!([{ "user": share.user, "pass": share.pass }]);
    let socks = if share.auth {
        json!({ "auth": "password", "accounts": account, "udp": true })
    } else {
        json!({ "auth": "noauth", "udp": true })
    };
    let http = if share.auth { json!({ "accounts": account }) } else { json!({}) };
    if let Some(inbounds) = config["inbounds"].as_array_mut() {
        inbounds.push(json!({
            "tag": SOCKS_TAG, "listen": "0.0.0.0", "port": share.port, "protocol": "socks",
            "settings": socks, "sniffing": sniffing
        }));
        inbounds.push(json!({
            "tag": HTTP_TAG, "listen": "0.0.0.0", "port": share.port.saturating_add(1), "protocol": "http",
            "settings": http, "sniffing": sniffing
        }));
    }
    if let Some(rules) = config["routing"]["rules"].as_array_mut() {
        for rule in rules {
            if let Some(tags) = rule["inboundTag"].as_array_mut()
                && tags.iter().any(|t| t == "socks")
            {
                tags.push(json!(SOCKS_TAG));
                tags.push(json!(HTTP_TAG));
            }
        }
    }
}

/// The phone's addresses other devices can reach: Wi-Fi, the hotspot, USB
/// and Bluetooth tethering. Mobile data and the VPN itself are left out.
pub fn lan_addresses() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    #[cfg(unix)]
    // SAFETY: getifaddrs fills a list that freeifaddrs releases; every
    // pointer read is checked for null and the list outlives the loop.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return found;
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || i32::from((*ifa.ifa_addr).sa_family) != libc::AF_INET || ifa.ifa_name.is_null() {
                continue;
            }
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy();
            let addr = &*(ifa.ifa_addr as *const libc::sockaddr_in);
            let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
            if shared_interface(&name) && !ip.is_loopback() && !found.contains(&ip) {
                found.push(ip);
            }
        }
        libc::freeifaddrs(head);
    }
    found
}

fn shared_interface(name: &str) -> bool {
    ["wlan", "swlan", "ap", "softap", "rndis", "usb", "ncm", "bt-pan", "eth"].iter().any(|p| name.starts_with(p))
}

/// What the settings show: SOCKS5 and HTTP per address, or how to get one.
pub fn describe(share: &ShareProxy) -> String {
    let ips = lan_addresses();
    if ips.is_empty() {
        return "Подключите телефон к Wi-Fi или включите точку доступа: здесь появится адрес прокси.".into();
    }
    ips.iter()
        .map(|ip| format!("SOCKS5 {ip}:{}\nHTTP   {ip}:{}", share.port, share.port.saturating_add(1)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(enabled: bool, auth: bool) -> ShareProxy {
        ShareProxy { enabled, auth, user: "u".into(), pass: "p".into(), port: 10808 }
    }

    #[test]
    fn shared_inbounds_route_like_the_tun() {
        let base = json!({
            "inbounds": [{ "tag": "socks", "protocol": "tun", "sniffing": { "enabled": true } }, { "tag": "duoray-app" }],
            "routing": { "rules": [{ "inboundTag": ["socks"], "port": "53" }, { "inboundTag": ["duoray-app"] }, { "domain": ["x"] }] }
        });
        let mut off = base.clone();
        add_inbounds(&mut off, &share(false, true));
        assert_eq!(off, base);

        let mut c = base.clone();
        add_inbounds(&mut c, &share(true, true));
        let inbounds = c["inbounds"].as_array().unwrap();
        assert_eq!((inbounds[2]["port"].as_u64(), inbounds[3]["port"].as_u64()), (Some(10808), Some(10809)));
        assert_eq!(inbounds[2]["listen"], "0.0.0.0");
        assert_eq!(inbounds[2]["settings"]["auth"], "password");
        assert_eq!(inbounds[3]["settings"]["accounts"][0]["user"], "u");
        assert_eq!(inbounds[2]["sniffing"]["enabled"], true);
        assert_eq!(c["routing"]["rules"][0]["inboundTag"], json!(["socks", SOCKS_TAG, HTTP_TAG]));
        assert_eq!(c["routing"]["rules"][1]["inboundTag"], json!(["duoray-app"]));

        let mut open = base;
        add_inbounds(&mut open, &share(true, false));
        assert_eq!(open["inbounds"][2]["settings"]["auth"], "noauth");
        assert!(open["inbounds"][3]["settings"].get("accounts").is_none());
    }

    #[test]
    fn only_lan_interfaces() {
        assert!(shared_interface("wlan0") && shared_interface("ap0") && shared_interface("rndis0"));
        assert!(!shared_interface("rmnet_data1") && !shared_interface("tun0") && !shared_interface("lo"));
    }
}
