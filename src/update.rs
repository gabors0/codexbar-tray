//! Updates from GitHub releases: `limitray update` and the tray's "Update …" items.
//! Only the copy `install` put in ~/.local/bin replaces itself; other copies (a dev build, a
//! distro package) never offer updates. Likewise only a CLI that `install_cli` put in place is
//! offered CLI updates.

use crate::config::Config;
use crate::setup::{download, fetch_text, installed_path, managed_cli_tag, sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const REPO: &str = "gabors0/limitray";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    tag: String,
    asset: String,
}

impl Release {
    pub fn notes_url(&self) -> String {
        notes_url(&self.version)
    }
}

pub fn notes_url(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `v1.2.3` / `1.2.3` / `v1.2` (ignoring any `-pre` or `+build` suffix) → (1, 2, 3). Older
/// CodexBar tags have only two parts.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (
        parts.next()??,
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
    );
    parts.next().is_none().then_some(v)
}

fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// Pick this machine's binary out of a GitHub "latest release" response, if it is newer.
fn release_from_json(json: &serde_json::Value, current: &str, arch: &str) -> Option<Release> {
    let tag = json["tag_name"].as_str()?;
    if json["draft"].as_bool() == Some(true) || json["prerelease"].as_bool() == Some(true) {
        return None;
    }
    if !is_newer(tag, current) {
        return None;
    }
    let asset = format!("limitray-{arch}");
    let names: Vec<&str> = json["assets"]
        .as_array()?
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    (names.contains(&asset.as_str()) && names.contains(&format!("{asset}.sha256").as_str())).then(
        || Release {
            version: parse_version(tag)
                .map(|(a, b, c)| format!("{a}.{b}.{c}"))
                .unwrap_or_default(),
            tag: tag.to_string(),
            asset,
        },
    )
}

/// The latest published release, when it is newer than this build and has a binary for us.
pub fn check() -> Result<Option<Release>, String> {
    let arch = match std::env::consts::ARCH {
        a @ ("x86_64" | "aarch64") => a,
        other => return Err(format!("No limitray build for {other}")),
    };
    let json: serde_json::Value = serde_json::from_str(&fetch_text(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))?)
    .map_err(|e| format!("GitHub API: {e}"))?;
    Ok(release_from_json(&json, current(), arch))
}

/// A newer CodexBar CLI than the one limitray installed.
pub struct CliUpdate {
    pub installed: String,
    pub latest: String,
    pub notes_url: String,
}

/// Whether a newer CLI release exists. Always `None` for a CLI limitray didn't install.
pub fn check_cli(cfg: &Config) -> Result<Option<CliUpdate>, String> {
    let Some(installed) = managed_cli_tag(cfg) else {
        return Ok(None);
    };
    let latest = crate::setup::latest_cli_release()?;
    Ok(is_newer(&latest.tag, &installed).then(|| CliUpdate {
        notes_url: latest.notes_url(),
        latest: latest.tag,
        installed,
    }))
}

/// Whether this process is the installed copy, i.e. the one an update would replace.
pub fn self_managed() -> bool {
    let canon = |p: PathBuf| fs::canonicalize(p).ok();
    let me = std::env::current_exe().ok().and_then(canon);
    me.is_some() && me == canon(installed_path())
}

/// Download, verify and swap in `release` at the installed path. The previous binary is kept as
/// `limitray.bak`. Running instances keep the old code until they restart.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let target = installed_path();
    if !target.exists() {
        return Err(format!(
            "{} not found; run `limitray install` first",
            target.display()
        ));
    }
    // Stage next to the target so the final rename is atomic (same filesystem).
    let staged = target.with_file_name(".limitray.update");
    let result = stage(release, &staged).and_then(|()| {
        let _ = fs::copy(&target, target.with_extension("bak"));
        fs::rename(&staged, &target).map_err(|e| format!("{}: {e}", target.display()))
    });
    let _ = fs::remove_file(&staged);
    result.map(|()| target)
}

fn stage(release: &Release, staged: &std::path::Path) -> Result<(), String> {
    let base = format!(
        "https://github.com/{REPO}/releases/download/{}/{}",
        release.tag, release.asset
    );
    let _ = fs::remove_file(staged);
    download(&base, staged)?;
    let expected = fetch_text(&format!("{base}.sha256"))?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if expected.len() != 64 || expected != sha256(staged)? {
        return Err(format!("Checksum mismatch for {}", release.asset));
    }
    fs::set_permissions(staged, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    // Refuse a binary that doesn't run here or isn't the version we asked for.
    let out = Command::new(staged)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("The downloaded binary does not run: {e}"))?;
    let reported = String::from_utf8_lossy(&out.stdout);
    if reported.trim() != format!("limitray {}", release.version) {
        return Err(format!(
            "The downloaded binary reports {:?}, expected {}",
            reported.trim(),
            release.version
        ));
    }
    Ok(())
}

