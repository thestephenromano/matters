#!/usr/bin/env bash
# Repository hygiene: no tracked files that .gitignore says to ignore, the
# CI scripts pass shellcheck, the pinned versions agree, and the
# translation catalogs hold every string Matters shows.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

require_tool git
require_tool shellcheck "dnf install ShellCheck"

group "tracked files that should be ignored"
ignored="$(git ls-files --cached --ignored --exclude-standard)" \
    || cannot_run "git could not read the index (in a container, is the checkout a safe.directory?)"
if [ -n "$ignored" ]; then
    echo "$ignored"
    refuse "tracked files match .gitignore; remove them from the index or fix .gitignore"
fi
endgroup

group "shellcheck ci/*.sh"
shellcheck -x ci/*.sh || refuse "shellcheck found problems in ci/*.sh"
endgroup

group "pinned versions"
channel="$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)"
[ -n "$channel" ] || cannot_run "no channel in rust-toolchain.toml"
echo "toolchain $channel"
# The Flatpak is built with the same compiler: on each architecture, its
# rustc, cargo and rust-std archives carry the toolchain's version.
manifest=packaging/flatpak/io.github.thestephenromano.Matters.yml
flatpak_arches=(x86_64 aarch64)
archives="$(grep -c 'url: https://static.rust-lang.org/dist/' "$manifest")" \
    || cannot_run "no Rust archives in $manifest"
[ "$archives" -eq $((3 * ${#flatpak_arches[@]})) ] \
    || refuse "$manifest has $archives Rust archives, not rustc, cargo and rust-std for each of ${flatpak_arches[*]}"
for arch in "${flatpak_arches[@]}"; do
    matching="$(grep -c "url: https://static.rust-lang.org/dist/[a-z-]*-$channel-$arch-unknown-linux-gnu.tar.xz$" "$manifest")" \
        || refuse "$manifest builds on $arch with another Rust than $channel (rust-toolchain.toml)"
    [ "$matching" -eq 3 ] \
        || refuse "$manifest builds on $arch with another Rust than $channel (rust-toolchain.toml)"
done
# Each archive is fetched only on its own architecture: one without
# only-arches, or with another architecture's, would unpack a second
# toolchain over the first.
misplaced="$(awk '
    function check() { if (url != "" && index(url, "-" arch "-unknown-linux-gnu") == 0) print url }
    /url: https:\/\/static\.rust-lang\.org\/dist\// { arch = ""; check(); url = $2; next }
    url != "" && /only-arches:/ { arch = $2; gsub(/[][]/, "", arch); check(); url = "" }
    END { arch = ""; check() }
' "$manifest")"
[ -z "$misplaced" ] || refuse "$manifest fetches these Rust archives on another architecture than their own: $misplaced"
echo "Flatpak toolchain $channel on ${flatpak_arches[*]}"
for tool in cargo-deny cargo-mutants cargo-llvm-cov cargo-fuzz fuzz-nightly; do
    echo "$tool $(pinned_version "$tool")"
done
endgroup

group "translation catalogs"
# lupdate extracts the strings again and compares (#75).
crates/matters-ui/translations/update.sh --check \
    || refuse "crates/matters-ui/translations is stale: run crates/matters-ui/translations/update.sh"
endgroup
