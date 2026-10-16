//! Background service: polls the CodexBar CLI, owns the tray icon, serves the popup over IPC.

use crate::config::{cache_path, socket_path, Config};
use crate::fetch;
use crate::ipc::{Request, State};
use crate::model::{CostSummary, ProviderUsage, Snapshot};
use crate::tray::CodexTray;
use crate::{format, theme};
use chrono::Utc;
use ksni::blocking::TrayMethods;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        pos: (i32, i32),
    },
    ShowPopup {
        token: Option<String>,
    },
    PopupClosing,
    Subscribe(UnixStream),
    Get(UnixStream),
    Quit,
}

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
    n.appname("CodexBar")
        .summary(summary)
        .body(body)
        .icon("codexbar-tray")
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
    if !cfg.notifications {
        return;
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
                let critical = Some(&t) == cfg.notify_thresholds.last();
                notify(
                    &format!("{} {} at {:.0}%", p.name(), w.label.to_lowercase(), w.used),
                    &format!("Crossed {t}% · {}", format::resets_in(w.resets_at)),
                    critical,
                );
            } else if cfg.notify_reset
                && w.is_session()
                && before >= 50.0
                && w.used + 30.0 < before
                && old_reset != w.resets_at
            {
                notify(
                    &format!("{} {} reset", p.name(), w.label.to_lowercase()),
                    &format!("Back to {:.0}% used. Fresh window.", w.used),
                    false,
                );
            }
        }
    }
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
            if crate::setup::working_cli(&cfg).is_none() {
                let _ = tx.send(Cmd::Notice(Some("Downloading the CodexBar CLI…".into())));
                let result = crate::setup::install_cli();
                let _ = tx.send(Cmd::Notice(result.err().map(|e| {
                    format!("CodexBar CLI missing: {e}. Run `codexbar-tray update-cli`.")
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
        // Keep the last good windows for a provider whose fetch failed, flagged with the error.
        let mut merged = Vec::new();
        for mut p in providers {
            if p.error.is_some() && p.windows.is_empty() {
                if let Some(old) = self.snap.provider(&p.id).filter(|o| !o.windows.is_empty()) {
                    let mut keep = old.clone();
                    keep.error = p.error.take();
                    merged.push(keep);
                    continue;
                }
            }
            merged.push(p);
        }
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

    fn toggle_popup(&mut self, token: Option<String>, pos: (i32, i32)) {
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
        self.show_popup(token, Some(pos));
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
                    pos: (0, 0),
                },
                Request::Quit => Cmd::Quit,
                Request::SetConfig { config } => Cmd::SetConfig(config),
                Request::PopupClosing => Cmd::PopupClosing,
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
            eprintln!("codexbar-tray: cannot bind {}: {e}", sock.display());
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
            eprintln!("codexbar-tray: tray unavailable ({e}); running headless");
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
    };
    d.refresh(false);
    if show_on_start {
        d.show_popup(None, None);
    }
    // Countdown text in the tooltip/menu is derived on demand; nudge the tray each minute anyway
    // so the stale indicator and status stay current.
    let mut next_poll = Instant::now() + Duration::from_secs(d.cfg.refresh_secs);
    let mut next_tick = Instant::now() + Duration::from_secs(60);
    loop {
        let wake = next_poll.min(next_tick);
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
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Some(mut c) = d.popup.take() {
        let _ = c.kill();
    }
    if let Some(t) = d.tray.take() {
        t.shutdown().wait();
    }
    let _ = std::fs::remove_file(&sock);
    0
}
