#!/usr/bin/env bash
# Point PKGBUILD and .SRCINFO at a published release: packaging/aur/bump.sh 0.4.3
# Then copy both files into your clone of ssh://aur@aur.archlinux.org/limitray-bin.git and push.
set -euo pipefail
ver=${1:?usage: bump.sh <version>}
ver=${ver#v}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
repo=https://github.com/gabors0/limitray

asset_sum() { curl -fsSL "$repo/releases/download/v$ver/limitray-$1.sha256" | cut -d' ' -f1; }
file_sum() { git -C "$root" show "v$ver:$1" | sha256sum | cut -d' ' -f1; }

x86=$(asset_sum x86_64)
arm=$(asset_sum aarch64)
desktop=$(file_sum packaging/limitray.desktop)
icon=$(file_sum packaging/limitray.png)
license=$(file_sum LICENSE)

sed -i \
  -e "s/^pkgver=.*/pkgver=$ver/" \
  -e "s/^pkgrel=.*/pkgrel=1/" \
  -e "/^sha256sums=(/,/)/c\\
sha256sums=('$desktop'\\
            '$icon'\\
            '$license')" \
  -e "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$x86')/" \
  -e "s/^sha256sums_aarch64=.*/sha256sums_aarch64=('$arm')/" \
  "$here/PKGBUILD"

# What `makepkg --printsrcinfo` prints, without needing makepkg.
(
  cd "$here"
  # shellcheck disable=SC1091
  . ./PKGBUILD
  {
    printf 'pkgbase = %s\n' "$pkgname"
    printf '\tpkgdesc = %s\n\tpkgver = %s\n\tpkgrel = %s\n\turl = %s\n' "$pkgdesc" "$pkgver" "$pkgrel" "$url"
    for a in "${arch[@]}"; do printf '\tarch = %s\n' "$a"; done
    for l in "${license[@]}"; do printf '\tlicense = %s\n' "$l"; done
    for d in "${depends[@]}"; do printf '\tdepends = %s\n' "$d"; done
    for d in "${optdepends[@]}"; do printf '\toptdepends = %s\n' "$d"; done
    for p in "${provides[@]}"; do printf '\tprovides = %s\n' "$p"; done
    for c in "${conflicts[@]}"; do printf '\tconflicts = %s\n' "$c"; done
    for o in "${options[@]}"; do printf '\toptions = %s\n' "$o"; done
    for s in "${source[@]}"; do printf '\tsource = %s\n' "$s"; done
    for s in "${sha256sums[@]}"; do printf '\tsha256sums = %s\n' "$s"; done
    for s in "${source_x86_64[@]}"; do printf '\tsource_x86_64 = %s\n' "$s"; done
    for s in "${sha256sums_x86_64[@]}"; do printf '\tsha256sums_x86_64 = %s\n' "$s"; done
    for s in "${source_aarch64[@]}"; do printf '\tsource_aarch64 = %s\n' "$s"; done
    for s in "${sha256sums_aarch64[@]}"; do printf '\tsha256sums_aarch64 = %s\n' "$s"; done
    printf '\npkgname = %s\n' "$pkgname"
  } > .SRCINFO
)
echo "PKGBUILD and .SRCINFO now point at v$ver"
