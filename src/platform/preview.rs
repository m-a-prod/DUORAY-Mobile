//! The phone UI in a desktop window, for working on the screens without a
//! phone: subscriptions and pings are real, the VPN only pretends.
//!
//! `DUORAY_PREVIEW_DATA` picks the data directory, `DUORAY_PAGE` the page to
//! open (0..2, or 3 for the first server's editor), and `DUORAY_SHOT=file.png` saves a screenshot and quits.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use slint::ComponentHandle;

use crate::netpolicy::{NetKind, NetworkEnv};
use crate::platform::{AppInfo, Platform};
use crate::service::{self, Host};

struct Preview;

impl Platform for Preview {
    fn start_vpn(&self) {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(700));
            // No TUN here: a negative descriptor makes the service pretend.
            service::on_tun_up(-1);
        });
    }

    fn stop_vpn(&self) {
        std::thread::spawn(service::on_tun_down);
    }

    fn request_wifi_names(&self) {
        service::on_network(NetworkEnv { kind: NetKind::Wifi, ssid: Some("Home".into()) });
    }

    fn open_url(&self, url: &str) {
        eprintln!("open {url}");
    }

    fn list_apps(&self) -> Vec<AppInfo> {
        [("Telegram", "org.telegram.messenger"), ("Сбербанк", "ru.sberbankmobile"), ("Госуслуги", "ru.rostel"), ("YouTube", "com.google.android.youtube")]
            .into_iter()
            .map(|(label, package)| AppInfo { package: package.into(), label: label.into(), icon: String::new() })
            .collect()
    }
}

pub fn main() -> Result<()> {
    let data = match std::env::var_os("DUORAY_PREVIEW_DATA") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME")?).join(".local/share/duoray-mobile-preview"),
    };
    std::fs::create_dir_all(&data)?;
    service::init(Host {
        xray: PathBuf::new(),
        assets: data.join("xray"),
        data,
        device_id: "preview".into(),
        os: "Android",
        os_version: String::new(),
        model: "DUORAY preview".into(),
    });
    service::set_platform(Arc::new(Preview));
    service::on_network(NetworkEnv { kind: NetKind::Wifi, ssid: None });

    let page = std::env::var("DUORAY_PAGE").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let shot = std::env::var_os("DUORAY_SHOT").map(PathBuf::from);
    crate::app::run(|ui| {
        ui.window().set_size(slint::LogicalSize::new(412.0, 892.0));
        if page == 3 {
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(300), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<crate::Actions>().invoke_open_server(0);
                }
            });
        } else {
            ui.set_page(page);
        }
        let Some(path) = shot else { return };
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(2500), move || {
            if let Some(ui) = weak.upgrade()
                && let Err(e) = save_png(&ui, &path)
            {
                eprintln!("screenshot: {e:#}");
            }
            let _ = slint::quit_event_loop();
        });
    })?;
    Ok(())
}

fn save_png(ui: &crate::AppWindow, path: &std::path::Path) -> Result<()> {
    let shot = ui.window().take_snapshot()?;
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, shot.width(), shot.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(shot.as_bytes())?;
    Ok(())
}
