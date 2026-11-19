# Matters

> A keyboard-driven personal task manager for Linux, built around Getting Things Done.

Matters pairs a calm, uncluttered interface and a disciplined GTD hierarchy with the power and freedom of native Linux. Built from the ground up in **Rust** and **Qt 6 / QML**, it is designed to feel 100% at home on modern Linux desktops like **KDE Plasma** while running cleanly across all desktop environments.

---

## Philosophy

1. **Restraint over Bloat:** No endless subtask nesting, no overwhelming Kanban boards. A clean hierarchy: `Area` → `Project` → `Heading` → `Task` → `Checklist`.
2. **The Two-Date Mental Model:** A clear separation between **When** to start thinking about a task and its hard **Deadline**.
3. **Tactile Micro-Interactions:** Hardware-accelerated fluid transitions, spring physics, and inline expansion without disruptive modals.
4. **Keyboard-First Ergonomics:** Every operation can be executed effortlessly from the keyboard.
5. **Local-First & Fast:** Sub-millisecond SQLite queries, tiny memory footprint, zero webview bloat.

---

## Architecture Overview

```
matters/
├── Cargo.toml                  # Workspace definition
├── docs/
│   └── SPEC.md                 # Product & technical specification
├── crates/
│   ├── matters-core/           # Pure Rust domain & SQLite storage engine
│   ├── matters-ui/             # Qt 6 / QML interface layer (cxx-qt)
│   └── matters-gtk/            # GTK 4 / libadwaita interface (secondary)
├── ci/                         # System packages and the gates CI runs
```

For full architectural, data model, and interaction specifications, see [docs/SPEC.md](docs/SPEC.md).

Planned synchronization requirements and evaluation gates are recorded in the [encrypted sync design](docs/SYNC_DESIGN.md), including post-quantum end-to-end encryption and an independently usable sync service.

---

## Building and Testing

Matters needs Qt 6.8 or later, GTK 4 with libadwaita 1.5 or later, and the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it). On Fedora, `sudo ci/fedora-deps.sh` installs the system packages; the cargo tools the gates use (cargo-deny, cargo-mutants, cargo-llvm-cov) are pinned in `ci/tool-versions`.

```sh
cargo run -p matters-ui     # the Qt app
ci/check.sh                 # every gate CI runs
```

Each gate is a script in `ci/` that runs the same locally and in CI. A gate that ran and refused the change exits 1. One that could not run (a missing tool, no test output) exits 2, so the two can't be confused.

- `ci/hygiene.sh`: no tracked files that `.gitignore` excludes, and shellcheck on `ci/*.sh`.
- `ci/lint.sh`: rustfmt, clippy with warnings denied, and rustdoc with warnings denied.
- `ci/deny.sh`: cargo-deny against `deny.toml` (advisories, licenses, banned crates, sources). Every advisory exception names its reason and the issue that retires it.
- `ci/workspace-tests.sh`: the core tests, a `qmllint` check of the QML against the generated bridge types, and GTK widget tests on a headless Broadway display.
- `ci/ui-lane.sh reduced|animations`: the Qt Quick interaction tests, offscreen, with animations off or on. The lane checks that every test in `tests/qml` ran, keeps to a time budget, and writes a JUnit report. Set `MATTERS_UI_TEST_PLATFORM=wayland` to watch the tests run.
- `ci/style-check.sh`: Matters' control style holds under the desktop's Qt platform theme, COSMIC's included.
- `ci/coverage.sh`: matters-core's line coverage, held to a floor set just under the measured figure. It writes an lcov report.
- `ci/mutants.sh`: mutation testing of the matters-core code a branch changes, relative to `origin/main`. Every mutant must be caught; one that cannot change behaviour is excluded in `.cargo/mutants.toml`, with the reason. `ci/mutants.sh --all` mutates the whole crate.

Matters skips its animations when the desktop asks for reduced motion (KDE's instant animation speed, or GNOME's animations turned off). `MATTERS_REDUCE_MOTION=1` or `=0` overrides that.

CI (`.github/workflows/ci.yml`) runs these scripts as parallel jobs in a `fedora:44` container on every pull request and every push to `main`. The required check, "clippy and tests", passes only when every job passes. A weekly workflow (`.github/workflows/weekly.yml`) checks main's dependencies against new advisories and runs each UI lane three times to catch tests that pass only sometimes.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
