//! Background service: polls the CodexBar CLI, owns the tray icon, serves the popup over IPC.

use crate::config::{cache_path, socket_path, Config};
use crate::fetch;
use crate::ipc::{Request, State};
use crate::model::{CostSummary, ProviderUsage, Snapshot, UpdateOffer, UpdateTarget};
use crate::tray::CodexTray;
use crate::{format, theme};
use chrono::Utc;
use ksni::blocking::TrayMethods;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// When the CLI is missing or broken, download it again at most this often (a manual refresh
/// retries right away), so a CLI that installs but still won't run isn't re-fetched every poll.
static CLI_INSTALL_AT: Mutex<Option<Instant>> = Mutex::new(None);
const CLI_INSTALL_RETRY: Duration = Duration::from_secs(60 * 60);

fn cli_install_due(last: Option<Instant>, now: Instant, force: bool) -> bool {
    force || last.is_none_or(|t| now.duration_since(t) >= CLI_INSTALL_RETRY)
}

/// True (and records the attempt) when a CLI download may start now.
fn claim_cli_install(force: bool) -> bool {
    let mut last = CLI_INSTALL_AT.lock().unwrap();
    let now = Instant::now();
    let due = cli_install_due(*last, now, force);
    if due {
        *last = Some(now);
    }
    due
}

pub enum Cmd {
    Refresh {
        force: bool,
    },
    RefreshIfStale(i64),
    FetchDone(
        Vec<ProviderUsage>,
        Option<Result<Vec<CostSummary>, String>>,
        u64,
    ),
    Notice(Option<String>),
    SetConfig(Config),
    TogglePopup {
        token: Option<String>,
        /// Where the tray icon was clicked; `None` (e.g. `--toggle` from a hotkey) centers it.
        pos: Option<(i32, i32)>,
    },
    ShowPopup {
        token: Option<String>,
    },
    PopupClosing,
    Subscribe(UnixStream),
    Get(UnixStream),
    /// Result of a background update check: the newer version and its release notes, if any.
    UpdateChecked(UpdateTarget, Option<(String, String)>),
    InstallUpdate(UpdateTarget),
    UpdateFailed(UpdateTarget, String),
    /// A new CodexBar CLI is in place; fetch with it.
    CliUpdated,
    /// The installed binary was replaced; exec it.
    Restart,
    Quit,
}

const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

