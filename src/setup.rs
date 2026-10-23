//! Self-installation: `limitray install` puts the app, the CodexBar CLI, a launcher entry,
//! an icon and login autostart in place, so a single downloaded binary is all a user needs.

use crate::config::{runtime_dir, Config};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLI_REPO: &str = "steipete/CodexBar";
const MARKER: &str = ".installed-by-limitray";
const APP_ID: &str = "limitray";
/// This app's name until v0.3.0.
const LEGACY_APP_ID: &str = "codexbar-tray";
const LEGACY_MARKER: &str = ".installed-by-codexbar-tray";

fn home() -> PathBuf {
    dirs::home_dir().expect("no home directory")
}
fn bin_dir() -> PathBuf {
    home().join(".local/bin")
}
/// Where `install` puts the app; launcher and autostart entries point here.
pub fn installed_path() -> PathBuf {
    bin_dir().join(APP_ID)
}
fn cli_dir() -> PathBuf {
    home().join(".local/lib/codexbar-cli")
}
fn applications_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("applications")
}
fn autostart_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart")
}
fn icon_path_for(app: &str) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("icons/hicolor/256x256/apps")
        .join(format!("{app}.png"))
}
fn icon_path() -> PathBuf {
    icon_path_for(APP_ID)
}
fn installed_by_us(cli_dir: &Path) -> bool {
    cli_dir.join(MARKER).exists() || cli_dir.join(LEGACY_MARKER).exists()
}

fn step(msg: &str) {
    println!("  • {msg}");
}

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Download `url` to `dest` with curl (or wget).
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let status = if have("curl") {
        Command::new("curl")
            .args([
                "-fL",
                "--retry",
                "2",
                if tty { "--progress-bar" } else { "-sS" },
                "-o",
            ])
            .arg(dest)
            .arg(url)
            .status()
    } else if have("wget") {
        Command::new("wget")
            .args(if tty {
                ["-q", "--show-progress", "-O"]
            } else {
                ["-q", "-q", "-O"]
            })
            .arg(dest)
            .arg(url)
            .status()
    } else {
        return Err("Need curl or wget to download the CodexBar CLI".into());
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("Download failed: {url}")),
    }
}

pub fn fetch_text(url: &str) -> Result<String, String> {
    let out = if have("curl") {
        Command::new("curl")
            .args(["-fsSL", "-H", "Accept: application/vnd.github+json", url])
            .output()
    } else {
        Command::new("wget").args(["-qO-", url]).output()
    }
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("Request failed: {url}"));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

pub fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|e| format!("sha256sum: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "sha256sum failed".into())
}

/// The CLI the app would use right now, if it actually runs.
pub fn working_cli(cfg: &Config) -> Option<PathBuf> {
    let path = cfg.resolved_cli();
    let ok = Command::new(&path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// A CodexBar CLI release with a static Linux build for this machine.
pub struct CliRelease {
    pub tag: String,
    archive: String,
    arch: &'static str,
}

impl CliRelease {
    pub fn notes_url(&self) -> String {
        format!("https://github.com/{CLI_REPO}/releases/tag/{}", self.tag)
    }
}

/// The newest release in a GitHub "list releases" response that has a checksummed
/// linux-musl archive for `arch`.
fn pick_cli_release(releases: &serde_json::Value, arch: &'static str) -> Option<CliRelease> {
    let suffix = format!("-linux-musl-{arch}.tar.gz");
    releases
        .as_array()?
        .iter()
        .filter(|r| r["draft"].as_bool() != Some(true) && r["prerelease"].as_bool() != Some(true))
        .find_map(|r| {
            let names: Vec<&str> = r["assets"]
                .as_array()?
                .iter()
                .filter_map(|a| a["name"].as_str())
                .collect();
            let archive = names
                .iter()
                .find(|n| n.starts_with("CodexBarCLI-") && n.ends_with(&suffix))?;
            names
                .contains(&format!("{archive}.sha256").as_str())
                .then(|| CliRelease {
                    tag: r["tag_name"].as_str().unwrap_or("?").to_string(),
                    archive: archive.to_string(),
                    arch,
                })
        })
}

pub fn latest_cli_release() -> Result<CliRelease, String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(format!("No CodexBar CLI build for {other}")),
    };
    // Linux archives can lag a fresh tag by a while, so take the newest release that has ours.
    let releases: serde_json::Value = serde_json::from_str(&fetch_text(&format!(
        "https://api.github.com/repos/{CLI_REPO}/releases?per_page=10"
    ))?)
    .map_err(|e| format!("GitHub API: {e}"))?;
    pick_cli_release(&releases, arch)
        .ok_or_else(|| format!("No published CodexBar CLI build for linux-musl-{arch}"))
}

