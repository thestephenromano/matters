# The automation interface

Everything outside the app reaches Matters through one versioned D-Bus interface on the session bus. That includes the command line, `matters://` links, global capture and the MCP server (#197). The design and how it was chosen are recorded on [#203](https://github.com/thestephenromano/matters/issues/203). The code is the `matters-automation` crate, which either front end serves over its own database.

This document describes what is built so far: names, client tokens and the read methods. Writes and batches, activation, and the front ends' hook-up follow, as the order of work on #203 sets out.

## Names

| | |
|---|---|
| Bus name | `io.github.thestephenromano.Matters` (the app's ID, as `DBusActivatable` and Flatpak require) |
| Object path | `/io/github/thestephenromano/Matters/Automation` |
| Interface | `io.github.thestephenromano.Matters.Automation1` |
| Errors | `io.github.thestephenromano.Matters.Automation1.Error.<Name>` |

The version is part of the interface's name. A change that breaks callers gets a new interface (`Automation2`), and the old one keeps working for at least one release. Adding a method, or a key to an item, does not break callers and stays within `Automation1`.

## Clients and tokens

Every call carries the caller's client token as its first argument. Each client (the command line, links, an agent) has its own token and its own permissions:

| Permission | Allows |
|---|---|
| Read | Views, find and get |
| Create | Adding tasks, projects, headings and checklist items |
| Organise | Changing, scheduling, completing, moving and ordering items |
| Areas | Creating, renaming and ordering areas |
| Trash | Moving items to the Trash and putting them back; deleting headings and checklist items |

The rules for tokens:
- **Format:** a token is `matters1_` followed by 64 lower-case hex digits, which encode 32 bytes from the operating system's random generator.
- **Storage:** the app keeps only each token's SHA-256, never the token itself. A fast hash is enough because a token has nothing to guess.
- **Scope:** clients belong to this device and are not synced. Where their records are kept is decided on #200.
- **Fixed limits:** whatever a client is allowed, no method empties the Trash or permanently deletes anything.

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

## Errors

| Name | When |
|---|---|
| `UnknownToken` | No token was given, or no client of this device has it |
| `RevokedToken` | The client was revoked and needs setting up again in preferences |
| `PermissionDenied` | The client lacks the permission the method needs; the message names it |
| `InvalidArgument` | An argument the method doesn't accept, such as an unknown view |
| `NotFound` | No item has that ID |
| `Failed` | Matters couldn't complete the call, for example because its database failed |

## Tests

`crates/matters-automation/tests/reads.rs` serves the interface to a client over a socket pair, so no bus daemon is needed. The tests cover:
- every view;
- items' keys;
- revisions;
- unknown views and IDs;
- for every method, refusal of a missing, unknown or revoked token and of a missing permission.
