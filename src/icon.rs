//! Tray icon rasterisation (ARGB32 pixmaps for StatusNotifierItem).

use crate::config::TrayStyle;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use resvg::tiny_skia::{
    self, Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform,
};
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Normal,
    Warn,
    Critical,
    Stale,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IconSpec {
    pub style: TrayStyle,
    /// Fill fractions in 0..=1 (already converted to used/remaining per config).
    pub primary: Option<f32>,
    pub secondary: Option<f32>,
    pub primary_level: Level,
    pub secondary_level: Level,
    pub text: Option<String>,
    pub provider: String,
    pub dark_panel: bool,
    pub monochrome: bool,
    pub brand: (u8, u8, u8),
}

fn rgba(c: (u8, u8, u8), a: f32) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, (a * 255.0) as u8)
}

impl IconSpec {
    fn fg(&self) -> (u8, u8, u8) {
        if self.dark_panel {
            (252, 252, 252)
        } else {
            (35, 38, 41)
        }
    }
    fn track(&self) -> Color {
        if self.dark_panel {
            rgba((255, 255, 255), 0.26)
        } else {
            rgba((0, 0, 0), 0.20)
        }
    }
    fn color(&self, level: Level) -> Color {
        if self.monochrome {
            return match level {
                Level::Stale => rgba(self.fg(), 0.45),
                _ => rgba(self.fg(), 1.0),
            };
        }
        match level {
            Level::Normal => rgba(self.brand, 1.0),
            Level::Warn => rgba((245, 165, 36), 1.0),
            Level::Critical => rgba((239, 68, 68), 1.0),
            Level::Stale => rgba(self.fg(), 0.45),
        }
    }
}

fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

fn rounded(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if w <= 0.2 || h <= 0.2 {
        return;
    }
    let r = (h / 2.0).min(w / 2.0);
    let mut pb = PathBuilder::new();
    // Capsule: two half-circles joined by straight edges.
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(
        x + w - r + r * 0.5523,
        y,
        x + w,
        y + r - r * 0.5523,
        x + w,
        y + r,
    );
    pb.cubic_to(
        x + w,
        y + r + r * 0.5523,
        x + w - r + r * 0.5523,
        y + h,
        x + w - r,
        y + h,
    );
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - r * 0.5523, y + h, x, y + r + r * 0.5523, x, y + r);
    pb.cubic_to(x, y + r - r * 0.5523, x + r - r * 0.5523, y, x + r, y);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn bar(
    pixmap: &mut Pixmap,
    spec: &IconSpec,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    value: Option<f32>,
    level: Level,
) {
    rounded(pixmap, x, y, w, h, spec.track());
    if let Some(v) = value {
        let v = v.clamp(0.0, 1.0);
        // Keep a visible stub so 1–3% still reads as "some".
        let fill = if v > 0.0 { (w * v).max(h) } else { 0.0 };
        rounded(pixmap, x, y, fill.min(w), h, spec.color(level));
    }
}

#[allow(clippy::too_many_arguments)]
fn arc(
    pixmap: &mut Pixmap,
    cx: f32,
    cy: f32,
    r: f32,
    from: f32,
    sweep: f32,
    width: f32,
    color: Color,
) {
    if sweep <= 0.001 {
        return;
    }
    let steps = ((sweep / (2.0 * PI)) * 72.0).ceil().max(2.0) as usize;
    let mut pb = PathBuilder::new();
    for i in 0..=steps {
        let a = from + sweep * i as f32 / steps as f32;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 {
            pb.move_to(x, y)
        } else {
            pb.line_to(x, y)
        }
    }
    if let Some(path) = pb.finish() {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }
}

fn ring(
    pixmap: &mut Pixmap,
    spec: &IconSpec,
    r: f32,
    width: f32,
    value: Option<f32>,
    level: Level,
    s: f32,
) {
    let c = s / 2.0;
    arc(pixmap, c, c, r, 0.0, 2.0 * PI, width, spec.track());
    if let Some(v) = value {
        arc(
            pixmap,
            c,
            c,
            r,
            -PI / 2.0,
            2.0 * PI * v.clamp(0.0, 1.0),
            width,
            spec.color(level),
        );
    }
}

fn draw_text(
    pixmap: &mut Pixmap,
    font: &FontVec,
    text: &str,
    max_w: f32,
    max_h: f32,
    top: f32,
    color: (u8, u8, u8),
) {
    let probe = PxScale::from(100.0);
    let sf = font.as_scaled(probe);
    let width: f32 = text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum();
    let cap = sf.ascent() * 0.72; // digit height ≈ cap height
    let k = (max_w / width).min(max_h / cap);
    let scale = PxScale::from(100.0 * k);
    let sf = font.as_scaled(scale);
    let total: f32 = text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum();
    let mut x = (pixmap.width() as f32 - total) / 2.0;
    let baseline = top + (max_h + cap * k) / 2.0;
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        let glyph = id.with_scale_and_position(scale, ab_glyph::point(x, baseline));
        x += sf.h_advance(id);
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px < 0 || py < 0 || px >= pw || py >= ph {
                    return;
                }
                let i = ((py * pw + px) * 4) as usize;
                let a = cov.clamp(0.0, 1.0);
                let blend =
                    |dst: u8, src: u8| (src as f32 * a + dst as f32 * (1.0 - a)).round() as u8;
                data[i] = blend(data[i], color.0);
                data[i + 1] = blend(data[i + 1], color.1);
                data[i + 2] = blend(data[i + 2], color.2);
                data[i + 3] = blend(data[i + 3], 255);
            });
        }
    }
}

