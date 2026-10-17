//! The popover window, launched by the daemon on tray click. Renders from the daemon's pushed
//! state (or the disk cache), so it is populated on the very first frame.

use crate::config::{Config, QuotaDisplay, TrayStyle, TrayWindow};
use crate::format;
use crate::icon::brand_color;
use crate::ipc::{self, Request, State};
use crate::model::{
    provider_name, CostSummary, ProviderUsage, Snapshot, UpdateOffer, UpdateTarget, Window,
};
use crate::theme::{self, Rgb};
use chrono::{Duration as ChronoDuration, Local};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIDTH: f32 = 380.0;
const MAX_HEIGHT: f32 = 820.0;
const KNOWN: [&str; 10] = [
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

#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    card: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    faint: Color32,
    track: Color32,
    hover: Color32,
    good: Color32,
    warn: Color32,
    bad: Color32,
    dark: bool,
}

fn c(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

impl Palette {
    fn from_desktop(d: &theme::Desktop) -> Self {
        let (bg, fg) = (d.window_bg, d.window_fg);
        let card = if d.dark {
            bg.mix(fg, 0.055)
        } else {
            bg.mix(Rgb(255, 255, 255), 0.7)
        };
        Self {
            bg: c(bg),
            card: c(card),
            border: c(bg.mix(fg, if d.dark { 0.11 } else { 0.12 })),
            text: c(fg),
            muted: c(fg.mix(bg, 0.38)),
            faint: c(fg.mix(bg, 0.58)),
            track: c(bg.mix(fg, if d.dark { 0.14 } else { 0.10 })),
            hover: c(bg.mix(fg, 0.09)),
            good: Color32::from_rgb(76, 195, 138),
            warn: Color32::from_rgb(245, 165, 36),
            bad: Color32::from_rgb(239, 83, 80),
            dark: d.dark,
        }
    }
}

fn brand(id: &str) -> Color32 {
    let (r, g, b) = brand_color(id);
    Color32::from_rgb(r, g, b)
}

fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}
fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}
fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

fn install_fonts(ctx: &egui::Context, family: &str) {
    let mut fonts = FontDefinitions::default();
    let mut add = |key: &str, style: &str, fam: FontFamily| {
        let data =
            theme::font_file(family, style).or_else(|| theme::font_file("sans-serif", style));
        let mut chain = fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        if let Some(bytes) = data {
            fonts
                .font_data
                .insert(key.into(), Arc::new(FontData::from_owned(bytes)));
            chain.insert(0, key.into());
        }
        fonts.families.insert(fam, chain);
    };
    add("ui-medium", "Medium", medium());
    add("ui-semibold", "SemiBold", semibold());
    add("ui-bold", "Bold", bold());
    if let Some(bytes) =
        theme::font_file(family, "Regular").or_else(|| theme::font_file("sans-serif", "Regular"))
    {
        fonts
            .font_data
            .insert("ui-regular".into(), Arc::new(FontData::from_owned(bytes)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "ui-regular".into());
    }
    ctx.set_fonts(fonts);
}

#[derive(PartialEq)]
enum View {
    Usage,
    Settings,
}

pub struct PopupApp {
    state: Arc<Mutex<Option<State>>>,
    connected: Arc<AtomicBool>,
    pal: Palette,
    view: View,
    selected: String,
    draft: Option<Config>,
    new_provider: String,
    thresholds_text: String,
    had_focus: bool,
    opened: Instant,
    last_height: f32,
    resized_at: Instant,
    closing: bool,
    frames: u32,
    snapshot_to: Option<String>,
}

impl PopupApp {
    fn new(cc: &eframe::CreationContext<'_>, desktop: &theme::Desktop) -> Self {
        let ctx = &cc.egui_ctx;
        egui_extras::install_image_loaders(ctx);
        install_fonts(ctx, desktop.font_family.as_deref().unwrap_or("sans-serif"));
        let pal = Palette::from_desktop(desktop);
        let mut visuals = if pal.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = pal.bg;
        visuals.window_fill = pal.bg;
        visuals.extreme_bg_color = pal.card;
        visuals.override_text_color = Some(pal.text);
        visuals.widgets.inactive.weak_bg_fill = pal.card;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.border);
        visuals.widgets.hovered.weak_bg_fill = pal.hover;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.border);
        visuals.selection.bg_fill = brand("claude").gamma_multiply(0.85);
        visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        ctx.set_visuals(visuals);
        ctx.global_style_mut(|s| {
            s.spacing.item_spacing = vec2(8.0, 6.0);
            s.spacing.button_padding = vec2(10.0, 5.0);
            s.spacing.slider_width = 200.0;
            s.text_styles.insert(
                egui::TextStyle::Body,
                FontId::new(13.5, FontFamily::Proportional),
            );
            s.text_styles
                .insert(egui::TextStyle::Button, FontId::new(13.0, medium()));
            s.text_styles.insert(
                egui::TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            );
            for w in [
                &mut s.visuals.widgets.inactive,
                &mut s.visuals.widgets.hovered,
                &mut s.visuals.widgets.active,
                &mut s.visuals.widgets.noninteractive,
            ] {
                w.corner_radius = CornerRadius::same(8);
            }
        });

        let state: Arc<Mutex<Option<State>>> = Arc::new(Mutex::new(None));
        let connected = Arc::new(AtomicBool::new(false));
        // Immediate first paint from the daemon (one round trip) or the cache file.
        if let Some(s) = ipc::get() {
            connected.store(true, Ordering::Relaxed);
            *state.lock().unwrap() = Some(s);
        } else {
            let snapshot = std::fs::read(crate::config::cache_path())
                .ok()
                .and_then(|b| serde_json::from_slice::<Snapshot>(&b).ok())
                .unwrap_or_default();
            *state.lock().unwrap() = Some(State {
                snapshot,
                config: Config::load(),
            });
        }
        {
            let (state, connected, ctx) = (state.clone(), connected.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let ok = ipc::subscribe(|s| {
                    connected.store(true, Ordering::Relaxed);
                    *state.lock().unwrap() = Some(s);
                    ctx.request_repaint();
                });
                connected.store(false, Ordering::Relaxed);
                ctx.request_repaint();
                std::thread::sleep(Duration::from_secs(if ok { 1 } else { 3 }));
            });
        }
        ipc::send(&Request::RefreshIfStale { max_age: 60 });
        let selected = state
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.config.providers[0].clone())
            .unwrap_or_else(|| "claude".into());
        Self {
            state,
            connected,
            pal,
            view: if std::env::var("CODEXBAR_VIEW").as_deref() == Ok("settings") {
                View::Settings
            } else {
                View::Usage
            },
            selected,
            draft: None,
            new_provider: String::new(),
            thresholds_text: String::new(),
            had_focus: false,
            opened: Instant::now(),
            last_height: 0.0,
            resized_at: Instant::now(),
            closing: false,
            frames: 0,
            snapshot_to: std::env::var("CODEXBAR_SNAPSHOT_TO").ok(),
        }
    }

    fn close(&mut self, ctx: &egui::Context, blur: bool) {
        if self.closing {
            return;
        }
        self.closing = true;
        if blur {
            ipc::send(&Request::PopupClosing);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

// ---------- small painted widgets ----------

fn icon_button(
    ui: &mut Ui,
    pal: &Palette,
    tooltip: &str,
    paint: impl FnOnce(&egui::Painter, Rect, Color32),
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), pal.hover);
    }
    let color = if resp.hovered() { pal.text } else { pal.muted };
    paint(ui.painter(), rect.shrink(7.0), color);
    resp.on_hover_text(tooltip)
}

