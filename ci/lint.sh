#!/usr/bin/env bash
# Formatting, clippy and documentation for the workspace.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

require_tool cargo

group "rustfmt"
cargo fmt --all --check || refuse "code is not formatted; run cargo fmt --all"
endgroup

group "clippy"
cargo clippy --workspace --all-targets --features matters-ui/ui-tests --locked -- -D warnings \
    || refuse "clippy found problems"
endgroup

group "rustdoc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked \
    || refuse "the documentation has warnings"
endgroup
