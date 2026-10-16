//! Self-installation: `codexbar-tray install` puts the app, the CodexBar CLI, a launcher entry,
//! an icon and login autostart in place, so a single downloaded binary is all a user needs.

use crate::config::{runtime_dir, Config};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLI_REPO: &str = "steipete/CodexBar";
const MARKER: &str = ".installed-by-codexbar-tray";
const APP_ID: &str = "codexbar-tray";

fn home() -> PathBuf {
    dirs::home_dir().expect("no home directory")
}
fn bin_dir() -> PathBuf {
    home().join(".local/bin")
}
fn cli_dir() -> PathBuf {
    home().join(".local/lib/codexbar-cli")
}
fn applications_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| home().join(".local/share")).join("applications")
}
fn autostart_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("autostart")
}
fn icon_path() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| home().join(".local/share")).join("icons/hicolor/256x256/apps").join(format!("{APP_ID}.png"))
}

fn step(msg: &str) {
    println!("  • {msg}");
}

fn have(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {cmd}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Download `url` to `dest` with curl (or wget).
fn download(url: &str, dest: &Path) -> Result<(), String> {
    use std::io::IsTerminal;
    let tty = std::io::stderr().is_terminal();
    let status = if have("curl") {
        Command::new("curl").args(["-fL", "--retry", "2", if tty { "--progress-bar" } else { "-sS" }, "-o"]).arg(dest).arg(url).status()
    } else if have("wget") {
        Command::new("wget").args(if tty { ["-q", "--show-progress", "-O"] } else { ["-q", "-q", "-O"] }).arg(dest).arg(url).status()
    } else {
        return Err("Need curl or wget to download the CodexBar CLI".into());
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("Download failed: {url}")),
    }
}

fn fetch_text(url: &str) -> Result<String, String> {
    let out = if have("curl") {
        Command::new("curl").args(["-fsSL", "-H", "Accept: application/vnd.github+json", url]).output()
    } else {
        Command::new("wget").args(["-qO-", url]).output()
    }
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("Request failed: {url}"));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum").arg(path).output().map_err(|e| format!("sha256sum: {e}"))?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().map(str::to_string).ok_or_else(|| "sha256sum failed".into())
}

/// The CLI the app would use right now, if it actually runs.
pub fn working_cli(cfg: &Config) -> Option<PathBuf> {
    let path = cfg.resolved_cli();
    let ok = Command::new(&path).arg("--version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// Download the latest static (musl) CodexBar CLI release into ~/.local/lib/codexbar-cli and link
/// ~/.local/bin/codexbar to it. Verifies the published SHA-256 before extracting anything.
pub fn install_cli() -> Result<PathBuf, String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(format!("No CodexBar CLI build for {other}")),
    };
    // Linux archives can lag a fresh tag by a while, so take the newest release that has ours.
    let releases: serde_json::Value = serde_json::from_str(&fetch_text(&format!("https://api.github.com/repos/{CLI_REPO}/releases?per_page=10"))?)
        .map_err(|e| format!("GitHub API: {e}"))?;
    let suffix = format!("-linux-musl-{arch}.tar.gz");
    let (tag, name) = releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"].as_bool() != Some(true) && r["prerelease"].as_bool() != Some(true))
        .find_map(|r| {
            let names: Vec<&str> = r["assets"].as_array()?.iter().filter_map(|a| a["name"].as_str()).collect();
            let archive = names.iter().find(|n| n.starts_with("CodexBarCLI-") && n.ends_with(&suffix))?;
            names.contains(&format!("{archive}.sha256").as_str()).then(|| (r["tag_name"].as_str().unwrap_or("?").to_string(), archive.to_string()))
        })
        .ok_or_else(|| format!("No published CodexBar CLI build for linux-musl-{arch}"))?;
    let base = format!("https://github.com/{CLI_REPO}/releases/download/{tag}");
    let dir = cli_dir();
    let parent = dir.parent().unwrap().to_path_buf();
    fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let work = parent.join(".codexbar-cli.download");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let archive = work.join(&name);
    step(&format!("Downloading CodexBar CLI {tag} ({arch}, static)…"));
    if let Err(e) = download(&format!("{base}/{name}"), &archive) {
        let _ = fs::remove_dir_all(&work);
        return Err(e);
    }
    let expected = fetch_text(&format!("{base}/{name}.sha256"))?.split_whitespace().next().unwrap_or_default().to_lowercase();
    let actual = sha256(&archive)?;
    if expected.len() != 64 || expected != actual {
        let _ = fs::remove_dir_all(&work);
        return Err(format!("Checksum mismatch for {name}"));
    }
    step("Checksum verified");
    let staged = work.join("cli");
    fs::create_dir_all(&staged).map_err(|e| e.to_string())?;
    let ok = Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&staged).status().is_ok_and(|s| s.success());
    if !ok || !staged.join("CodexBarCLI").exists() {
        let _ = fs::remove_dir_all(&work);
        return Err("Could not extract the CLI archive".into());
    }
    let _ = fs::write(staged.join(MARKER), tag);
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&staged, &dir).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&work);
    // Point ~/.local/bin/codexbar at it, unless the user already has a real binary there.
    let link = bin_dir().join("codexbar");
    let _ = fs::create_dir_all(bin_dir());
    if link.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) || !link.exists() {
        let _ = fs::remove_file(&link);
        symlink(dir.join("codexbar"), &link).map_err(|e| e.to_string())?;
    }
    step(&format!("CLI installed to {}", dir.display()));
    Ok(dir.join("codexbar"))
}

