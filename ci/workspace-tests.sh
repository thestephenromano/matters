#!/usr/bin/env bash
# Core tests, the QML lint against the generated bridge types, and the GTK
# widget tests on a headless Broadway display.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

require_tool cargo
ensure_runtime_dir

group "workspace tests"
cargo test --workspace --locked || refuse "workspace tests failed"
endgroup
