# Matters — Product & Technical Specification

**Version:** 0.1.0-draft  
**Target Platform:** Linux (KDE Plasma 6, GNOME 4x+, Wayland/X11)  
**Core Technologies:** Rust, Qt 6 / QML (`cxx-qt`), SQLite  

---

## 1. Product Vision & Principles

Matters is a native Linux desktop task manager built around Getting Things Done (GTD). Linux users mostly choose between plain-text and terminal tools and cross-platform apps built on web technology. Matters is a carefully crafted native application that puts aesthetic calm, tactile feedback and strict GTD discipline first.

### Core Principles
1. **Restraint over Configuration:** Prevent task bloat by enforcing a proven, finite hierarchy.
2. **Temporal Clarity (When vs. Deadline):** A task's start date ("When") dictates when it demands attention. Its "Deadline" dictates urgency. Most tasks only need a "When".
3. **In-Place Interaction:** Expanding a task should never yank the user into an inspector pane or pop-up modal. Notes, tags, and checklists unfold inline.
4. **Native Linux & KDE Citizen:** Respect system color schemes (Breeze Light/Dark), native font rendering, Wayland window management, and native system shortcuts.
5. **Local-First Speed:** Instant launches, sub-millisecond local SQLite queries via WAL mode, zero network requirement for core functionality.

---

## 2. Information Architecture & Hierarchy

Matters enforces a strict 4-tier organizational model:

```
Area (e.g. Work, Personal, Finances)
 └── Project (e.g. "Q4 Roadmap", "Kitchen Renovation")
      └── Heading (e.g. "Planning", "Execution")
           └── Task (e.g. "Draft budget spreadsheet")
                └── Checklist Item (e.g. "Export CSV", "Calculate totals")
```

### Entities

1. **Area:**
   * High-level sphere of responsibility.
   * Cannot be completed or archived; has no start date or deadline.
   * Can contain Projects and standalone Tasks.

