use crate::config::{Config, QuotaDisplay, TrayStyle, TrayWindow};
use crate::daemon::Cmd;
use crate::format;
use crate::icon::{self, IconSpec, Level};
use crate::model::{ProviderUsage, Snapshot, Window};
use ab_glyph::FontVec;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, Status, ToolTip};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

const KNOWN_PROVIDERS: [&str; 10] = [
    "claude",
    "codex",
    "gemini",
    "cursor",
    "copilot",
    "openrouter",
    "zai",
    "opencode",
    "antigravity",
    "kiro",
];

pub struct CodexTray {
    pub snapshot: Snapshot,
    pub config: Config,
    pub tx: Sender<Cmd>,
    pub dark_panel: bool,
    pub font: Option<Arc<FontVec>>,
    pub activation_token: Option<(String, Instant)>,
    icons: Vec<ksni::Icon>,
    spec: Option<IconSpec>,
}

pub fn level(cfg: &Config, w: &Window) -> Level {
    let t = &cfg.notify_thresholds;
    let critical = t.last().copied().unwrap_or(95) as f64;
    let warn = t.first().copied().unwrap_or(80) as f64;
    if w.used >= critical {
        Level::Critical
    } else if w.used >= warn {
        Level::Warn
    } else {
        Level::Normal
    }
}

impl CodexTray {
    pub fn new(
        snapshot: Snapshot,
        config: Config,
        tx: Sender<Cmd>,
        dark_panel: bool,
        font: Option<Arc<FontVec>>,
    ) -> Self {
        let mut t = Self {
            snapshot,
            config,
            tx,
            dark_panel,
            font,
            activation_token: None,
            icons: Vec::new(),
            spec: None,
        };
        t.rebuild();
        t
    }

    fn provider(&self) -> Option<&ProviderUsage> {
        self.snapshot
            .provider(&self.config.tray_provider)
            .or_else(|| self.snapshot.providers.first())
    }

