#!/bin/sh
# Run cargo for limitray, e.g. `./cargo.sh build --release` or `./cargo.sh test`.
# Uses the host toolchain when a C linker exists (sudo dnf install gcc); otherwise runs inside a
# small Fedora container that provides one, reusing the host's rustup toolchain and cargo cache.
set -eu
cd "$(dirname "$0")"
if command -v cc >/dev/null 2>&1; then
  exec cargo "$@"
fi
command -v podman >/dev/null || { echo "Need either gcc (sudo dnf install gcc) or podman" >&2; exit 1; }
if ! podman image exists localhost/limitray-build; then
  printf 'FROM registry.fedoraproject.org/fedora:44\nRUN dnf install -y --setopt=install_weak_deps=False gcc && dnf clean all\n' |
    podman build -t limitray-build -f - .
fi
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
toolchain=$(ls "$rustup_home/toolchains" | grep -m1 '^stable-x86_64')
exec podman run --rm --userns=keep-id --security-opt label=disable \
  -v "$PWD:/src" -v "$cargo_home:/cargo" -v "$rustup_home:/rustup" \
  -e CARGO_HOME=/cargo -e RUSTUP_HOME=/rustup -e "PATH=/rustup/toolchains/$toolchain/bin:/usr/bin:/bin" \
  -w /src localhost/limitray-build cargo "$@"
