//! Choosing the xray core, as in MaterialXray: the one in the APK, or an
//! official XTLS/Xray-core release downloaded from GitHub. A download is
//! checked against the SHA-256 the release publishes (its .dgst file) before
//! it is unpacked; Android then runs it through the system linker, since it
//! runs no executable from an app's data (see tunnel::xray_command).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASES: &str = "https://api.github.com/repos/XTLS/Xray-core/releases?per_page=30";
const DOWNLOAD: &str = "https://github.com/XTLS/Xray-core/releases/download";

/// A release with a build for this phone.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    /// "2026-10-31".
    pub date: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    #[serde(default)]
    size: u64,
}

/// The release archive for this phone's ABI.
pub fn asset_name() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "Xray-android-amd64.zip",
        "arm" => "Xray-android-arm32-v7a.zip",
        _ => "Xray-android-arm64-v8a.zip",
    }
}

fn agent(proxy: Option<&str>, timeout: Duration) -> Result<ureq::Agent> {
    let proxy = proxy.map(ureq::Proxy::new).transpose()?;
    Ok(ureq::Agent::config_builder().proxy(proxy).timeout_global(Some(timeout)).build().into())
}

/// Recent releases with an Android build, newest first. `proxy`: the
/// tunnel's SOCKS while connected (GitHub may be what the network blocks).
pub fn releases(proxy: Option<&str>) -> Result<Vec<Release>> {
    let body = agent(proxy, Duration::from_secs(20))?
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", crate::diag::USER_AGENT)
        .call()
        .context("список версий xray")?
        .body_mut()
        .read_to_string()?;
    parse_releases(&body)
}

fn parse_releases(body: &str) -> Result<Vec<Release>> {
    let list: Vec<ApiRelease> = serde_json::from_str(body).context("ответ GitHub")?;
    Ok(list
        .into_iter()
        .filter(|r| !r.draft && valid_tag(&r.tag_name))
        .filter_map(|r| {
            let asset = r.assets.iter().find(|a| a.name == asset_name())?;
            Some(Release { tag: r.tag_name, prerelease: r.prerelease, date: r.published_at.chars().take(10).collect(), size: asset.size })
        })
        .collect())
}

/// Tags go into paths and URLs: "v26.10.31" and the like only.
fn valid_tag(tag: &str) -> bool {
    tag.len() <= 32 && tag.starts_with('v') && tag[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn cores_dir(data: &Path) -> PathBuf {
    data.join("cores")
}

/// Where the core of `tag` lives once installed.
pub fn executable(data: &Path, tag: &str) -> PathBuf {
    cores_dir(data).join(tag).join("xray")
}

/// Installed cores, newest first.
pub fn installed(data: &Path) -> Vec<String> {
    let mut tags: Vec<String> = std::fs::read_dir(cores_dir(data))
        .map(|d| d.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|t| valid_tag(t)).collect())
        .unwrap_or_default();
    tags.retain(|t| executable(data, t).is_file());
    tags.sort_by(|a, b| b.cmp(a));
    tags
}

pub fn remove(data: &Path, tag: &str) -> Result<()> {
    if !valid_tag(tag) {
        bail!("неверная версия");
    }
    std::fs::remove_dir_all(cores_dir(data).join(tag)).context("удаление ядра")
}

/// Downloads the core of `tag`, checks it against the release's SHA-256 and
/// unpacks `xray`. `progress(done, total)` reports bytes.
pub fn install(data: &Path, tag: &str, proxy: Option<&str>, progress: impl Fn(u64, u64)) -> Result<PathBuf> {
    if !valid_tag(tag) {
        bail!("неверная версия");
    }
    let name = asset_name();
    let agent = agent(proxy, Duration::from_secs(15 * 60))?;
    let digest = agent
        .get(format!("{DOWNLOAD}/{tag}/{name}.dgst"))
        .call()
        .context("контрольная сумма ядра")?
        .body_mut()
        .read_to_string()?;
    let want = parse_dgst(&digest).context("в релизе нет SHA-256 для этой сборки")?;

    let mut response = agent.get(format!("{DOWNLOAD}/{tag}/{name}")).call().context("загрузка ядра")?;
    let total = response.body().content_length().unwrap_or(0);
    let mut reader = response.body_mut().with_config().limit(200 * 1024 * 1024).reader();
    let (mut zip, mut hasher, mut buf) = (Vec::new(), Sha256::new(), vec![0u8; 256 * 1024]);
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        zip.extend_from_slice(&buf[..n]);
        progress(zip.len() as u64, total);
    }
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(&want) {
        bail!("ядро повреждено: контрольная сумма не сходится");
    }

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).context("архив ядра")?;
    let mut entry = archive.by_name("xray").context("в архиве нет xray")?;
    let dir = cores_dir(data).join(tag);
    std::fs::create_dir_all(&dir)?;
    let part = dir.join("xray.part");
    {
        let mut out = std::fs::File::create(&part)?;
        std::io::copy(&mut entry, &mut out)?;
        out.flush()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
    }
    let target = executable(data, tag);
    std::fs::rename(&part, &target)?;
    Ok(target)
}

/// "SHA2-256= <hex>" from a release's .dgst file.
fn parse_dgst(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("SHA2-256="))
        .map(|h| h.trim().to_string())
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// "26.10.31" from `xray version`.
pub fn parse_version(output: &str) -> Option<String> {
    output.lines().find_map(|l| l.trim().strip_prefix("Xray ")).and_then(|rest| rest.split_whitespace().next()).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_with_a_build_for_this_phone() {
        let body = format!(
            r#"[{{"tag_name":"v26.10.31","prerelease":false,"published_at":"2026-10-31T10:00:00Z","assets":[{{"name":"{a}","size":12}}]}},
               {{"tag_name":"v26.11.1-beta","prerelease":true,"draft":true,"assets":[{{"name":"{a}"}}]}},
               {{"tag_name":"v26.9.1","assets":[{{"name":"Xray-windows-64.zip"}}]}},
               {{"tag_name":"../x","assets":[{{"name":"{a}"}}]}}]"#,
            a = asset_name()
        );
        let list = parse_releases(&body).unwrap();
        assert_eq!(list, [Release { tag: "v26.10.31".into(), prerelease: false, date: "2026-10-31".into(), size: 12 }]);
    }

    #[test]
    fn digests_and_versions() {
        let hex = "ab".repeat(32);
        assert_eq!(parse_dgst(&format!("MD5= 00\nSHA2-256= {hex}\n")), Some(hex));
        assert_eq!(parse_dgst("SHA2-256= nothex"), None);
        assert_eq!(parse_version("Xray 26.10.31 (Xray, Penetrates Everything.) a4a3dff (go1.27.3 android/arm64)\n"), Some("26.10.31".into()));
        assert!(valid_tag("v1.8.24") && !valid_tag("../etc") && !valid_tag("1.0"));
    }
}
