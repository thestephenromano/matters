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

---

## Building and Testing

Matters needs Qt 6.8 or later, GTK 4 with libadwaita 1.5 or later, and a stable Rust toolchain. On Fedora, `sudo ci/fedora-deps.sh` installs the system packages.

```sh
cargo run -p matters-ui     # the Qt app
ci/check.sh                 # every gate CI runs
```

`ci/check.sh` runs:

- clippy with warnings denied;
- the workspace tests, which include the core tests, a `qmllint` check of the QML against the generated bridge types, and GTK widget tests on a headless Broadway display;
- the Qt Quick interaction tests, offscreen. Set `MATTERS_UI_TEST_PLATFORM=wayland` to watch them run.

CI (`.github/workflows/ci.yml`) runs the same script in a `fedora:44` container on every pull request and every push to `main`.
