//! The Android shell: `android_main` runs the Slint window inside
//! MainActivity (a NativeActivity), and the Kotlin side reports the VPN and
//! the network through `space.dualizm.duoray.Native`'s native methods.
//!
//! Rust calls back into Kotlin on the objects it was handed: the activity
//! (for the VPN consent dialog and permissions) and the VpnService (to stop
//! it when xray dies with no window open).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject, JString, JValue};
use jni::refs::{Global, Reference};
use jni::strings::JNIStr;
use jni::sys::{jboolean, jint};
use jni::{Env, EnvUnowned, JavaVM, jni_sig, jni_str};
use slint::android::AndroidApp;

use crate::netpolicy::{NetKind, NetworkEnv, normalize_ssid};
use crate::platform::{AppInfo, Platform};
use crate::service::{self, Host};

type Target = Mutex<Option<Global<JObject<'static>>>>;

/// The MainActivity on screen, while there is one.
static ACTIVITY: Target = Mutex::new(None);
/// Which android_main set ACTIVITY: a replaced activity's thread may end
/// after the new one started, and must leave the new reference alone.
static ACTIVITY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The running DuorayVpnService, while the TUN is open.
static VPN: Target = Mutex::new(None);
/// `space.dualizm.duoray.Native`, found while a Java thread called in: a
/// thread Rust started cannot look the app's classes up.
static NATIVE: Mutex<Option<Global<JClass<'static>>>> = Mutex::new(None);

pub fn main(app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("duoray"));
    // SAFETY: android-activity hands over the process's JavaVM.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let activity = vm.attach_current_thread(|env| -> jni::errors::Result<_> {
        // SAFETY: android-activity holds a global reference to the activity
        // for as long as it lives; we take our own below.
        let local = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        env.new_global_ref(&local)
    });
    let generation = ACTIVITY_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    match activity {
        Ok(a) => *lock(&ACTIVITY) = Some(a),
        Err(e) => log::error!("activity reference: {e}"),
    }
    service::set_platform(Arc::new(Android));
    if let Err(e) = slint::android::init(app) {
        log::error!("slint: {e}");
        return;
    }
    if let Err(e) = crate::app::run(|_| {}) {
        log::error!("window: {e}");
    }
    let mut current = lock(&ACTIVITY);
    if ACTIVITY_GEN.load(std::sync::atomic::Ordering::SeqCst) == generation {
        *current = None;
    }
}

fn lock(target: &Target) -> std::sync::MutexGuard<'_, Option<Global<JObject<'static>>>> {
    target.lock().unwrap_or_else(|e| e.into_inner())
}

/// Calls `void name()` or `void name(String)` on `target`, if it is there.
fn call(target: &Target, name: &JNIStr, arg: Option<&str>) -> bool {
    let guard = lock(target);
    let Some(obj) = guard.as_ref() else { return false };
    let Ok(vm) = JavaVM::singleton() else { return false };
    let result = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        match arg {
            None => env.call_method(obj, name, jni_sig!("()V"), &[])?,
            Some(s) => {
                let s = env.new_string(s)?;
                env.call_method(obj, name, jni_sig!("(Ljava/lang/String;)V"), &[(&s).into()])?
            }
        };
        Ok(())
    });
    if let Err(e) = &result {
        log::warn!("call into Kotlin failed: {e}");
    }
    result.is_ok()
}

/// Calls `static void name(String)` on Native (works with no window open).
fn call_static(name: &JNIStr, arg: &str) -> bool {
    let guard = NATIVE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(class) = guard.as_ref() else { return false };
    let Ok(vm) = JavaVM::singleton() else { return false };
    let result = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let s = env.new_string(arg)?;
        env.call_static_method(class, name, jni_sig!("(Ljava/lang/String;)V"), &[(&s).into()])?;
        Ok(())
    });
    if let Err(e) = &result {
        log::warn!("static call into Kotlin failed: {e}");
    }
    result.is_ok()
}

/// Calls `void name(boolean)` on `target`, if it is there.
fn call_bool(target: &Target, name: &JNIStr, arg: bool) -> bool {
    let guard = lock(target);
    let Some(obj) = guard.as_ref() else { return false };
    let Ok(vm) = JavaVM::singleton() else { return false };
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        env.call_method(obj, name, jni_sig!("(Z)V"), &[JValue::Bool(arg)])?;
        Ok(())
    })
    .is_ok()
}

struct Android;

impl Platform for Android {
    fn start_vpn(&self) {
        // No window (the tile, a shortcut, always-on): the service starts by
        // itself when the VPN is allowed already, else the app opens to ask.
        if !call(&ACTIVITY, jni_str!("startVpn"), None) && !call_static(jni_str!("startVpnInBackground"), "") {
            service::on_vpn_denied();
        }
    }

    fn show_status(&self, packed: &str) {
        call_static(jni_str!("onStatus"), packed);
    }

    fn set_back_enabled(&self, on: bool) {
        call_bool(&ACTIVITY, jni_str!("setBackEnabled"), on);
    }

    fn request_local_network(&self) {
        call(&ACTIVITY, jni_str!("requestLocalNetwork"), None);
    }

    fn request_background(&self) {
        call(&ACTIVITY, jni_str!("requestBackground"), None);
    }

    fn set_light_bars(&self, light: bool) {
        call_bool(&ACTIVITY, jni_str!("setLightBars"), light);
    }

    fn open_settings(&self, which: &str) {
        call(&ACTIVITY, jni_str!("openSettings"), Some(which));
    }

