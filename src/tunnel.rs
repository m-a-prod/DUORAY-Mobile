//! xray on the TUN the platform opened (Android's VpnService today): the
//! official xray binary runs as a child process and its own `tun` inbound
//! takes the descriptor from `xray.tun.fd`, the way Material Xray does it.

use std::fs::File;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub struct Paths {
    /// The xray executable (lib/<abi>/libxray.so on Android).
    pub xray: PathBuf,
    /// geoip.dat / geosite.dat.
    pub assets: PathBuf,
    pub config: PathBuf,
    pub log: PathBuf,
    pub pid: PathBuf,
}

/// A running xray. Dropping it does not stop the process: call [`Xray::stop`].
pub struct Xray {
    pid: libc::pid_t,
    /// Set before an intentional stop, so the exit is not reported.
    stopping: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

/// How xray ended when nobody asked it to.
pub struct Exit {
    pub ran_for: Duration,
    pub log_tail: String,
}

impl Xray {
    pub fn stop(self) {
        self.stopping.store(true, Ordering::SeqCst);
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: plain signal to our own child; it is not reaped yet (exited is false).
        unsafe { libc::kill(self.pid, libc::SIGTERM) };
        let (pid, exited) = (self.pid, self.exited);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            if !exited.load(Ordering::SeqCst) {
                // SAFETY: still our unreaped child, so the pid cannot be reused yet.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        });
    }

    /// Stops xray and waits until it is gone (killed after `grace`), so the
    /// next one does not share the TUN with it.
    pub fn stop_wait(self, grace: Duration) {
        let exited = self.exited.clone();
        let pid = self.pid;
        self.stopping.store(true, Ordering::SeqCst);
        if exited.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: our unreaped child (exited is false).
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = Instant::now() + grace;
        while !exited.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !exited.load(Ordering::SeqCst) {
            // SAFETY: still unreaped, see above.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            while !exited.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Starts xray on `tun_fd`. `on_exit` runs on a waiter thread when xray stops
/// without [`Xray::stop`].
pub fn spawn(paths: &Paths, tun_fd: RawFd, on_exit: impl FnOnce(Exit) + Send + 'static) -> Result<Xray> {
    if !paths.xray.exists() {
        bail!("нет xray: {}", paths.xray.display());
    }
    // A duplicate above stdio for the child; close-on-exec until the child
    // clears it, so other processes started meanwhile do not inherit it.
    // SAFETY: fcntl on a descriptor the platform keeps open while we run.
    let child_fd = unsafe { libc::fcntl(tun_fd, libc::F_DUPFD_CLOEXEC, 3) };
    if child_fd < 0 {
        return Err(io::Error::last_os_error()).context("копия дескриптора TUN");
    }
    let result = spawn_with(paths, child_fd, on_exit);
    // SAFETY: the parent's copy; the child has its own after exec.
    unsafe { libc::close(child_fd) };
    result
}

fn spawn_with(paths: &Paths, child_fd: RawFd, on_exit: impl FnOnce(Exit) + Send + 'static) -> Result<Xray> {
    let log = File::create(&paths.log).context("журнал xray")?;
    let mut cmd = Command::new(&paths.xray);
    cmd.args(["run", "-c"])
        .arg(&paths.config)
        .env("xray.tun.fd", child_fd.to_string())
        .env("XRAY_TUN_FD", child_fd.to_string())
        .env("XRAY_LOCATION_ASSET", &paths.assets)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fcntl(child_fd, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            // xray must not outlive the app when the system kills it.
            #[cfg(any(target_os = "android", target_os = "linux"))]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let mut child = spawner::spawn(cmd).context("запуск xray")?;
    let pid = child.id() as libc::pid_t;
    let _ = std::fs::write(&paths.pid, pid.to_string());

    let stopping = Arc::new(AtomicBool::new(false));
    let exited = Arc::new(AtomicBool::new(false));
    let (stop_flag, exit_flag, log_path, pid_path) = (stopping.clone(), exited.clone(), paths.log.clone(), paths.pid.clone());
    let started = Instant::now();
    std::thread::Builder::new().name("xray-waiter".into()).spawn(move || {
        let _ = child.wait();
        exit_flag.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&pid_path);
        if !stop_flag.load(Ordering::SeqCst) {
            on_exit(Exit { ran_for: started.elapsed(), log_tail: log_tail(&log_path, 12) });
        }
    })?;
    Ok(Xray { pid, stopping, exited })
}

/// Starts a throwaway xray without a TUN (pings go through one); the caller
/// kills it.
pub fn spawn_plain(xray: &Path, assets: &Path, config: &Path) -> Result<Child> {
    let mut cmd = Command::new(xray);
    cmd.args(["run", "-c"])
        .arg(config)
        .env("XRAY_LOCATION_ASSET", assets)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only an async-signal-safe call between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            #[cfg(any(target_os = "android", target_os = "linux"))]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    spawner::spawn(cmd).context("запуск xray")
}

/// `xray api <args>` against a running xray's API; its standard output.
pub fn api(xray: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(xray).arg("api").args(args).stdin(Stdio::null()).output().context("xray api")?;
    if !out.status.success() {
        bail!("xray api: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Kills an xray left from a previous run of the app (the app process died
/// but the child survived, which PR_SET_PDEATHSIG makes rare).
pub fn kill_orphan(pid_file: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse::<libc::pid_t>().ok()) else {
        return;
    };
    let ours = std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| String::from_utf8_lossy(&c).contains("libxray"));
    if ours {
        // SAFETY: a process running our xray binary, checked just above.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_file(pid_file);
}

/// The last `lines` lines of xray's log.
pub fn log_tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// The death signal follows the parent *thread*, and connections start from
/// short-lived threads, so children are forked from this one, which lives as
/// long as the process.
mod spawner {
    use std::process::{Child, Command};
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Mutex, OnceLock};

    type Job = (Command, Sender<std::io::Result<Child>>);

    fn queue() -> &'static Mutex<Sender<Job>> {
        static QUEUE: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
        QUEUE.get_or_init(|| {
            let (tx, rx) = channel::<Job>();
            std::thread::Builder::new()
                .name("child-spawner".into())
                .spawn(move || {
                    for (mut cmd, reply) in rx {
                        let _ = reply.send(cmd.spawn());
                    }
                })
                .expect("spawner thread");
            Mutex::new(tx)
        })
    }

    pub fn spawn(cmd: Command) -> std::io::Result<Child> {
        let (tx, rx) = channel();
        let sent = queue().lock().map_err(|_| std::io::Error::other("spawner poisoned"))?.send((cmd, tx));
        sent.map_err(|_| std::io::Error::other("spawner gone"))?;
        rx.recv().map_err(|_| std::io::Error::other("spawner gone"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_skips_blank_lines() {
        let dir = std::env::temp_dir().join(format!("duoray-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("x.log");
        std::fs::write(&log, "a\n\nb\nc\n\n").unwrap();
        assert_eq!(log_tail(&log, 2), "b\nc");
        assert_eq!(log_tail(&log, 10), "a\nb\nc");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn child_gets_the_descriptor() {
        // A shell stands in for xray: it reports the fd number it was given
        // and whether that descriptor is open in it.
        let dir = std::env::temp_dir().join(format!("duoray-fd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("libxray.so");
        std::fs::write(&fake, "#!/bin/sh\nfd=$XRAY_TUN_FD\n[ -e /proc/self/fd/$fd ] && echo open:$fd || echo closed:$fd\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let paths = Paths {
            xray: fake,
            assets: dir.clone(),
            config: dir.join("c.json"),
            log: dir.join("x.log"),
            pid: dir.join("x.pid"),
        };
        let file = File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
        let (tx, rx) = std::sync::mpsc::channel();
        let _x = spawn(&paths, fd, move |e| tx.send(e).unwrap()).unwrap();
        let exit = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(exit.log_tail.starts_with("open:"), "{}", exit.log_tail);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
