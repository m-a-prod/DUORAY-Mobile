//! Self-update from the hub, as on the desktop (duoray-gui/src/update.rs).
//!
//! The desktop's signed manifest carries an `android` section with its own
//! version and one APK per ABI (scripts/release.sh adds it, the desktop's
//! release keeps it). A download is used only if the manifest signature
//! checks out and the file matches the manifest's SHA-256 and size, so a
//! compromised server cannot push code; Android then shows its own install
//! prompt and checks that the APK is signed by the same key as this one.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::diag::{self, USER_AGENT};

/// Git commit of this build (see build.rs).
pub const BUILD: &str = env!("DUORAY_BUILD");

/// Public half of ~/.config/duoray-release/update-signing.pem (the desktop's key).
const PUBLIC_KEY: [u8; 32] = hex32("e2d7d74a8fffaee8f4c3bb9f566f7bb2a533afd23755eb671bdc6fe280f478ab");

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    /// Missing until the first mobile release.
    #[serde(default)]
    android: Option<Release>,
}

#[derive(Debug, Clone, Deserialize)]
struct Release {
    version: String,
    /// Git commit the builds were made from.
    #[serde(default)]
    build: String,
    #[serde(default)]
    notes: String,
    assets: BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

/// A newer version with an APK for this phone.
#[derive(Debug, Clone)]
pub struct Found {
    /// The hub that served the manifest; downloads try it first.
    pub hub: String,
    pub version: String,
    pub notes: String,
    pub asset: Asset,
}

/// Manifest key of this build: `android-<abi>-apk`.
fn platform() -> String {
    let abi = match std::env::consts::ARCH {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86" => "x86",
        other => other,
    };
    format!("android-{abi}-apk")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}

fn get(url: &str) -> Result<Vec<u8>> {
    let mut body = agent(Duration::from_secs(20))
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("запрос {}", url.rsplit('/').next().unwrap_or(url)))?
        .into_body();
    Ok(body.with_config().limit(1024 * 1024).read_to_vec()?)
}

/// Asks the hubs for a newer version, mirrors in turn. `Ok(None)`: up to date.
pub fn check() -> Result<Option<Found>> {
    let mut last = anyhow::anyhow!("нет серверов обновлений");
    for hub in diag::hubs() {
        match check_at(&hub) {
            Ok(m) => {
                diag::hub_ok(&hub);
                return Ok(select(m, env!("CARGO_PKG_VERSION"), BUILD, &platform()).map(|f| Found { hub, ..f }));
            }
            // A mirror with a bad signature is skipped like a dead one.
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn check_at(hub: &str) -> Result<Manifest> {
    let base = format!("{hub}/v1/update");
    let manifest = get(&format!("{base}/manifest.json"))?;
    let sig = get(&format!("{base}/manifest.sig"))?;
    verify(&manifest, &String::from_utf8_lossy(&sig), &PUBLIC_KEY)?;
    serde_json::from_slice(&manifest).context("манифест обновления")
}

/// Offered: a newer version, or the same version built from another commit
/// (a fix shipped without a version bump), and only with an APK for this ABI.
fn select(m: Manifest, current: &str, build: &str, key: &str) -> Option<Found> {
    let r = m.android?;
    let same_version = !newer(&r.version, current) && !newer(current, &r.version);
    let rebuilt = same_version && !r.build.is_empty() && r.build != build;
    if !newer(&r.version, current) && !rebuilt {
        return None;
    }
    let asset = r.assets.get(key)?.clone();
    Some(Found { hub: String::new(), version: r.version, notes: r.notes, asset })
}

fn verify(data: &[u8], sig_hex: &str, key: &[u8; 32]) -> Result<()> {
    let sig = hex(sig_hex.trim()).context("подпись манифеста")?;
    let sig = Signature::from_slice(&sig).context("подпись манифеста")?;
    VerifyingKey::from_bytes(key)?
        .verify_strict(data, &sig)
        .map_err(|_| anyhow::anyhow!("подпись манифеста не сходится"))
}

/// `a > b` for dotted versions; a pre-release suffix ("0.4.0-rc1") ranks lower.
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p.to_string())));
        let nums: Vec<u64> = core.split('.').map(|n| n.parse().unwrap_or(0)).collect();
        (nums, pre)
    };
    let ((mut na, pa), (mut nb, pb)) = (parse(a), parse(b));
    let len = na.len().max(nb.len());
    na.resize(len, 0);
    nb.resize(len, 0);
    match na.cmp(&nb) {
        std::cmp::Ordering::Equal => match (pa, pb) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
        o => o.is_gt(),
    }
}