fn paint_refresh(p: &egui::Painter, r: Rect, color: Color32, angle: f32) {
    let center = r.center();
    let radius = r.width() / 2.0;
    let n = 28;
    let start = angle - 0.3;
    let sweep = std::f32::consts::TAU * 0.78;
    let pts: Vec<_> = (0..=n)
        .map(|i| {
            let a = start + sweep * i as f32 / n as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    let end = *pts.last().unwrap();
    p.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
    let a = start + sweep;
    let tangent = vec2(-a.sin(), a.cos());
    let normal = vec2(a.cos(), a.sin());
    p.add(egui::Shape::convex_polygon(
        vec![
            end + tangent * 3.2,
            end + normal * 2.6 - tangent * 0.8,
            end - normal * 2.6 - tangent * 0.8,
        ],
        color,
        Stroke::NONE,
    ));
}

fn paint_sliders(p: &egui::Painter, r: Rect, color: Color32) {
    for (i, knob) in [0.3f32, 0.7, 0.45].iter().enumerate() {
        let y = r.top() + r.height() * (i as f32 + 0.5) / 3.0;
        p.line_segment(
            [pos2(r.left(), y), pos2(r.right(), y)],
            Stroke::new(1.4, color),
        );
        p.circle_filled(pos2(r.left() + r.width() * knob, y), 2.4, color);
    }
}

fn paint_back(p: &egui::Painter, r: Rect, color: Color32) {
    let c = r.center();
    p.add(egui::Shape::line(
        vec![c + vec2(3.0, -6.0), c + vec2(-3.0, 0.0), c + vec2(3.0, 6.0)],
        Stroke::new(1.8, color),
    ));
}

fn toggle(ui: &mut Ui, pal: &Palette, on: &mut bool, label: &str, hint: Option<&str>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(label).color(pal.text));
            if let Some(h) = hint {
                ui.label(RichText::new(h).small().color(pal.faint));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
            if resp.clicked() {
                *on = !*on;
                changed = true;
            }
            let t = ui.ctx().animate_bool(resp.id, *on);
            let track = pal.track.lerp_to_gamma(brand("claude"), t);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(10), track);
            let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
            ui.painter()
                .circle_filled(pos2(x, rect.center().y), 7.0, Color32::WHITE);
        });
    });
    changed
}

fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    pal: &Palette,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    let width = ui.available_width();
    let h = 28.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), pal.track);
    let w = width / options.len() as f32;
    for (i, (opt, label)) in options.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + w * i as f32, rect.top()), vec2(w, h))
            .shrink(2.0);
        let resp = ui.interact(r, ui.scope_id().with(("seg", i, label)), Sense::click());
        let selected = *value == *opt;
        if selected {
            ui.painter().rect_filled(r, CornerRadius::same(6), pal.card);
            ui.painter().rect_stroke(
                r,
                CornerRadius::same(6),
                Stroke::new(1.0, pal.border),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, CornerRadius::same(6), pal.hover);
        }
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::new(
                12.5,
                if selected {
                    semibold()
                } else {
                    FontFamily::Proportional
                },
            ),
            if selected { pal.text } else { pal.muted },
        );
        if resp.clicked() && !selected {
            *value = *opt;
            changed = true;
        }
    }
    changed
}

