#!/usr/bin/env bash
# Mutation testing of matters-core with cargo-mutants. Usage:
#   ci/mutants.sh [--all] [report-dir]
#
# By default only code this branch changed, relative to origin/main, is
# mutated (`--in-diff`), and every mutant there must be caught: new and
# changed code arrives with tests that notice when it breaks. `--all`
# mutates the whole crate, to the same standard. A mutant that cannot change
# behaviour is excluded in .cargo/mutants.toml, with the reason.
#
# MATTERS_MUTANTS_SHARD=k/n tests only every n-th mutant from the k-th
# (counting from 0), so that CI can split a large diff across n jobs, each
# with its own budget.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

all=0
if [ "${1:-}" = "--all" ]; then all=1; shift; fi
reports="${1:-target/mutants}"
base="${MATTERS_MUTANTS_BASE:-origin/main}"
# 120 minutes, for each shard. Four mutants at a time took about 25 s each on #193. With
# sync's tests in the suite they take about two minutes each (#245), so a
# diff of 190 mutants needs over an hour and a half. Small diffs finish as
# before.
budget="${MATTERS_MUTANTS_TIMEOUT:-7200}"
jobs="${MATTERS_MUTANTS_JOBS:-4}"

require_tool git
require_pinned_cargo_tool cargo-mutants
mkdir -p "$reports"
reports="$(cd "$reports" && pwd)"

args=(-p matters-core --no-shuffle -j "$jobs" -o "$reports")
if [ "$all" -eq 0 ]; then
    merge_base="$(git merge-base "$base" HEAD)" \
        || cannot_run "no merge base with $base (fetch it, or set MATTERS_MUTANTS_BASE)"
    git diff "$merge_base" -- crates/matters-core > "$reports/diff.patch" \
        || cannot_run "git could not diff against $base"
    if [ ! -s "$reports/diff.patch" ]; then
        echo "No changes to matters-core since $base: nothing to mutate."
        exit 0
    fi
    args+=(--in-diff "$reports/diff.patch")
fi

shard="${MATTERS_MUTANTS_SHARD:-}"
if [ -n "$shard" ]; then
    # Round-robin spreads each file's mutants, and so the slow tests, over
    # every shard.
    args+=(--shard "$shard" --sharding round-robin)
    listed="$(cargo mutants "${args[@]}" --list)" \
        || cannot_run "cargo mutants could not list shard $shard"
    if [ -z "$listed" ]; then
        echo "Shard $shard of the diff has no mutants."
        exit 0
    fi
fi

# cargo-mutants builds in copies of the tree under TMPDIR; keep them on
# disk, next to the build output, not in a RAM-backed /tmp.
scratch="$PWD/target/mutants-tmp"
mkdir -p "$scratch"

group "cargo mutants ($([ "$all" -eq 1 ] && echo all || echo "diff against $base"))"
status=0
TMPDIR="$scratch" timeout "$budget" cargo mutants "${args[@]}" || status=$?
endgroup

# cargo-mutants: 0 all caught, 2 some missed, 3 some timed out, 1 usage
# error, 4 the unmutated tests failed.
case "$status" in
    0) echo "Every mutant was caught." ;;
    2) refuse "mutants survived; see $reports/mutants.out/missed.txt" ;;
    3) refuse "mutants timed out; see $reports/mutants.out/timeout.txt" ;;
    124) cannot_run "cargo mutants ran past its ${budget}s budget" ;;
    4) cannot_run "the tests fail without any mutation; fix them first" ;;
    *) cannot_run "cargo mutants could not run (exit $status)" ;;
esac
