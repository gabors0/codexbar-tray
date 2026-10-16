//! Wayland clients cannot position their own windows, so on KDE a one-shot KWin script
//! anchors the popup next to the cursor (i.e. the tray icon that was just clicked).

use crate::config::runtime_dir;
use zbus::blocking::Connection;

const NAME: &str = "codexbar-popup-placement";

const SCRIPT: &str = r#"
// Copy the cursor into plain numbers now: `workspace.cursorPos` is a live value-type
// reference, so reading `.x/.y` later (e.g. when the popup resizes after switching to
// Settings) would return wherever the cursor is *then*, not where the tray click was.
const cursor = workspace.cursorPos;
const anchorX = Number(cursor.x);
const anchorY = Number(cursor.y);
function place(w) {
    if (!w || String(w.resourceClass) !== "codexbar-tray-popup") return;
    let area = null;
    let top = true;
    const fit = () => {
        // Resolve the screen area and the pinned edge once, from the tray click.
        if (!area) {
            area = workspace.clientArea(KWin.PlacementArea, w);
            top = anchorY < area.y + area.height / 2;
        }
        const a = area;
        const g = w.frameGeometry;
        const m = 8;
        const x = Math.max(a.x + m, Math.min(Math.round(anchorX - g.width / 2), a.x + a.width - g.width - m));
        const y = top ? a.y + m : Math.max(a.y + m, a.y + a.height - g.height - m);
        if (Math.abs(g.x - x) > 0.5 || Math.abs(g.y - y) > 0.5) {
            w.frameGeometry = { x: x, y: y, width: g.width, height: g.height };
        }
    };
    fit();
    // The popup resizes itself to its content; keep the edge nearest the tray pinned.
    w.frameGeometryChanged.connect(fit);
}
workspace.windowAdded.connect(place);
workspace.windowList().forEach(place);
"#;

pub fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("KDE")))
}

/// Drop the placement script left over from the previous popup, then (if `anchor`) load a fresh one.
pub fn place_popup(anchor: bool) {
    if !available() {
        return;
    }
    let Ok(conn) = Connection::session() else {
        return;
    };
    let _ = conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "unloadScript",
        &(NAME,),
    );
    if !anchor {
        return;
    }
    let path = runtime_dir().join(format!("{NAME}.js"));
    if std::fs::write(&path, SCRIPT).is_err() {
        return;
    }
    let Some(id) = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "loadScript",
            &(path.display().to_string(), NAME),
        )
        .ok()
        .and_then(|r| r.body().deserialize::<i32>().ok())
        .filter(|id| *id >= 0)
    else {
        return;
    };
    let _ = conn.call_method(
        Some("org.kde.KWin"),
        format!("/Scripting/Script{id}").as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    );
}