fn card<R>(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn level_color(pal: &Palette, cfg: &Config, used: f64, brand_c: Color32) -> Color32 {
    let t = &cfg.notify_thresholds;
    if used >= t.last().copied().unwrap_or(95) as f64 {
        pal.bad
    } else if used >= t.first().copied().unwrap_or(80) as f64 {
        pal.warn
    } else {
        brand_c
    }
}

fn pace_color(pal: &Palette, w: &Window) -> Color32 {
    let Some(p) = &w.pace else { return pal.muted };
    if p.will_last == Some(false) && p.delta.unwrap_or(0.0) > 0.0 {
        return pal.warn;
    }
    match p.delta {
        Some(d) if d <= -5.0 => pal.good,
        Some(d) if d >= 15.0 => pal.bad,
        Some(d) if d >= 5.0 => pal.warn,
        _ => pal.muted,
    }
}

// ---------- usage view ----------

impl PopupApp {
    fn header(&mut self, ui: &mut Ui, snap: &Snapshot, cfg: &Config) {
        let pal = self.pal;
        let p = snap.provider(&self.selected);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let id = p.map(|p| p.id.as_str()).unwrap_or(self.selected.as_str());
            provider_logo(ui, id, 22.0, brand(id));
            ui.label(
                RichText::new(provider_name(id))
                    .size(17.0)
                    .family(semibold())
                    .color(pal.text),
            );
            if let Some(plan) = p.and_then(|p| p.plan.as_deref()) {
                pill(ui, plan, brand(id));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button(ui, &pal, "Settings", paint_sliders).clicked() {
                    self.view = View::Settings;
                    self.draft = Some(cfg.clone());
                    self.thresholds_text = cfg
                        .notify_thresholds
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                }
                let busy = snap.busy;
                let angle = if busy {
                    ui.input(|i| i.time) as f32 * 5.0
                } else {
                    0.0
                };
                let tip = if busy {
                    "Refreshing…"
                } else {
                    "Refresh (Ctrl+R)"
                };
                if icon_button(ui, &pal, tip, |p, r, col| paint_refresh(p, r, col, angle)).clicked()
                    && !busy
                {
                    ipc::send(&Request::Refresh);
                }
                if busy {
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                }
            });
        });
        // Status line
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (dot, text) = match (
                p.and_then(|p| p.status.as_ref()),
                p.and_then(|p| p.error.as_ref()),
            ) {
                (_, Some(_)) if p.is_some_and(|p| p.windows.is_empty()) => {
                    (pal.bad, "Usage unavailable".to_string())
                }
                (Some(s), _) if !s.is_ok() => (
                    if s.indicator == "minor" {
                        pal.warn
                    } else {
                        pal.bad
                    },
                    s.description.clone(),
                ),
                (Some(s), _) => (pal.good, s.description.clone()),
                (None, _) => (pal.faint, String::new()),
            };
            let (r, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 3.5, dot);
            let updated = if snap.busy {
                "refreshing…".to_string()
            } else if !self.connected.load(Ordering::Relaxed) {
                format!("offline cache · {}", format::ago(snap.updated_at))
            } else {
                format!("updated {}", format::ago(snap.updated_at))
            };
            let line = if text.is_empty() {
                updated
            } else {
                format!("{text} · {updated}")
            };
            let resp = ui.label(RichText::new(line).size(12.0).color(pal.muted));
            if let Some(url) = p
                .and_then(|p| p.status.as_ref())
                .map(|s| s.url.clone())
                .filter(|u| !u.is_empty())
            {
                if resp
                    .on_hover_text("Open status page")
                    .interact(Sense::click())
                    .clicked()
                {
                    crate::daemon::open_url(&url);
                }
            }
        });
    }

    fn provider_tabs(&mut self, ui: &mut Ui, cfg: &Config, snap: &Snapshot) {
        if cfg.providers.len() < 2 {
            return;
        }
        let pal = self.pal;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for id in &cfg.providers {
                let selected = *id == self.selected;
                let used = snap.provider(id).and_then(|p| p.session()).map(|w| w.used);
                let label = match used {
                    Some(u) => format!("{}  {:.0}%", provider_name(id), cfg.display_value(u)),
                    None => provider_name(id),
                };
                let galley = ui.painter().layout_no_wrap(
                    label.clone(),
                    FontId::new(12.5, medium()),
                    pal.text,
                );
                let size = vec2(galley.size().x + 38.0, 28.0);
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                let fill = if selected {
                    pal.card
                } else if resp.hovered() {
                    pal.hover
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
                if selected {
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(14),
                        Stroke::new(1.0, pal.border),
                        StrokeKind::Inside,
                    );
                }
                let icon_rect = Rect::from_center_size(
                    pos2(rect.left() + 16.0, rect.center().y),
                    vec2(14.0, 14.0),
                );
                logo_at(
                    ui,
                    id,
                    icon_rect,
                    if selected { brand(id) } else { pal.muted },
                );
                ui.painter().text(
                    pos2(rect.left() + 28.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::new(12.5, medium()),
                    if selected { pal.text } else { pal.muted },
                );
                if resp.clicked() {
                    self.selected = id.clone();
                }
            }
        });
    }

    fn window_card(&self, ui: &mut Ui, cfg: &Config, p: &ProviderUsage, w: &Window) {
        let pal = self.pal;
        let accent = level_color(&pal, cfg, w.used, brand(&p.id));
        card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&w.label)
                        .size(13.5)
                        .family(semibold())
                        .color(pal.text),
                );
                let cadence = w.cadence();
                if !cadence.is_empty() && !w.label.to_lowercase().contains(&cadence) {
                    ui.label(RichText::new(cadence).size(11.5).color(pal.faint));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut t = format::resets_in(w.resets_at);
                    if let Some(at) = w.resets_at {
                        t = format!("{t} · {}", format::clock(at));
                    }
                    ui.label(RichText::new(t).size(12.0).color(pal.muted));
                });
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let v = cfg.display_value(w.used);
                ui.label(
                    RichText::new(format!("{v:.0}%"))
                        .size(28.0)
                        .family(bold())
                        .color(pal.text),
                );
                ui.label(
                    RichText::new(cfg.display_suffix())
                        .size(13.0)
                        .color(pal.muted),
                );
                if let Some(pace) = &w.pace {
                    if let Some(d) = pace.delta {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let (txt, col) = if d.abs() < 1.0 {
                                ("on pace".to_string(), pal.muted)
                            } else if d < 0.0 {
                                (format!("{:.0}% in reserve", -d), pace_color(&pal, w))
                            } else {
                                (format!("{:.0}% ahead of pace", d), pace_color(&pal, w))
                            };
                            pill(ui, &txt, col);
                        });
                    }
                }
            });
            ui.add_space(4.0);
            // Bar with the pace marker: where usage "should" be at this point in the window.
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(4), pal.track);
            let frac = (cfg.display_value(w.used) / 100.0).clamp(0.0, 1.0) as f32;
            if frac > 0.0 {
                let fill = Rect::from_min_size(
                    rect.min,
                    vec2((rect.width() * frac).max(8.0), rect.height()),
                );
                painter.rect_filled(fill, CornerRadius::same(4), accent);
            }
            if let Some(expected) = w.pace.as_ref().and_then(|p| p.expected_used) {
                let ef = (cfg.display_value(expected) / 100.0).clamp(0.0, 1.0) as f32;
                let x = rect.left() + rect.width() * ef;
                painter.line_segment(
                    [pos2(x, rect.top() - 3.0), pos2(x, rect.bottom() + 3.0)],
                    Stroke::new(2.0, pal.text.gamma_multiply(0.75)),
                );
                resp.on_hover_text(format!(
                    "Pace marker: {:.0}% {} expected by now",
                    cfg.display_value(expected),
                    cfg.display_suffix()
                ));
            }
            if let Some(pace) = &w.pace {
                let parts: Vec<&str> = pace
                    .summary
                    .split('|')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                let detail: Vec<&str> = parts
                    .iter()
                    .copied()
                    .filter(|s| {
                        !s.contains("in reserve")
                            && !s.contains("Expected")
                            && !s.eq_ignore_ascii_case("on pace")
                    })
                    .collect();
                let mut line = detail.join(" · ");
                if line.is_empty() {
                    if let Some(eta) = pace.eta_secs.filter(|_| pace.will_last == Some(false)) {
                        line = format!("Runs out in {}", format::duration(eta as i64));
                    }
                }
                if !line.is_empty() {
                    ui.add_space(2.0);
                    let col = if pace.will_last == Some(false) {
                        pace_color(&pal, w)
                    } else {
                        pal.muted
                    };
                    ui.label(RichText::new(line).size(12.0).color(col));
                }
            }
        });
    }

    fn scoped_rows(&self, ui: &mut Ui, cfg: &Config, p: &ProviderUsage) {
        let pal = self.pal;
        let scoped: Vec<&Window> = p.windows.iter().filter(|w| w.scoped).collect();
        if scoped.is_empty() {
            return;
        }
        card(ui, &pal, |ui| {
            ui.label(
                RichText::new("Model limits")
                    .size(12.0)
                    .family(semibold())
                    .color(pal.muted),
            );
            for w in scoped {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&w.label).size(12.5).color(pal.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", cfg.display_value(w.used)))
                                .size(12.5)
                                .family(semibold()),
                        );
                        let (rect, _) = ui.allocate_exact_size(vec2(90.0, 5.0), Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(3), pal.track);
                        let f = (cfg.display_value(w.used) / 100.0).clamp(0.0, 1.0) as f32;
                        ui.painter().rect_filled(
                            Rect::from_min_size(rect.min, vec2(rect.width() * f, 5.0)),
                            CornerRadius::same(3),
                            level_color(&pal, cfg, w.used, brand(&p.id)),
                        );
                    });
                });
            }
        });
    }

    fn spend_card(&self, ui: &mut Ui, cost: &CostSummary, accent: Color32) {
        let pal = self.pal;
        card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Spend").size(13.5).family(semibold()));
                ui.label(
                    RichText::new(if cost.estimated {
                        "API list-price estimate · local logs"
                    } else {
                        "reported"
                    })
                    .size(11.5)
                    .color(pal.faint),
                );
            });
            ui.add_space(4.0);
            ui.columns(3, |cols| {
                stat(
                    &mut cols[0],
                    &pal,
                    "Today",
                    &format::money(cost.today),
                    &format!("{} tokens", format::tokens(cost.today_tokens)),
                );
                stat(
                    &mut cols[1],
                    &pal,
                    "30 days",
                    &format::money(cost.last30),
                    &format!("{} tokens", format::tokens(cost.tokens30)),
                );
                let avg = cost.last30 / 30.0;
                stat(
                    &mut cols[2],
                    &pal,
                    "Daily avg",
                    &format::money(avg),
                    "last 30 days",
                );
            });
            ui.add_space(6.0);
            // Daily bars for the last 30 days (missing days are zero).
            let today = Local::now().date_naive();
            let days: Vec<(chrono::NaiveDate, f64, u64)> = (0..30)
                .rev()
                .map(|i| {
                    let d = today - ChronoDuration::days(i);
                    let row = cost.daily.iter().find(|r| r.date == d);
                    (
                        d,
                        row.map(|r| r.cost).unwrap_or(0.0),
                        row.map(|r| r.tokens).unwrap_or(0),
                    )
                })
                .collect();
            let max = days.iter().map(|d| d.1).fold(0.0, f64::max).max(0.01);
            let (rect, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
            let step = rect.width() / days.len() as f32;
            let hovered = resp
                .hover_pos()
                .map(|p| (((p.x - rect.left()) / step) as usize).min(days.len() - 1));
            for (i, (_, v, _)) in days.iter().enumerate() {
                let h = ((*v / max) as f32 * (rect.height() - 2.0)).max(if *v > 0.0 {
                    2.0
                } else {
                    1.0
                });
                let x = rect.left() + step * i as f32;
                let bar = Rect::from_min_max(
                    pos2(x + 1.0, rect.bottom() - h),
                    pos2(x + step - 1.0, rect.bottom()),
                );
                let col = if hovered == Some(i) {
                    accent
                } else if *v > 0.0 {
                    accent.gamma_multiply(if i == days.len() - 1 { 0.9 } else { 0.55 })
                } else {
                    pal.track
                };
                ui.painter().rect_filled(bar, CornerRadius::same(2), col);
            }
            let caption = match hovered {
                Some(i) => {
                    let (d, v, t) = days[i];
                    format!(
                        "{} · {} · {} tokens",
                        d.format("%a %b %-d"),
                        format::money(v),
                        format::tokens(t)
                    )
                }
                None => {
                    let top: Vec<String> = cost
                        .models
                        .iter()
                        .take(3)
                        .map(|(m, c)| format!("{} {}", format::model_name(m), format::money(*c)))
                        .collect();
                    if top.is_empty() {
                        "Hover a bar for details".into()
                    } else {
                        top.join(" · ")
                    }
                }
            };
            ui.label(RichText::new(caption).size(11.5).color(pal.muted));
        });
    }

    fn usage_view(&mut self, ui: &mut Ui, snap: &Snapshot, cfg: &Config) {
        let pal = self.pal;
        if !cfg.providers.contains(&self.selected) {
            self.selected = cfg.providers[0].clone();
        }
        self.header(ui, snap, cfg);
        self.provider_tabs(ui, cfg, snap);
        ui.add_space(6.0);
        let Some(p) = snap.provider(&self.selected).cloned() else {
            card(ui, &pal, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(pal.muted));
                    ui.label(
                        RichText::new(if snap.busy {
                            "Fetching usage…"
                        } else {
                            "No data yet"
                        })
                        .color(pal.muted),
                    );
                });
            });
            return;
        };
        if let Some(err) = &p.error {
            Frame::new()
                .fill(pal.bad.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0, pal.bad.gamma_multiply(0.4)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let mut text = err.clone();
                    if p.id == "claude" && !p.windows.is_empty() {
                        text = format!("Showing last known values. {text}");
                    } else if p.id == "claude" {
                        text.push_str("\nRun `claude` in a terminal and log in, then refresh.");
                    }
                    ui.label(RichText::new(text).size(12.0).color(pal.text));
                });
            ui.add_space(4.0);
        }
        for w in p.windows.iter().filter(|w| !w.scoped) {
            self.window_card(ui, cfg, &p, w);
            ui.add_space(2.0);
        }
        self.scoped_rows(ui, cfg, &p);
        if let Some(extra) = &p.extra {
            card(ui, &pal, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Extra usage").size(13.0).family(semibold()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let txt = match (extra.used, extra.limit) {
                            (Some(u), Some(l)) => {
                                format!("{} / {}", format::money(u), format::money(l))
                            }
                            (Some(u), None) => format::money(u),
                            (None, Some(l)) => format!("limit {}", format::money(l)),
                            _ => String::new(),
                        };
                        ui.label(RichText::new(txt).size(13.0));
                    });
                });
            });
        }
        if let Some(credits) = p.credits {
            card(ui, &pal, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Credits").size(13.0).family(semibold()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(format!("{credits:.2}"))
                    });
                });
            });
        }
        if cfg.show_costs {
            if let Some(cost) = snap.cost(&p.id) {
                ui.add_space(2.0);
                self.spend_card(ui, cost, brand(&p.id));
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if p.id == "claude" && link(ui, &pal, "claude.ai usage ↗").clicked() {
                crate::daemon::open_url("https://claude.ai/settings/usage");
            }
            if link(ui, &pal, "Copy summary").clicked() {
                ui.ctx().copy_text(summary_text(snap, cfg));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("Esc to close").size(11.0).color(pal.faint));
            });
        });
    }
}

