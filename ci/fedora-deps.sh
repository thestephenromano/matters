#!/usr/bin/env bash
# System packages for building and testing Matters on Fedora. CI runs this in
# the fedora:44 container; the same list works on a Fedora 44 workstation.
#
#   ci/fedora-deps.sh                   build, test and lint
#   ci/fedora-deps.sh --desktop-themes  also COSMIC's Qt platform theme and the
#                                       KDE desktop style, for ci/style-check.sh
set -euo pipefail

packages=(
    gcc gcc-c++ git pkgconf-pkg-config
    qt6-qtbase-devel qt6-qtdeclarative-devel
    # lrelease builds the translation catalogs in; lupdate checks them (#75).
    qt6-linguist
    gtk4-devel libadwaita-devel
    mesa-dri-drivers dejavu-sans-fonts
    ShellCheck tar gzip
)
if [ "${1:-}" = "--desktop-themes" ]; then
    packages+=(cutecosmic-qt6 kf6-qqc2-desktop-style)
fi

dnf -y install --setopt=install_weak_deps=False "${packages[@]}"