pub fn open_url(url: &str) {
    let _ = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn load_cache() -> Snapshot {
    std::fs::read(cache_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok())
        .map(|mut s| {
            s.busy = false;
            s.from_cache = true;
            s.update = None;
            s.cli_update = None;
            s
        })
        .unwrap_or_default()
}

fn save_cache(s: &Snapshot) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_vec(s).unwrap()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn notify(summary: &str, body: &str, urgent: bool) {
    let mut n = notify_rust::Notification::new();
    n.appname("Limitray")
        .summary(summary)
        .body(body)
        .icon("limitray")
        .timeout(notify_rust::Timeout::Milliseconds(8000));
    if urgent {
        n.urgency(notify_rust::Urgency::Critical);
    }
    let _ = n.show();
}

/// Threshold crossings and resets between two consecutive live fetches.
fn notifications(
    cfg: &Config,
    prev: &HashMap<String, (f64, Option<chrono::DateTime<Utc>>)>,
    providers: &[ProviderUsage],
) {
    for n in pending_notifications(cfg, prev, providers) {
        notify(&n.summary, &n.body, n.urgent);
    }
}

#[derive(Debug, PartialEq)]
struct Alert {
    summary: String,
    body: String,
    urgent: bool,
}

fn pending_notifications(
    cfg: &Config,
    prev: &HashMap<String, (f64, Option<chrono::DateTime<Utc>>)>,
    providers: &[ProviderUsage],
) -> Vec<Alert> {
    let mut out = Vec::new();
    if !cfg.notifications {
        return out;
    }
    for p in providers.iter().filter(|p| p.error.is_none()) {
        for w in &p.windows {
            let Some(&(before, old_reset)) = prev.get(&format!("{}/{}", p.id, w.key)) else {
                continue;
            };
            let crossed = cfg
                .notify_thresholds
                .iter()
                .rev()
                .find(|&&t| before < t as f64 && w.used >= t as f64);
            if let Some(&t) = crossed {
                out.push(Alert {
                    summary: format!("{} {} at {:.0}%", p.name(), w.label.to_lowercase(), w.used),
                    body: format!("Crossed {t}% · {}", format::resets_in(w.resets_at)),
                    urgent: Some(&t) == cfg.notify_thresholds.last(),
                });
            } else if cfg.notify_reset
                && w.is_session()
                && before >= 50.0
                && w.used + 30.0 < before
                && old_reset != w.resets_at
            {
                out.push(Alert {
                    summary: format!("{} {} reset", p.name(), w.label.to_lowercase()),
                    body: format!("Back to {:.0}% used. Fresh window.", w.used),
                    urgent: false,
                });
            }
        }
    }
    out
}

/// Keep the last good windows for a provider whose fetch failed, flagged with the error.
fn merge_last_good(old: &Snapshot, providers: Vec<ProviderUsage>) -> Vec<ProviderUsage> {
    let mut merged = Vec::new();
    for mut p in providers {
        if p.error.is_some() && p.windows.is_empty() {
            if let Some(o) = old.provider(&p.id).filter(|o| !o.windows.is_empty()) {
                let mut keep = o.clone();
                keep.error = p.error.take();
                merged.push(keep);
                continue;
            }
        }
        merged.push(p);
    }
    merged
}

struct Daemon {
    cfg: Config,
    snap: Snapshot,
    tx: Sender<Cmd>,
    tray: Option<ksni::blocking::Handle<CodexTray>>,
    subscribers: Vec<UnixStream>,
    popup: Option<Child>,
    popup_closed_at: Option<Instant>,
    last_values: HashMap<String, (f64, Option<chrono::DateTime<Utc>>)>,
    generation: u64,
    last_cost: Option<Instant>,
    live_fetches: u32,
    /// Only the installed copy offers to update itself. (CLI updates have their own check.)
    self_managed: bool,
    next_update_check: Instant,
}

impl Daemon {
    fn state_line(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(&State {
            snapshot: self.snap.clone(),
            config: self.cfg.clone(),
        })
        .unwrap();
        line.push(b'\n');
        line
    }

    fn publish(&mut self) {
        let line = self.state_line();
        self.subscribers.retain_mut(|s| s.write_all(&line).is_ok());
        if let Some(tray) = &self.tray {
            let (snap, cfg) = (self.snap.clone(), self.cfg.clone());
            tray.update(move |t| {
                t.snapshot = snap;
                t.config = cfg;
                t.rebuild();
            });
        }
    }

    fn refresh(&mut self, force: bool) {
        if self.snap.busy {
            return;
        }
        if force {
            fetch::reset_fast_path();
        }
        self.snap.busy = true;
        self.publish();
        let cfg = self.cfg.clone();
        let tx = self.tx.clone();
        let generation = self.generation;
        // Local spend scans are cheap but not free; refresh them at most every 5 minutes unless forced.
        let want_costs = cfg.show_costs
            && (force
                || self
                    .last_cost
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(300)));
        if want_costs {
            self.last_cost = Some(Instant::now());
        }
        std::thread::spawn(move || {
            // First run without a CLI: fetch it, so a lone downloaded binary just works.
            if crate::setup::working_cli(&cfg).is_none() && claim_cli_install(force) {
                let _ = tx.send(Cmd::Notice(Some("Downloading the CodexBar CLI…".into())));
                let result = crate::setup::install_cli();
                let _ = tx.send(Cmd::Notice(result.err().map(|e| {
                    format!("CodexBar CLI missing: {e}. Run `limitray update-cli`.")
                })));
            }
            let costs = want_costs.then(|| {
                let cfg = cfg.clone();
                std::thread::spawn(move || fetch::fetch_costs(&cfg))
            });
            let handles: Vec<_> = cfg
                .providers
                .iter()
                .cloned()
                .map(|p| {
                    let cfg = cfg.clone();
                    std::thread::spawn(move || fetch::fetch_provider(&cfg, &p))
                })
                .collect();
            let providers = handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect();
            let costs = costs.map(|h| h.join().unwrap_or_else(|_| Err("cost scan crashed".into())));
            let _ = tx.send(Cmd::FetchDone(providers, costs, generation));
        });
    }

    fn fetch_done(
        &mut self,
        providers: Vec<ProviderUsage>,
        costs: Option<Result<Vec<CostSummary>, String>>,
        generation: u64,
    ) {
        self.snap.busy = false;
        if generation != self.generation {
            self.refresh(false);
            return;
        }
        let merged = merge_last_good(&self.snap, providers);
        if self.live_fetches > 0 {
            notifications(&self.cfg, &self.last_values, &merged);
        }
        self.live_fetches += 1;
        for p in merged.iter().filter(|p| p.error.is_none()) {
            for w in &p.windows {
                self.last_values
                    .insert(format!("{}/{}", p.id, w.key), (w.used, w.resets_at));
            }
        }
        let any_ok = merged.iter().any(|p| p.error.is_none());
        self.snap.error = (!any_ok).then(|| {
            merged
                .iter()
                .find_map(|p| p.error.clone())
                .unwrap_or_else(|| "Usage unavailable".into())
        });
        self.snap.providers = merged;
        if let Some(Ok(c)) = costs {
            self.snap.costs = c;
        }
        if any_ok {
            self.snap.updated_at = Some(Utc::now());
            self.snap.from_cache = false;
        }
        save_cache(&self.snap);
        self.publish();
    }

    fn set_config(&mut self, mut cfg: Config) {
        cfg.sanitize();
        if cfg == self.cfg {
            return;
        }
        let refetch = cfg.providers != self.cfg.providers
            || cfg.cli != self.cfg.cli
            || cfg.source != self.cfg.source
            || cfg.show_status != self.cfg.show_status
            || (cfg.show_costs && !self.cfg.show_costs);
        if cfg.providers != self.cfg.providers {
            self.snap
                .providers
                .retain(|p| cfg.providers.contains(&p.id));
            self.snap
                .providers
                .sort_by_key(|p| cfg.providers.iter().position(|x| *x == p.id));
        }
        if !cfg.show_costs {
            self.snap.costs.clear();
        }
        if cfg.check_updates && !self.cfg.check_updates {
            self.next_update_check = Instant::now();
        } else if !cfg.check_updates {
            self.snap.update = None;
            self.snap.cli_update = None;
        }
        let _ = cfg.save();
        self.cfg = cfg;
        if refetch {
            self.generation += 1;
            self.last_cost = None;
            if self.snap.busy {
                // fetch_done will see the stale generation and re-run.
            } else {
                self.refresh(true);
            }
        }
        self.publish();
    }

    fn offer(&mut self, target: UpdateTarget) -> &mut Option<UpdateOffer> {
        match target {
            UpdateTarget::App => &mut self.snap.update,
            UpdateTarget::Cli => &mut self.snap.cli_update,
        }
    }

    fn check_updates(&self) {
        if !self.cfg.check_updates {
            return;
        }
        let (tx, cfg, app) = (self.tx.clone(), self.cfg.clone(), self.self_managed);
        // Offline or rate-limited: keep what we knew and try again next time.
        std::thread::spawn(move || {
            if app {
                if let Ok(release) = crate::update::check() {
                    let found = release.map(|r| (r.version.clone(), r.notes_url()));
                    let _ = tx.send(Cmd::UpdateChecked(UpdateTarget::App, found));
                }
            }
            if let Ok(cli) = crate::update::check_cli(&cfg) {
                let found = cli.map(|u| (u.latest, u.notes_url));
                let _ = tx.send(Cmd::UpdateChecked(UpdateTarget::Cli, found));
            }
        });
    }

    fn update_checked(&mut self, target: UpdateTarget, found: Option<(String, String)>) {
        if !self.cfg.check_updates || self.offer(target).as_ref().is_some_and(|u| u.installing) {
            return;
        }
        let offer = self.offer(target);
        *offer = found.map(|(version, notes_url)| match offer.take() {
            // Same release as before: keep a previous install error visible.
            Some(u) if u.version == version => u,
            _ => UpdateOffer {
                version,
                notes_url,
                ..Default::default()
            },
        });
        self.publish();
    }

    fn install_update(&mut self, target: UpdateTarget) {
        if target == UpdateTarget::App && !self.self_managed {
            return;
        }
        let Some(u) = self.offer(target).as_mut() else {
            return;
        };
        if u.installing {
            return;
        }
        u.installing = true;
        u.error = None;
        self.publish();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match target {
                UpdateTarget::App => crate::update::check()
                    .and_then(|r| r.ok_or_else(|| "No newer release found".to_string()))
                    .and_then(|r| crate::update::install(&r))
                    .map(|_| Cmd::Restart),
                UpdateTarget::Cli => crate::setup::install_cli().map(|_| Cmd::CliUpdated),
            };
            let _ = tx.send(result.unwrap_or_else(|e| Cmd::UpdateFailed(target, e)));
        });
    }

    fn update_failed(&mut self, target: UpdateTarget, error: String) {
        let what = match target {
            UpdateTarget::App => "Limitray update failed",
            UpdateTarget::Cli => "CodexBar CLI update failed",
        };
        notify(what, &error, false);
        if let Some(u) = self.offer(target).as_mut() {
            u.installing = false;
            u.error = Some(error);
        }
        self.publish();
    }

    fn popup_alive(&mut self) -> bool {
        match &mut self.popup {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                _ => {
                    self.popup = None;
                    false
                }
            },
            None => false,
        }
    }

    fn show_popup(&mut self, token: Option<String>, pos: Option<(i32, i32)>) {
        if self.popup_alive() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut cmd = Command::new(exe);
        cmd.arg("popup").stdin(Stdio::null());
        if let Some((x, y)) = pos {
            cmd.arg("--at").arg(format!("{x},{y}"));
        }
        if let Some(t) = token {
            cmd.env("XDG_ACTIVATION_TOKEN", t);
        }
        crate::kwin::place_popup(pos.is_some());
        self.popup = cmd.spawn().ok();
        if self.snap.age_secs().is_none_or(|a| a > 60) {
            self.refresh(false);
        }
    }

    fn toggle_popup(&mut self, token: Option<String>, pos: Option<(i32, i32)>) {
        if self.popup_alive() {
            if let Some(mut c) = self.popup.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            return;
        }
        // Clicking the icon steals focus from an open popup, which closes itself on blur just
        // before this click arrives. Treat that as "close", not "reopen".
        if self
            .popup_closed_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
        {
            self.popup_closed_at = None;
            return;
        }
        self.show_popup(token, pos);
    }
}