    fn stop_vpn(&self) {
        if !call(&VPN, jni_str!("stopFromNative"), None) {
            call(&ACTIVITY, jni_str!("stopVpn"), None);
        }
    }

    fn rebuild_vpn(&self) {
        call(&VPN, jni_str!("rebuildFromNative"), None);
    }

    fn request_wifi_names(&self) {
        call(&ACTIVITY, jni_str!("requestWifiNames"), None);
    }

    fn scan_qr(&self) {
        call(&ACTIVITY, jni_str!("scanQr"), None);
    }

    fn copy_text(&self, text: &str) {
        call(&ACTIVITY, jni_str!("copyText"), Some(text));
    }

    fn haptic(&self) {
        call(&ACTIVITY, jni_str!("haptic"), None);
    }

    fn open_url(&self, url: &str) {
        call(&ACTIVITY, jni_str!("openUrl"), Some(url));
    }

    fn install_apk(&self, path: &str) {
        call(&ACTIVITY, jni_str!("installApk"), Some(path));
    }

    /// The activity lists launchable apps into files/apps.json (icons as
    /// PNGs next to it), on this thread.
    fn list_apps(&self) -> Vec<AppInfo> {
        if !call(&ACTIVITY, jni_str!("listApps"), None) {
            return Vec::new();
        }
        let Some(host) = service::host() else { return Vec::new() };
        std::fs::read_to_string(host.data.join("apps.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }
}

fn string(env: &Env, s: &JString) -> jni::errors::Result<String> {
    s.try_to_string(env)
}

/// `Native.init(filesDir, nativeLibraryDir, androidId, osVersion, model)`:
/// first thing the activity and the VPN service do.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_init<'a>(
    mut env: EnvUnowned<'a>,
    _class: JClass<'a>,
    files_dir: JString<'a>,
    lib_dir: JString<'a>,
    device_id: JString<'a>,
    os_version: JString<'a>,
    model: JString<'a>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("duoray"));
        {
            let mut native = NATIVE.lock().unwrap_or_else(|e| e.into_inner());
            if native.is_none() {
                let class = env.find_class(jni_str!("space/dualizm/duoray/Native"))?;
                *native = Some(env.new_global_ref(&class)?);
            }
        }
        if service::platform().is_none() {
            // The tile or always-on may start the process with no window.
            service::set_platform(Arc::new(Android));
        }
        let data = PathBuf::from(string(env, &files_dir)?);
        service::init(Host {
            xray: PathBuf::from(string(env, &lib_dir)?).join("libxray.so"),
            assets: data.join("xray"),
            data,
            device_id: string(env, &device_id)?,
            os: "Android",
            os_version: string(env, &os_version)?,
            model: string(env, &model)?,
        });
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.tunUp(service, fd)`: the VpnService opened the TUN.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_tunUp<'a>(
    mut env: EnvUnowned<'a>,
    _class: JClass<'a>,
    vpn: JObject<'a>,
    fd: jint,
) -> jboolean {
    env.with_env(|env| -> jni::errors::Result<jboolean> {
        *lock(&VPN) = Some(env.new_global_ref(&vpn)?);
        Ok(service::on_tun_up(fd))
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.tunDown()`: the VpnService closed the TUN.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_tunDown<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        *lock(&VPN) = None;
        service::on_tun_down();
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.vpnDenied()`: the user declined the VPN dialog.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_vpnDenied<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        service::on_vpn_denied();
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.networkChanged(kind, ssid)`: kind 0 offline, 1 Wi-Fi, 2 mobile,
/// 3 other; ssid as Android reports it (quoted), or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_networkChanged<'a>(
    mut env: EnvUnowned<'a>,
    _class: JClass<'a>,
    kind: jint,
    ssid: JString<'a>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let kind = NetKind::from_code(kind);
        let ssid = if ssid.is_null() { None } else { normalize_ssid(&string(env, &ssid)?) };
        service::on_network(NetworkEnv { kind, ssid: ssid.filter(|_| kind == NetKind::Wifi) });
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.openLink(url)`: the app was opened with a `duoray://` link.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_openLink<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>, link: JString<'a>) {
    env.with_env(|env| -> jni::errors::Result<()> {
        crate::open_link(&string(env, &link)?);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.scanned(text)`: what the QR scanner read.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_scanned<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>, text: JString<'a>) {
    env.with_env(|env| -> jni::errors::Result<()> {
        crate::scanned(&string(env, &text)?);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.back()`: the system Back, while the window takes it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_back<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        crate::back();
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.toggle()`: the Quick Settings tile or the launcher shortcut.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_toggle<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        std::thread::spawn(crate::toggle);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.wifiAccess(code)`: whether Wi-Fi names are readable (see
/// `service::wifi_access`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_wifiAccess<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>, code: jint) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        service::set_wifi_access(code.clamp(0, 3) as u8);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.batteryUnrestricted(on)`: whether Android leaves DUORAY alone in
/// the background (battery optimization off).
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_batteryUnrestricted<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>, on: jboolean) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        crate::app::set_battery_unrestricted(on);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// `Native.localNetwork(granted)`: Android 17's permission for the LAN,
/// which the hotspot proxy needs.
#[unsafe(no_mangle)]
pub extern "system" fn Java_space_dualizm_duoray_Native_localNetwork<'a>(mut env: EnvUnowned<'a>, _class: JClass<'a>, granted: jboolean) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        crate::app::set_local_network(granted);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
