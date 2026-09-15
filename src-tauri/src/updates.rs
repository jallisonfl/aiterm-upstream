//! Updates — "is there a newer aiterm than the one running?"
//!
//! The answer comes from the GitHub releases of the repo that ships this
//! build. Nothing is signed or self-replacing: the release workflow already
//! publishes a .deb, an .rpm and an AppImage per tag, so an update is
//! "download the package that matches how this copy was installed, hand it to
//! the package manager". That keeps the installed copy under dpkg/rpm — the
//! same place `apt`/`dnf` will find it — instead of a second copy living
//! wherever an in-app updater would put one.
//!
//! An AppImage cannot be installed for the user: it is downloaded and the pane
//! says where it landed.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where releases are cut. The fork that carries the daily work is the one
/// that ships packages, so that is the one the installed copy asks.
pub const REPO: &str = "jallisonfl/aiterm-upstream";

const API: &str = "https://api.github.com";
/// GitHub refuses requests without a User-Agent.
const USER_AGENT: &str = concat!("aiterm/", env!("CARGO_PKG_VERSION"));
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Release metadata is a few KB; a body beyond this is not a release list.
const MAX_META_BYTES: u64 = 4 * 1024 * 1024;
/// Packages are ~25 MB today. A tenfold ceiling leaves room for growth and
/// still refuses a runaway body.
const MAX_ASSET_BYTES: u64 = 500 * 1024 * 1024;

/// How this copy of aiterm got onto the machine, which decides which package
/// an update is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Deb,
    Rpm,
    AppImage,
    /// A `cargo tauri build --no-bundle` binary or a dev run: nothing to
    /// install over. The check still works; the pane says why install does not.
    Unpackaged,
}

impl PackageKind {
    fn extension(self) -> Option<&'static str> {
        match self {
            PackageKind::Deb => Some(".deb"),
            PackageKind::Rpm => Some(".rpm"),
            PackageKind::AppImage => Some(".AppImage"),
            PackageKind::Unpackaged => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheck {
    /// Version of the running build.
    pub current: String,
    /// Newest release seen (stable, or newest of any kind when pre-releases
    /// are included).
    pub latest: String,
    pub tag: String,
    /// `latest` is strictly newer than `current`.
    pub newer: bool,
    pub prerelease: bool,
    /// Release page, for the notes in full and the other packages.
    pub url: String,
    pub published_at: String,
    /// Release body (markdown as GitHub holds it).
    pub notes: String,
    pub package: PackageKind,
    /// The one asset that matches `package`. None when the release did not
    /// ship that package, or this copy is unpackaged.
    pub asset: Option<UpdateAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// `v0.12.0` / `v0.12.0-beta.2` → a comparable version. Nightly tags carry no
/// version and are not releases in this sense; they are skipped.
fn parse_tag(tag: &str) -> Option<semver::Version> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(v).ok()
}

/// The asset for a package kind, by extension.
fn pick_asset<'a>(assets: &'a [GhAsset], kind: PackageKind) -> Option<&'a GhAsset> {
    let ext = kind.extension()?;
    assets.iter().find(|a| a.name.ends_with(ext))
}

/// Which release the check should report: the newest by version among those
/// that qualify. Stable-only is what almost everyone wants; pre-releases opt
/// in per setting.
fn newest_qualifying<'a>(
    releases: &'a [GhRelease],
    include_prerelease: bool,
) -> Option<(&'a GhRelease, semver::Version)> {
    releases
        .iter()
        .filter(|r| !r.draft && (include_prerelease || !r.prerelease))
        .filter_map(|r| parse_tag(&r.tag_name).map(|v| (r, v)))
        .filter(|(r, v)| include_prerelease || (v.pre.is_empty() && !r.prerelease))
        .max_by(|a, b| a.1.cmp(&b.1))
}

