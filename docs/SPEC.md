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
   * Can serve one goal from the Goals app (planned; see *Goals*).

3. **Heading:**
   * A visual section divider inside a Project to group related tasks.
   * Cannot be completed independently; exists purely for spatial organization.
   * Added from the project's menu; renamed, moved up or down or to another project (with its tasks), and deleted from its own. Deleting a heading keeps its tasks in the project, above the headings, and can be undone. An empty heading is still shown, so tasks can be moved into it.

4. **Task (To-Do):**
   * The atomic unit of work.
   * Contains: Title, Notes (markdown-formatted), Checklist, Tags, `When`, `Deadline`, `Status`.
   * A task is open, completed or canceled. A canceled task is finished without being done: it leaves Today, Upcoming, Anytime and Someday and goes to the Logbook like a completed one; its project and area still list it, and it counts as finished in its project's progress. It is shown struck through with a cross instead of a tick. Ticking its box, or **Reopen Task**, opens it again.
   * A task's menu (a right click, or `Menu` / `Shift + F10` on the selected task) cancels or reopens it, duplicates it, converts it to a project, or moves it to the Trash. A **duplicate** is an open copy just below the original, with its notes, When, deadline, tags and checklist (unticked), and is selected. **Convert to Project** makes an open task a project with its title, notes, When, deadline and tags, in the task's area or its project's area; each checklist item becomes one of the project's tasks, done if it was ticked, and the project opens.
   * Notes are written as Markdown (CommonMark with GitHub's extensions) and shown formatted until clicked; a click edits the source with the caret where it was. Links, and bare web addresses, open with the desktop's handler: web and mail links at once, any other kind of link only after asking. A closed task's row shows a mark when it has notes.
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

Below them, the sidebar lists the projects with no area, then each area with its projects. Everything there is arranged by its grip: areas among themselves, and a project within its area, to a place in another area, or out of every area. A project dropped on an area's own row goes last in that area.

Above any list, a tag bar offers the tags its tasks have, their own and those of their projects and areas, with the tags those are nested under. Choosing one narrows the list to the tasks (and listed projects) with that tag, a parent tag including the tags under it; choosing it again or **All** shows everything. Narrowing never reorders the list, and navigating clears it. A tag chosen in Quick Find opens Anytime already narrowed to it, and the tag bar shows a chosen tag even when no task in the list has it.

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
* Today's calendar events are listed above everything else (see *Calendar events* below).

### 3. Upcoming
* Chronological future horizon.
* Groups scheduled tasks by date (e.g., *Tomorrow*, *Thursday, Nov 19*, *Next Week*).
* Focuses exclusively on future `When` dates.
* Each of the next 14 days lists its calendar events at its head, and a day with events but no tasks gets its own heading.

### Calendar events
Calendar events are context for planning, never tasks (#62). Matters reads them from Evolution Data Server, which holds the desktop's calendars, including GNOME Online Accounts. It never stores them in its database and never changes them.

* **Where they show:** today's events above Today's tasks; Upcoming lists them on each day from tomorrow through 14 days ahead. An event spanning days is listed on each, and one ending at midnight isn't listed on the next day. Each day lists all-day events, and ones continuing from the day before, first, then the rest by start time. Each row shows its calendar's colour, when it happens on that day ("All day", "9:00 AM – 10:00 AM", "Until 6:00 AM", "From 10:00 PM") and its title. Events can't be selected, ticked or dragged, and a list narrowed to a tag hides them.
* **Choosing them:** Preferences turns events on or off and shows or hides each calendar. A calendar added later is shown.
* **Freshness:** Matters reads the calendars when it starts, then every minute and when the day changes, on a worker thread. A change appears within a minute.
* **Without a calendar service:** if Evolution Data Server isn't installed or can't be reached, Matters works unchanged, and Preferences says why there are no events. `libecal` is loaded when Matters starts rather than linked, so Matters builds and runs without it.

The rules are in `matters_core::calendar` (`Event::days`, `events_by_day`, `merge_events`); the source is `matters_ui::eds`.

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

That is the default. **Preferences** (`Ctrl + ,`) chooses when completed tasks move to the Logbook:

* **When I leave the list** (the default), as above.
* **Immediately:** a ticked task leaves its list at once.
* **At the end of the day:** tasks finished today stay in Inbox, Today, Upcoming, Anytime and Someday, shown done, until the day is over.
* **When I choose Log Completed:** finished tasks stay in those lists until **Log Completed** (`Ctrl + Shift + L`, or the button above the list) moves them to the Logbook. It works at the end of the day too.

Until then the Logbook doesn't list them, and the badges count open tasks only. Choosing a policy logs what is finished so far, so nothing comes back into its list. A project's own list and an area's always show their finished tasks. The rule is `matters_core::Settings::logged_until`, applied by `QueryEngine::logged_until`.

Preferences also choose which calendar events Today and Upcoming show (see *Calendar events*), and whether Matters follows the desktop's light or dark scheme (the default) or is always light or always dark, and whether it uses the desktop's accent colour (the default, when the desktop has one). They are this device's own, in `$XDG_CONFIG_HOME/matters/settings.toml` (`matters_core::SettingsFile`). If that file can't be read, Matters uses the defaults and says so.

### Reminders
A task can remind at a time of day on its start date (#60). The When popover's reminder field takes a typed time (`9`, `9:30`, `21:30`, `9am`, `2:30 pm`, `noon`, `midnight`); anything else is refused, never guessed. **Clear** removes the reminder. The open task's When pill says the time ("Today, 9:30 PM"), and closed rows show a bell and the time. A task with no date, or deferred to Someday, is scheduled for Today when given a reminder.

* **When it is due:** at that local time on the task's start date, in the system's time zone. A time the clocks skip is due just after the gap; a time that happens twice is due the first time. Moving the task to another day moves its reminder; completing, canceling or trashing it, or moving it to Someday or to no date, cancels it.
* **How it is shown:** as a desktop notification (the freedesktop notification service), with **Open**, which shows the task in its list, and **Complete**. More than three due at once are one notification.
* **Shown once, on time:** Matters checks the wall clock every five seconds, so a reminder comes within seconds of its time, and of waking from sleep or a change of clock or time zone. Each device records the reminders it has shown, by task and local date and time, and never shows one twice; that record isn't synced. Reminders that came due while Matters was closed are shown when it starts, as missed. If the notification service can't be reached, Matters says so once and shows them when it can.

The rules are `matters_core::reminders`; the notifications, `matters_ui::reminders` and `matters_ui::notify`.

### Repeating tasks
A task can repeat (#61). The When popover says how it repeats, and **Repeat…** opens the Repeat dialog:

* **How often:** every N days; weeks, on the days chosen; months, on the task's day of the month, on its weekday of the month (the third Saturday), on the last such weekday, or on the month's last day; or years, on the task's date.
* **Counted from:** on a schedule from the task's date, or a set time after each occurrence is completed.
* **Ends:** never, on a day (typed, as for a deadline), or after a number of times. A repeating task can also be **Paused**.

On a schedule, the dates are those on or after the start that match. A month without the day (the 31st, a fifth Monday, 29 February) is skipped, as iCalendar (RFC 5545) has it. After completion, the next date is the day it was completed plus the interval; a month or a year after the 31st or 29 February falls on the month's last day.

* **The series:** the newest occurrence carries the rule; rows that carry one show a repeat mark. When the next date comes (on a schedule) or the newest occurrence is completed (after completion), the next occurrence is created as a copy: title, notes, place, This Evening, reminder, tags, the checklist unticked, and the deadline as far from its date as before. The rule passes to it. After days away only the latest date due is created, not one per missed day. Editing the series means editing its newest occurrence, which can also change, pause or end the rule. A task with no date, or deferred to Someday, is scheduled for Today when it starts repeating.
* **Never twice:** an occurrence's id comes from its series and date, and it is created only if missing, in the same change as the completion or the catch-up. A crash, a second check or a second device never makes a second one.
* **Upcoming** previews the coming occurrences of each series on a schedule over its next 14 days, quieter than tasks, without creating them. Occurrences are ordinary tasks: done, they go to the Logbook like any other. A series waits while its newest occurrence is in the Trash or its project is finished, and stops when that occurrence is deleted.

Matters checks for occurrences due when it starts and every minute. The rules are `matters_core::repeat` (checked against python-dateutil's RFC 5545 dates); the rows, `matters_core::storage::repeats`.

### Goals (deferred, #202)
Goals live in a separate Goals app (#223), not in Matters. Linking the two is deferred (2026-11-22): Matters has no interface to the Goals app and shows no goals. The reference below is stored and synced, but nothing sets or shows it; [ADR 0004](decisions/0004-goal-references.md) has the design, kept for when the link resumes.

* **One goal at most:**
  * a project, or a repeating series, serves at most one goal;
  * the series' goal is carried by its newest occurrence and passes to the next occurrence with the rule;
  * a single task serves its project's goal;
  * duplicating a project keeps its goal, and converting a task to a project starts with none.
* **Linking:**
  * a filtered popover like Move lists the Goals app's active goals, from the project's menu, the repeating task's menu or a keyboard shortcut;
  * the same popover clears the link;
  * both can be undone.
* **Showing:**
  * a project's page shows its goal's title and state (on track, at risk, late, achieved or abandoned), read from the Goals app, and opens it there;
  * repeating rows that serve a goal show a goal mark.
* **Matters stores no goal data:**
  * it keeps only the goal's ID, plus a cache on this device of the last title and state it read;
  * it never changes a link because of something done in the Goals app;
  * without the Goals app, a linked project shows its goal as unavailable, and nothing else changes.

---

## 4. Interaction Design & Keyboard Ergonomics

Matters is designed to be operated with minimal mouse reliance.

| Shortcut | Context | Action |
| :--- | :--- | :--- |
| `Enter` | Selected task | Opens a field for a new task directly below the selected item (in the Inbox, Today and projects; elsewhere it focuses the quick-add field). `Enter` again saves it and opens the next; `Escape` or an empty `Enter` closes the field. In Today the new task joins the selected task's section |
| `Space` | Selected task | Toggles inline expansion (notes + checklist) |
| `F2` | Selected task | Opens the task with its title selected for editing; `Enter` saves and moves to the notes, `Escape` abandons the edit |
| `Ctrl + S` | Selected task(s) | Opens **When / Schedule** popover (*Today*, *This Evening*, *Someday*, *Clear*, a typed date such as `tomorrow`, `fri`, `in 3 days` or `dec 24` with a preview of its meaning, a calendar operable by keyboard, and a reminder time; see *Reminders*). Past days, times of day and phrases that name no day (`week`, `this month`) are refused as dates (`Schedule::parse_typed`) |
| `Ctrl + D` | Selected task(s) | Opens the **Deadline** popover: a typed day (`fri`, `in 3 days`, `dec 24`), a calendar, *Today* or *Clear*. Rows show the deadline, in red when due today or overdue |
| `Ctrl + Shift + T` | Selected task(s) | With several selected, asks for one tag (existing or new) and gives it to all of them. With one, opens the task with its tag field focused. Typing suggests tags (`Up`/`Down` to pick); `Enter` adds the suggestion or the typed tag, creating it if new; `Backspace` in the empty field removes the last tag; `Escape` clears the field, then closes the task |
| `Ctrl + Shift + M` | Selected task(s) | Opens the **Move** popover: the Inbox, areas, projects and their headings in sidebar order, with the task's current place ticked. Typing filters it (a project's name finds its headings too); `Enter` or a click files the task at the end of the chosen one. Dragging a task onto the sidebar's Inbox, an area or a project does the same, and onto Today or Someday schedules it; dragging a project onto an area files it there. The sidebar scrolls while a dragged item is held near its edge |
| `Enter` | Checklist item | Opens a field for a new item directly below; `Enter` again saves it and opens the next. `Escape` leaves the checklist, with the task still open |
| `Up` / `Down` | Checklist item | Goes to the item above or below; `Ctrl + Up` / `Ctrl + Down` move the item |
| `Ctrl + .` | Checklist item | Ticks or unticks the item |
| `Backspace` | Empty checklist item | Deletes the item and goes to the one above; the Undo notice brings it back |
| `Ctrl + K` or `/` | Anywhere | Opens **Quick Find**: the lists (Inbox, Today and the other fixed views), areas, projects, tags and open tasks whose names contain what is typed, in that order. `Up`/`Down` pick one and `Enter` opens it (the first, unless another is picked): a list, area or project opens; a tag opens Anytime narrowed to it; a task opens the list that holds it, with the task open |
| Typing a character other than `/` | Task list | Opens Quick Find searching for what is typed, so any list is a few keystrokes away. Keys typed into a title, notes, checklist or tag field stay there |
| `Ctrl + Alt + K` | Selected task | Cancels it, or reopens it if it is finished |
| `Ctrl + Shift + D` | Selected task | Duplicates it below itself and selects the copy |
| `Menu` or `Shift + F10` | Selected task | Opens the task's menu: cancel or reopen, duplicate, convert to project, move to Trash |
| `Ctrl + N` | Anywhere | Creates a new task in the current view |
| `Ctrl + ,` | Anywhere | Opens **Preferences** on the current choice; `Tab` and `Space` choose another, and each takes effect at once |
| `Ctrl + Shift + L` | Anywhere, when finished tasks wait to be logged | **Log Completed**: the finished tasks still in their lists move to the Logbook |
| `Ctrl + Shift + N` | Anywhere | Creates a new Project |
| `Delete` / `Backspace` | Selected task(s) | Moves task to Trash |
| `Up` / `Down` | Task list | Moves list selection |
| `Shift + Up` / `Shift + Down` | Task list | Selects several tasks: extends the selection from where it started. `Shift` + click selects up to the clicked task, `Ctrl` + click adds or removes one; a plain arrow or click selects one again. The selection survives list refreshes and clears when navigating |
| `Ctrl + .` | Selected task(s) | Completes them, or reopens them when none is open |
| `Escape` | Task list, task editor | Closes the open task, then clears the selection; returns focus to the list |
| `Ctrl + Z` | After moving to Trash, deleting a heading or checklist item, or acting on several tasks | Undoes, while the Undo notice shows. An action on several selected tasks (completing, canceling, trashing, scheduling, setting a deadline, moving, tagging or dragging them) is undone at once, each task put back exactly as it was (`matters_core::TaskSnapshot`) |
| `Ctrl + Up` / `Ctrl + Down`| Selected task | Reorders task vertically within the list. In Today, moving past the This Evening heading moves the task into or out of This Evening; in a project, moving past a heading files the task under it, or above the headings (dragging does the same) |
| `Tab` / `Shift + Tab` | Anywhere | Moves through the controls, the sidebar's entries included. `Space` or `Enter` opens the focused entry. Entries Matters draws itself show a ring in the accent colour while focused |

### Accessibility

Matters can be used with a screen reader and with the keyboard alone (#74):

* **Names:** every control a screen reader can act on has an accessible name, role and state. That includes the sidebar's entries (list items, selected when open), task rows (list items with their state), their circles (check boxes named "Complete …"), icon buttons and text fields.
* **The list cursor:** the task list keeps the keyboard focus and moves its own cursor with `Up` and `Down`. The row under the cursor tells the screen reader it is focused, so the reader follows it.
* **Keyboard reach:** every control can be reached with the keyboard. List and menu items are reached through their list or menu, with the arrow keys.
* **Contrast:** the palette is one set of tokens in `MainView.qml`. In light and in dark mode, every text colour meets WCAG 2.2 AA (4.5:1) on every surface it is drawn on, and the accent meets 3:1 against all of them as a focus ring, tick or selection mark.
* **The desktop's choices (#76):** Matters reads the accent colour and the contrast preference from the XDG desktop portal (`org.freedesktop.appearance`, over D-Bus; `matters_ui::appearance`) and follows their changes while it runs.
  * **The accent:** the desktop's accent replaces Matters' blue, made darker in light mode or lighter in dark mode only as far as the limits above need, and the selection is tinted with it.
  * **Higher contrast:** text and the accent are held to 7:1 (WCAG AAA), the accent's marks to 4.5:1, and lines become visible (3:1).
  * **Without a portal**, or with Preferences keeping Matters' own accent, the palette is unchanged.

These are checked by tests:

* `tst_accessibility.qml` walks Qt's accessibility tree, the one Orca reads over AT-SPI, in every view and with every dialog and popover open. It fails if a control has no name, if the keyboard can't reach one, or if something clickable is hidden from the screen reader. The tree is reported by the test-only `AccessibilityProbe`.
* `tst_contrast.qml` computes the ratio of every pair in light and in dark, with and without higher contrast, and with Matters' accent or eight desktop accents, from white to black, set through the test-only `FakeDesktop`.
* `qml_lint` fails on a colour literal outside the palette.

### Languages

Every string Matters shows can be translated (#75), through Qt Linguist:

* **Marking strings:** QML marks its strings with `qsTr`, whole sentences with `%1` for what goes in them and `%n` for counts. The strings Rust shows (errors, reminders, how a task repeats, the calendar's problems) are declared in `matters_ui::strings`. The fixed lists' names are given by the window, not by `matters-core`.
* **Catalogs:** `crates/matters-ui/translations/matters.ts` is the template, refreshed by `translations/update.sh` with `lupdate`. A translation is `matters_<lang>.ts` beside it, and `build.rs` compiles each into Matters with `lrelease` (qt6-linguist). `matters_en.ts` holds the English plural forms ("1 open task"), and is the catalog for languages Matters has none for. Qt's own catalog translates Qt's buttons (OK, Cancel).
* **Choosing one:** the catalog for the first of the system's languages that has one is used.
* **Dates and times:** they follow the locale's own long date format, keeping its order and the words between its parts, with the locale's names (`matters_ui::locale_format`). Times use the locale's short format.
* **Right to left:** in a right-to-left language the window is mirrored.

These are checked by tests:

* `tst_translation.qml` switches to a pseudo-translation `build.rs` makes from the template: every string marked and a third longer. In every view and with every dialog and popover open, it fails on a string that isn't translated, apart from the locale's own names and the test's data. Long strings must fit the sidebar, popovers and dialogs, left to right and right to left, and the window must mirror.
* `qml_lint` fails on a `qsTr` without a literal, which `lupdate` couldn't extract, and on a string with a count that has no English forms.
* CI (`ci/hygiene.sh`) fails when the template is stale.

Typed dates and times (`tomorrow`, `fri`, `9:00`) are read in English (`Schedule::parse_typed`), though the hints that show them can be translated; reading them in other languages is #227.

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

The [encrypted sync design](SYNC_DESIGN.md) records the security contract, proposed key lifecycle, adoption alternatives and evaluation gates. The CRDT engine is Loro 1.18, adopted for integration ([ADR 0001](decisions/0001-crdt-adoption.md)). The post-quantum record and key-distribution stack is approved for prototype evaluation ([ADR 0002](decisions/0002-post-quantum-sync.md)), and the relay contract and components are recorded in the [service contract](SYNC_SERVICE_CONTRACT.md) and [ADR 0003](decisions/0003-relay-components.md). This is planned functionality.

### 5.5 Installing as a Flatpak

Matters' application ID is `io.github.thestephenromano.Matters`. It names the desktop entry, the icon, the AppStream metainfo (`packaging/linux`) and the Flatpak (`packaging/flatpak`), and the app tells the desktop which entry its window belongs to (`matters_ui::APP_ID`).

* **Runtime and build:** the Flatpak runs on KDE's Qt 6.11 runtime (`org.kde.Platform//6.11`). It is built offline: the crates are generated from `Cargo.lock` at build time, and the compiler is the official toolchain pinned in `rust-toolchain.toml`, downloaded as checksummed archives. `ci/hygiene.sh` checks that the two versions agree.
* **Data:** inside the sandbox, `$XDG_DATA_HOME` is `~/.var/app/io.github.thestephenromano.Matters/data`, so the database is `matters/matters.db` there (`Database::default_path`). The settings follow `$XDG_CONFIG_HOME` the same way.
* **Permissions:** Wayland (X11 as a fallback), GPU access and desktop notifications for reminders. Nothing else.
* **Calendar events:** the runtime has no Evolution Data Server client library, so the Flatpak shows no calendar events, and Preferences says why (see *Calendar events*).
* **Checked:** `ci/flatpak.sh`, a CI job, validates the desktop entry and metainfo, builds and installs the Flatpak, starts Matters in the sandbox, and checks that its data is still there after a `flatpak update`.

---

## 6. MVP Scope (v0.1) vs. Future Roadmap

### In Scope for MVP (v0.1)
Status of the Qt app. **Done** items are built; **Partly** items say what is missing and link the issue tracking it.

- [x] **Done:** local SQLite persistence with WAL mode.
- [x] **Done:** domain hierarchy Area → Project → Heading → Task → Checklist. The app has areas (sidebar, area view, filing projects, rename, Trash), projects with headings, tasks and their checklists; Move files a task in the Inbox, an area, a project or a heading. The core refuses any write that would break the hierarchy.
- [x] **Done:** core views Inbox, Today (with a This Evening section), Upcoming (grouped by day), Anytime (grouped by area and project), Someday, Logbook (grouped by the day tasks were finished) and Trash.
- [x] **Done:** When vs. Deadline. Any When (typed, from a calendar, or Today, This Evening, Someday) and any deadline can be set; rows show deadlines, in red when due today or overdue.
- [x] **Done:** inline task expansion. Title, notes (shown as formatted Markdown), checklist, When and deadline are edited in place; a closed row shows how much of its checklist is done, and whether it has notes.
- [x] **Done:** keyboard control. Selection with `Up`/`Down`, `Space`, `Delete`/`Backspace`, `Escape`, `Ctrl+Z`, `Enter`, `Ctrl+N`, `Ctrl+Shift+N`, `F2`, `Ctrl+Up`/`Ctrl+Down`, Quick Find, `Ctrl+S` (When popover), `Ctrl+D` (Deadline popover) and `Ctrl+Shift+M` (Move popover) work on one selected task or several: `Shift`/`Ctrl` select several, and every command applies to all of them, with one Undo.
- [x] **Done:** project progress, as a pie beside the project's title that fills as its tasks are done.
- [x] **Done:** Quick Find popover. Finds lists, areas, projects, tags and tasks, and opens the one chosen; typing on the task list opens it with what was typed.
- [x] **Done:** automatic dark/light theme tracking, following the system color scheme, or a light or dark scheme chosen in Preferences. The app uses its own palette rather than Breeze.

### Post-MVP (v0.2+)
- [ ] System-wide floating Quick Capture daemon via Wayland Layer Shell / KGlobalAccel.
- [x] Recurrence engine (Daily, Weekly, Monthly, Yearly, after completion) (#61).
- [x] Calendar events in Today and Upcoming via Evolution Data Server (EDS) (#62).
- [ ] KRunner plugin (`Alt+Space` quick task entry).
- [ ] Post-quantum end-to-end encrypted sync with an independently usable client/service and managed or self-hosted deployment; peer-to-peer transport remains a future option. See [sync design and evaluation gates](SYNC_DESIGN.md).
- [ ] Import from Things 3 JSON export.
