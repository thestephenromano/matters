#!/usr/bin/env bash
# Repository hygiene: no tracked files that .gitignore says to ignore, the
# CI scripts pass shellcheck, and the pinned versions agree.
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
deny_version="$(tr -d '[:space:]' < ci/cargo-deny-version)"
[ -n "$deny_version" ] || cannot_run "ci/cargo-deny-version is empty"
echo "toolchain $channel, cargo-deny $deny_version"
endgroup
