#!/usr/bin/env bash
# The Flatpak (#73): validate the desktop entry and AppStream metainfo, build
# packaging/flatpak offline, install it, and check that Matters starts in the
# sandbox, keeps its database at the sandbox's XDG data path, and still has
# that data, and starts on it, after a `flatpak update`.
#
# Usage: ci/flatpak.sh [work-dir]   (default target/flatpak)
#
# Needs flatpak, flatpak-builder (or the org.flatpak.Builder Flatpak),
# desktop-file-validate, appstreamcli, sqlite3, dbus-run-session, curl and a
# Python 3 with aiohttp and tomlkit for the cargo source generator
# (MATTERS_FLATPAK_PYTHON names another interpreter). It installs the KDE
# runtime from Flathub for the current user if it is missing.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

app=io.github.thestephenromano.Matters
manifest="packaging/flatpak/$app.yml"
work="${1:-target/flatpak}"
python="${MATTERS_FLATPAK_PYTHON:-python3}"
# flatpak-builder-tools' cargo generator, pinned by commit and checksum.
generator_commit=41c20aa10819cdb2a4f3ca171758a96d1955c018
generator_sha256=0a2db6be87d75910facef28ab46d4d6460802e8419ab850d0caa6a364d26b380
# How long Matters must keep running for a start to count.
run_seconds=20

for tool in flatpak desktop-file-validate appstreamcli sqlite3 dbus-run-session curl; do
    require_tool "$tool"
done
"$python" -c "import aiohttp, tomlkit" 2>/dev/null \
    || cannot_run "$python lacks aiohttp or tomlkit (dnf install python3-aiohttp python3-tomlkit)"
if command -v flatpak-builder >/dev/null; then
    builder=(flatpak-builder)
elif flatpak info org.flatpak.Builder >/dev/null 2>&1; then
    builder=(flatpak run org.flatpak.Builder)
else
    cannot_run "flatpak-builder is not installed (dnf install flatpak-builder, or flatpak install flathub org.flatpak.Builder)"
fi

mkdir -p "$work"
work="$(cd "$work" && pwd)"
# The user installation, which the runs below keep using with their own home.
flatpak_user_dir="${FLATPAK_USER_DIR:-$HOME/.local/share/flatpak}"

group "desktop entry and metainfo"
desktop-file-validate "packaging/linux/$app.desktop" \
    || refuse "desktop-file-validate rejected packaging/linux/$app.desktop"
appstreamcli validate --no-net --explain "packaging/linux/$app.metainfo.xml" \
    || refuse "appstreamcli rejected packaging/linux/$app.metainfo.xml"
endgroup

group "cargo sources from Cargo.lock"
curl -fsSLo "$work/flatpak-cargo-generator.py" \
    "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$generator_commit/cargo/flatpak-cargo-generator.py" \
    || cannot_run "could not download the cargo source generator"
echo "$generator_sha256  $work/flatpak-cargo-generator.py" | sha256sum -c - \
    || cannot_run "the cargo source generator does not match its pinned checksum"
"$python" "$work/flatpak-cargo-generator.py" Cargo.lock -o packaging/flatpak/cargo-sources.json \
    || cannot_run "the cargo source generator failed"
endgroup

group "build"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
"${builder[@]}" --user --install-deps-from=flathub --force-clean --disable-rofiles-fuse \
    --state-dir="$work/state" --repo="$work/repo" "$work/build" "$manifest" \
    || refuse "the Flatpak did not build"
endgroup

group "install"
flatpak remote-add --user --no-gpg-verify --if-not-exists matters-local "$work/repo"
flatpak install --user -y --noninteractive --reinstall matters-local "$app" \
    || refuse "the Flatpak did not install"
exports="$flatpak_user_dir/exports/share"
for file in "applications/$app.desktop" \
    "icons/hicolor/scalable/apps/$app.svg" "icons/hicolor/symbolic/apps/$app-symbolic.svg"; do
    [ -e "$exports/$file" ] || refuse "the installed Flatpak does not export $file"
done
endgroup

# Matters runs with its own home directory, so its data is this run's alone:
# inside the sandbox, $XDG_DATA_HOME is <home>/.var/app/<app>/data.
home="$work/home"
rm -rf "$home"
mkdir -p "$home"
db="$home/.var/app/$app/data/matters/matters.db"
# Run Matters offscreen until $run_seconds pass; it must still be running.
run_matters() {
    local log="$1" status=0
    HOME="$home" FLATPAK_USER_DIR="$flatpak_user_dir" dbus-run-session -- \
        timeout --kill-after=10 "$run_seconds" \
        flatpak run --user --env=QT_QPA_PLATFORM=offscreen "$app" >"$log" 2>&1 || status=$?
    HOME="$home" FLATPAK_USER_DIR="$flatpak_user_dir" flatpak kill "$app" >/dev/null 2>&1 || true
    if [ "$status" -ne 124 ]; then
        cat "$log"
        refuse "Matters stopped (exit $status) instead of running for ${run_seconds}s"
    fi
    if grep -E "failed to load component|cannot open database|panicked" "$log"; then
        refuse "Matters reported an error while starting (above)"
    fi
}

group "start, and keep data at the sandbox's data path"
run_matters "$work/first-run.log"
[ -f "$db" ] || refuse "Matters did not create its database at $db"
sqlite3 "$db" "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'tasks'" | grep -qx 1 \
    || refuse "the database at $db has no tasks table"
now="$(date +%s)000"
sqlite3 "$db" "INSERT INTO tasks (id, title, sort_order, created_at, updated_at)
    VALUES ('01890000-0000-7000-8000-000000000073', 'Survives an update', 1, $now, $now)" \
    || refuse "could not add a task to $db"
endgroup

group "update"
before="$(flatpak info --user --show-commit "$app")"
flatpak build-export --subject="update check" "$work/repo" "$work/build" \
    || refuse "could not export a new build"
flatpak update --user -y --noninteractive "$app" || refuse "flatpak update failed"
after="$(flatpak info --user --show-commit "$app")"
[ "$before" != "$after" ] || refuse "flatpak update did not deploy a new commit ($after)"
echo "updated $before -> $after"
run_matters "$work/updated-run.log"
sqlite3 "$db" "SELECT title FROM tasks WHERE id = '01890000-0000-7000-8000-000000000073'" \
    | grep -qx "Survives an update" || refuse "the task added before the update is gone"
endgroup

echo "The Flatpak builds, installs, starts, keeps its data and survives an update."
