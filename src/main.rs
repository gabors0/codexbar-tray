mod config;
mod daemon;
mod fetch;
mod format;
mod history;
mod icon;
mod icons;
mod ipc;
mod model;
mod popup;
mod setup;
mod theme;
mod tray;
mod update;
mod waybar;

const HELP: &str =
    "limitray — Claude (and other AI) usage limits in your tray, via the CodexBar CLI

USAGE:
  limitray install         Install app + CodexBar CLI + launcher + autostart, then start
        [--no-autostart] [--no-start] [--update-cli]
  limitray uninstall       Remove everything install added [--purge also drops settings]
  limitray update          Update this app and the CLI it installed [--check only reports]
  limitray update-cli      Download the latest CodexBar CLI release
  limitray                 Start the tray; if it is already running, open the popup
  limitray --show          Start the tray (if needed) and open the popup
  limitray --background    Start the tray only, never open the popup
  limitray --toggle        Toggle the popup of the running instance
  limitray --refresh       Refresh now
  limitray --status        Print the current state as JSON
  limitray --quit          Stop the running instance
  limitray waybar          Stream JSON for a Waybar custom module [--once] [--no-start]
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let code = if args.first().map(String::as_str) == Some("popup") {
        popup::run()
    } else if args.first().map(String::as_str) == Some("install") {
        setup::install(&args)
    } else if args.first().map(String::as_str) == Some("uninstall") {
        setup::uninstall(&args)
    } else if args.first().map(String::as_str) == Some("waybar") {
        waybar::run(&args)
    } else if args.first().map(String::as_str) == Some("update") {
        update::run_cli(&args)
    } else if args.first().map(String::as_str) == Some("update-cli") {
        match setup::install_cli() {
            Ok(_) => {
                ipc::send(&ipc::Request::Refresh);
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        }
    } else if has("-h") || has("--help") {
        print!("{HELP}");
        0
    } else if has("--version") || has("-V") {
        println!("limitray {}", env!("CARGO_PKG_VERSION"));
        0
    } else if has("--quit") {
        client(ipc::Request::Quit)
    } else if has("--refresh") {
        client(ipc::Request::Refresh)
    } else if has("--toggle") {
        client(ipc::Request::Toggle)
    } else if has("--status") || has("--snapshot") {
        match ipc::get() {
            Some(state) => {
                println!("{}", serde_json::to_string_pretty(&state).unwrap());
                0
            }
            None => {
                eprintln!("limitray is not running");
                1
            }
        }
    } else {
        // A fresh start only puts the icon in the tray; relaunching a running instance (e.g. from
        // the app menu) opens the popup. Legacy Qt-app flags all mean "show".
        let explicit = [
            "--show",
            "--usage",
            "--quick-view",
            "--settings",
            "--spending",
        ]
        .iter()
        .any(|f| has(f));
        let background = has("--background");
        daemon::run(!background, explicit && !background)
    };
    std::process::exit(code);
}

fn client(req: ipc::Request) -> i32 {
    if ipc::send(&req) {
        0
    } else {
        eprintln!("limitray is not running");
        1
    }
}