2. **Project:**
   * An objective with a definite outcome that can be completed.
   * Has an iconic **circular progress ring** displaying `completed / total` ratio.
   * Can have a start date (`When`), `Deadline`, tags, and notes. Notes, When and Deadline are edited under its title, with the same popover as a task's.
   * A project that is Someday, or starts after today, holds its tasks back: they leave Today and Anytime until it starts (a Someday project's tasks leave Upcoming too), and the project is listed instead, in Someday or in Upcoming on the day it starts. Today lists a project whose deadline has come. A project's deadline is not copied onto its tasks.
   * Can reside inside an Area or at the top level.
   * Completed or canceled from its menu, or with **Complete Project** once every task is done. If tasks are still open, a dialog asks whether to complete or cancel them with the project, or keep it open: no task is closed silently.
   * A finished project leaves the sidebar, and its tasks leave Today, Upcoming, Anytime and Someday. The Logbook lists it as one row, which opens it.
   * **Reopen Project** in its menu reopens it, along with the tasks finishing it closed. Tasks finished on their own stay finished.

3. **Heading:**
   * A visual section divider inside a Project to group related tasks.
   * Cannot be completed independently; exists purely for spatial organization.
   * Added from the project's menu; renamed, moved (with its tasks) and deleted from its own. Deleting a heading keeps its tasks in the project, above the headings, and can be undone. An empty heading is still shown, so tasks can be moved into it.

4. **Task (To-Do):**
   * The atomic unit of work.
   * Contains: Title, Notes (markdown-formatted), Checklist, Tags, `When`, `Deadline`, `Status`.
   * Tags are given to tasks, projects and areas from a tag field that suggests existing tags as you type, and are shown on a closed task's row. A task also matches the tags of its project and area. Tags can be nested; matching a tag matches the tags under it. The Tags window (the tag button in the sidebar) renames, nests and deletes them; deleting a tag never deletes what had it.
   * Can reside in:
     * Inbox (unassigned)
     * Direct child of an Area
     * Child of a Project (optionally under a Heading)

5. **Checklist Item:**
   * Lightweight sub-step inside a Task.
   * Plain text title with a simple boolean checkbox.
   * Does not have dates, tags, or notes.

---

## 3. The Core Views (GTD Navigation)

The sidebar provides access to the foundational GTD buckets:

### 1. Inbox
* Capture zone for incoming thoughts and to-dos.
* Every task created without an explicit project or date defaults here.
* Goal: Regularly processed to zero.

### 2. Today
* The daily execution dashboard.
* Contains:
  * Tasks scheduled with `When == Today` (or earlier if overdue).
  * Tasks manually flagged for Today.
* **This Evening Section:**
  * Separated by the iconic moon header (`🌙 This Evening`).
  * Dedicated to tasks deferred to personal/evening hours to prevent daytime cognitive clutter.

### 3. Upcoming
* Chronological future horizon.
* Groups scheduled tasks by date (e.g., *Tomorrow*, *Thursday, Nov 19*, *Next Week*).
* Focuses exclusively on future `When` dates.

### 4. Anytime
* All active, actionable tasks ready to be worked on immediately:
  * Tasks with `When <= Today` or no scheduled date (excluding Someday and Inbox).
  * Grouped by Area and Project.

### 5. Someday
* Incubator for long-term ideas, backlog items, or deferred projects.
* Tasks and projects marked `When = Someday` are hidden from Today, Upcoming, and Anytime, and so are the tasks of a Someday project. Someday lists such projects above its tasks.

### 6. Logbook
* Archive of completed and canceled tasks and projects, grouped by completion date. A finished project is one row standing for its tasks.
* Provides a sense of accomplishment and an auditable history.

### 7. Trash
* Soft-deleted items, recoverable until explicitly emptied.
* Deleting a task moves it to the Trash. It keeps everything about it (status, schedule, project, notes, checklist), and is hidden from every other view, the sidebar counts, project progress and Quick Find.
* **Put Back** returns a task exactly where it was. A trashed task that was completed returns to the Logbook.
* Moving a project to the Trash takes its tasks with it: they leave every view, and the Trash lists the project in their place. Put Back restores the project and its tasks as they were. A task trashed on its own before its project stays in the Trash.
* Moving an area to the Trash takes its projects and tasks with it in the same way, and the Trash lists the area in their place. Anything trashed on its own before its area or project stays in the Trash when the container is put back.
* After a task or project is moved to the Trash, a notice offers **Undo** for 5 seconds. Undoing a project also reopens it.
* **Empty Trash** permanently deletes everything in the Trash, including trashed areas and projects with everything in them, after a confirmation. It is the only way a task, project or area is destroyed.
* Listed most recently trashed first, projects above tasks. The sidebar shows no count for the Trash.
* Trash is stored as `trashed_at` on tasks, projects and areas, separate from `status`. Only the `trash_*` and `restore_*` operations change it; saving never does. Which rows are in the Trash is defined once, in `matters_core::storage::trash`.

### Capturing into a view
A task typed into the quick-add field is placed where the current view can show it, when the model allows:

| View typed into | New task |
| :--- | :--- |
| Inbox | Unscheduled, no project (Inbox) |
| Today | When = Today |
| Upcoming | When = tomorrow |
| Someday | When = Someday |
| Project | Filed in that project, unscheduled |
| Anytime | Inbox: Anytime only lists tasks filed under an area or project |
| Logbook | None: the Logbook holds only finished tasks, so it has no quick-add field |
| Trash | None: the Trash holds only trashed tasks, so it has no quick-add field |

This rule lives in `matters_core::View::new_task` (and `View::accepts_new_tasks`) and is shared by every frontend.

### Completing a task
Ticking a task done (or not done) leaves it where it is in the current list, shown with its new state, until the user navigates away, including re-selecting the same view. This lets the user see what they finished and undo a misclick. Badge counts update immediately. Any other change that takes a task out of the view, such as rescheduling, moving, trashing or putting it back, applies at once. This rule lives in `matters_core::Visit`.

---

## 4. Interaction Design & Keyboard Ergonomics

Matters is designed to be operated with minimal mouse reliance.

| Shortcut | Context | Action |
| :--- | :--- | :--- |
| `Enter` | Selected task | Opens a field for a new task directly below the selected item (in the Inbox, Today and projects; elsewhere it focuses the quick-add field). `Enter` again saves it and opens the next; `Escape` or an empty `Enter` closes the field. In Today the new task joins the selected task's section |
| `Space` | Selected task | Toggles inline expansion (notes + checklist) |
| `F2` | Selected task | Opens the task with its title selected for editing; `Enter` saves and moves to the notes, `Escape` abandons the edit |
| `Ctrl + S` | Selected task(s) | Opens **When / Schedule** popover (*Today*, *This Evening*, *Someday*, *Clear*, a typed date such as `tomorrow`, `fri`, `in 3 days` or `dec 24` with a preview of its meaning, and a calendar operable by keyboard). Past days, times of day and phrases that name no day (`week`, `this month`) are refused (`Schedule::parse_typed`) |
| `Ctrl + D` | Selected task(s) | Opens the **Deadline** popover: a typed day (`fri`, `in 3 days`, `dec 24`), a calendar, *Today* or *Clear*. Rows show the deadline, in red when due today or overdue |
| `Ctrl + Shift + T` | Selected task | Opens the task with its tag field focused. Typing suggests tags (`Up`/`Down` to pick); `Enter` adds the suggestion or the typed tag, creating it if new; `Backspace` in the empty field removes the last tag; `Escape` clears the field, then closes the task |
| `Ctrl + Shift + M` | Selected task(s) | Opens the **Move** popover: the Inbox, areas, projects and their headings in sidebar order, with the task's current place ticked. Typing filters it (a project's name finds its headings too); `Enter` or a click files the task at the end of the chosen one. Dragging a task onto the sidebar's Inbox, an area or a project does the same, and onto Today or Someday schedules it; dragging a project onto an area files it there. The sidebar scrolls while a dragged item is held near its edge |
| `Enter` | Checklist item | Opens a field for a new item directly below; `Enter` again saves it and opens the next. `Escape` leaves the checklist, with the task still open |
| `Up` / `Down` | Checklist item | Goes to the item above or below; `Ctrl + Up` / `Ctrl + Down` move the item |
| `Ctrl + .` | Checklist item | Ticks or unticks the item |
| `Backspace` | Empty checklist item | Deletes the item and goes to the one above |
| `Ctrl + K` or `/` | Anywhere | Opens **Quick Find** (instant fuzzy search across all items) |
| `Ctrl + N` | Anywhere | Creates a new task in the current view |
| `Ctrl + Shift + N` | Anywhere | Creates a new Project |
| `Delete` / `Backspace` | Selected task(s) | Moves task to Trash |
| `Up` / `Down` | Task list | Moves list selection |
| `Escape` | Task list, task editor | Closes the open task, then clears the selection; returns focus to the list |
| `Ctrl + Z` | After moving to Trash | Undoes, while the Undo notice shows |
| `Ctrl + Up` / `Ctrl + Down`| Selected task | Reorders task vertically within the list. In Today, moving past the This Evening heading moves the task into or out of This Evening; in a project, moving past a heading files the task under it, or above the headings (dragging does the same) |