/// Downloads `asset` into `dir` (reusing a verified earlier download), from
/// `hub` first and then the mirrors. `progress(done, total)` reports bytes.
pub fn download(asset: &Asset, hub: &str, dir: &Path, progress: impl Fn(u64, u64)) -> Result<PathBuf> {
    let mut order = vec![hub.to_string()];
    order.extend(diag::hubs().into_iter().filter(|h| h != hub));
    let mut last = anyhow::anyhow!("нет серверов обновлений");
    for h in order.iter().filter(|h| !h.is_empty()) {
        match download_from(asset, h, dir, &progress) {
            Ok(path) => return Ok(path),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn download_from(asset: &Asset, hub: &str, dir: &Path, progress: &impl Fn(u64, u64)) -> Result<PathBuf> {
    if asset.file.is_empty() || !asset.file.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        bail!("неверное имя файла в манифесте");
    }
    std::fs::create_dir_all(dir)?;
    let target = dir.join(&asset.file);
    if sha256_file(&target).is_ok_and(|h| h.eq_ignore_ascii_case(&asset.sha256)) {
        return Ok(target);
    }
    // Older downloads are of no use any more.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let url = format!("{hub}/v1/update/files/{}", asset.file);
    let mut body = agent(Duration::from_secs(30 * 60))
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .call()
        .context("загрузка обновления")?
        .into_body();
    let part = dir.join(format!("{}.part", asset.file));
    let mut out = std::fs::File::create(&part)?;
    let mut reader = body.as_reader();
    let (mut hasher, mut done, mut buf) = (Sha256::new(), 0u64, vec![0u8; 256 * 1024]);
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > asset.size {
            bail!("файл обновления больше заявленного");
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        progress(done, asset.size);
    }
    out.sync_all()?;
    drop(out);
    let hash = to_hex(&hasher.finalize());
    if done != asset.size || !hash.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(&part);
        bail!("файл обновления повреждён (контрольная сумма не сходится)");
    }
    std::fs::rename(&part, &target)?;
    Ok(target)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("нечётная длина");
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("не hex")).collect()
}

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("bad hex"),
    }
}

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = hex_digit(b[2 * i]) << 4 | hex_digit(b[2 * i + 1]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn versions() {
        assert!(newer("0.3.1", "0.3.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
        assert!(newer("0.4.0", "0.4.0-rc1"));
        assert!(!newer("0.4.0-rc1", "0.4.0"));
        assert!(newer("v0.3.1", "0.3.0"));
    }

    #[test]
    fn signature_and_selection() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = key.verifying_key().to_bytes();
        // The desktop's manifest with the android section added.
        let manifest = br#"{"version":"0.4.5","assets":{"windows-x86_64-setup":{"file":"a.exe","sha256":"00","size":1}},
            "android":{"version":"0.2.0","assets":{"android-arm64-v8a-apk":{"file":"DUORAY-0.2.0-arm64-v8a.apk","sha256":"00","size":1}}}}"#;
        let sig = to_hex(&key.sign(manifest).to_bytes());
        assert!(verify(manifest, &sig, &public).is_ok());
        let mut tampered = manifest.to_vec();
        tampered[13] = b'9';
        assert!(verify(&tampered, &sig, &public).is_err(), "edited manifest");
        assert!(verify(manifest, &sig, &PUBLIC_KEY).is_err(), "other key");
        assert!(verify(manifest, "zz", &public).is_err());

        let m: Manifest = serde_json::from_slice(manifest).unwrap();
        let arm = "android-arm64-v8a-apk";
        let found = select(m.clone(), "0.1.0", "abc", arm).unwrap();
        assert_eq!((found.version.as_str(), found.asset.file.as_str()), ("0.2.0", "DUORAY-0.2.0-arm64-v8a.apk"));
        assert!(select(m.clone(), "0.1.0", "abc", "android-x86_64-apk").is_none(), "no APK for this ABI");
        assert!(select(m.clone(), "0.2.0", "abc", arm).is_none(), "same version, manifest without build");
        assert!(select(m.clone(), "0.3.0", "abc", arm).is_none(), "older than installed");

        // Same version: offered only when built from another commit.
        let mut rebuilt = m.clone();
        rebuilt.android.as_mut().unwrap().build = "def".into();
        assert!(select(rebuilt.clone(), "0.2.0", "abc", arm).is_some());
        assert!(select(rebuilt, "0.2.0", "def", arm).is_none(), "re-uploaded same build");

        // Desktop-only manifests (before the first mobile release).
        let desktop: Manifest = serde_json::from_str(r#"{"version":"0.4.5","assets":{}}"#).unwrap();
        assert!(select(desktop, "0.1.0", "abc", arm).is_none());
    }

    #[test]
    fn downloads_are_checked_against_the_manifest() {
        let dir = std::env::temp_dir().join(format!("duoray-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body = "fake apk";
        let good = Asset { file: "DUORAY-0.2.0-arm64-v8a.apk".into(), sha256: to_hex(&Sha256::digest(body)), size: body.len() as u64 };

        let (hub, request) = crate::testing::fake_hub("200 OK", body);
        let path = download_from(&good, &hub, &dir, &|_, _| {}).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let (head, _) = request.join().unwrap();
        assert!(head.starts_with("GET /v1/update/files/DUORAY-0.2.0-arm64-v8a.apk "), "{head}");
        // Verified already: no second download.
        assert_eq!(download_from(&good, "http://127.0.0.1:9", &dir, &|_, _| {}).unwrap(), path);

        let bad = Asset { file: "DUORAY-0.2.1-arm64-v8a.apk".into(), sha256: "00".repeat(32), size: body.len() as u64 };
        let (hub, _) = crate::testing::fake_hub("200 OK", body);
        assert!(download_from(&bad, &hub, &dir, &|_, _| {}).is_err(), "checksum");
        assert!(!dir.join("DUORAY-0.2.1-arm64-v8a.apk").exists() && !dir.join("DUORAY-0.2.1-arm64-v8a.apk.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_checks_name() {
        let a = Asset { file: "../x".into(), sha256: String::new(), size: 0 };
        assert!(download(&a, "https://h", Path::new("/nonexistent"), |_, _| {}).is_err());
    }
}
