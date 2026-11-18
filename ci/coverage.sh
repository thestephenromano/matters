#!/usr/bin/env bash
# Line coverage of matters-core, held to a floor. Usage:
#   ci/coverage.sh [report-dir]
# Writes an lcov report and a text summary to report-dir.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

# The floor sits just under the measured coverage: 95.62% of lines on
# 2026-11-18 (Rust 1.100.0, cargo-llvm-cov 0.8.7, Fedora 44). Raise it when
# coverage rises; lowering it needs a reason in the commit.
floor=95

reports="${1:-target/coverage}"
require_pinned_cargo_tool cargo-llvm-cov
rustup component list --installed 2>/dev/null | grep -q '^llvm-tools' \
    || cannot_run "the llvm-tools component is not installed (rustup component add llvm-tools)"
mkdir -p "$reports"

group "core tests with coverage"
cargo llvm-cov clean --workspace >/dev/null 2>&1 || true
cargo llvm-cov -p matters-core --locked --no-report || refuse "core tests failed under coverage"
endgroup

group "coverage report (floor ${floor}% of lines)"
cargo llvm-cov report -p matters-core --lcov --output-path "$reports/lcov.info" \
    || cannot_run "could not write the lcov report"
cargo llvm-cov report -p matters-core --summary-only | tee "$reports/summary.txt" \
    || cannot_run "could not write the coverage summary"
cargo llvm-cov report -p matters-core --summary-only --fail-under-lines "$floor" >/dev/null \
    || refuse "matters-core line coverage is below ${floor}% (see $reports/summary.txt)"
endgroup