fn summary_text(snap: &Snapshot, cfg: &Config) -> String {
    let mut out = Vec::new();
    for p in &snap.providers {
        let windows: Vec<String> = p
            .windows
            .iter()
            .filter(|w| !w.scoped)
            .map(|w| {
                format!(
                    "{} {:.0}% {} ({})",
                    w.label,
                    cfg.display_value(w.used),
                    cfg.display_suffix(),
                    format::resets_in(w.resets_at)
                )
            })
            .collect();
        out.push(format!(
            "{}: {}",
            p.name(),
            if windows.is_empty() {
                "unavailable".into()
            } else {
                windows.join(", ")
            }
        ));
        if let Some(c) = snap.cost(&p.id) {
            out.push(format!(
                "  spend today {} · 30d {}",
                format::money(c.today),
                format::money(c.last30)
            ));
        }
    }
    out.join("\n")
}

fn stat(ui: &mut Ui, pal: &Palette, label: &str, value: &str, sub: &str) {
    ui.label(RichText::new(label).size(11.5).color(pal.muted));
    ui.label(
        RichText::new(value)
            .size(17.0)
            .family(semibold())
            .color(pal.text),
    );
    ui.label(RichText::new(sub).size(11.0).color(pal.faint));
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.5).family(medium()).color(color));
        });
}

