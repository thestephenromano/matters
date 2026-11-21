#!/usr/bin/env bash
# Fuzz the sync parsers (#36) with cargo-fuzz (libFuzzer). Usage:
#   ci/fuzz.sh [--all] [seconds-per-target]
#
# By default this runs only when matters-sync or fuzz/ changed relative to
# origin/main, for 60 seconds per target, starting from the committed
# corpus (fuzz/corpus, seeded from the golden vectors). A failing input
# refuses the change: libFuzzer leaves it in fuzz/artifacts/<target>/, and
# it belongs in fuzz/regressions/<target>/ with the fix, where the stable
# replay test (crates/matters-sync/tests/fuzz_replay.rs) keeps checking it.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

all=0
if [ "${1:-}" = "--all" ]; then all=1; shift; fi
seconds="${1:-60}"
base="${MATTERS_FUZZ_BASE:-origin/main}"

require_tool git
require_tool rustup
require_pinned_cargo_tool cargo-fuzz
toolchain="$(pinned_version fuzz-nightly)"

if [ "$all" -eq 0 ]; then
    merge_base="$(git merge-base "$base" HEAD)" \
        || cannot_run "no merge base with $base (fetch it, or set MATTERS_FUZZ_BASE)"
    if git diff --quiet "$merge_base" -- crates/matters-sync fuzz; then
        echo "No changes to matters-sync or fuzz/ since $base: nothing to fuzz."
        exit 0
    fi
fi

group "install $toolchain"
rustup toolchain install "$toolchain" --profile minimal \
    || cannot_run "could not install $toolchain"
endgroup

# A scratch copy of the corpus: the fuzzer adds to it, the commit does not.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for target in $(cargo +"$toolchain" fuzz list --fuzz-dir fuzz); do
    group "fuzz $target for ${seconds}s"
    mkdir -p "$work/$target"
    cp -r "fuzz/corpus/$target/." "$work/$target/"
    if [ -d "fuzz/regressions/$target" ]; then
        cp -r "fuzz/regressions/$target/." "$work/$target/"
    fi
    cargo +"$toolchain" fuzz run --fuzz-dir fuzz "$target" "$work/$target" -- \
        -max_total_time="$seconds" -rss_limit_mb=2048 -max_len=131072 \
        || refuse "fuzz target $target found a failing input; see fuzz/artifacts/$target/"
    endgroup
done
