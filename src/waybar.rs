//! `limitray waybar`: one JSON line per change for a Waybar `custom` module with
//! `"return-type": "json"`. Follows the tray's provider and window choice.

use crate::config::Config;
use crate::format;
use crate::icon::Level;
use crate::ipc::{self, State};
use crate::model::Snapshot;
use serde::Serialize;
use std::io::Write;
use std::time::Duration;

#[derive(Debug, PartialEq, Serialize)]
pub struct Output {
    pub text: String,
    pub alt: String,
    pub tooltip: String,
    pub class: String,
    pub percentage: u8,
}

/// Pango markup is on in Waybar tooltips.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn offline() -> Output {
    Output {
        text: "–".into(),
        alt: "offline".into(),
        tooltip: "Limitray is not running. Start it with `limitray --background`.".into(),
        class: "offline".into(),
        percentage: 0,
    }
}

pub fn render(snap: &Snapshot, cfg: &Config) -> Output {
    let p = snap
        .provider(&cfg.tray_provider)
        .or_else(|| snap.providers.first());
    let w = p.and_then(|p| cfg.tray_window.pick(p));
    let (text, percentage, mut class) = match (p, w) {
        (_, Some(w)) => {
            let v = cfg.display_value(w.used);
            let class = match crate::tray::level(cfg, w) {
                Level::Critical => "critical",
                Level::Warn => "warning",
                _ => "normal",
            };
            (format!("{v:.0}%"), v.round().clamp(0.0, 100.0) as u8, class)
        }
        (Some(_), None) => ("!".into(), 0, "error"),
        (None, None) => ("…".into(), 0, "loading"),
    };
    if class != "error" && snap.is_stale(cfg.refresh_secs) {
        class = "stale";
    }

    let mut lines = Vec::new();
    for p in &snap.providers {
        let mut head = format!("<b>{}</b>", esc(&p.name()));
        if let Some(plan) = &p.plan {
            head.push_str(&format!(" · {}", esc(plan)));
        }
        lines.push(head);
        if let Some(e) = p.error.as_ref().filter(|_| p.windows.is_empty()) {
            lines.push(esc(e));
            continue;
        }
        for w in p.windows.iter().filter(|w| !w.scoped) {
            let mut line = format!(
                "{}: {:.0}% {} · {}",
                esc(&w.label),
                cfg.display_value(w.used),
                cfg.display_suffix(),
                format::resets_in(w.resets_at)
            );
            if let Some(at) = w.resets_at {
                line.push_str(&format!(" ({})", format::clock(at)));
            }
            lines.push(line);
        }
        if let Some(c) = snap.cost(&p.id) {
            lines.push(format!(
                "Today {} · 30 days {}",
                format::money(c.today),
                format::money(c.last30)
            ));
        }
        if let Some(s) = p.status.as_ref().filter(|s| !s.is_ok()) {
            lines.push(format!("⚠ {}", esc(&s.description)));
        }
    }
    if let Some(n) = &snap.notice {
        lines.push(format!("<i>{}</i>", esc(n)));
    }
    lines.push(format!(
        "<i>{}</i>",
        if snap.busy {
            "Refreshing…".to_string()
        } else {
            format!("Updated {}", format::ago(snap.updated_at))
        }
    ));
    Output {
        text,
        alt: p.map(|p| p.id.clone()).unwrap_or_default(),
        tooltip: lines.join("\n"),
        class: class.into(),
        percentage,
    }
}

/// Prints `out` unless it repeats the last line. False once stdout is gone (Waybar quit).
fn emit(out: &Output, last: &mut String) -> bool {
    let line = serde_json::to_string(out).unwrap();
    if line == *last {
        return true;
    }
    let mut stdout = std::io::stdout().lock();
    let ok = writeln!(stdout, "{line}")
        .and_then(|_| stdout.flush())
        .is_ok();
    *last = line;
    ok
}

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--once") {
        let out = match ipc::get() {
            Some(State {
                snapshot, config, ..
            }) => render(&snapshot, &config),
            None => offline(),
        };
        println!("{}", serde_json::to_string(&out).unwrap());
        return 0;
    }
    // A bar module usually starts with the session; bring the tray up if autostart hasn't yet.
    if ipc::connect().is_none() && !args.iter().any(|a| a == "--no-start") {
        if let Ok(exe) = std::env::current_exe() {
            crate::setup::start_detached(&exe);
        }
    }
    let mut last = String::new();
    loop {
        // The daemon pushes a state after every change and once a minute, which keeps the
        // "resets in" countdown in the tooltip current.
        let connected = ipc::subscribe(|s| {
            if !emit(&render(&s.snapshot, &s.config), &mut last) {
                std::process::exit(0);
            }
        });
        if !emit(&offline(), &mut last) {
            return 0;
        }
        std::thread::sleep(Duration::from_secs(if connected { 1 } else { 5 }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{QuotaDisplay, TrayWindow};
    use crate::model::{ProviderUsage, Window};
    use chrono::Utc;

    fn snap(session: f64, weekly: f64) -> Snapshot {
        let w = |key: &str, label: &str, minutes, used| Window {
            key: key.into(),
            label: label.into(),
            minutes: Some(minutes),
            used,
            ..Default::default()
        };
        Snapshot {
            providers: vec![ProviderUsage {
                id: "claude".into(),
                plan: Some("Max".into()),
                windows: vec![
                    w("primary", "Session", 300, session),
                    w("secondary", "Weekly", 10080, weekly),
                ],
                ..Default::default()
            }],
            updated_at: Some(Utc::now()),
            ..Default::default()
        }
    }

    #[test]
    fn follows_the_tray_window_and_thresholds() {
        let mut cfg = Config::default();
        let out = render(&snap(42.4, 81.0), &cfg);
        assert_eq!((out.text.as_str(), out.percentage), ("42%", 42));
        assert_eq!((out.class.as_str(), out.alt.as_str()), ("normal", "claude"));
        assert!(out.tooltip.contains("<b>Claude</b> · Max"));
        assert!(out.tooltip.contains("Weekly: 81% used"));

        cfg.tray_window = TrayWindow::Tightest;
        assert_eq!(render(&snap(42.4, 81.0), &cfg).class, "warning");
        assert_eq!(render(&snap(97.0, 81.0), &cfg).class, "critical");

        cfg.quota_display = QuotaDisplay::Remaining;
        assert_eq!(render(&snap(42.4, 81.0), &cfg).text, "19%");
    }

    #[test]
    fn flags_stale_errors_and_empty_states() {
        let cfg = Config::default();
        let mut s = snap(10.0, 10.0);
        s.from_cache = true;
        assert_eq!(render(&s, &cfg).class, "stale");

        s.providers[0].windows.clear();
        s.providers[0].error = Some("Token <expired> & gone".into());
        let out = render(&s, &cfg);
        assert_eq!((out.text.as_str(), out.class.as_str()), ("!", "error"));
        assert!(out.tooltip.contains("Token &lt;expired&gt; &amp; gone"));

        assert_eq!(render(&Snapshot::default(), &cfg).class, "loading");
    }
}
