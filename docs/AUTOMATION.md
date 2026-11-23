# The automation interface

Everything outside the app reaches Matters through one versioned D-Bus interface on the session bus. That includes the command line, `matters://` links, global capture and the MCP server (#197). The design and how it was chosen are recorded on [#203](https://github.com/thestephenromano/matters/issues/203). The code is the `matters-automation` crate, which either front end serves over its own database.

This document describes what is built so far: names, client tokens, the read methods, and how the Qt app serves them. Writes and batches follow, as the order of work on #203 sets out.

## Names

| | |
|---|---|
| Bus name | `io.github.thestephenromano.Matters` (the app's ID, as `DBusActivatable` and Flatpak require) |
| Object path | `/io/github/thestephenromano/Matters/Automation` |
| Interface | `io.github.thestephenromano.Matters.Automation1` |
| Errors | `io.github.thestephenromano.Matters.Automation1.Error.<Name>` |

The version is part of the interface's name. A change that breaks callers gets a new interface (`Automation2`), and the old one keeps working for at least one release. Adding a method, or a key to an item, does not break callers and stays within `Automation1`.

## Starting and stopping

The Qt app owns the bus name, so only one instance runs (`crates/matters-ui/src/service.rs`). It also serves the standard `org.freedesktop.Application` interface at `/io/github/thestephenromano/Matters`:

| Method | Does |
|---|---|
| `Activate` | Shows and raises the window |
| `Open` | The same, until `matters://` links arrive (#69) |
| `ActivateAction` | Refused with `NotSupported`: Matters has no actions |

**Starting Matters while it runs.** The new process asks the running one to `Activate`, then exits.

**A call while Matters is closed.** D-Bus activation starts Matters with `--gapplication-service` and no window. The desktop entry sets `DBusActivatable=true`, and the Flatpak installs the service file `packaging/linux/io.github.thestephenromano.Matters.service`.

**Stopping.** An instance started with no window quits once nothing has called either interface for five minutes. It keeps running while a window is open or sync is on. The five minutes means a burst of calls, such as an agent planning the day, starts Qt only once, while Matters doesn't hold a Qt process all day. Closing a window that is showing quits Matters as before.

**Without a session bus.** Matters runs as before, alone, and other programs can't reach it.

**Access on a new install.** Agent access is off: no client is allowed until the preferences page for clients exists (#200). Until then, every call is refused with `UnknownToken`.

**Not yet included.** Today's calendar events aren't in `GetView("today")` when the app serves it. The app's calendar reader isn't connected to the interface yet.

## Clients and tokens

Every call carries the caller's client token as its first argument. Each client (the command line, links, an agent) has its own token and its own permissions:

| Permission | Allows |
|---|---|
| Read | Views, find and get |
| Create | Adding tasks, projects, headings and checklist items |
| Organise | Changing, scheduling, completing, moving and ordering items |
| Areas | Creating, renaming and ordering areas |
| Trash | Moving items to the Trash and putting them back (an area needs Areas too) |

The rules for tokens:
- **Format:** a token is `matters1_` followed by 64 lower-case hex digits, which encode 32 bytes from the operating system's random generator.
- **Storage:** the app keeps only each token's SHA-256, never the token itself. A fast hash is enough because a token has nothing to guess.
- **Scope:** clients belong to this device and are not synced. Where their records are kept is decided on #200.
- **Fixed limits:** whatever a client is allowed, no method empties the Trash or permanently deletes anything, and the Links client never trashes or changes areas.

## Items

Every item is a D-Bus dictionary (`a{sv}`) with a `kind` key: `task`, `project`, `area` or `event`. A key is present only when it has a value. Callers must ignore keys they don't know, because new keys can be added within `Automation1`.

The value formats are:
- **Dates:** ISO 8601, for example `2026-11-22`.
- **Times:** `HH:MM`.
- **Event times:** local `YYYY-MM-DDTHH:MM`.
- **Timestamps:** milliseconds since the Unix epoch.

| Key | Type | On |
|---|---|---|
| `kind`, `id`, `title` | s | all |
| `section` | s | `evening` (This Evening, in Today); the day, in Upcoming; `events` (Today's calendar events) |
| `notes` | s | tasks, projects |
| `status` | s | `open`, `completed` or `canceled` |
| `start_date`, `deadline` | s | tasks, projects |
| `evening`, `someday` | b | tasks, projects |
| `reminder` | s | tasks |
| `repeats` | b | tasks |
| `area_id`, `project_id`, `heading_id` | s | where the item is filed |
| `completed_at` | x | finished tasks and projects |
| `trashed` | b | tasks, projects, areas |
| `tags` | as | tasks with tags, in the tag list's order |
| `checklist_done`, `checklist_total` | u | tasks with a checklist |
| `start`, `end`, `all_day`, `calendar_id` | s, s, b, s | events |

## Methods

### `GetView(token s, view s) → (revision s, items aa{sv})`

**Needs:** Read.

Returns a view's items in the order the window shows them, with the view's revision. The revision changes whenever anything the view returns changes, and only then.

| `view` | Returns |
|---|---|
| `inbox` | Tasks not yet filed |
| `today` | Today's calendar events (the calendars the user shows), then today's tasks, then This Evening's |
| `upcoming` | Scheduled tasks, each with its day as `section` |
| `anytime` | Placed tasks (in an area or project) that can be done now |
| `someday` | Tasks deferred to Someday |
| `logbook` | Finished tasks, newest first, at most 500 |
| `trash` | Trashed tasks |
| `project:<id>` | A project's tasks |
| `area:<id>` | The tasks filed directly in an area, not in its projects |

### `Find(token s, query s) → aa{sv}`

**Needs:** Read.

Returns the tasks, projects and areas whose titles match, as Quick Find shows them.

### `Get(token s, id s) → a{sv}`

**Needs:** Read.

Returns one task, project or area by its ID.

### `ListProjects(token s) → aa{sv}` and `ListAreas(token s) → aa{sv}`

**Needs:** Read.

Return the open projects and the areas that aren't trashed, in sidebar order.

### `Apply(token s, changes aa{sv}, options a{sv}) → aa{sv}`

**Needs:** each change's own permission (below), and Read when `options` names a revision.

Makes `changes` in order, as one batch: all of them or none. Every change is parsed and its permission checked before anything changes. If any change is refused, or the batch is too large, nothing changes: the batch runs in one transaction and, with sync on, is recorded as one sealed record. Returns one dictionary per change, in order, with the `id` of what it created (empty for a change that creates nothing). After a batch, the window reloads its lists.

**Options**, all optional:

| Key | Type | Meaning |
|---|---|---|
| `view`, `revision` | s, s | The view and the revision `GetView` gave when the changes were built. If the view has changed since, the batch is refused with `StaleRevision`. The check runs inside the batch's transaction, so nothing can change in between. Both or neither. |
| `request` | s | A request ID, 1 to 128 bytes. A retry with the same ID and the same changes returns the first answer and changes nothing; the same ID with other changes is refused. IDs are this client's own, kept on this device for a day. |

**Limits.** A batch has 1 to 200 changes (`MAX_CHANGES`). A batch whose sync record would be larger than 1 MiB is refused with `BatchTooLarge`; it is never split, so send smaller batches. No operation empties the Trash or permanently deletes anything.

**Changes.** Each change is a dictionary with `op`, the operation, and its arguments, all strings unless shown. A key the operation doesn't take is refused, so a misspelt argument is never ignored.

| `op` | Needs | Arguments |
|---|---|---|
| `create_task` | Create | `title`; optional `notes`, `when`, `deadline`, `tags` (as, names of existing tags), and `area_id` or `project_id` (with `heading_id`). Without a home it goes to the Inbox. |
| `create_project` | Create | `title`; optional `notes`, `when`, `deadline`, `area_id` |
| `create_heading` | Create | `project_id`, `title` |
| `add_checklist_item` | Create | `task_id`, `title` |
| `create_area` | Areas | `title` |
| `rename_area` | Areas | `id`, `title` |
| `update` | Organise | `id` of a task or project; any of `title`, `notes`, `when`, `deadline` |
| `complete`, `cancel` | Organise | `id`. A project needs `open_tasks`: `complete` or `cancel`, for its open tasks. |
| `reopen` | Organise | `id` of a finished task or project |
| `move` | Organise | `id`, `to`: a task goes to `inbox`, `area:<id>` or `project:<id>` (with `heading_id`); a project to `area:<id>` or `none` |
| `place` | Organise | `id` of a task, `view` (`inbox`, `today`, `project:<id>` or `area:<id>`), optional `after`: the task it follows. Without `after` it goes first. |
| `tag`, `untag` | Organise | `id` of a task, project or area; `tag`, an existing tag's name |
| `check` | Organise | `id` of a checklist item; optional `done` (b, default true) |
| `trash`, `restore` | Trash | `id` of a task, project or area. An area needs Areas too. |

**Refs.** A change that creates something may give it a `ref`. A later change in the same batch can then name it as `ref:<name>`, wherever it takes an ID. That makes "a project with its tasks" one batch.

**Dates.** `when` takes what the When field takes: `today`, `evening`, `someday`, `anytime` (no date), a date such as `2026-12-01`, or a phrase such as `tomorrow` or `fri`. `deadline` takes the same days, or `none` to clear it. A numeric date that reads as different days month first and day first (`3/4`) is refused, because a program has no locale to settle it; use `YYYY-MM-DD`.

**The Links client** (`matters://` links, #69) never trashes or restores anything and never changes areas, whatever it was allowed.

## Errors

| Name | When |
|---|---|
| `UnknownToken` | No token was given, or no client of this device has it |
| `RevokedToken` | The client was revoked and needs setting up again in preferences |
| `PermissionDenied` | The client lacks the permission the method needs; the message names it |
| `InvalidArgument` | An argument the method doesn't accept, such as an unknown view, an unknown option or a reused request ID |
| `NotFound` | No item has that ID; in a batch, the message names the change (`change 2: …`, counting from 0) |
| `InvalidChange` | A change in a batch isn't one Matters can make: an unknown operation or key, a missing argument, a blank title, an unreadable date. The message names the change. |
| `StaleRevision` | The view a batch was built from has changed; read it again |
| `BatchTooLarge` | The batch is too large to record at once; send smaller batches |
| `Failed` | Matters couldn't complete the call, for example because its database failed |

## Tests

`matters-ui`'s `service` tests start a private `dbus-daemon`. They check that:
- a second start hands over and the running instance is asked to activate;
- a background start hands over without showing a window;
- the automation interface answers on the bus name and refuses a client a new install doesn't know;
- calls count as activity;
- an idle instance quits only after the idle period, and never while sync is on.


`crates/matters-automation/tests/reads.rs` serves the interface to a client over a socket pair, so no bus daemon is needed. The tests cover:
- every view;
- items' keys;
- revisions;
- unknown views and IDs;
- for every method, refusal of a missing, unknown or revoked token and of a missing permission.

`crates/matters-automation/tests/writes.rs` serves the interface the same way. The tests cover:
- every operation, and refs across a batch;
- a batch with one refused change, or refused while parsing, leaves every view as it was, and doesn't reload the window;
- a stale revision refuses the batch;
- each change's own permission, an area's trash needing Areas, and Read for a revision check;
- the Links client's limits;
- refusal of a missing, unknown or revoked token;
- retried requests, and request IDs being each client's own;
- the limits on changes and options;
- dates that read differently by locale;
- with sync on, a batch is one journal row and the engine agrees with the rows.

In `matters-core`, `tests/batch_tests.rs` covers the batches themselves: one record, all or nothing, the size cap and request IDs.