/// `limitray update [--check]`: the app, and the CodexBar CLI if limitray installed it.
pub fn run_cli(args: &[String]) -> i32 {
    let check_only = args.iter().any(|a| a == "--check");
    let mut ok = true;

    let mut new_app = None;
    match check() {
        Ok(None) => println!("limitray {} is up to date", current()),
        Ok(Some(release)) => {
            println!(
                "limitray update available: {} → {}  ({})",
                current(),
                release.version,
                release.notes_url()
            );
            if !check_only {
                match install(&release) {
                    Ok(target) => {
                        println!("  Installed limitray {}", release.version);
                        new_app = Some(target);
                    }
                    Err(e) => {
                        eprintln!("  Update failed: {e}");
                        ok = false;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Cannot check for limitray updates: {e}");
            ok = false;
        }
    }

    let cfg = Config::load();
    let mut new_cli = false;
    match check_cli(&cfg) {
        Ok(None) => match managed_cli_tag(&cfg) {
            Some(tag) => println!("CodexBar CLI {tag} is up to date"),
            None if !cfg.resolved_cli().is_file() => {
                println!("No CodexBar CLI found; `limitray update-cli` installs it")
            }
            None => println!(
                "CodexBar CLI at {} was not installed by limitray; update it yourself",
                cfg.resolved_cli().display()
            ),
        },
        Ok(Some(u)) => {
            println!(
                "CodexBar CLI update available: {} → {}  ({})",
                u.installed, u.latest, u.notes_url
            );
            if !check_only {
                match crate::setup::install_cli() {
                    Ok(_) => new_cli = true,
                    Err(e) => {
                        eprintln!("  CLI update failed: {e}");
                        ok = false;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Cannot check for CodexBar CLI updates: {e}");
            ok = false;
        }
    }

    if let Some(target) = new_app {
        // Restart a running instance on the new binary. Quit + start works whatever its version.
        if crate::ipc::send(&crate::ipc::Request::Quit) {
            for _ in 0..50 {
                if crate::ipc::connect().is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            crate::setup::start_detached(&target);
            println!("Restarted the tray");
        }
    } else if new_cli {
        crate::ipc::send(&crate::ipc::Request::Refresh);
    }
    if ok {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10.3-rc.1"), Some((1, 10, 3)));
        assert_eq!(
            parse_version("v0.25"),
            Some((0, 25, 0)),
            "older CodexBar tags"
        );
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"), "numeric, not lexical");
        assert!(is_newer("v1.0.0", "0.99.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("nonsense", "0.2.0"));
        assert!(is_newer("v0.77.0", "v0.76.0"));
        assert!(is_newer("v0.26.0", "v0.25"));
        assert!(!is_newer("v0.77.0", "v0.77.0"));
    }

    fn release_json(tag: &str, assets: &[&str]) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": assets.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn picks_this_machines_binary() {
        let all = [
            "limitray-x86_64",
            "limitray-x86_64.sha256",
            "limitray-aarch64",
            "limitray-aarch64.sha256",
        ];
        let r = release_from_json(&release_json("v0.3.0", &all), "0.2.0", "aarch64").unwrap();
        assert_eq!(r.version, "0.3.0");
        assert_eq!(r.tag, "v0.3.0");
        assert_eq!(r.asset, "limitray-aarch64");
        assert_eq!(
            r.notes_url(),
            "https://github.com/gabors0/limitray/releases/tag/v0.3.0"
        );
    }

    #[test]
    fn ignores_releases_it_should_not_install() {
        let all = ["limitray-x86_64", "limitray-x86_64.sha256"];
        // Not newer.
        assert!(release_from_json(&release_json("v0.2.0", &all), "0.2.0", "x86_64").is_none());
        // No checksum published: never install unverified.
        let no_sum = release_json("v0.3.0", &["limitray-x86_64"]);
        assert!(release_from_json(&no_sum, "0.2.0", "x86_64").is_none());
        // No build for this architecture.
        assert!(release_from_json(&release_json("v0.3.0", &all), "0.2.0", "aarch64").is_none());
        // Pre-releases.
        let mut pre = release_json("v0.3.0", &all);
        pre["prerelease"] = json!(true);
        assert!(release_from_json(&pre, "0.2.0", "x86_64").is_none());
    }
}