fn link(ui: &mut Ui, pal: &Palette, text: &str) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::new(12.0, medium()), pal.muted);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(14.0, 10.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), pal.hover);
    }
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        if resp.hovered() { pal.text } else { pal.muted },
    );
    resp
}

fn provider_logo(ui: &mut Ui, id: &str, size: f32, tint: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    logo_at(ui, id, rect, tint);
}

fn logo_at(ui: &mut Ui, id: &str, rect: Rect, tint: Color32) {
    match crate::icons::provider_svg(id) {
        Some(bytes) => {
            egui::Image::from_bytes(format!("bytes://provider-{id}.svg"), bytes)
                .tint(tint)
                .paint_at(ui, rect);
        }
        None => {
            ui.painter()
                .circle_filled(rect.center(), rect.width() / 2.0, tint.gamma_multiply(0.2));
            let initial = id
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
                .to_string();
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                initial,
                FontId::new(rect.height() * 0.6, bold()),
                tint,
            );
        }
    }
}

// ---------- settings view ----------

impl PopupApp {
    fn settings_view(&mut self, ui: &mut Ui, cfg: &Config) {
        let pal = self.pal;
        let mut draft = self.draft.take().unwrap_or_else(|| cfg.clone());
        ui.horizontal(|ui| {
            if icon_button(ui, &pal, "Back", paint_back).clicked() {
                self.view = View::Usage;
            }
            ui.label(RichText::new("Settings").size(17.0).family(semibold()));
        });
        ui.add_space(4.0);
        let mut text_focus = false;

        section(ui, &pal, "Providers");
        card(ui, &pal, |ui| {
            let mut ids: Vec<String> = KNOWN.iter().map(|s| s.to_string()).collect();
            for p in &draft.providers {
                if !ids.contains(p) {
                    ids.push(p.clone());
                }
            }
            egui::Grid::new("providers")
                .num_columns(2)
                .spacing(vec2(12.0, 6.0))
                .show(ui, |ui| {
                    for (i, id) in ids.iter().enumerate() {
                        let mut on = draft.providers.contains(id);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    draft.providers.push(id.clone());
                                } else if draft.providers.len() > 1 {
                                    draft.providers.retain(|p| p != id);
                                }
                            }
                            provider_logo(ui, id, 14.0, if on { brand(id) } else { pal.faint });
                            ui.label(RichText::new(provider_name(id)).size(13.0).color(if on {
                                pal.text
                            } else {
                                pal.muted
                            }));
                        });
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.new_provider)
                        .hint_text("other provider id, e.g. zed")
                        .desired_width(200.0),
                );
                text_focus |= r.has_focus();
                let submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Add").clicked() || submit) && !self.new_provider.trim().is_empty() {
                    let id = self.new_provider.trim().to_lowercase();
                    if !draft.providers.contains(&id) {
                        draft.providers.push(id);
                    }
                    self.new_provider.clear();
                }
            });
            ui.label(
                RichText::new("Order = tray order. The first provider opens by default.")
                    .size(11.0)
                    .color(pal.faint),
            );
        });

        section(ui, &pal, "Display");
        card(ui, &pal, |ui| {
            ui.label(
                RichText::new("Percentages show")
                    .size(12.5)
                    .color(pal.muted),
            );
            segmented(
                ui,
                &pal,
                &mut draft.quota_display,
                &[
                    (QuotaDisplay::Used, "Used"),
                    (QuotaDisplay::Remaining, "Remaining"),
                ],
            );
            ui.add_space(6.0);
            ui.label(RichText::new("Tray icon").size(12.5).color(pal.muted));
            segmented(
                ui,
                &pal,
                &mut draft.tray_style,
                &[
                    (TrayStyle::Bars, "Bars"),
                    (TrayStyle::Ring, "Rings"),
                    (TrayStyle::Percent, "Number"),
                    (TrayStyle::Logo, "Logo"),
                ],
            );
            if matches!(draft.tray_style, TrayStyle::Percent | TrayStyle::Logo) {
                ui.add_space(4.0);
                segmented(
                    ui,
                    &pal,
                    &mut draft.tray_window,
                    &[
                        (TrayWindow::Session, "Session"),
                        (TrayWindow::Weekly, "Weekly"),
                        (TrayWindow::Tightest, "Tightest"),
                    ],
                );
            }
            ui.label(RichText::new("Tip: scroll on the tray icon to flip between session and weekly; middle-click refreshes.").size(11.0).color(pal.faint));
            ui.add_space(6.0);
            toggle(
                ui,
                &pal,
                &mut draft.show_costs,
                "Show local spend",
                Some("Estimated from ~/.claude and ~/.codex logs"),
            );
            toggle(
                ui,
                &pal,
                &mut draft.show_status,
                "Show service status",
                None,
            );
            toggle(
                ui,
                &pal,
                &mut draft.close_on_blur,
                "Close popup when it loses focus",
                None,
            );
        });

        section(ui, &pal, "Refresh & alerts");
        card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                ui.label("Refresh every");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format::duration(draft.refresh_secs as i64))
                            .family(semibold()),
                    );
                });
            });
            ui.spacing_mut().slider_width = ui.available_width() - 8.0;
            let mut mins = (draft.refresh_secs / 60) as u32;
            if ui
                .add(egui::Slider::new(&mut mins, 1..=30).show_value(false))
                .changed()
            {
                draft.refresh_secs = mins as u64 * 60;
            }
            ui.add_space(4.0);
            toggle(
                ui,
                &pal,
                &mut draft.notifications,
                "Desktop notifications",
                None,
            );
            if draft.notifications {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Alert at % used").color(pal.muted));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.thresholds_text).desired_width(90.0),
                    );
                    text_focus |= r.has_focus();
                    if r.lost_focus() {
                        draft.notify_thresholds = self
                            .thresholds_text
                            .split(|c: char| !c.is_ascii_digit())
                            .filter_map(|s| s.parse().ok())
                            .collect();
                    }
                });
                toggle(
                    ui,
                    &pal,
                    &mut draft.notify_reset,
                    "Notify when the session resets",
                    None,
                );
            }
        });

        section(ui, &pal, "Advanced");
        card(ui, &pal, |ui| {
            ui.label(RichText::new("CodexBar CLI").size(12.5).color(pal.muted));
            let r = ui.add(egui::TextEdit::singleline(&mut draft.cli).desired_width(f32::INFINITY));
            text_focus |= r.has_focus();
            ui.add_space(4.0);
            ui.label(RichText::new("Source").size(12.5).color(pal.muted));
            let mut src = draft.source.clone();
            let opts = [
                ("auto", "Auto (OAuth first)"),
                ("oauth", "OAuth"),
                ("cli", "Claude CLI"),
            ];
            let mut sel = opts.iter().position(|o| o.0 == src).unwrap_or(0);
            if segmented(
                ui,
                &pal,
                &mut sel,
                &[(0, opts[0].1), (1, opts[1].1), (2, opts[2].1)],
            ) {
                src = opts[sel].0.to_string();
                draft.source = src;
            }
            ui.add_space(4.0);
            toggle(
                ui,
                &pal,
                &mut draft.check_updates,
                "Check for app updates",
                Some(&format!(
                    "Asks GitHub once a day · this is {}",
                    crate::update::current()
                )),
            );
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if link(ui, &pal, "Open config file").clicked() {
                crate::daemon::open_url(&format!(
                    "file://{}",
                    crate::config::config_path().display()
                ));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let quit = egui::Button::new(RichText::new("Quit CodexBar").color(pal.bad))
                    .fill(pal.bad.gamma_multiply(0.1));
                if ui.add(quit).clicked() {
                    ipc::send(&Request::Quit);
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });

        let mut clean = draft.clone();
        clean.sanitize();
        if !text_focus && clean != *cfg {
            ipc::send(&Request::SetConfig {
                config: clean.clone(),
            });
            // Optimistic local update so the UI does not flicker before the daemon echoes back.
            if let Some(s) = self.state.lock().unwrap().as_mut() {
                s.config = clean;
            }
        }
        self.draft = Some(draft);
    }
}

