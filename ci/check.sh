#!/usr/bin/env bash
# Every gate CI runs (.github/workflows/ci.yml), one after another, for a
# local check from the repository root. CI runs the same scripts as
# separate jobs.
set -euo pipefail
cd "$(dirname "$0")/.."

ci/hygiene.sh
ci/lint.sh
ci/deny.sh
ci/workspace-tests.sh
ci/ui-lane.sh reduced
ci/ui-lane.sh animations
ci/style-check.sh
