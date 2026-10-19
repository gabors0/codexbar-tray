use std::{env, fs, path::PathBuf};

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/provider-icons");
    println!("cargo:rerun-if-changed={}", dir.display());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let svg_out = out.join("provider-icons");
    fs::create_dir_all(&svg_out).unwrap();
    let mut arms = String::new();
    let mut colored = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        let mut names: Vec<_> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            if let Some(id) = name
                .strip_prefix("ProviderIcon-")
                .and_then(|n| n.strip_suffix(".svg"))
            {
                let src = fs::read_to_string(dir.join(&name)).unwrap();
                let (svg, is_colored) = normalize(&src);
                if is_colored {
                    colored.push(format!("{id:?}"));
                }
                let dest = svg_out.join(&name);
                fs::write(&dest, svg).unwrap();
                arms.push_str(&format!(
                    "        {id:?} => Some(include_bytes!({:?})),\n",
                    dest.display().to_string()
                ));
            }
        }
    }
    let colored = if colored.is_empty() {
        "false".to_string()
    } else {
        format!("matches!(id, {})", colored.join(" | "))
    };
    let code = format!(
        "pub fn provider_svg(id: &str) -> Option<&'static [u8]> {{\n    match id {{\n{arms}        _ => None,\n    }}\n}}\n\n\
         /// Multicolour logos that should be shown untinted.\n\
         pub fn provider_colored(id: &str) -> bool {{\n    {colored}\n}}\n"
    );
    fs::write(out.join("provider_icons.rs"), code).unwrap();
}

/// Turns monochrome logos into white templates (so a multiplicative tint works on any
/// background); multicolour logos are left untouched and reported as coloured.
fn normalize(src: &str) -> (String, bool) {
    let colors = hex_colors(src);
    let colored = colors
        .iter()
        .any(|&(r, g, b)| r.max(g).max(b) - r.min(g).min(b) > 40);
    if colored {
        return (src.to_string(), true);
    }
    let mut svg = String::with_capacity(src.len() + 32);
    let mut rest = src;
    while let Some(i) = rest.find('#') {
        svg.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let len = hex_len(tail);
        let is_ref = svg.ends_with("url(") || svg.ends_with("href=\"");
        if len > 0 && !is_ref {
            svg.push_str("#ffffff");
            rest = &tail[len..];
        } else {
            svg.push('#');
            rest = tail;
        }
    }
    svg.push_str(rest);
    let svg = svg
        .replace("currentColor", "#ffffff")
        .replace("\"black\"", "\"#ffffff\"")
        .replace(":black", ":#ffffff");
    // Unstyled shapes default to black fill: make the root default white.
    let root_has_fill = svg
        .find("<svg")
        .and_then(|i| svg[i..].find('>').map(|j| svg[i..i + j].contains(" fill=")))
        .unwrap_or(true);
    let svg = match svg.find("<svg") {
        Some(i) if !root_has_fill => {
            format!("{}<svg fill=\"#ffffff\"{}", &svg[..i], &svg[i + 4..])
        }
        _ => svg,
    };
    (svg, false)
}

fn hex_len(s: &str) -> usize {
    let n = s.bytes().take_while(|b| b.is_ascii_hexdigit()).count();
    let next_ok = s
        .as_bytes()
        .get(n)
        .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_');
    if (n == 3 || n == 6) && next_ok {
        n
    } else {
        0
    }
}

fn hex_colors(s: &str) -> Vec<(i32, i32, i32)> {
    let mut v = Vec::new();
    for (i, _) in s.match_indices('#') {
        let tail = &s[i + 1..];
        let n = hex_len(tail);
        let h = |a: &str| i32::from_str_radix(a, 16).unwrap_or(0);
        match n {
            6 => v.push((h(&tail[0..2]), h(&tail[2..4]), h(&tail[4..6]))),
            3 => v.push((
                h(&tail[0..1]) * 17,
                h(&tail[1..2]) * 17,
                h(&tail[2..3]) * 17,
            )),
            _ => {}
        }
    }
    v
}
