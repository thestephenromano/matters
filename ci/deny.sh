#!/usr/bin/env bash
# Dependency policy (deny.toml): advisories, licenses, bans and sources,
# checked with the cargo-deny version pinned in ci/cargo-deny-version.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

pinned="$(tr -d '[:space:]' < ci/cargo-deny-version)"
require_tool cargo-deny "cargo install cargo-deny --version $pinned --locked"
installed="$(cargo deny --version | awk '{print $2}')"
[ "$installed" = "$pinned" ] \
    || cannot_run "cargo-deny $installed is installed; this gate is pinned to $pinned (ci/cargo-deny-version)"

group "cargo deny check"
cargo deny --locked check || refuse "dependencies break the policy in deny.toml"
endgroup
