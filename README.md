# codexbar-tray

A Linux frontend for [CodexBar](https://github.com/steipete/CodexBar): a fast tray icon and
popup for Claude (and Codex, Gemini, …) usage limits, pace and local spend.

It is a single Rust binary (egui + StatusNotifierItem). It works on KDE Plasma, GNOME (with the
AppIndicator extension), Sway/Hyprland (waybar tray) and X11. The CodexBar CLI does the provider
fetching; the installer downloads it for you.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/gabors0/codexbar-tray/main/install.sh | sh
```

You can also download `codexbar-tray-x86_64` (or `-aarch64`) from
[Releases](https://github.com/gabors0/codexbar-tray/releases), then run:

```sh
chmod +x codexbar-tray-x86_64 && ./codexbar-tray-x86_64 install
```

`install` does the following, all inside your home directory:

- copies the app to `~/.local/bin/codexbar-tray`
- downloads the latest static CodexBar CLI into `~/.local/lib/codexbar-cli`, verifies its
  checksum and links it to `~/.local/bin/codexbar`. A CLI you already have is reused.
- adds a launcher entry, an icon and login autostart, then starts the tray

Options: `--no-autostart`, `--no-start`, `--update-cli`.

**Claude login:** sign in once with Claude Code (`claude`). CodexBar reads the same credentials,
so there is nothing to paste.

Without the CLI, the app still downloads it on first start, so the bare binary on its own works too.

## Use

| Action | Tray icon |
|---|---|
| Left click | open/close the popup |
| Middle click | refresh now |
| Scroll | switch session ↔ weekly (and provider) |
| Right click | live numbers, icon style, providers, notifications |

Popup keys: `Ctrl+R` refresh, `Ctrl+,` settings, `Esc` close.

Commands: `codexbar-tray --status` (JSON), `--refresh`, `--toggle` (bind it to a hotkey),
`--quit`, `update`, `update-cli`, `uninstall [--purge]`.

## Updates

Once a day the tray checks GitHub for new releases of itself and of the CodexBar CLI it installed.
When one is out, the right-click menu and the popup offer to install it; nothing updates on its
own. `codexbar-tray update` updates both from a terminal (`--check` only reports), and
`update-cli` reinstalls just the CLI. Downloads are verified against their published SHA-256.
The previous app version is kept as `~/.local/bin/codexbar-tray.bak`, and the tray restarts on
the new one.

Only the app copy in `~/.local/bin` updates itself, and only a CLI the app installed is offered
updates; a CLI you installed yourself is left alone. Turn the daily check off under
Settings → Advanced (`"check_updates": false`).

## Files

- Settings: `~/.config/codexbar-tray/config.json`
- Cache (instant first paint): `~/.cache/codexbar-tray/snapshot.json`

## Build from source

```sh
cargo build --release && ./install.sh
```

Building needs a C linker (`gcc`). `./cargo.sh build --release` builds in a small Podman container
when you don't have one. Pushing a `v*` tag makes GitHub Actions publish the release
binaries (`.github/workflows/release.yml`).

## Credits

Built on [CodexBar](https://github.com/steipete/CodexBar) by Peter Steinberger (MIT). Provider
icons come from that project. See `LICENSE`.
