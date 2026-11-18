#!/usr/bin/env bash
# Shared helpers for the gate scripts in ci/. Sourced, not run.

# Group output in GitHub Actions logs; plain headings elsewhere.
group() {
    if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::group::$1"; else echo "== $1"; fi
}
endgroup() {
    if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::endgroup::"; fi
}

# Report a gate that could not run (a missing tool, an unreadable input):
# distinct from a gate that ran and refused the change.
cannot_run() {
    echo "GATE COULD NOT RUN: $*" >&2
    exit 2
}

# Report a gate that ran and refused the change.
refuse() {
    echo "GATE REFUSED: $*" >&2
    exit 1
}

# The version of `tool` pinned in ci/tool-versions.
pinned_version() {
    local version
    version="$(sed -n "s/^$1=//p" ci/tool-versions)"
    [ -n "$version" ] || cannot_run "no version for $1 in ci/tool-versions"
    echo "$version"
}

# Require cargo subcommand `tool` (cargo-<name>) at its pinned version.
require_pinned_cargo_tool() {
    local tool="$1" pinned installed
    pinned="$(pinned_version "$tool")"
    require_tool "$tool" "cargo install $tool --version $pinned --locked"
    installed="$(cargo "${tool#cargo-}" --version | awk '{print $2}')"
    [ "$installed" = "$pinned" ] \
        || cannot_run "$tool $installed is installed; this gate is pinned to $pinned (ci/tool-versions)"
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || cannot_run "$1 is not installed${2:+ ($2)}"
}

# The GTK widget tests start a Broadway display, which puts its socket in
# XDG_RUNTIME_DIR; containers and CI runners usually don't set one.
ensure_runtime_dir() {
    if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
        XDG_RUNTIME_DIR="$(mktemp -d)"
        chmod 700 "$XDG_RUNTIME_DIR"
        export XDG_RUNTIME_DIR
        trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT
    fi
}
