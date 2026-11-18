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

# Qt Quick interaction tests (offscreen).
echo "::group::Qt UI tests"
cargo test -p matters-ui --features ui-tests --locked
echo "::endgroup::"