fn write(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

fn desktop_entry(exec: &Path, background: bool) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=CodexBar Tray\nGenericName=AI usage monitor\nComment=Claude and Codex usage limits in your tray\nExec=\"{}\"{}\nIcon={APP_ID}\nTerminal=false\nCategories=Utility;Monitor;\nKeywords=claude;codex;usage;quota;limits;\nStartupNotify=false\n{}",
        exec.display(),
        if background { " --background" } else { "" },
        if background { "X-GNOME-Autostart-enabled=true\nX-KDE-autostart-after=panel\n" } else { "" }
    )
}

/// Hide launcher/autostart entries of the CodexBar Qt desktop app and of the earlier
/// `codexbar-linux` build of this app, so the launcher shows one CodexBar and login starts one.
fn retire_qt_entries() {
    // The earlier build listened on its own socket; ask it to quit.
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(runtime_dir().join("codexbar-linux-tray.sock")) {
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

fn claude_logged_in() -> bool {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".claude"));
    base.join(".credentials.json").exists()
}

fn start_detached(exe: &Path) {
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
    println!("Installing CodexBar for Linux");
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Cannot locate this executable: {e}");
            return 1;
        }
    };
    let target = bin_dir().join(APP_ID);

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
        if let Err(e) = fs::copy(&me, &tmp).and_then(|_| fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))).and_then(|_| fs::rename(&tmp, &target)) {
            eprintln!("Cannot install to {}: {e}", target.display());
            return 1;
        }
    }
    step(&format!("App installed to {}", target.display()));

    // 3. The CodexBar CLI (reuse a working one unless --update-cli).
    let mut cfg = Config::load();
    match working_cli(&cfg).filter(|_| !flag("--update-cli")) {
        Some(path) => step(&format!("Using existing CodexBar CLI at {}", path.display())),
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
    let _ = cfg.save();

    // 4. Launcher entry, icon, autostart.
    let (r, g, b) = crate::icon::brand_color("claude");
    if let Some(rgba) = crate::icon::render_rgba_logo("claude", (r, g, b), 256) {
        if let Some(dir) = icon_path().parent() {
            let _ = fs::create_dir_all(dir);
        }
        if image::save_buffer(icon_path(), &rgba, 256, 256, image::ExtendedColorType::Rgba8).is_ok() {
            let _ = Command::new("gtk-update-icon-cache").args(["-q", "-t"]).arg(icon_path().parent().unwrap().parent().unwrap().parent().unwrap()).status();
        }
    }
    if let Err(e) = write(&applications_dir().join(format!("{APP_ID}.desktop")), desktop_entry(&target, false).as_bytes()) {
        eprintln!("  ! {e}");
    }
    step("Added CodexBar to the application launcher");
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
    if !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| p == bin_dir()) {
        println!("\nNote: {} is not on your PATH. Add it to run `codexbar-tray` from a terminal.", bin_dir().display());
    }
    if !claude_logged_in() {
        println!("\nClaude: no login found in ~/.claude. Install Claude Code and run `claude` once to sign in;\nCodexBar reads the same credentials (nothing to paste).");
    }
    println!("\nDone. Click the tray icon for details, scroll on it to switch session/weekly, middle-click to refresh.");
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
        bin_dir().join(APP_ID),
    ] {
        if fs::remove_file(&p).is_ok() {
            removed.push(p);
        }
    }
    // Only remove the CLI if this tool installed it.
    let dir = cli_dir();
    if dir.join(MARKER).exists() {
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
    let _ = fs::remove_file(runtime_dir().join("codexbar-tray.sock"));
    for p in &removed {
        println!("  removed {}", p.display());
    }
    if !purge {
        println!("Settings kept in {} (use --purge to remove).", crate::config::config_path().display());
    }
    0
}