fn update_banner(ui: &mut Ui, pal: &Palette, target: UpdateTarget, update: &UpdateOffer) {
    let name = match target {
        UpdateTarget::App => "CodexBar Tray",
        UpdateTarget::Cli => "CodexBar CLI",
    };
    let tint = if update.error.is_some() {
        pal.bad
    } else {
        pal.good
    };
    Frame::new()
        .fill(tint.gamma_multiply(0.14))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let text = if update.installing {
                    format!("Updating {name} to {}…", update.version)
                } else {
                    format!("{name} {} is available", update.version)
                };
                ui.label(RichText::new(text).size(12.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !update.installing {
                        let label = if update.error.is_some() {
                            "Retry"
                        } else {
                            "Update"
                        };
                        if ui.small_button(label).clicked() {
                            ipc::send(&Request::InstallUpdate { target });
                        }
                    }
                    if link(ui, pal, "What's new").clicked() {
                        crate::daemon::open_url(&update.notes_url);
                    }
                });
            });
            if let Some(e) = &update.error {
                ui.label(RichText::new(e).size(11.0).color(pal.muted));
            }
        });
}

fn section(ui: &mut Ui, pal: &Palette, title: &str) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.0)
            .family(semibold())
            .color(pal.faint),
    );
}

impl eframe::App for PopupApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let Some(State { snapshot, config }) = self.state.lock().unwrap().clone() else {
            return;
        };

        let focused = ctx.input(|i| i.focused);
        if focused {
            self.had_focus = true;
        } else if self.had_focus
            && config.close_on_blur
            && self.snapshot_to.is_none()
            && self.opened.elapsed() > Duration::from_millis(300)
        {
            self.close(&ctx, true);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.view == View::Settings {
                self.view = View::Usage;
            } else {
                self.close(&ctx, false);
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::R)) {
            ipc::send(&Request::Refresh);
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Comma)) {
            self.view = View::Settings;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Q)) {
            self.close(&ctx, false);
        }

        let pal = self.pal;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, CornerRadius::ZERO, pal.bg);
        ui.painter().rect_stroke(
            full,
            CornerRadius::ZERO,
            Stroke::new(1.0, pal.border),
            StrokeKind::Inside,
        );

        let content = Frame::new()
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(WIDTH - 32.0);
                // The scroll area is clipped to the current window, so size the window from its full
                // content height rather than from what happened to fit.
                egui::ScrollArea::vertical()
                    .max_height(MAX_HEIGHT - 28.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if !self.connected.load(Ordering::Relaxed) {
                            Frame::new()
                                .fill(pal.warn.gamma_multiply(0.14))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("Background service not running")
                                                .size(12.0),
                                        );
                                        if ui.small_button("Start").clicked() {
                                            if let Ok(exe) = std::env::current_exe() {
                                                let _ = std::process::Command::new(exe)
                                                    .arg("--background")
                                                    .spawn();
                                            }
                                        }
                                    });
                                });
                            ui.add_space(6.0);
                        }
                        if let Some(notice) = &snapshot.notice {
                            Frame::new()
                                .fill(pal.warn.gamma_multiply(0.14))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(notice).size(12.0));
                                });
                            ui.add_space(6.0);
                        }
                        for (target, offer) in [
                            (UpdateTarget::App, &snapshot.update),
                            (UpdateTarget::Cli, &snapshot.cli_update),
                        ] {
                            if let Some(offer) = offer {
                                update_banner(ui, &pal, target, offer);
                                ui.add_space(6.0);
                            }
                        }
                        match self.view {
                            View::Usage => self.usage_view(ui, &snapshot, &config),
                            View::Settings => self.settings_view(ui, &config),
                        }
                    })
                    .content_size
                    .y
            });
        let height = (content.inner + 28.0).clamp(120.0, MAX_HEIGHT).ceil();
        let mismatch = (full.height() - height).abs() > 2.0 && self.frames % 10 == 5;
        if (height - self.last_height).abs() > 1.0 || mismatch {
            self.last_height = height;
            self.resized_at = Instant::now();
            // Wayland ignores InnerSize for fixed-size windows, so pin min = max = target instead.
            let size = vec2(WIDTH, height);
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            // X11 keeps the top-left corner on resize; re-pin so a bottom-panel popup stays put.
            if let Some(pos) = ANCHOR.get().and_then(|&at| x11_position(at, height)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            }
        }
        // Debug aid: render one settled frame to a PNG and exit (CODEXBAR_SNAPSHOT_TO=/tmp/x.png).
        self.frames += 1;
        let inner_h = full.height();
        if self.frames >= 12
            && (inner_h - self.last_height).abs() < 2.0
            && self.snapshot_to.is_some()
        {
            if let Some(path) = self.snapshot_to.take() {
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let _ = image::save_buffer(
                        &path,
                        img.as_raw(),
                        w as u32,
                        h as u32,
                        image::ExtendedColorType::Rgba8,
                    );
                    std::process::exit(0);
                });
            }
            ctx.request_repaint();
        }
        // egui only repaints on input, so the resize above would not be laid out until the cursor
        // moves over the window. Keep repainting until the window reaches its target height
        // (bounded, in case the compositor never grants it).
        if (inner_h - self.last_height).abs() > 2.0
            && self.resized_at.elapsed() < Duration::from_secs(2)
        {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if std::env::var_os("CODEXBAR_DEBUG").is_some() {
            eprintln!(
                "frame {} focused={focused} inner={inner_h} want={}",
                self.frames, self.last_height
            );
        }
        if self.snapshot_to.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        // Countdowns and "updated Xm ago" tick over without input.
        ctx.request_repaint_after(Duration::from_secs(20));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.pal.bg.to_normalized_gamma_f32()
    }
}