/// The release tag of the CLI that `install_cli` put in place, if that is the CLI `cfg` uses.
/// A CLI the user installed or configured themselves is theirs to update.
pub fn managed_cli_tag(cfg: &Config) -> Option<String> {
    managed_tag(&cli_dir(), &cfg.resolved_cli())
}

fn managed_tag(cli_dir: &Path, used: &Path) -> Option<String> {
    let dir = fs::canonicalize(cli_dir).ok()?;
    if !fs::canonicalize(used).ok()?.starts_with(&dir) {
        return None;
    }
    let tag = fs::read_to_string(dir.join(MARKER))
        .or_else(|_| fs::read_to_string(dir.join(LEGACY_MARKER)))
        .ok()?;
    Some(tag.trim().to_string()).filter(|t| !t.is_empty())
}

/// Download the latest static (musl) CodexBar CLI release into ~/.local/lib/codexbar-cli and link
/// ~/.local/bin/codexbar to it. Verifies the published SHA-256 before extracting anything.
pub fn install_cli() -> Result<PathBuf, String> {
    install_cli_release(&latest_cli_release()?)
}

fn install_cli_release(release: &CliRelease) -> Result<PathBuf, String> {
    let (tag, name, arch) = (&release.tag, &release.archive, release.arch);
    let base = format!("https://github.com/{CLI_REPO}/releases/download/{tag}");
    let dir = cli_dir();
    let parent = dir.parent().unwrap().to_path_buf();
    fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let work = parent.join(".codexbar-cli.download");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive = work.join(name);
    step(&format!("Downloading CodexBar CLI {tag} ({arch}, static)…"));
    if let Err(e) = download(&format!("{base}/{name}"), &archive) {
        let _ = fs::remove_dir_all(&work);
        return Err(e);
    }
    let expected = fetch_text(&format!("{base}/{name}.sha256"))?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let actual = sha256(&archive)?;
    if expected.len() != 64 || expected != actual {
        let _ = fs::remove_dir_all(&work);
        return Err(format!("Checksum mismatch for {name}"));
    }
    step("Checksum verified");
    let staged = work.join("cli");
    fs::create_dir_all(&staged).map_err(|e| e.to_string())?;
    let ok = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&staged)
        .status()
        .is_ok_and(|s| s.success());
    if !ok || !staged.join("CodexBarCLI").exists() {
        let _ = fs::remove_dir_all(&work);
        return Err("Could not extract the CLI archive".into());
    }
    let _ = fs::write(staged.join(MARKER), tag);
    swap_dir(&staged, &dir).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&work);
    // Point ~/.local/bin/codexbar at it, unless the user already has a real binary there.
    let link = bin_dir().join("codexbar");
    let _ = fs::create_dir_all(bin_dir());
    if link
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
        || !link.exists()
    {
        let _ = fs::remove_file(&link);
        symlink(dir.join("codexbar"), &link).map_err(|e| e.to_string())?;
    }
    step(&format!("CLI installed to {}", dir.display()));
    Ok(dir.join("codexbar"))
}

