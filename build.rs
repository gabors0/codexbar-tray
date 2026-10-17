use std::{env, fs, path::PathBuf};

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/provider-icons");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut arms = String::new();
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
                arms.push_str(&format!(
                    "        {id:?} => Some(include_bytes!({:?})),\n",
                    dir.join(&name)
                        .canonicalize()
                        .unwrap()
                        .display()
                        .to_string()
                ));
            }
        }
    }
    let code = format!(
        "pub fn provider_svg(id: &str) -> Option<&'static [u8]> {{\n    match id {{\n{arms}        _ => None,\n    }}\n}}\n"
    );
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("provider_icons.rs"),
        code,
    )
    .unwrap();
}