---

## 5. Technical Architecture

### 5.1 Technology Stack
* **Language (Backend & Core):** Rust (edition 2024 / stable 1.80+)
* **UI Framework:** Qt 6.8+ / QML (Qt Quick) via `cxx-qt` (KDAB)
* **Storage:** SQLite 3 via `rusqlite` (WAL mode enabled)
* **Build System:** Cargo + CMake (`cxx-qt-build`)
* **Desktop Integration:** FreeDesktop standards, Wayland Layer Shell, KDE KGlobalAccel

### 5.2 Workspace Layout
```
matters/
├── Cargo.toml
├── docs/
│   └── SPEC.md
├── crates/
│   ├── matters-core/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── domain/         # Area, Project, Heading, Task, Checklist, Value Types
│   │   │   ├── storage/        # SQLite connection pool, schema, migrations
│   │   │   ├── query/          # GTD view query engines (Inbox, Today, Upcoming, etc.)
│   │   │   ├── error.rs        # Typed domain errors
│   │   │   └── lib.rs
│   │   └── tests/              # GTD logic & persistence integration tests
│   │
│   └── matters-ui/
│       ├── Cargo.toml
│       ├── build.rs            # cxx-qt code generator
│       ├── src/
│       │   ├── bridge/         # QObject wrappers (TaskListModel, ProjectListModel)
│       │   └── main.rs
│       └── qml/
│           ├── Main.qml
│           ├── style/          # Colors, typography, spacing tokens
│           ├── components/     # TaskRow, ChecklistRow, ProgressRing, DatePicker
│           └── views/          # TodayView, UpcomingView, ProjectView, Sidebar
```

### 5.3 Database Schema (SQLite)

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS areas (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    trashed_at INTEGER             -- NULL = not in the Trash (added in schema v4)
);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY NOT NULL,
    area_id TEXT REFERENCES areas(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    start_date TEXT,               -- ISO 8601 'YYYY-MM-DD', NULL = no date
    time_of_day TEXT,              -- NULL or 'evening'
    deadline TEXT,                 -- ISO 8601 'YYYY-MM-DD'
    status TEXT NOT NULL DEFAULT 'open', -- 'open', 'completed', 'canceled'
    completed_at INTEGER,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    trashed_at INTEGER             -- NULL = not in the Trash (added in schema v3)
);

