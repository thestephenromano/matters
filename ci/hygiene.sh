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
# The Flatpak is built with the same compiler: its rustc, cargo and rust-std
# archives carry the toolchain's version.
manifest=packaging/flatpak/io.github.thestephenromano.Matters.yml
archives="$(grep -c 'url: https://static.rust-lang.org/dist/' "$manifest")" \
    || cannot_run "no Rust archives in $manifest"
matching="$(grep -c "url: https://static.rust-lang.org/dist/[a-z-]*-$channel-x86_64-unknown-linux-gnu.tar.xz$" "$manifest")" \
    || refuse "$manifest builds with another Rust than $channel (rust-toolchain.toml)"
if [ "$archives" -ne 3 ] || [ "$matching" -ne 3 ]; then
    refuse "$manifest builds with another Rust than $channel (rust-toolchain.toml)"
fi
echo "Flatpak toolchain $channel"
for tool in cargo-deny cargo-mutants cargo-llvm-cov cargo-fuzz fuzz-nightly; do
    echo "$tool $(pinned_version "$tool")"
done
endgroup

group "translation catalogs"
# lupdate extracts the strings again and compares (#75).
crates/matters-ui/translations/update.sh --check \
    || refuse "crates/matters-ui/translations is stale: run crates/matters-ui/translations/update.sh"
endgroup
