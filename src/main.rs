mod config;
mod daemon;
mod fetch;
mod format;
mod icon;
mod icons;
mod ipc;
mod kwin;
mod model;
mod popup;
mod setup;
mod theme;
mod tray;
mod update;

const HELP: &str = "codexbar-tray — tray + popup for CodexBar (Claude-first)

USAGE:
  codexbar-tray install         Install app + CodexBar CLI + launcher + autostart, then start
        [--no-autostart] [--no-start] [--update-cli]
  codexbar-tray uninstall       Remove everything install added [--purge also drops settings]
  codexbar-tray update          Update this app and the CLI it installed [--check only reports]
  codexbar-tray update-cli      Download the latest CodexBar CLI release
  codexbar-tray                 Start the tray; if it is already running, open the popup
  codexbar-tray --show          Start the tray (if needed) and open the popup
  codexbar-tray --background    Start the tray only, never open the popup
  codexbar-tray --toggle        Toggle the popup of the running instance
  codexbar-tray --refresh       Refresh now
  codexbar-tray --status        Print the current state as JSON
  codexbar-tray --quit          Stop the running instance
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let code = if args.first().map(String::as_str) == Some("popup") {
        let at = args
            .iter()
            .position(|a| a == "--at")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| {
                let (x, y) = v.split_once(',')?;
                Some((x.parse().ok()?, y.parse().ok()?))
            });
        popup::run(at)
    } else if args.first().map(String::as_str) == Some("install") {
        setup::install(&args)
    } else if args.first().map(String::as_str) == Some("uninstall") {
        setup::uninstall(&args)
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
        println!("codexbar-tray {}", env!("CARGO_PKG_VERSION"));
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
                eprintln!("codexbar-tray is not running");
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
        eprintln!("codexbar-tray is not running");
        1
    }
}