fn command_ok(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Detect how the running copy was installed. Asks the package databases
/// rather than guessing from the path: a deb and a bare `cargo build` binary
/// can both sit in /usr/bin.
fn detect_package_kind() -> PackageKind {
    if std::env::var_os("APPIMAGE").is_some() {
        return PackageKind::AppImage;
    }
    let exe = std::env::current_exe().ok();
    let exe_str = exe.as_deref().map(|p| p.to_string_lossy().to_string());
    // dpkg -S / rpm -qf answer "which package owns this file" — the precise
    // question. A binary under target/ or ~/.local is owned by nothing.
    if let Some(p) = exe_str.as_deref() {
        if Path::new("/usr/bin/dpkg").exists() && command_ok("dpkg", &["-S", p]) {
            return PackageKind::Deb;
        }
        if Path::new("/usr/bin/rpm").exists() && command_ok("rpm", &["-qf", p]) {
            return PackageKind::Rpm;
        }
    }
    PackageKind::Unpackaged
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

async fn fetch_releases(include_prerelease: bool) -> Result<Vec<GhRelease>, String> {
    let c = client(CHECK_TIMEOUT)?;
    // /releases/latest is GitHub's own "newest non-prerelease, non-draft".
    // With pre-releases wanted, the list is needed to find the newest of any kind.
    let url = if include_prerelease {
        format!("{API}/repos/{REPO}/releases?per_page=30")
    } else {
        format!("{API}/repos/{REPO}/releases/latest")
    };
    let resp = c
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("GitHub unreachable: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(format!("{REPO} has no releases yet"));
    }
    if status == reqwest::StatusCode::FORBIDDEN || status.as_u16() == 429 {
        return Err("GitHub rate limit reached — try again in an hour".into());
    }
    if !status.is_success() {
        return Err(format!("GitHub answered {status}"));
    }
    if resp.content_length().is_some_and(|n| n > MAX_META_BYTES) {
        return Err("release list is unexpectedly large".into());
    }
    let bytes = resp.bytes().await.map_err(|e| format!("reading release list: {e}"))?;
    if bytes.len() as u64 > MAX_META_BYTES {
        return Err("release list is unexpectedly large".into());
    }
    if include_prerelease {
        serde_json::from_slice::<Vec<GhRelease>>(&bytes).map_err(|e| format!("release list: {e}"))
    } else {
        serde_json::from_slice::<GhRelease>(&bytes)
            .map(|r| vec![r])
            .map_err(|e| format!("latest release: {e}"))
    }
}

/// Ask GitHub for the newest release and compare it with the running build.
#[tauri::command]
pub async fn update_check(prerelease: bool) -> Result<UpdateCheck, String> {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| format!("own version: {e}"))?;
    let releases = fetch_releases(prerelease).await?;
    let (rel, latest) = newest_qualifying(&releases, prerelease)
        .ok_or_else(|| "no versioned release found".to_string())?;
    let package = crate::run_blocking(detect_package_kind).await;
    let asset = pick_asset(&rel.assets, package).map(|a| UpdateAsset {
        name: a.name.clone(),
        url: a.browser_download_url.clone(),
        size: a.size,
    });
    Ok(UpdateCheck {
        current: current.to_string(),
        newer: latest > current,
        latest: latest.to_string(),
        tag: rel.tag_name.clone(),
        prerelease: rel.prerelease,
        url: rel.html_url.clone(),
        published_at: rel.published_at.clone().unwrap_or_default(),
        notes: rel.body.clone().unwrap_or_default(),
        package,
        asset,
    })
}

/// Where downloaded packages land. Under the cache dir: safe to lose, and
/// not somewhere a stray .deb looks like the user's own file.
fn updates_dir() -> Result<PathBuf, String> {
    let d = dirs::cache_dir()
        .ok_or_else(|| "no cache directory".to_string())?
        .join("aiterm")
        .join("updates");
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// Only a bare file name is accepted: the URL comes back from GitHub, but the
/// name decides the path written, and a name with a slash in it would decide
/// the directory too.
fn safe_asset_name(name: &str) -> Result<&str, String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.starts_with('.')
        || name.contains("..")
    {
        return Err(format!("refusing asset name {name:?}"));
    }
    Ok(name)
}

/// Download one release asset. Returns the path it was written to. Only
/// release-asset URLs on GitHub are followed — the URL is data from the API
/// answer, and this is the one place the app writes what a URL hands back.
#[tauri::command]
pub async fn update_download(url: String, name: String, size: u64) -> Result<String, String> {
    let name = safe_asset_name(&name)?.to_string();
    if !url.starts_with(&format!("https://github.com/{REPO}/releases/download/")) {
        return Err("refusing to download from outside the release page".into());
    }
    if size > MAX_ASSET_BYTES {
        return Err("package is larger than aiterm will download".into());
    }
    let dir = updates_dir()?;
    let dest = dir.join(&name);
    let part = dir.join(format!("{name}.part"));

    let c = client(DOWNLOAD_TIMEOUT)?;
    let resp = c
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download answered {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("download: {e}"))?;
    if size > 0 && bytes.len() as u64 != size {
        return Err(format!(
            "download was {} bytes, release says {size} — not installing it",
            bytes.len()
        ));
    }
    let dest2 = dest.clone();
    crate::run_blocking(move || -> Result<(), String> {
        std::fs::write(&part, &bytes).map_err(|e| format!("{}: {e}", part.display()))?;
        std::fs::rename(&part, &dest2).map_err(|e| format!("{}: {e}", dest2.display()))
    })
    .await?;
    Ok(dest.to_string_lossy().to_string())
}

/// Hand a downloaded package to the system's package manager.
///
/// Same shape as the font-package install: `sudo -n` first (silent where sudo
/// is passwordless), then `pkexec`, which raises polkit's own graphical
/// prompt — the only kind of password prompt that can work behind a button.
/// Only files this module downloaded are accepted.
#[tauri::command]
pub async fn update_install(path: String) -> Result<String, String> {
    crate::run_blocking(move || update_install_blocking(path)).await
}

fn update_install_blocking(path: String) -> Result<String, String> {
    let dir = updates_dir()?;
    let p = PathBuf::from(&path);
    let Some(name) = p.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return Err("no file name".into());
    };
    if p.parent() != Some(dir.as_path()) || !p.is_file() {
        return Err("not a package aiterm downloaded".into());
    }
    let abs = p.to_string_lossy().to_string();
    let args: Vec<&str> = if name.ends_with(".deb") {
        vec!["apt-get", "install", "-y", "--allow-downgrades", &abs]
    } else if name.ends_with(".rpm") {
        vec!["dnf", "install", "-y", &abs]
    } else {
        return Err(format!("{name} is not a package the system can install"));
    };

    let run = |prog: &str, extra: &[&str]| -> Result<(bool, String), String> {
        let out = Command::new(prog)
            .args(extra)
            .args(&args)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .output()
            .map_err(|e| format!("{prog}: {e}"))?;
        let msg = if out.status.success() {
            String::from_utf8_lossy(&out.stdout).to_string()
        } else {
            String::from_utf8_lossy(&out.stderr).to_string()
        };
        Ok((out.status.success(), msg))
    };

    let (ok, msg) = run("sudo", &["-n"])?;
    let (ok, msg) = if ok { (ok, msg) } else { run("pkexec", &[])? };
    if !ok {
        let t = msg.trim();
        return Err(if t.is_empty() { format!("installing {name} failed") } else { t.to_string() });
    }
    let _ = std::fs::remove_file(&p);
    Ok(name)
}

