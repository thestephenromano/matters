#!/usr/bin/env bash
# System packages for building and testing Matters on Fedora. CI runs this in
# the fedora:44 container; the same list works on a Fedora 44 workstation.
#
#   ci/fedora-deps.sh                   build, test and lint
#   ci/fedora-deps.sh --desktop-themes  also COSMIC's Qt platform theme and the
#                                       KDE desktop style, for ci/style-check.sh
#   ci/fedora-deps.sh --flatpak         only what ci/flatpak.sh needs: the
#                                       manifest brings its own toolchain and
#                                       the runtime brings Qt
set -euo pipefail

packages=(
    gcc gcc-c++ git pkgconf-pkg-config
    qt6-qtbase-devel qt6-qtdeclarative-devel
    # lrelease builds the translation catalogs in; lupdate checks them (#75).
    qt6-linguist
    mesa-dri-drivers dejavu-sans-fonts
    ShellCheck tar gzip
    # A private bus for the single-instance and activation tests (#203).
    dbus-daemon
    # diff, for the translation-catalog check (until #261 it came in with
    # GTK's development packages).
    diffutils
)
if [ "${1:-}" = "--desktop-themes" ]; then
    packages+=(cutecosmic-qt6 kf6-qqc2-desktop-style)
elif [ "${1:-}" = "--flatpak" ]; then
    packages=(
        git curl tar xz
        flatpak flatpak-builder desktop-file-utils appstream sqlite dbus-daemon
        # The cargo source generator.
        python3-aiohttp python3-tomlkit
    )
fi

dnf -y install --setopt=install_weak_deps=False "${packages[@]}"
