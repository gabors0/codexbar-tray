use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaDisplay {
    Used,
    Remaining,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayStyle {
    Bars,
    Ring,
    Percent,
    Logo,
}

impl TrayStyle {
    pub const ALL: [TrayStyle; 4] = [
        TrayStyle::Bars,
        TrayStyle::Ring,
        TrayStyle::Percent,
        TrayStyle::Logo,
    ];
    pub fn label(self) -> &'static str {
        match self {
            TrayStyle::Bars => "Two bars (session + weekly)",
            TrayStyle::Ring => "Rings (session + weekly)",
            TrayStyle::Percent => "Percentage",
            TrayStyle::Logo => "Provider logo",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayWindow {
    Session,
    Weekly,
    Tightest,
}

impl TrayWindow {
    pub const ALL: [TrayWindow; 3] = [
        TrayWindow::Session,
        TrayWindow::Weekly,
        TrayWindow::Tightest,
    ];
    pub fn label(self) -> &'static str {
        match self {
            TrayWindow::Session => "Session (5h)",
            TrayWindow::Weekly => "Weekly",
            TrayWindow::Tightest => "Whichever is closest to the limit",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// CodexBar CLI executable. Bare names are resolved via PATH and ~/.local/bin.
    pub cli: String,
    /// Providers in display order. The first one drives the tray icon unless changed by scrolling.
    pub providers: Vec<String>,
    /// Provider shown in the tray icon (scroll on the icon to cycle).
    pub tray_provider: String,
    pub refresh_secs: u64,
    pub show_costs: bool,
    pub show_status: bool,
    pub quota_display: QuotaDisplay,
    pub tray_style: TrayStyle,
    pub tray_window: TrayWindow,
    pub notifications: bool,
    /// Used-percent thresholds that trigger a notification when crossed upwards.
    pub notify_thresholds: Vec<u8>,
    pub notify_reset: bool,
    pub close_on_blur: bool,
    /// Source passed to the CLI. "auto" tries OAuth first for Claude (fast), then falls back.
    pub source: String,
    /// Ask GitHub once a day whether a newer codexbar-tray release exists.
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cli: "codexbar".into(),
            providers: vec!["claude".into()],
            tray_provider: "claude".into(),
            refresh_secs: 300,
            show_costs: true,
            show_status: true,
            quota_display: QuotaDisplay::Used,
            tray_style: TrayStyle::Bars,
            tray_window: TrayWindow::Session,
            notifications: true,
            notify_thresholds: vec![80, 95],
            notify_reset: true,
            close_on_blur: true,
            source: "auto".into(),
            check_updates: true,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("codexbar-tray/config.json")
}

pub fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("codexbar-tray/snapshot.json")
}

pub fn runtime_dir() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("codexbar-{}", unsafe { libc::getuid() }))
    })
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("codexbar-tray.sock")
}

impl Config {
    pub fn load() -> Self {
        let mut cfg: Config = std::fs::read(config_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .or_else(|| {
                // Settings from the earlier in-repo build of this app.
                let old = dirs::config_dir()?.join("codexbar/linux-tray.json");
                serde_json::from_slice(&std::fs::read(old).ok()?).ok()
            })
            .unwrap_or_else(|| {
                // First run: inherit the CLI path the CodexBar Qt app was configured with.
                let mut c = Config::default();
                if let Some(old) = dirs::config_dir()
                    .and_then(|d| std::fs::read(d.join("codexbar/linux.json")).ok())
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                {
                    if let Some(exe) = old["executable"].as_str() {
                        c.cli = exe.to_string();
                    }
                }
                c
            });
        cfg.sanitize();
        cfg
    }

    pub fn sanitize(&mut self) {
        self.refresh_secs = self.refresh_secs.clamp(60, 3600);
        self.providers.retain(|p| {
            !p.is_empty()
                && p.len() <= 40
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
        self.providers.dedup();
        if self.providers.is_empty() {
            self.providers.push("claude".into());
        }
        if !self.providers.contains(&self.tray_provider) {
            self.tray_provider = self.providers[0].clone();
        }
        self.notify_thresholds.retain(|t| (1..=100).contains(t));
        self.notify_thresholds.sort_unstable();
        self.notify_thresholds.dedup();
        if !["auto", "oauth", "cli", "api", "web"].contains(&self.source.as_str()) {
            self.source = "auto".into();
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap())?;
        std::fs::rename(tmp, path)
    }

    pub fn resolved_cli(&self) -> PathBuf {
        resolve_executable(&self.cli)
    }

    pub fn display_value(&self, used: f64) -> f64 {
        match self.quota_display {
            QuotaDisplay::Used => used,
            QuotaDisplay::Remaining => 100.0 - used,
        }
    }

    pub fn display_suffix(&self) -> &'static str {
        match self.quota_display {
            QuotaDisplay::Used => "used",
            QuotaDisplay::Remaining => "left",
        }
    }
}

fn resolve_executable(name: &str) -> PathBuf {
    let expanded = if let Some(rest) = name.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(name))
    } else {
        PathBuf::from(name)
    };
    if expanded.components().count() > 1 {
        return expanded;
    }
    let mut dirs_to_search: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    // A portable bundle can keep the CLI beside this executable.
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        dirs_to_search.push(dir.join("codexbar-cli"));
        dirs_to_search.push(dir);
    }
    if let Some(home) = dirs::home_dir() {
        dirs_to_search.push(home.join(".local/bin"));
        dirs_to_search.push(home.join(".local/lib/codexbar-cli"));
    }
    dirs_to_search
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
        .unwrap_or(expanded)
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