/// Tray click position the popup was opened from, fixed for the popup's lifetime.
static ANCHOR: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();

/// Client-side placement next to the tray click. Only X11 honours it; on KDE Wayland the
/// daemon places the popup via a KWin script (kwin.rs).
fn x11_position((x, y): (i32, i32), height: f32) -> Option<egui::Pos2> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return None;
    }
    let px = (x as f32 - WIDTH / 2.0).max(8.0);
    let py = if y > 400 {
        (y as f32 - 12.0 - height - 28.0).max(8.0)
    } else {
        y as f32 + 12.0
    };
    Some(pos2(px, py))
}

pub fn run(at: Option<(i32, i32)>) -> i32 {
    let desktop = theme::detect();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CodexBar")
        .with_app_id("codexbar-tray-popup")
        .with_inner_size(vec2(WIDTH, 420.0))
        .with_resizable(true)
        .with_decorations(false)
        .with_taskbar(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop);
    if let Some(rgba) = crate::icon::render_rgba_logo("claude", brand_color("claude"), 64) {
        viewport = viewport.with_icon(egui::IconData {
            rgba,
            width: 64,
            height: 64,
        });
    }
    // Only X11 honours client positioning; on KDE Wayland the daemon places it via a KWin script (kwin.rs).
    if let Some(at) = at {
        let _ = ANCHOR.set(at);
        if let Some(pos) = x11_position(at, 420.0) {
            viewport = viewport.with_position(pos);
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: at.is_none(),
        persist_window: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        "codexbar-tray-popup",
        options,
        Box::new(move |cc| Ok(Box::new(PopupApp::new(cc, &desktop)))),
    );
    if let Err(e) = result {
        eprintln!("codexbar-tray popup: {e}");
        return 1;
    }
    0
}
