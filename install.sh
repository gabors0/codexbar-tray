#!/bin/sh
# CodexBar for Linux — one-line install:
#   curl -fsSL https://raw.githubusercontent.com/gabors0/codexbar-tray/main/install.sh | sh
# Downloads the app, which then installs itself and the CodexBar CLI into ~/.local.
# Extra arguments are passed through: --no-autostart, --no-start, --update-cli.
set -eu
repo=${CODEXBAR_TRAY_REPO:-gabors0/codexbar-tray}
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) echo "Unsupported architecture: $arch" >&2; exit 1 ;; esac

# Run from a checkout or an extracted release: use the binary next to this script.
case "$0" in
  */install.sh)
    here=$(cd "$(dirname "$0")" && pwd)
    for candidate in "$here/codexbar-tray" "$here/target/release/codexbar-tray"; do
      if [ -x "$candidate" ]; then exec "$candidate" install "$@"; fi
    done ;;
esac

command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
url="https://github.com/$repo/releases/latest/download/codexbar-tray-$arch"
echo "Downloading codexbar-tray ($arch)…"
curl -fL --progress-bar -o "$tmp/codexbar-tray" "$url"
if curl -fsSL -o "$tmp/codexbar-tray.sha256" "$url.sha256"; then
  expected=$(cut -d' ' -f1 "$tmp/codexbar-tray.sha256")
  actual=$(sha256sum "$tmp/codexbar-tray" | cut -d' ' -f1)
  [ "$expected" = "$actual" ] || { echo "Checksum mismatch, aborting" >&2; exit 1; }
fi
chmod +x "$tmp/codexbar-tray"
"$tmp/codexbar-tray" install "$@"
