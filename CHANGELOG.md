# Changelog

All notable changes to Limitray. Each release's section becomes its GitHub release description, so add one here (under the version in `Cargo.toml`) before tagging; the release workflow fails without it.

## [0.4.2] - 2026-10-19

### Fixed
- Black provider logos (OpenRouter, OpenCode, Vercel, Exa and others) were invisible on dark backgrounds in the window. Monochrome logos are now rendered as light templates so they take the provider's colour, and multicolour logos keep their original colours.

## [0.4.1] - 2026-10-19

### Added
- **Check for updates on demand.** Settings → Advanced → *Check for updates now*, or *Check for updates* in the tray menu, checks GitHub for new releases of both Limitray and the CodexBar CLI. The result is shown next to the button: *Up to date · checked HH:MM*, *Update available* (with the usual update banner), or the error if GitHub can't be reached.

### Changed
- Manual checks work even when *Check for app updates* is off; that setting now only controls the automatic daily check. A manual check also restarts the 24-hour countdown, so the automatic check doesn't repeat it straight away.

## [0.4.0] - 2026-10-19

### Changed
- Limitray opens as a regular, undecorated app window placed by the compositor instead of a popup anchored to the tray icon. It has a close button next to refresh and settings, and can be dragged from any empty area. The tray menu's *Open Limitray* entry toggles it.
- The KWin/X11 placement logic is gone, along with `--at` and close-on-blur. The background service unloads placement scripts left behind by older releases.

### Added
- Releases can be made from the GitHub Actions tab (*Run workflow* with a version), with no tag push needed.

## [0.3.0] - 2026-10-17

### Changed
- **Renamed from codexbar-tray to Limitray.** The binary, release assets (`limitray-<arch>`), settings and cache directories, socket, launcher, autostart entry and icon all use the new name. Running `limitray install` (what install.sh runs) stops the old app, carries its settings over and removes its leftovers.

### Added
- **Self-update.** A minute after start and then daily, the tray checks GitHub for a newer release and offers to install it from the tray menu and the window. The download must match its published SHA-256 and pass a `--version` check before it's swapped in; the old binary is kept as a `.bak`. `limitray update [--check]` does the same from a terminal. Only the copy that `install` placed in `~/.local/bin` updates itself. Turn this off in Settings → Advanced.
- **CodexBar CLI updates.** The same check offers newer releases of a CodexBar CLI that Limitray installed. A CLI you installed or configured yourself is left alone.

### Fixed
- When the CodexBar CLI fails without output, the error shown is its last stderr line (or exit status) instead of "Invalid CLI output: EOF…".
- `--toggle` (for hotkeys) centers the window instead of putting it in the top-left corner on X11.
- Providers listed more than once in the config are no longer fetched and shown twice.
- A CodexBar CLI that is missing or won't run is re-downloaded at most once an hour instead of on every poll. A manual refresh still retries right away.
- Swapping in a new CLI no longer leaves a moment with no CLI installed.

## [0.2.0] - 2026-10-16

### Changed
- Sturdier popup placement, and the app starts in the tray only.

### Fixed
- Popup resizing.

## [0.1.0] - 2026-10-16

- First release, as codexbar-tray: a Linux tray app for the CodexBar CLI.

[0.4.2]: https://github.com/gabors0/limitray/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/gabors0/limitray/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/gabors0/limitray/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/gabors0/limitray/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/gabors0/limitray/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/gabors0/limitray/releases/tag/v0.1.0
