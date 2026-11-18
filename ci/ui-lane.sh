#!/usr/bin/env bash
# One lane of the Qt Quick interaction tests (offscreen), in Matters' own
# control style. Usage: ci/ui-lane.sh reduced|animations [report-dir]
#
# Fails closed: besides Qt Quick Test's own result, the lane checks that it
# ran every test in tests/qml, so a test file that fails to load, or a run
# that executes nothing, can't pass. Writes a JUnit report to report-dir.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

lane="${1:-}"
reports="${2:-target/ui-reports}"
case "$lane" in
    reduced) motion=0 ;;
    animations) motion=1 ;;
    *) cannot_run "usage: ci/ui-lane.sh reduced|animations [report-dir]" ;;
esac
require_tool cargo

# Expected: each test_ function, plus Qt's initTestCase and cleanupTestCase
# for each TestCase.
shopt -s nullglob
files=(crates/matters-ui/tests/qml/tst_*.qml)
[ ${#files[@]} -gt 0 ] || cannot_run "no tst_*.qml files in crates/matters-ui/tests/qml"
functions="$(cat "${files[@]}" | grep -cE '^\s*function test_[A-Za-z0-9_]+\(' || true)"
cases="$(cat "${files[@]}" | grep -cE '^\s*TestCase \{' || true)"
expected=$((functions + 2 * cases))
[ "$expected" -gt 0 ] || cannot_run "found no QML tests to run"

mkdir -p "$reports"
# Absolute: cargo runs the test binary from the crate's directory.
reports="$(cd "$reports" && pwd)"
junit="$reports/ui-$lane.xml"
log="$reports/ui-$lane.log"
rm -f "$junit" "$log"

group "Qt UI tests ($lane): expecting $expected"
# A time budget, build included: a TestCase whose `when` never turns true
# makes Qt Quick Test wait forever.
budget="${MATTERS_UI_LANE_TIMEOUT:-900}"
status=0
MATTERS_UI_TEST_MOTION="$motion" timeout "$budget" \
    cargo test -p matters-ui --features ui-tests --locked --test ui \
    -- -o "$junit,junitxml" -o "-,txt" 2>&1 | tee "$log" || status=$?
[ "$status" -ne 124 ] || cannot_run "the UI tests ran past their ${budget}s budget ($lane)"
endgroup

totals="$(grep -E '^Totals: ' "$log" | tail -1 || true)"
[ -n "$totals" ] || cannot_run "Qt Quick Test printed no totals (exit $status); see $log"
passed="$(sed -E 's/^Totals: ([0-9]+) passed.*/\1/' <<<"$totals")"
failed="$(sed -E 's/^Totals: [0-9]+ passed, ([0-9]+) failed.*/\1/' <<<"$totals")"
echo "$totals"
if [ "$status" -ne 0 ] || [ "$failed" -ne 0 ]; then
    refuse "$failed UI test(s) failed ($lane)"
fi
[ "$passed" -eq "$expected" ] \
    || refuse "$passed UI tests passed but $expected were expected ($lane): a test file failed to load or tests were skipped"
[ -s "$junit" ] || cannot_run "no JUnit report at $junit"