/// Relaunch the running binary — after an install, that is the new one.
#[tauri::command]
pub fn app_restart(app: tauri::AppHandle) {
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool, assets: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            html_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
            prerelease: pre,
            draft: false,
            published_at: None,
            body: None,
            assets: assets
                .iter()
                .map(|n| GhAsset {
                    name: n.to_string(),
                    browser_download_url: format!(
                        "https://github.com/{REPO}/releases/download/{tag}/{n}"
                    ),
                    size: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn tags_parse_with_and_without_v_and_skip_nightlies() {
        assert_eq!(parse_tag("v0.12.0").unwrap().to_string(), "0.12.0");
        assert_eq!(parse_tag("0.12.0").unwrap().to_string(), "0.12.0");
        assert!(parse_tag("v0.13.0-beta.2").unwrap().pre.as_str() == "beta.2");
        assert!(parse_tag("nightly-20260915").is_none());
    }

    #[test]
    fn stable_channel_ignores_prereleases_even_when_newer() {
        let rs = vec![
            rel("v0.12.0", false, &[]),
            rel("v0.13.0-alpha.1", true, &[]),
            rel("nightly-20260915", true, &[]),
        ];
        let (r, v) = newest_qualifying(&rs, false).unwrap();
        assert_eq!(r.tag_name, "v0.12.0");
        assert_eq!(v.to_string(), "0.12.0");
        let (r, _) = newest_qualifying(&rs, true).unwrap();
        assert_eq!(r.tag_name, "v0.13.0-alpha.1");
    }

    #[test]
    fn a_stable_tag_flagged_prerelease_on_github_stays_out_of_stable() {
        // Someone ticks "pre-release" on a v-tag: GitHub's flag wins.
        let rs = vec![rel("v0.14.0", true, &[]), rel("v0.12.0", false, &[])];
        assert_eq!(newest_qualifying(&rs, false).unwrap().0.tag_name, "v0.12.0");
    }

    #[test]
    fn asset_is_picked_by_package_kind() {
        let r = rel("v0.12.0", false, &[
            "aiterm_0.12.0_amd64.deb",
            "aiterm-0.12.0-1.x86_64.rpm",
            "aiterm_0.12.0_amd64.AppImage",
            "aiterm-phone-v0.12.0.apk",
        ]);
        assert_eq!(pick_asset(&r.assets, PackageKind::Deb).unwrap().name, "aiterm_0.12.0_amd64.deb");
        assert_eq!(pick_asset(&r.assets, PackageKind::Rpm).unwrap().name, "aiterm-0.12.0-1.x86_64.rpm");
        assert_eq!(pick_asset(&r.assets, PackageKind::AppImage).unwrap().name, "aiterm_0.12.0_amd64.AppImage");
        assert!(pick_asset(&r.assets, PackageKind::Unpackaged).is_none());
    }

    #[test]
    fn newer_means_strictly_greater() {
        let cur = semver::Version::parse("0.12.0").unwrap();
        assert!(parse_tag("v0.12.1").unwrap() > cur);
        assert!(parse_tag("v0.13.0-beta.1").unwrap() > cur);
        assert!(!(parse_tag("v0.12.0").unwrap() > cur));
        assert!(!(parse_tag("v0.12.0-rc.1").unwrap() > cur));
    }

    #[test]
    fn asset_names_cannot_escape_the_updates_dir() {
        assert!(safe_asset_name("aiterm_0.13.0_amd64.deb").is_ok());
        assert!(safe_asset_name("../evil.deb").is_err());
        assert!(safe_asset_name("a/b.deb").is_err());
        assert!(safe_asset_name(".hidden").is_err());
        assert!(safe_asset_name("").is_err());
    }
}
