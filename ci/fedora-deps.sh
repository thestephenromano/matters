#!/usr/bin/env bash
# System packages for building and testing Matters on Fedora. CI runs this in
# the fedora:44 container; the same list works on a Fedora 44 workstation.
set -euo pipefail

dnf -y install --setopt=install_weak_deps=False \
    gcc gcc-c++ git pkgconf-pkg-config \
    qt6-qtbase-devel qt6-qtdeclarative-devel \
    gtk4-devel libadwaita-devel \
    mesa-dri-drivers dejavu-sans-fonts