fn draw_svg(pixmap: &mut Pixmap, svg: &[u8], x: f32, y: f32, size: f32, tint: Color) {
    let Ok(tree) = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()) else {
        return;
    };
    let Some(mut layer) = Pixmap::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    let ts = tree.size();
    let k = size / ts.width().max(ts.height());
    resvg::render(
        &tree,
        Transform::from_scale(k, k).post_translate(x, y),
        &mut layer.as_mut(),
    );
    // Template icons: keep alpha, replace colour.
    let c = tint.to_color_u8();
    for px in layer.pixels_mut() {
        let a = px.alpha() as f32 / 255.0 * tint.alpha();
        let pm = |v: u8| (v as f32 * a) as u8;
        *px = tiny_skia::PremultipliedColorU8::from_rgba(
            pm(c.red()),
            pm(c.green()),
            pm(c.blue()),
            (a * 255.0) as u8,
        )
        .unwrap();
    }
    pixmap.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

pub fn render(spec: &IconSpec, font: Option<&FontVec>, size: u32) -> ksni::Icon {
    let mut pixmap = Pixmap::new(size, size).unwrap();
    let s = size as f32;
    let style = if spec.style == TrayStyle::Percent && (font.is_none() || spec.text.is_none()) {
        TrayStyle::Bars
    } else {
        spec.style
    };
    match style {
        TrayStyle::Bars => {
            let pad = (s * 0.06).max(1.0);
            let h = s * 0.30;
            let gap = s * 0.14;
            let top = (s - 2.0 * h - gap) / 2.0;
            bar(
                &mut pixmap,
                spec,
                pad,
                top,
                s - 2.0 * pad,
                h,
                spec.primary,
                spec.primary_level,
            );
            bar(
                &mut pixmap,
                spec,
                pad,
                top + h + gap,
                s - 2.0 * pad,
                h,
                spec.secondary,
                spec.secondary_level,
            );
        }
        TrayStyle::Ring => {
            let w = s * 0.15;
            ring(
                &mut pixmap,
                spec,
                s / 2.0 - w / 2.0 - s * 0.02,
                w,
                spec.primary,
                spec.primary_level,
                s,
            );
            ring(
                &mut pixmap,
                spec,
                s / 2.0 - w * 1.5 - s * 0.08,
                w * 0.85,
                spec.secondary,
                spec.secondary_level,
                s,
            );
        }
        TrayStyle::Percent => {
            let bar_h = (s * 0.16).max(2.0);
            let color = match spec.primary_level {
                Level::Normal => spec.fg(),
                Level::Stale => spec.fg(),
                l => {
                    let c = spec.color(l).to_color_u8();
                    (c.red(), c.green(), c.blue())
                }
            };
            draw_text(
                &mut pixmap,
                font.unwrap(),
                spec.text.as_deref().unwrap_or("–"),
                s * 0.98,
                s - bar_h - s * 0.14,
                0.0,
                color,
            );
            bar(
                &mut pixmap,
                spec,
                s * 0.04,
                s - bar_h - s * 0.02,
                s * 0.92,
                bar_h,
                spec.primary,
                spec.primary_level,
            );
        }
        TrayStyle::Logo => {
            let bar_h = (s * 0.16).max(2.0);
            let logo = s - bar_h - s * 0.10;
            let tint = if spec.primary_level == Level::Normal && !spec.monochrome {
                rgba(spec.brand, 1.0)
            } else {
                spec.color(spec.primary_level)
            };
            match crate::icons::provider_svg(&spec.provider) {
                Some(svg) => draw_svg(&mut pixmap, svg, (s - logo) / 2.0, 0.0, logo, tint),
                None => arc(
                    &mut pixmap,
                    s / 2.0,
                    logo / 2.0,
                    logo * 0.35,
                    0.0,
                    2.0 * PI,
                    logo * 0.2,
                    tint,
                ),
            }
            bar(
                &mut pixmap,
                spec,
                s * 0.04,
                s - bar_h - s * 0.02,
                s * 0.92,
                bar_h,
                spec.primary,
                spec.primary_level,
            );
        }
    }
    // Premultiplied RGBA → straight ARGB (network byte order).
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        data.extend_from_slice(&[c.alpha(), c.red(), c.green(), c.blue()]);
    }
    ksni::Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}

const APP_SVG: &[u8] = include_bytes!("../assets/limitray.svg");

/// Renders the app icon (assets/limitray.svg) for windows/desktop files.
pub fn render_rgba_app_icon(size: u32) -> Option<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_data(APP_SVG, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = Pixmap::new(size, size)?;
    let k = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, Transform::from_scale(k, k), &mut pixmap.as_mut());
    Some(
        pixmap
            .pixels()
            .iter()
            .flat_map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue(), c.alpha()]
            })
            .collect(),
    )
}

pub fn brand_color(provider: &str) -> (u8, u8, u8) {
    match provider {
        "claude" => (217, 119, 87),
        "codex" => (16, 163, 127),
        "gemini" => (66, 133, 244),
        "cursor" => (160, 160, 170),
        "copilot" => (130, 80, 223),
        "openrouter" => (100, 116, 255),
        _ => (61, 174, 233),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_renders() {
        let rgba = render_rgba_app_icon(64).unwrap();
        assert_eq!(rgba.len(), 64 * 64 * 4);
        // Transparent corner, opaque middle.
        assert_eq!(rgba[3], 0);
        assert_eq!(rgba[(32 * 64 + 32) * 4 + 3], 255);
    }
}
