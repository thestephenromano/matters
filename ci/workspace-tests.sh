#!/usr/bin/env bash
# The workspace tests, including the QML lint against the generated bridge
# types.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

require_tool cargo
ensure_runtime_dir

group "workspace tests"
cargo test --workspace --locked || refuse "workspace tests failed"
endgroup