fn serve(listener: UnixListener, tx: Sender<Cmd>) {
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let Ok(reader_stream) = stream.try_clone() else {
                return;
            };
            let mut line = String::new();
            if BufReader::new(reader_stream).read_line(&mut line).is_err() || line.len() > 1 << 20 {
                return;
            }
            let Ok(req) = serde_json::from_str::<Request>(&line) else {
                return;
            };
            let _ = stream.set_read_timeout(None);
            let cmd = match req {
                Request::Subscribe => Cmd::Subscribe(stream),
                Request::Get => Cmd::Get(stream),
                Request::Refresh => Cmd::Refresh { force: true },
                Request::RefreshIfStale { max_age } => Cmd::RefreshIfStale(max_age),
                Request::Show => Cmd::ShowPopup {
                    token: std::env::var("XDG_ACTIVATION_TOKEN").ok(),
                },
                Request::Toggle => Cmd::TogglePopup {
                    token: None,
                    pos: None,
                },
                Request::Quit => Cmd::Quit,
                Request::SetConfig { config } => Cmd::SetConfig(config),
                Request::PopupClosing => Cmd::PopupClosing,
                Request::InstallUpdate { target } => Cmd::InstallUpdate(target),
            };
            let _ = tx.send(cmd);
        });
    }
}

