//! Self-update from GitHub releases: `codexbar-tray update` and the tray's "Update to …" item.
//! Only the copy `install` put in ~/.local/bin replaces itself; other copies (a dev build, a
//! distro package) never offer updates.

use crate::setup::{download, fetch_text, installed_path, sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const REPO: &str = "gabors0/codexbar-tray";

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

/// `v1.2.3` / `1.2.3` (ignoring any `-pre` or `+build` suffix) → (1, 2, 3).
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
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
    let asset = format!("codexbar-tray-{arch}");
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
        other => return Err(format!("No codexbar-tray build for {other}")),
    };
    let json: serde_json::Value = serde_json::from_str(&fetch_text(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))?)
    .map_err(|e| format!("GitHub API: {e}"))?;
    Ok(release_from_json(&json, current(), arch))
}

/// Whether this process is the installed copy, i.e. the one an update would replace.
pub fn self_managed() -> bool {
    let canon = |p: PathBuf| fs::canonicalize(p).ok();
    let me = std::env::current_exe().ok().and_then(canon);
    me.is_some() && me == canon(installed_path())
}

/// Download, verify and swap in `release` at the installed path. The previous binary is kept as
/// `codexbar-tray.bak`. Running instances keep the old code until they restart.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let target = installed_path();
    if !target.exists() {
        return Err(format!(
            "{} not found; run `codexbar-tray install` first",
            target.display()
        ));
    }
    // Stage next to the target so the final rename is atomic (same filesystem).
    let staged = target.with_file_name(".codexbar-tray.update");
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
    if reported.trim() != format!("codexbar-tray {}", release.version) {
        return Err(format!(
            "The downloaded binary reports {:?}, expected {}",
            reported.trim(),
            release.version
        ));
    }
    Ok(())
}

/// `codexbar-tray update [--check]`
pub fn run_cli(args: &[String]) -> i32 {
    let check_only = args.iter().any(|a| a == "--check");
    if !check_only && !installed_path().exists() {
        eprintln!(
            "codexbar-tray is not installed in {}; run `codexbar-tray install` first",
            installed_path().display()
        );
        return 1;
    }
    let release = match check() {
        Ok(Some(r)) => r,
        Ok(None) => {
            println!("codexbar-tray {} is up to date", current());
            return 0;
        }
        Err(e) => {
            eprintln!("Cannot check for updates: {e}");
            return 1;
        }
    };
    println!(
        "Update available: {} → {}  ({})",
        current(),
        release.version,
        release.notes_url()
    );
    if check_only {
        return 0;
    }
    let target = match install(&release) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Update failed: {e}");
            return 1;
        }
    };
    println!(
        "Installed codexbar-tray {} to {}",
        release.version,
        target.display()
    );
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
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10.3-rc.1"), Some((1, 10, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"), "numeric, not lexical");
        assert!(is_newer("v1.0.0", "0.99.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(!is_newer("nonsense", "0.2.0"));
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
            "codexbar-tray-x86_64",
            "codexbar-tray-x86_64.sha256",
            "codexbar-tray-aarch64",
            "codexbar-tray-aarch64.sha256",
        ];
        let r = release_from_json(&release_json("v0.3.0", &all), "0.2.0", "aarch64").unwrap();
        assert_eq!(r.version, "0.3.0");
        assert_eq!(r.tag, "v0.3.0");
        assert_eq!(r.asset, "codexbar-tray-aarch64");
        assert_eq!(
            r.notes_url(),
            "https://github.com/gabors0/codexbar-tray/releases/tag/v0.3.0"
        );
    }

    #[test]
    fn ignores_releases_it_should_not_install() {
        let all = ["codexbar-tray-x86_64", "codexbar-tray-x86_64.sha256"];
        // Not newer.
        assert!(release_from_json(&release_json("v0.2.0", &all), "0.2.0", "x86_64").is_none());
        // No checksum published: never install unverified.
        let no_sum = release_json("v0.3.0", &["codexbar-tray-x86_64"]);
        assert!(release_from_json(&no_sum, "0.2.0", "x86_64").is_none());
        // No build for this architecture.
        assert!(release_from_json(&release_json("v0.3.0", &all), "0.2.0", "aarch64").is_none());
        // Pre-releases.
        let mut pre = release_json("v0.3.0", &all);
        pre["prerelease"] = json!(true);
        assert!(release_from_json(&pre, "0.2.0", "x86_64").is_none());
    }
}
