#!/usr/bin/env bash
# Matters' control style survives the desktop's Qt platform theme
# (crates/matters-ui/tests/style.rs). Set MATTERS_REQUIRE_COSMIC_THEME=1 where
# COSMIC's theme (cutecosmic-qt6) and the KDE desktop style are installed, so
# its cases can't be skipped.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

require_tool cargo

group "control style under platform themes"
cargo test -p matters-ui --features ui-tests --locked --test style \
    || refuse "the control style did not hold"
endgroup