CREATE TABLE IF NOT EXISTS headings (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY NOT NULL,
    area_id TEXT REFERENCES areas(id) ON DELETE SET NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    heading_id TEXT REFERENCES headings(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    start_date TEXT,               -- ISO 8601 'YYYY-MM-DD'
    time_of_day TEXT,              -- NULL or 'evening'
    is_someday INTEGER NOT NULL DEFAULT 0,
    deadline TEXT,                 -- ISO 8601 'YYYY-MM-DD'
    status TEXT NOT NULL DEFAULT 'open', -- 'open', 'completed', 'canceled'
    completed_at INTEGER,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    trashed_at INTEGER             -- NULL = not in the Trash (added in schema v2)
);

CREATE TABLE IF NOT EXISTS checklist_items (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    is_completed INTEGER NOT NULL DEFAULT 0,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS task_tags (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, tag_id)
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_start_date ON tasks(start_date);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
CREATE INDEX IF NOT EXISTS idx_tasks_area ON tasks(area_id);
CREATE INDEX IF NOT EXISTS idx_tasks_trashed ON tasks(trashed_at);
```

### 5.4 Planned encrypted synchronization

Sync must preserve offline SQLite operation and use post-quantum end-to-end encryption, including device enrollment, sharing and recovery. Matters will be the first customer of a sync client and service that can later serve other applications, with managed and self-hosted deployment options. Task semantics remain in the application domain.

The [encrypted sync design](SYNC_DESIGN.md) records the security contract, proposed key lifecycle, adoption alternatives and evaluation gates. The CRDT engine is Loro 1.18, adopted for integration ([ADR 0001](decisions/0001-crdt-adoption.md)). The post-quantum record and key-distribution stack is approved for prototype evaluation ([ADR 0002](decisions/0002-post-quantum-sync.md)), and the service components remain to be selected. This is planned functionality.

---

## 6. MVP Scope (v0.1) vs. Future Roadmap

### In Scope for MVP (v0.1)
Status of the Qt app. **Done** items are built; **Partly** items say what is missing and link the issue tracking it.

- [x] **Done:** local SQLite persistence with WAL mode.
- [x] **Done:** domain hierarchy Area → Project → Heading → Task → Checklist. The app has areas (sidebar, area view, filing projects, rename, Trash), projects with headings, tasks and their checklists; Move files a task in the Inbox, an area, a project or a heading. The core refuses any write that would break the hierarchy.
- [x] **Done:** core views Inbox, Today (with a This Evening section), Upcoming (grouped by day), Anytime (grouped by area and project), Someday, Logbook (grouped by the day tasks were finished) and Trash.
- [x] **Done:** When vs. Deadline. Any When (typed, from a calendar, or Today, This Evening, Someday) and any deadline can be set; rows show deadlines, in red when due today or overdue.
- [x] **Done:** inline task expansion. Title, notes, checklist, When and deadline are edited in place; a closed row shows how much of its checklist is done.
- [x] **Done:** keyboard control. Selection with `Up`/`Down`, `Space`, `Delete`/`Backspace`, `Escape`, `Ctrl+Z`, `Enter`, `Ctrl+N`, `Ctrl+Shift+N`, `F2`, `Ctrl+Up`/`Ctrl+Down`, Quick Find, `Ctrl+S` (When popover), `Ctrl+D` (Deadline popover) and `Ctrl+Shift+M` (Move popover) work, for one selected task at a time (several: #64).
- [x] **Done:** project progress, as a pie beside the project's title that fills as its tasks are done.
- [x] **Done:** Quick Find popover. Finds areas, projects and tasks, and opens the one chosen.
- [x] **Done:** automatic dark/light theme tracking, following the system color scheme. The app uses its own palette rather than Breeze.

### Post-MVP (v0.2+)
- [ ] System-wide floating Quick Capture daemon via Wayland Layer Shell / KGlobalAccel.
- [ ] Recurrence engine (Daily, Weekly, Monthly, after completion).
- [ ] Calendar overlay in Upcoming via Evolution Data Server (EDS) / CalDAV.
- [ ] KRunner plugin (`Alt+Space` quick task entry).
- [ ] Post-quantum end-to-end encrypted sync with an independently usable client/service and managed or self-hosted deployment; peer-to-peer transport remains a future option. See [sync design and evaluation gates](SYNC_DESIGN.md).
- [ ] Import from Things 3 JSON export.
