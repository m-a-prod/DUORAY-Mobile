//! «Ядро Xray»: the bundled core or an official release downloaded from
//! GitHub (src/core.rs), as MaterialXray picks its core.

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{App, later};
use crate::core::{self, Release};
use crate::service::{self, Status};
use crate::{AppState, CoreRow, tunnel};

#[derive(Default)]
pub(super) struct Cores {
    releases: Vec<Release>,
    loading: bool,
    error: String,
    /// The download under way: (tag, "47%").
    busy: Option<(String, String)>,
    /// `xray version` of the core in the APK.
    bundled: Option<String>,
}

impl App {
    pub(super) fn open_cores(&mut self) {
        self.render_cores();
        if self.cores.bundled.is_none() {
            let xray = self.host.xray.clone();
            std::thread::spawn(move || {
                let out = std::process::Command::new(&xray).arg("version").output().ok();
                let version = out.and_then(|o| core::parse_version(&String::from_utf8_lossy(&o.stdout)));
                later(move |app| {
                    app.cores.bundled = version;
                    app.render_cores();
                    app.render_settings();
                });
            });
        }
        if self.cores.releases.is_empty() && !self.cores.loading {
            self.cores.loading = true;
            self.cores.error.clear();
            self.render_cores();
            let proxy = self.conn.link.as_ref().map(service::Link::proxy_url);
            std::thread::spawn(move || {
                // Through the tunnel while connected, else (or failing that) directly.
                let found = proxy.as_deref().map(|p| core::releases(Some(p))).filter(Result::is_ok).unwrap_or_else(|| core::releases(None));
                later(move |app| {
                    app.cores.loading = false;
                    match found {
                        Ok(list) => app.cores.releases = list,
                        Err(e) => app.cores.error = format!("Не удалось получить список версий: {e:#}"),
                    }
                    app.render_cores();
                });
            });
        }
    }

    /// "Встроенное · 26.10.31" or the picked release, for the settings row.
    pub(super) fn core_label(&self) -> String {
        match self.selected_core() {
            Some(tag) => format!("{tag} · скачанное"),
            None => match &self.cores.bundled {
                Some(v) => format!("Встроенное · {v}"),
                None => "Встроенное".into(),
            },
        }
    }

    /// The picked release, if it is still installed.
    fn selected_core(&self) -> Option<&str> {
        self.settings.core.as_deref().filter(|tag| core::executable(&self.host.data, tag).is_file())
    }

    pub(super) fn render_cores(&self) {
        let Some(ui) = self.ui() else { return };
        let installed = core::installed(&self.host.data);
        let selected = self.selected_core();
        let progress = |tag: &str| self.cores.busy.as_ref().filter(|(t, _)| t == tag).map(|(_, p)| p.clone()).unwrap_or_default();
        let release = |tag: &str| self.cores.releases.iter().find(|r| r.tag == tag);
        let mut rows = vec![CoreRow {
            tag: "".into(),
            detail: match &self.cores.bundled {
                Some(v) => format!("В приложении · {v}").into(),
                None => "В приложении".into(),
            },
            pre: false,
            installed: true,
            selected: selected.is_none(),
            progress: "".into(),
        }];
        for tag in &installed {
            let r = release(tag);
            rows.push(CoreRow {
                tag: tag.as_str().into(),
                detail: match r {
                    Some(r) => format!("Скачано · {}", r.date).into(),
                    None => "Скачано".into(),
                },
                pre: r.is_some_and(|r| r.prerelease),
                installed: true,
                selected: selected == Some(tag.as_str()),
                progress: "".into(),
            });
        }
        for r in self.cores.releases.iter().filter(|r| !installed.contains(&r.tag)) {
            rows.push(CoreRow {
                tag: r.tag.as_str().into(),
                detail: format!("{} · {:.1} МБ", r.date, r.size as f64 / 1_048_576.0).into(),
                pre: r.prerelease,
                installed: false,
                selected: false,
                progress: progress(&r.tag).into(),
            });
        }
        let st = ui.global::<AppState>();
        st.set_cores(ModelRc::new(VecModel::from(rows)));
        st.set_cores_loading(self.cores.loading);
        st.set_cores_error(self.cores.error.as_str().into());
        st.set_core_label(self.core_label().into());
    }

    pub(super) fn install_core(&mut self, tag: &str) {
        if self.cores.busy.is_some() {
            return self.toast("Дождитесь загрузки другого ядра".into());
        }
        self.cores.busy = Some((tag.to_string(), "0%".into()));
        self.cores.error.clear();
        self.render_cores();
        let (data, tag, proxy) = (self.host.data.clone(), tag.to_string(), self.conn.link.as_ref().map(service::Link::proxy_url));
        std::thread::spawn(move || {
            let shown = std::sync::atomic::AtomicU64::new(0);
            let progress = |done: u64, total: u64| {
                let pct = if total > 0 { done * 100 / total } else { 0 };
                if pct >= shown.load(std::sync::atomic::Ordering::Relaxed) + 5 {
                    shown.store(pct, std::sync::atomic::Ordering::Relaxed);
                    later(move |app| {
                        if let Some((_, p)) = app.cores.busy.as_mut() {
                            *p = format!("{pct}%");
                        }
                        app.render_cores();
                    });
                }
            };
            let result = match proxy.as_deref() {
                Some(p) => core::install(&data, &tag, Some(p), &progress).or_else(|_| core::install(&data, &tag, None, &progress)),
                None => core::install(&data, &tag, None, &progress),
            };
            later(move |app| {
                app.cores.busy = None;
                match result {
                    Ok(_) => app.select_core(&tag),
                    Err(e) => {
                        app.cores.error = format!("Не удалось скачать {tag}: {e:#}");
                        app.render_cores();
                    }
                }
            });
        });
    }

    /// `tag` "": the bundled core. A live connection restarts on the new one.
    pub(super) fn select_core(&mut self, tag: &str) {
        let tag = (!tag.is_empty()).then(|| tag.to_string());
        self.settings.core = tag.clone();
        self.save_settings();
        tunnel::select_core(tag.as_deref().map(|t| core::executable(&self.host.data, t)));
        self.render_cores();
        self.render_settings();
        self.toast(format!("Ядро: {}", self.core_label()));
        if matches!(self.conn.status, Status::On | Status::Connecting) {
            service::reconnect();
        }
    }

    pub(super) fn remove_core(&mut self, tag: &str) {
        if self.selected_core() == Some(tag) {
            return self.toast("Сначала выберите другое ядро".into());
        }
        if let Err(e) = core::remove(&self.host.data, tag) {
            self.toast(format!("{e:#}"));
        }
        self.render_cores();
    }
}