/// Put `staged` in place of `dir`. A running refresh only finds `dir` missing between two
/// renames, not for the whole recursive delete; on failure the old `dir` is put back.
fn swap_dir(staged: &Path, dir: &Path) -> std::io::Result<()> {
    let old = dir.with_extension("old");
    let _ = fs::remove_dir_all(&old);
    let moved = fs::rename(dir, &old).is_ok();
    if let Err(e) = fs::rename(staged, dir) {
        if moved {
            let _ = fs::rename(&old, dir);
        }
        return Err(e);
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

fn write(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

fn desktop_entry(exec: &Path, background: bool) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Limitray\nGenericName=AI usage monitor\nComment=Claude, Codex and other AI usage limits in your tray\nExec=\"{}\"{}\nIcon={APP_ID}\nTerminal=false\nCategories=Utility;Monitor;\nKeywords=claude;codex;usage;quota;limits;\nStartupNotify=false\n{}",
        exec.display(),
        if background { " --background" } else { "" },
        if background { "X-GNOME-Autostart-enabled=true\nX-KDE-autostart-after=panel\n" } else { "" }
    )
}

/// Hide launcher/autostart entries of the CodexBar Qt desktop app and of the earlier
/// `codexbar-linux` build of this app, so the launcher shows one usage tray and login starts one.
fn retire_qt_entries() {
    // The earlier build listened on its own socket; ask it to quit.
    if let Ok(mut s) =
        std::os::unix::net::UnixStream::connect(runtime_dir().join("codexbar-linux-tray.sock"))
    {
        let _ = s.write_all(b"{\"cmd\":\"quit\"}\n");
    }
    for path in [
        autostart_dir().join("com.steipete.CodexBar.desktop"),
        applications_dir().join("com.steipete.CodexBar.desktop"),
        autostart_dir().join("codexbar-linux.desktop"),
        applications_dir().join("codexbar-linux.desktop"),
    ] {
        if path.exists() {
            let to = path.with_extension("desktop.qt-disabled");
            if fs::rename(&path, &to).is_ok() {
                step(&format!("Disabled old entry → {}", to.display()));
            }
        }
    }
}

/// Replace an install of this app from before it was renamed (codexbar-tray → limitray): stop
/// it, and remove its binary, launcher, autostart, icon, cache and settings. Run after the new
/// config is saved, since `Config::load` reads the old settings when there are no new ones.
fn retire_legacy_app() {
    let legacy_sock = runtime_dir().join(format!("{LEGACY_APP_ID}.sock"));
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(&legacy_sock) {
        let _ = s.write_all(b"{\"cmd\":\"quit\"}\n");
        for _ in 0..30 {
            if !legacy_sock.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        step(&format!("Stopped the old {LEGACY_APP_ID}"));
    }
    let legacy_bin = bin_dir().join(LEGACY_APP_ID);
    let mut removed = false;
    for path in [
        legacy_bin.with_extension("bak"),
        legacy_bin,
        applications_dir().join(format!("{LEGACY_APP_ID}.desktop")),
        autostart_dir().join(format!("{LEGACY_APP_ID}.desktop")),
        icon_path_for(LEGACY_APP_ID),
    ] {
        removed |= fs::remove_file(path).is_ok();
    }
    for dir in [dirs::cache_dir(), dirs::config_dir()]
        .into_iter()
        .flatten()
    {
        removed |= fs::remove_dir_all(dir.join(LEGACY_APP_ID)).is_ok();
    }
    if removed {
        step(&format!("Replaced {LEGACY_APP_ID} (settings carried over)"));
    }
}

fn claude_logged_in() -> bool {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"));
    base.join(".credentials.json").exists()
}

pub fn start_detached(exe: &Path) {
    use std::os::unix::process::CommandExt;
    let _ = Command::new(exe)
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

pub fn install(args: &[String]) -> i32 {
    let flag = |f: &str| args.iter().any(|a| a == f);
    println!("Installing Limitray");
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Cannot locate this executable: {e}");
            return 1;
        }
    };
    let target = installed_path();

    // 1. Stop a running instance so its binary can be replaced.
    if crate::ipc::send(&crate::ipc::Request::Quit) {
        step("Stopped the running instance");
        std::thread::sleep(std::time::Duration::from_millis(600));
    }

    // 2. The app itself.
    if fs::canonicalize(&me).ok() != fs::canonicalize(&target).ok() {
        let _ = fs::create_dir_all(bin_dir());
        if target.exists() && fs::read(&target).ok() != fs::read(&me).ok() {
            let backup = target.with_extension("bak");
            let _ = fs::rename(&target, &backup);
            step(&format!("Kept previous {} as {}", APP_ID, backup.display()));
        }
        let tmp = target.with_extension("new");
        if let Err(e) = fs::copy(&me, &tmp)
            .and_then(|_| fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)))
            .and_then(|_| fs::rename(&tmp, &target))
        {
            eprintln!("Cannot install to {}: {e}", target.display());
            return 1;
        }
    }
    step(&format!("App installed to {}", target.display()));

    // 3. The CodexBar CLI (reuse a working one unless --update-cli).
    let mut cfg = Config::load();
    match working_cli(&cfg).filter(|_| !flag("--update-cli")) {
        Some(path) => step(&format!(
            "Using existing CodexBar CLI at {}",
            path.display()
        )),
        None => match install_cli() {
            Ok(path) => {
                if working_cli(&cfg).is_none() {
                    cfg.cli = path.display().to_string();
                }
            }
            Err(e) => {
                eprintln!("  ! {e}\n    The app is installed; it will retry downloading the CLI when it starts.");
            }
        },
    }
    if cfg.save().is_ok() {
        retire_legacy_app();
    }

    // 4. Launcher entry, icon, autostart.
    if let Some(rgba) = crate::icon::render_rgba_app_icon(256) {
        if let Some(dir) = icon_path().parent() {
            let _ = fs::create_dir_all(dir);
        }
        if image::save_buffer(
            icon_path(),
            &rgba,
            256,
            256,
            image::ExtendedColorType::Rgba8,
        )
        .is_ok()
        {
            let _ = Command::new("gtk-update-icon-cache")
                .args(["-q", "-t"])
                .arg(
                    icon_path()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap(),
                )
                .status();
        }
    }
    if let Err(e) = write(
        &applications_dir().join(format!("{APP_ID}.desktop")),
        desktop_entry(&target, false).as_bytes(),
    ) {
        eprintln!("  ! {e}");
    }
    step("Added Limitray to the application launcher");
    let autostart = autostart_dir().join(format!("{APP_ID}.desktop"));
    if flag("--no-autostart") {
        let _ = fs::remove_file(&autostart);
        step("Login autostart disabled");
    } else if write(&autostart, desktop_entry(&target, true).as_bytes()).is_ok() {
        step("Starts automatically at login");
    }
    retire_qt_entries();

    // 5. Go.
    if !flag("--no-start") {
        start_detached(&target);
        step("Started — look for the bars icon in your system tray");
    }
    if !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| p == bin_dir())
    {
        println!(
            "\nNote: {} is not on your PATH. Add it to run `limitray` from a terminal.",
            bin_dir().display()
        );
    }
    if !claude_logged_in() {
        println!("\nClaude: no login found in ~/.claude. Install Claude Code and run `claude` once to sign in;\nThe CodexBar CLI reads the same credentials (nothing to paste).");
    }
    println!("\nDone. Click the tray icon for details, middle-click to refresh.");
    let _ = std::io::stdout().flush();
    0
}

