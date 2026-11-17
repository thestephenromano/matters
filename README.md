# Matters

> A first-class, keyboard-driven personal task manager for Linux, crafted in the spirit of Things 3.

Matters combines the tactile elegance, spatial calm, and disciplined GTD hierarchy of Cultured Code's Things with the power and freedom of native Linux. Built from the ground up in **Rust** and **Qt 6 / QML**, it is designed to feel 100% at home on modern Linux desktops like **KDE Plasma** while running cleanly across all desktop environments.

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
│   └── matters-ui/             # Qt 6 / QML interface layer (cxx-qt)
```

For full architectural, data model, and interaction specifications, see [docs/SPEC.md](docs/SPEC.md).
