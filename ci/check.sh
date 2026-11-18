#!/usr/bin/env bash
# The project's gates, run by CI (.github/workflows/ci.yml) and runnable
# locally from the repository root.
set -euo pipefail

# The GTK widget tests start a Broadway display, which puts its socket in
# XDG_RUNTIME_DIR. Containers and CI runners usually don't set one.
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
    XDG_RUNTIME_DIR="$(mktemp -d)"
    chmod 700 "$XDG_RUNTIME_DIR"
    export XDG_RUNTIME_DIR
    trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT
fi

echo "::group::clippy"
cargo clippy --workspace --all-targets --features matters-ui/ui-tests --locked -- -D warnings
echo "::endgroup::"

# Core tests, the QML lint against the generated bridge types, and the GTK
# widget tests on a headless Broadway display.
echo "::group::workspace tests"
cargo test --workspace --locked
echo "::endgroup::"

# Qt Quick interaction tests (offscreen): with reduced motion, then with
# animations on, so neither path can break unnoticed.
echo "::group::Qt UI tests"
cargo test -p matters-ui --features ui-tests --locked
echo "::endgroup::"

echo "::group::Qt UI tests with animations"
MATTERS_UI_TEST_MOTION=1 cargo test -p matters-ui --features ui-tests --locked --test ui
echo "::endgroup::"

# The measurement helper has its own committed lockfile and cached target.
# Its graph stays independent of the sync prototype.
echo "::group::measurement baseline SQLite runtime"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/baseline" cargo test --locked --manifest-path experiments/sync-prototype/baseline/Cargo.toml -- --nocapture
echo "::endgroup::"