pub fn uninstall(args: &[String]) -> i32 {
    let purge = args.iter().any(|a| a == "--purge");
    crate::ipc::send(&crate::ipc::Request::Quit);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut removed = Vec::new();
    for p in [
        autostart_dir().join(format!("{APP_ID}.desktop")),
        applications_dir().join(format!("{APP_ID}.desktop")),
        icon_path(),
        installed_path(),
        installed_path().with_extension("bak"),
    ] {
        if fs::remove_file(&p).is_ok() {
            removed.push(p);
        }
    }
    // Only remove the CLI if this tool installed it.
    let dir = cli_dir();
    if installed_by_us(&dir) {
        let link = bin_dir().join("codexbar");
        if fs::read_link(&link).is_ok_and(|t| t.starts_with(&dir)) {
            let _ = fs::remove_file(&link);
        }
        if fs::remove_dir_all(&dir).is_ok() {
            removed.push(dir);
        }
    }
    if purge {
        for p in [crate::config::config_path(), crate::config::cache_path()] {
            if fs::remove_file(&p).is_ok() {
                removed.push(p);
            }
        }
    }
    let _ = fs::remove_file(runtime_dir().join("limitray.sock"));
    for p in &removed {
        println!("  removed {}", p.display());
    }
    if !purge {
        println!(
            "Settings kept in {} (use --purge to remove).",
            crate::config::config_path().display()
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("limitray-setup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn release(tag: &str, assets: &[&str]) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": assets.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn picks_newest_release_with_a_checksummed_linux_build() {
        let releases = json!([
            // Newest tag: Linux archives not uploaded yet.
            release("v0.78.0", &["CodexBar-v0.78.0.dmg"]),
            // Archive but no checksum: skipped.
            release("v0.77.1", &["CodexBarCLI-v0.77.1-linux-musl-x86_64.tar.gz"]),
            release(
                "v0.77.0",
                &[
                    "CodexBar-v0.77.0.dmg",
                    "CodexBarCLI-v0.77.0-linux-musl-aarch64.tar.gz",
                    "CodexBarCLI-v0.77.0-linux-musl-aarch64.tar.gz.sha256",
                    "CodexBarCLI-v0.77.0-linux-musl-x86_64.tar.gz",
                    "CodexBarCLI-v0.77.0-linux-musl-x86_64.tar.gz.sha256",
                ],
            ),
        ]);
        let r = pick_cli_release(&releases, "x86_64").unwrap();
        assert_eq!(r.tag, "v0.77.0");
        assert_eq!(r.archive, "CodexBarCLI-v0.77.0-linux-musl-x86_64.tar.gz");
        assert_eq!(
            r.notes_url(),
            "https://github.com/steipete/CodexBar/releases/tag/v0.77.0"
        );
        assert_eq!(
            pick_cli_release(&releases, "aarch64").unwrap().archive,
            "CodexBarCLI-v0.77.0-linux-musl-aarch64.tar.gz"
        );

        let mut pre = release(
            "v0.79.0-beta.1",
            &[
                "CodexBarCLI-v0.79.0-beta.1-linux-musl-x86_64.tar.gz",
                "CodexBarCLI-v0.79.0-beta.1-linux-musl-x86_64.tar.gz.sha256",
            ],
        );
        pre["prerelease"] = json!(true);
        assert!(pick_cli_release(&json!([pre]), "x86_64").is_none());
    }

    #[test]
    fn only_our_own_cli_counts_as_managed() {
        let root = scratch("managed");
        let ours = root.join("codexbar-cli");
        fs::create_dir_all(&ours).unwrap();
        fs::write(ours.join("codexbar"), "").unwrap();
        // ~/.local/bin/codexbar is a symlink into our directory.
        let link = root.join("codexbar");
        symlink(ours.join("codexbar"), &link).unwrap();
        let theirs = root.join("their-codexbar");
        fs::write(&theirs, "").unwrap();

        assert_eq!(managed_tag(&ours, &link), None, "no marker: not ours");
        fs::write(ours.join(MARKER), "v0.76.0\n").unwrap();
        assert_eq!(managed_tag(&ours, &link).as_deref(), Some("v0.76.0"));
        assert_eq!(
            managed_tag(&ours, &ours.join("codexbar")).as_deref(),
            Some("v0.76.0")
        );
        assert_eq!(managed_tag(&ours, &theirs), None, "user's own CLI");

        // A CLI installed before the rename still counts as ours.
        fs::remove_file(ours.join(MARKER)).unwrap();
        fs::write(ours.join(LEGACY_MARKER), "v0.75.0\n").unwrap();
        assert_eq!(managed_tag(&ours, &link).as_deref(), Some("v0.75.0"));
        assert!(installed_by_us(&ours));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_dir_replaces_and_rolls_back() {
        let root = scratch("swap");
        let dir = root.join("codexbar-cli");
        let staged = root.join("staged");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("version"), "old").unwrap();
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("version"), "new").unwrap();

        swap_dir(&staged, &dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join("version")).unwrap(), "new");
        assert!(!staged.exists());
        assert!(!dir.with_extension("old").exists(), "old copy cleaned up");

        // First install: nothing to move aside.
        let fresh = root.join("fresh");
        fs::create_dir_all(&staged).unwrap();
        swap_dir(&staged, &fresh).unwrap();
        assert!(fresh.is_dir());

        // A failed swap leaves the working CLI where it was.
        assert!(swap_dir(&root.join("missing"), &dir).is_err());
        assert_eq!(fs::read_to_string(dir.join("version")).unwrap(), "new");
        let _ = fs::remove_dir_all(&root);
    }
}