pub fn run(show_if_running: bool, show_on_start: bool) -> i32 {
    let sock = socket_path();
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if crate::ipc::connect().is_some() {
        if show_if_running {
            crate::ipc::send(&Request::Show);
        }
        return 0;
    }
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("limitray: cannot bind {}: {e}", sock.display());
            return 1;
        }
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));

    let cfg = Config::load();
    let snap = load_cache();
    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = channel();
    {
        let tx = tx.clone();
        std::thread::spawn(move || serve(listener, tx));
    }

    let desktop = theme::detect();
    let family = desktop
        .font_family
        .clone()
        .unwrap_or_else(|| "sans-serif".into());
    let font = theme::font_file(&family, "Bold")
        .or_else(|| theme::font_file("sans-serif", "Bold"))
        .and_then(|b| ab_glyph::FontVec::try_from_vec(b).ok())
        .map(Arc::new);
    let tray = CodexTray::new(snap.clone(), cfg.clone(), tx.clone(), desktop.dark, font);
    let tray = match tray.spawn() {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("limitray: tray unavailable ({e}); running headless");
            None
        }
    };

    let mut d = Daemon {
        cfg,
        snap,
        tx: tx.clone(),
        tray,
        subscribers: Vec::new(),
        popup: None,
        popup_closed_at: None,
        last_values: HashMap::new(),
        generation: 0,
        last_cost: None,
        live_fetches: 0,
        self_managed: crate::update::self_managed(),
        // Give login a minute to settle before talking to GitHub.
        next_update_check: Instant::now() + Duration::from_secs(60),
    };
    d.refresh(false);
    if show_on_start {
        d.show_popup(None, None);
    }
    // Countdown text in the tooltip/menu is derived on demand; nudge the tray each minute anyway
    // so the stale indicator and status stay current.
    let mut next_poll = Instant::now() + Duration::from_secs(d.cfg.refresh_secs);
    let mut next_tick = Instant::now() + Duration::from_secs(60);
    let mut restart = false;
    loop {
        let wake = next_poll.min(next_tick).min(d.next_update_check);
        match rx.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Refresh { force }) => {
                d.refresh(force);
                next_poll = Instant::now() + Duration::from_secs(d.cfg.refresh_secs);
            }
            Ok(Cmd::RefreshIfStale(max_age)) => {
                if d.snap.age_secs().is_none_or(|a| a > max_age) {
                    d.refresh(false);
                }
            }
            Ok(Cmd::FetchDone(p, c, g)) => d.fetch_done(p, c, g),
            Ok(Cmd::Notice(n)) => {
                d.snap.notice = n;
                d.publish();
            }
            Ok(Cmd::SetConfig(c)) => {
                d.set_config(c);
                next_poll = Instant::now() + Duration::from_secs(d.cfg.refresh_secs);
            }
            Ok(Cmd::TogglePopup { token, pos }) => d.toggle_popup(token, pos),
            Ok(Cmd::ShowPopup { token }) => d.show_popup(token, None),
            Ok(Cmd::PopupClosing) => d.popup_closed_at = Some(Instant::now()),
            Ok(Cmd::Subscribe(mut s)) => {
                if s.write_all(&d.state_line()).is_ok() {
                    d.subscribers.push(s);
                }
            }
            Ok(Cmd::Get(mut s)) => {
                let _ = s.write_all(&d.state_line());
            }
            Ok(Cmd::UpdateChecked(target, found)) => d.update_checked(target, found),
            Ok(Cmd::InstallUpdate(target)) => d.install_update(target),
            Ok(Cmd::UpdateFailed(target, e)) => d.update_failed(target, e),
            Ok(Cmd::CliUpdated) => {
                d.snap.cli_update = None;
                d.refresh(true);
                next_poll = Instant::now() + Duration::from_secs(d.cfg.refresh_secs);
            }
            Ok(Cmd::Restart) => {
                restart = true;
                break;
            }
            Ok(Cmd::Quit) => break,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                if now >= next_poll {
                    d.refresh(false);
                    next_poll = now + Duration::from_secs(d.cfg.refresh_secs);
                }
                if now >= next_tick {
                    d.publish();
                    next_tick = now + Duration::from_secs(60);
                }
                if now >= d.next_update_check {
                    d.check_updates();
                    d.next_update_check = now + UPDATE_CHECK_EVERY;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Some(mut c) = d.popup.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    if let Some(t) = d.tray.take() {
        t.shutdown().wait();
    }
    let _ = std::fs::remove_file(&sock);
    if restart {
        // Our own executable was just replaced; become the new one.
        let exe = crate::setup::installed_path();
        let err = Command::new(&exe).arg("--background").exec();
        eprintln!("limitray: cannot restart {}: {err}", exe.display());
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Window;
    use chrono::TimeZone;

    fn reset(hour: u32) -> Option<chrono::DateTime<Utc>> {
        Some(Utc.with_ymd_and_hms(2026, 10, 17, hour, 0, 0).unwrap())
    }

    fn window(
        key: &str,
        minutes: u32,
        used: f64,
        resets_at: Option<chrono::DateTime<Utc>>,
    ) -> Window {
        Window {
            key: key.into(),
            label: if minutes == 10080 {
                "Weekly"
            } else {
                "Session"
            }
            .into(),
            minutes: Some(minutes),
            used,
            resets_at,
            ..Default::default()
        }
    }

    fn claude(windows: Vec<Window>) -> ProviderUsage {
        ProviderUsage {
            id: "claude".into(),
            windows,
            ..Default::default()
        }
    }

    fn prev(
        entries: &[(&str, f64, Option<chrono::DateTime<Utc>>)],
    ) -> HashMap<String, (f64, Option<chrono::DateTime<Utc>>)> {
        entries
            .iter()
            .map(|(k, used, at)| (format!("claude/{k}"), (*used, *at)))
            .collect()
    }

    #[test]
    fn notifies_on_threshold_crossing() {
        let cfg = Config::default(); // thresholds 80, 95
        let alerts = pending_notifications(
            &cfg,
            &prev(&[("primary", 70.0, reset(17))]),
            &[claude(vec![window("primary", 300, 85.0, reset(17))])],
        );
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].summary, "Claude session at 85%");
        assert!(alerts[0].body.starts_with("Crossed 80%"));
        assert!(!alerts[0].urgent);
    }

    #[test]
    fn jumping_past_several_thresholds_notifies_once_for_the_highest() {
        let alerts = pending_notifications(
            &Config::default(),
            &prev(&[("primary", 70.0, reset(17))]),
            &[claude(vec![window("primary", 300, 97.0, reset(17))])],
        );
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].body.starts_with("Crossed 95%"));
        assert!(alerts[0].urgent, "the last threshold is critical");
    }

    #[test]
    fn stays_quiet_without_a_crossing() {
        let cfg = Config::default();
        // Already above 80 and still below 95.
        let p = [claude(vec![window("primary", 300, 90.0, reset(17))])];
        assert!(pending_notifications(&cfg, &prev(&[("primary", 85.0, reset(17))]), &p).is_empty());
        // First sighting of a window: nothing to compare against.
        assert!(pending_notifications(&cfg, &HashMap::new(), &p).is_empty());
        // Providers that failed to fetch never notify.
        let mut failed = claude(vec![window("primary", 300, 99.0, reset(17))]);
        failed.error = Some("offline".into());
        assert!(
            pending_notifications(&cfg, &prev(&[("primary", 10.0, reset(17))]), &[failed])
                .is_empty()
        );
        // Notifications turned off.
        let off = Config {
            notifications: false,
            ..Config::default()
        };
        let p = [claude(vec![window("primary", 300, 99.0, reset(17))])];
        assert!(pending_notifications(&off, &prev(&[("primary", 10.0, reset(17))]), &p).is_empty());
    }

    #[test]
    fn notifies_on_session_reset_only() {
        let cfg = Config::default();
        let alerts = pending_notifications(
            &cfg,
            &prev(&[("primary", 90.0, reset(17))]),
            &[claude(vec![window("primary", 300, 5.0, reset(22))])],
        );
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].summary, "Claude session reset");

        // Same reset time: a correction in the data, not a new window.
        let same = pending_notifications(
            &cfg,
            &prev(&[("primary", 90.0, reset(17))]),
            &[claude(vec![window("primary", 300, 5.0, reset(17))])],
        );
        assert!(same.is_empty());

        // Weekly windows don't announce resets.
        let weekly = pending_notifications(
            &cfg,
            &prev(&[("secondary", 90.0, reset(17))]),
            &[claude(vec![window("secondary", 10080, 5.0, reset(22))])],
        );
        assert!(weekly.is_empty());

        let quiet = Config {
            notify_reset: false,
            ..Config::default()
        };
        let off = pending_notifications(
            &quiet,
            &prev(&[("primary", 90.0, reset(17))]),
            &[claude(vec![window("primary", 300, 5.0, reset(22))])],
        );
        assert!(off.is_empty());
    }

    #[test]
    fn failed_fetch_keeps_last_good_windows() {
        let old = Snapshot {
            providers: vec![claude(vec![window("primary", 300, 42.0, reset(17))])],
            ..Default::default()
        };
        let failed = ProviderUsage {
            id: "claude".into(),
            error: Some("rate limited".into()),
            ..Default::default()
        };
        let unknown = ProviderUsage {
            id: "codex".into(),
            error: Some("not logged in".into()),
            ..Default::default()
        };
        let merged = merge_last_good(&old, vec![failed, unknown.clone()]);
        assert_eq!(merged[0].windows, old.providers[0].windows);
        assert_eq!(merged[0].error.as_deref(), Some("rate limited"));
        assert_eq!(merged[1], unknown, "nothing to fall back to");

        // A successful fetch replaces the old data.
        let fresh = claude(vec![window("primary", 300, 50.0, reset(17))]);
        assert_eq!(merge_last_good(&old, vec![fresh.clone()]), vec![fresh]);
    }

    #[test]
    fn cli_install_backs_off() {
        let now = Instant::now();
        assert!(cli_install_due(None, now, false));
        assert!(!cli_install_due(
            Some(now),
            now + Duration::from_secs(60),
            false
        ));
        assert!(cli_install_due(
            Some(now),
            now + Duration::from_secs(60),
            true
        ));
        assert!(cli_install_due(Some(now), now + CLI_INSTALL_RETRY, false));
    }
}
