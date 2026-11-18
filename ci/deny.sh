#!/usr/bin/env bash
# Dependency policy (deny.toml): advisories, licenses, bans and sources,
# checked with the cargo-deny version pinned in ci/tool-versions.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

require_pinned_cargo_tool cargo-deny

group "cargo deny check"
cargo deny --locked check || refuse "dependencies break the policy in deny.toml"
endgroup
