# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Limitray is a Linux tray app (single Rust binary, egui + StatusNotifierItem) that shows AI usage limits, pace and local spend. It does not talk to providers itself: it shells out to the external [CodexBar](https://github.com/steipete/CodexBar) CLI (`codexbar usage …`, `codexbar cost …`) and parses its JSON. It was called `codexbar-tray` before v0.3.0, so the code still migrates old config paths, binaries, launchers and KWin scripts.

## Commands

```sh
cargo build --release
cargo test                          # all tests (unit tests live in #[cfg(test)] modules in src/*.rs)
cargo test <name_substring>         # a single test, e.g. `cargo test notifies_on_threshold`
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
./cargo.sh <args>                   # same as cargo, but runs in a Podman container if there is no host C linker
./install.sh                        # install a local build (after cargo build --release)
```

CI (`.github/workflows/ci.yml`) runs fmt check, clippy with `-D warnings` and `cargo test --locked`. Keep all three passing.

Debug aids for the window: `LIMITRAY_SNAPSHOT_TO=/tmp/x.png limitray popup` renders one settled frame to a PNG and exits; `LIMITRAY_VIEW=settings` opens on the settings view; `LIMITRAY_DEBUG=1` logs frame sizes.

## Architecture

One binary, several roles, dispatched in `src/main.rs` by the first argument/flag:

- **Daemon** (`daemon.rs`, the default with no args): the single long-lived process. It owns the tray icon (`tray.rs`, built on the vendored `ksni` in `vendor/ksni`, a path dependency), polls the CLI, sends notifications, runs update checks and holds the authoritative `Snapshot` + `Config`. All state changes go through one `mpsc` channel of `Cmd`s handled by the main loop in `daemon::run`. Worker threads (fetches, update checks, the IPC accept loop) only send `Cmd`s back; they never mutate state directly. Fetch results carry a `generation` so stale results are ignored after a config change.
- **Popup/window** (`popup.rs`, `limitray popup`): a separate eframe/egui *process* that the daemon spawns and kills (toggle = kill the child). It has no fetch logic. It subscribes to the daemon over IPC and renders the pushed `State`, falling back to the disk cache for the first frame. Settings edits go back as `Request::SetConfig`.
- **CLI clients** (`--status`, `--refresh`, `--toggle`, `--quit`): one-shot IPC requests to the running daemon. `waybar.rs` (`limitray waybar`) is a long-lived subscriber that prints Waybar JSON per pushed state.
- **Setup/update** (`setup.rs`, `update.rs`): `install`/`uninstall` (copies to `~/.local/bin`, downloads the CodexBar CLI into `~/.local/lib/codexbar-cli`, writes the .desktop/autostart files, retires legacy codexbar-tray files) and self-update from GitHub releases. Downloads are checked against their published SHA-256. Only the copy at `setup::installed_path()` updates itself (`update::self_managed`), and only a CLI that Limitray installed is offered updates. After an app update the daemon `exec`s the new binary (`Cmd::Restart`).

**IPC** (`ipc.rs`): line-delimited JSON over a private Unix socket (`config::socket_path()`, mode 0600). `Request` is a serde-tagged enum (`{"cmd": "..."}`). `Subscribe` streams a `State` line now and after every `publish()`. A daemon is "already running" when this socket accepts a connection.

**Data flow**: `fetch.rs` runs the CLI with a timeout, in its own process group so it can be killed, with capped stdout/stderr. It runs one thread per provider. For Claude it tries `--source oauth` first and falls back with a 15-minute cooldown. `model.rs` turns the CLI JSON into `ProviderUsage`/`CostSummary`/`Snapshot`, parsing loosely through `serde_json::Value` helpers because the CLI schema varies by provider. The daemon merges results with the last good values, saves `~/.cache/limitray/snapshot.json` for an instant first paint, appends each live window value to `history.jsonl` (`history.rs`, 8 days kept; `State.history` carries each window's current period, thinned, for the popup's sparklines), and publishes. The tray icon is drawn with tiny-skia/resvg in `icon.rs`. `theme.rs` reads dark mode, window colours and the UI font from kdeglobals, with freedesktop fallbacks (the XDG portal colour scheme).

**Config** (`config.rs`): `~/.config/limitray/config.json`. `Config::sanitize()` clamps and validates every loaded or IPC-supplied config. Add new fields with serde defaults so old config files still load.

**Provider icons**: `build.rs` turns `assets/provider-icons/ProviderIcon-<id>.svg` into generated code (`provider_svg(id)`, `provider_colored(id)`) that `icons.rs` includes. Monochrome SVGs become white templates for tinting; multicolour ones (channel spread > 40) stay untinted. To add a provider logo, drop in an SVG with that file name.

## Releases and conventions

- Commits follow Conventional Commits (`feat:`, `fix:`, `ci:`, `chore(release): x.y.z`).
- To release: bump `version` in `Cargo.toml`, add a `## [x.y.z] - YYYY-MM-DD` section and its compare link to `CHANGELOG.md`, then push a `vX.Y.Z` tag (or run the Release workflow with the version). The release workflow fails if the tag doesn't match `Cargo.toml` or the CHANGELOG section is missing. That section becomes the GitHub release description, so write it for users.
- Release assets are named `limitray-<arch>` (x86_64, aarch64) with `.sha256` files. The self-updater and the AUR package (`packaging/aur`, bumped with `packaging/aur/bump.sh <version>` after a release) depend on these names.
- `docs/screenshot.png` is the README image. Re-render it with `LIMITRAY_SNAPSHOT_TO`, using made-up data, never a real account.
