//! Desktop look: colour scheme and UI font, read from KDE (kdeglobals) with freedesktop fallbacks.

use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn luminance(self) -> f32 {
        (0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32) / 255.0
    }
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

#[derive(Clone, Debug)]
pub struct Desktop {
    pub dark: bool,
    pub window_bg: Rgb,
    pub window_fg: Rgb,
    pub font_family: Option<String>,
}

fn kdeglobals() -> Option<String> {
    std::fs::read_to_string(dirs::config_dir()?.join("kdeglobals")).ok()
}

fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == format!("[{section}]");
        } else if in_section {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

fn parse_rgb(v: &str) -> Option<Rgb> {
    let mut it = v.split(',').map(|p| p.trim().parse::<u8>().ok());
    Some(Rgb(it.next()??, it.next()??, it.next()??))
}

/// org.freedesktop.appearance color-scheme: 1 = prefer dark, 2 = prefer light.
fn portal_prefers_dark() -> Option<bool> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "ReadOne",
            &("org.freedesktop.appearance", "color-scheme"),
        )
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    let scheme = u32::try_from(&value).ok()?;
    match scheme {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

pub fn detect() -> Desktop {
    let kde = kdeglobals();
    let get = |section: &str, key: &str| kde.as_deref().and_then(|t| ini_value(t, section, key));
    let bg = get("Colors:Window", "BackgroundNormal").and_then(|v| parse_rgb(&v));
    let fg = get("Colors:Window", "ForegroundNormal").and_then(|v| parse_rgb(&v));
    let font_family = get("General", "font").and_then(|f| f.split(',').next().map(str::to_string)).filter(|f| !f.is_empty());
    let dark = match bg {
        Some(bg) => bg.luminance() < 0.5,
        None => portal_prefers_dark().unwrap_or(true),
    };
    let (default_bg, default_fg) = if dark { (Rgb(30, 31, 34), Rgb(236, 236, 238)) } else { (Rgb(250, 249, 247), Rgb(29, 29, 31)) };
    Desktop { dark, window_bg: bg.unwrap_or(default_bg), window_fg: fg.unwrap_or(default_fg), font_family }
}

/// Resolve a font file through fontconfig, e.g. ("Inter", "SemiBold").
pub fn font_file(family: &str, style: &str) -> Option<Vec<u8>> {
    let out = Command::new("fc-match").args(["-f", "%{file}", &format!("{family}:style={style}")]).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    if path.is_empty() {
        return None;
    }
    std::fs::read(path).ok()
}