    fn tracked<'a>(&self, p: &'a ProviderUsage) -> Option<&'a Window> {
        match self.config.tray_window {
            TrayWindow::Session => p.session(),
            TrayWindow::Weekly => p.weekly().or_else(|| p.session()),
            TrayWindow::Tightest => p.tightest(),
        }
    }

    fn fraction(&self, w: Option<&Window>) -> Option<f32> {
        w.map(|w| (self.config.display_value(w.used) / 100.0) as f32)
    }

    /// Recompute the icon after any state change; pixmaps are only re-rendered when they differ.
    pub fn rebuild(&mut self) {
        let stale = self.snapshot.from_cache
            || self
                .snapshot
                .age_secs()
                .is_some_and(|a| a > (self.config.refresh_secs as i64 * 3).max(900));
        let p = self.provider();
        let (primary, secondary, text, provider) = match p {
            Some(p) if p.error.is_none() || !p.windows.is_empty() => {
                let first = match self.config.tray_style {
                    TrayStyle::Bars | TrayStyle::Ring => p.session(),
                    _ => self.tracked(p),
                };
                let second = match self.config.tray_style {
                    TrayStyle::Bars | TrayStyle::Ring => p.weekly(),
                    _ => None,
                };
                let text = first.map(|w| format!("{:.0}", self.config.display_value(w.used)));
                (first.cloned(), second.cloned(), text, p.id.clone())
            }
            Some(p) => (None, None, Some("!".into()), p.id.clone()),
            None => (None, None, None, self.config.tray_provider.clone()),
        };
        let lv = |w: &Option<Window>| match w {
            _ if stale => Level::Stale,
            Some(w) => level(&self.config, w),
            None => Level::Stale,
        };
        let spec = IconSpec {
            style: self.config.tray_style,
            primary: self.fraction(primary.as_ref()),
            secondary: self.fraction(secondary.as_ref()),
            primary_level: lv(&primary),
            secondary_level: lv(&secondary),
            text,
            brand: icon::brand_color(&provider),
            provider,
            dark_panel: self.dark_panel,
        };
        if self.spec.as_ref() != Some(&spec) {
            self.icons = [16, 22, 24, 32, 44, 48, 64]
                .iter()
                .map(|&s| icon::render(&spec, self.font.as_deref(), s))
                .collect();
            self.spec = Some(spec);
        }
    }

    fn window_line(&self, w: &Window) -> String {
        let v = self.config.display_value(w.used);
        let mut s = format!(
            "{} {:.0}% {} · {}",
            w.label,
            v,
            self.config.display_suffix(),
            format::resets_in(w.resets_at)
        );
        if let Some(at) = w.resets_at {
            s.push_str(&format!(" ({})", format::clock(at)));
        }
        s
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl ksni::Tray for CodexTray {
    fn id(&self) -> String {
        "codexbar-tray".into()
    }

    fn title(&self) -> String {
        "CodexBar".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn status(&self) -> Status {
        let critical = self
            .provider()
            .and_then(|p| self.tracked(p))
            .is_some_and(|w| level(&self.config, w) == Level::Critical);
        if critical {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        let mut title = String::from("CodexBar");
        let mut lines: Vec<String> = Vec::new();
        for p in &self.snapshot.providers {
            if self.snapshot.providers.len() > 1 || title == "CodexBar" {
                if self.snapshot.providers.len() == 1 {
                    title = match &p.plan {
                        Some(plan) => format!("{} · {}", p.name(), plan),
                        None => p.name(),
                    };
                } else {
                    lines.push(format!("<b>{}</b>", esc(&p.name())));
                }
            }
            if let Some(e) = &p.error {
                if p.windows.is_empty() {
                    lines.push(esc(e));
                    continue;
                }
            }
            for w in p.windows.iter().filter(|w| !w.scoped) {
                let v = self.config.display_value(w.used);
                lines.push(format!(
                    "{}: <b>{:.0}%</b> {} · {}",
                    esc(&w.label),
                    v,
                    self.config.display_suffix(),
                    format::resets_in(w.resets_at)
                ));
            }
            if let Some(c) = self.snapshot.cost(&p.id) {
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
        if let Some(n) = &self.snapshot.notice {
            lines.push(format!("<i>{}</i>", esc(n)));
        } else if self.snapshot.providers.is_empty() {
            lines.push("Loading usage…".into());
        }
        let footer = if self.snapshot.busy {
            "Refreshing…".to_string()
        } else {
            format!("Updated {}", format::ago(self.snapshot.updated_at))
        };
        lines.push(format!(
            "<i>{footer} · middle-click to refresh, scroll to switch</i>"
        ));
        ToolTip {
            title,
            description: lines.join("<br/>"),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        let token = self
            .activation_token
            .take()
            .filter(|(_, at)| at.elapsed().as_secs() < 10)
            .map(|(t, _)| t);
        self.send(Cmd::TogglePopup {
            token,
            pos: Some((x, y)),
        });
    }

    fn provide_xdg_activation_token(&mut self, token: String) {
        self.activation_token = Some((token, Instant::now()));
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.send(Cmd::Refresh { force: true });
    }

    fn scroll(&mut self, delta: i32, orientation: ksni::Orientation) {
        if orientation != ksni::Orientation::Vertical || delta == 0 {
            return;
        }
        // Cycle through (provider, window) views: Claude session → Claude weekly → next provider …
        let mut views = Vec::new();
        for p in &self.config.providers {
            views.push((p.clone(), TrayWindow::Session));
            views.push((p.clone(), TrayWindow::Weekly));
        }
        let current = views
            .iter()
            .position(|(p, w)| *p == self.config.tray_provider && *w == self.config.tray_window)
            .unwrap_or(0);
        let next = (current as i32 + delta.signum()).rem_euclid(views.len() as i32) as usize;
        let mut cfg = self.config.clone();
        (cfg.tray_provider, cfg.tray_window) = views[next].clone();
        // Bars/rings already show both windows; switching implies a single-window style.
        if matches!(cfg.tray_style, TrayStyle::Bars | TrayStyle::Ring) && cfg.providers.len() == 1 {
            cfg.tray_style = TrayStyle::Percent;
        }
        self.config = cfg.clone();
        self.rebuild();
        self.send(Cmd::SetConfig(cfg));
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        let open = |label: String| -> MenuItem<Self> {
            StandardItem {
                label,
                activate: Box::new(|t: &mut Self| t.send(Cmd::ShowPopup { token: None })),
                ..Default::default()
            }
            .into()
        };
        for p in &self.snapshot.providers {
            let head = match &p.plan {
                Some(plan) => format!("{} — {}", p.name(), plan),
                None => p.name(),
            };
            items.push(
                StandardItem {
                    label: head,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            if let Some(e) = &p.error {
                if p.windows.is_empty() {
                    items.push(open(format!(
                        "    {}",
                        e.chars().take(70).collect::<String>()
                    )));
                }
            }
            for w in p.windows.iter().filter(|w| !w.scoped) {
                items.push(open(format!("    {}", self.window_line(w))));
            }
            if let Some(c) = self.snapshot.cost(&p.id) {
                items.push(open(format!(
                    "    Spend: today {} · 30 days {}",
                    format::money(c.today),
                    format::money(c.last30)
                )));
            }
        }
        if self.snapshot.providers.is_empty() {
            items.push(
                StandardItem {
                    label: "Loading usage…".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open CodexBar".into(),
                icon_name: "view-statistics".into(),
                activate: Box::new(|t: &mut Self| t.send(Cmd::ShowPopup { token: None })),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: if self.snapshot.busy {
                    "Refreshing…".into()
                } else {
                    format!(
                        "Refresh now  (updated {})",
                        format::ago(self.snapshot.updated_at)
                    )
                },
                icon_name: "view-refresh".into(),
                enabled: !self.snapshot.busy,
                activate: Box::new(|t: &mut Self| t.send(Cmd::Refresh { force: true })),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        let style_idx = TrayStyle::ALL
            .iter()
            .position(|s| *s == self.config.tray_style)
            .unwrap_or(0);
        let window_idx = TrayWindow::ALL
            .iter()
            .position(|s| *s == self.config.tray_window)
            .unwrap_or(0);
        let mut icon_menu: Vec<MenuItem<Self>> = vec![RadioGroup {
            selected: style_idx,
            select: Box::new(|t: &mut Self, i| {
                t.update_config(|c| c.tray_style = TrayStyle::ALL[i])
            }),
            options: TrayStyle::ALL
                .iter()
                .map(|s| RadioItem {
                    label: s.label().into(),
                    ..Default::default()
                })
                .collect(),
        }
        .into()];
        icon_menu.push(MenuItem::Separator);
        icon_menu.push(
            StandardItem {
                label: "Percentage / logo tracks:".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        icon_menu.push(
            RadioGroup {
                selected: window_idx,
                select: Box::new(|t: &mut Self, i| {
                    t.update_config(|c| c.tray_window = TrayWindow::ALL[i])
                }),
                options: TrayWindow::ALL
                    .iter()
                    .map(|s| RadioItem {
                        label: s.label().into(),
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
        );
        if self.config.providers.len() > 1 {
            icon_menu.push(MenuItem::Separator);
            icon_menu.push(
                StandardItem {
                    label: "Provider in icon:".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            let providers = self.config.providers.clone();
            icon_menu.push(
                RadioGroup {
                    selected: providers
                        .iter()
                        .position(|p| *p == self.config.tray_provider)
                        .unwrap_or(0),
                    select: Box::new(move |t: &mut Self, i| {
                        let p = providers[i].clone();
                        t.update_config(|c| c.tray_provider = p)
                    }),
                    options: self
                        .config
                        .providers
                        .iter()
                        .map(|p| RadioItem {
                            label: crate::model::provider_name(p),
                            ..Default::default()
                        })
                        .collect(),
                }
                .into(),
            );
        }
        items.push(
            SubMenu {
                label: "Tray icon".into(),
                icon_name: "preferences-desktop-icons".into(),
                submenu: icon_menu,
                ..Default::default()
            }
            .into(),
        );

        items.push(
            SubMenu {
                label: "Show percentages as".into(),
                submenu: vec![RadioGroup {
                    selected: if self.config.quota_display == QuotaDisplay::Used {
                        0
                    } else {
                        1
                    },
                    select: Box::new(|t: &mut Self, i| {
                        t.update_config(|c| {
                            c.quota_display = if i == 0 {
                                QuotaDisplay::Used
                            } else {
                                QuotaDisplay::Remaining
                            }
                        })
                    }),
                    options: vec![
                        RadioItem {
                            label: "Used".into(),
                            ..Default::default()
                        },
                        RadioItem {
                            label: "Remaining".into(),
                            ..Default::default()
                        },
                    ],
                }
                .into()],
                ..Default::default()
            }
            .into(),
        );

        let mut provider_menu: Vec<MenuItem<Self>> = Vec::new();
        let mut known: Vec<String> = KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect();
        for p in &self.config.providers {
            if !known.contains(p) {
                known.push(p.clone());
            }
        }
        for id in known {
            let checked = self.config.providers.contains(&id);
            provider_menu.push(
                CheckmarkItem {
                    label: crate::model::provider_name(&id),
                    checked,
                    activate: Box::new(move |t: &mut Self| {
                        let id = id.clone();
                        t.update_config(|c| {
                            if c.providers.contains(&id) {
                                c.providers.retain(|p| *p != id);
                            } else {
                                c.providers.push(id);
                            }
                        })
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            SubMenu {
                label: "Providers".into(),
                icon_name: "network-server".into(),
                submenu: provider_menu,
                ..Default::default()
            }
            .into(),
        );
        items.push(
            CheckmarkItem {
                label: "Notifications".into(),
                checked: self.config.notifications,
                activate: Box::new(|t: &mut Self| {
                    t.update_config(|c| c.notifications = !c.notifications)
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        if self.config.providers.iter().any(|p| p == "claude") {
            items.push(
                StandardItem {
                    label: "claude.ai usage page".into(),
                    icon_name: "internet-web-browser".into(),
                    activate: Box::new(|_: &mut Self| {
                        crate::daemon::open_url("https://claude.ai/settings/usage")
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        let status_urls: Vec<(String, String)> = self
            .snapshot
            .providers
            .iter()
            .filter_map(|p| {
                p.status.as_ref().filter(|s| !s.url.is_empty()).map(|s| {
                    (
                        format!("{} status: {}", p.name(), s.description),
                        s.url.clone(),
                    )
                })
            })
            .collect();
        for (label, url) in status_urls {
            items.push(
                StandardItem {
                    label,
                    icon_name: "dialog-information".into(),
                    activate: Box::new(move |_: &mut Self| crate::daemon::open_url(&url)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(Cmd::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

impl CodexTray {
    fn update_config(&mut self, f: impl FnOnce(&mut Config)) {
        let mut cfg = self.config.clone();
        f(&mut cfg);
        cfg.sanitize();
        self.config = cfg.clone();
        self.rebuild();
        self.send(Cmd::SetConfig(cfg));
    }
}
