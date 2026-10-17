#!/bin/sh
# Limitray — one-line install:
#   curl -fsSL https://raw.githubusercontent.com/gabors0/limitray/main/install.sh | sh
# Downloads the app, which then installs itself and the CodexBar CLI into ~/.local.
# Extra arguments are passed through: --no-autostart, --no-start, --update-cli.
set -eu
repo=${LIMITRAY_REPO:-gabors0/limitray}
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) echo "Unsupported architecture: $arch" >&2; exit 1 ;; esac

# Run from a checkout or an extracted release: use the binary next to this script.
case "$0" in
  */install.sh)
    here=$(cd "$(dirname "$0")" && pwd)
    for candidate in "$here/limitray" "$here/target/release/limitray"; do
      if [ -x "$candidate" ]; then exec "$candidate" install "$@"; fi
    done ;;
esac

command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
url="https://github.com/$repo/releases/latest/download/limitray-$arch"
echo "Downloading limitray ($arch)…"
curl -fL --progress-bar -o "$tmp/limitray" "$url"
if curl -fsSL -o "$tmp/limitray.sha256" "$url.sha256"; then
  expected=$(cut -d' ' -f1 "$tmp/limitray.sha256")
  actual=$(sha256sum "$tmp/limitray" | cut -d' ' -f1)
  [ "$expected" = "$actual" ] || { echo "Checksum mismatch, aborting" >&2; exit 1; }
fi
chmod +x "$tmp/limitray"
"$tmp/limitray" install "$@"
