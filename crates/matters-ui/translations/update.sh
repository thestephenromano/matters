#!/usr/bin/env bash
# The catalog template (#75): every string Matters shows, from the QML
# (qsTr) and from Rust (src/strings.rs), for translators to start from.
#
#   crates/matters-ui/translations/update.sh          refresh matters.ts
#   crates/matters-ui/translations/update.sh --check  fail if it is stale
#
# Needs Qt's lupdate (qt6-linguist), found through $LUPDATE, Qt's host
# binaries (qmake6 -query QT_HOST_BINS) or lupdate-qt6 on the PATH.
# Translations go beside it as matters_<lang>.ts (`lupdate ... -ts
# matters_de.ts`, then Qt Linguist); build.rs compiles each into Matters.
# matters_en.ts, refreshed here too, holds only the English plural forms
# ("1 task", "2 tasks") for the strings with a count; English is also the
# catalog for languages Matters has none for.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
crate="$(dirname "$here")"

lupdate="${LUPDATE:-}"
if [ -z "$lupdate" ]; then
    bins="$(qmake6 -query QT_HOST_BINS 2>/dev/null || true)"
    if [ -n "$bins" ] && [ -x "$bins/lupdate" ]; then
        lupdate="$bins/lupdate"
    else
        lupdate="lupdate-qt6"
    fi
fi
command -v "$lupdate" >/dev/null || { echo "lupdate not found (install qt6-linguist or set LUPDATE)" >&2; exit 2; }

# $1: the catalog to refresh; the rest: lupdate options.
extract() {
    local ts="$1"
    shift
    # -locations none: lines move often; the strings are what matter.
    "$lupdate" -silent -extensions qml,js,rs -locations none -no-obsolete "$@" \
        "$crate/qml" "$crate/src/strings.rs" -ts "$ts"
}

refresh() {
    # The template from scratch, so its order is the sources'; English in
    # place, keeping its forms.
    rm -f "$1/matters.ts"
    extract "$1/matters.ts"
    extract "$1/matters_en.ts" -target-language en
}

case "${1:-}" in
    "")
        refresh "$here"
        ;;
    --check)
        scratch="$(mktemp -d)"
        trap 'rm -rf "$scratch"' EXIT
        cp "$here/matters_en.ts" "$scratch/"
        refresh "$scratch"
        for ts in matters.ts matters_en.ts; do
            if ! diff -u "$here/$ts" "$scratch/$ts"; then
                echo "translations/$ts is stale: run crates/matters-ui/translations/update.sh" >&2
                exit 1
            fi
        done
        ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 2
        ;;
esac
