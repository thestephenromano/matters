# ADR 0004: refer to goals in the Goals app from projects and repeating tasks

Date: 2026-11-21. Status: **proposed**. It needs agreement from the sync work, from the Goals app (#223) and from the maintainer, recorded on [#211](https://github.com/thestephenromano/matters/issues/211). Parent epic: [#202](https://github.com/thestephenromano/matters/issues/202).

## Context

The maintainer decided on 2026-11-21 that goals live in a separate Goals app ([#223](https://github.com/thestephenromano/matters/issues/223)), not in Matters:
- **Goals hold outcomes and evidence of progress:** what done looks like, why it matters, milestones, figures and reviews.
- **Matters holds actions:** the work.

Matters needs only to record which goal a project or repeating series serves, and to show that goal while the user works. This ADR defines that reference, how it merges under [ADR 0001](0001-crdt-adoption.md), and what Matters asks of the Goals app.

## Decision

### The reference

- **What it is:** a project, or a repeating series, serves at most one goal. The reference is the Goals app's ID for the goal, a UUID. Matters treats it as opaque, and never stores the goal's title, state or any other goal data in its document.
- **Where it lives:** a `goal` register in the entity's map, a scalar like `deadline`. It is either a UUID string or null. It follows the engine's scalar conflict order (ADR 0001, *Proposed domain representation*), so concurrent writes resolve the way every other register does. It is not a link map, because there is only ever one goal.
- **On a series:** the newest occurrence carries the series' goal, as it carries the rule. When the next occurrence is created, the goal passes to it with the rule. Earlier occurrences keep the goal they had when they were created, so a finished occurrence still records the goal it served.
- **Single tasks:** a task that does not repeat holds no reference. It serves its project's goal.
- **In SQLite:**
  - a nullable `goal_id` column on `projects`, and on `tasks` for occurrences;
  - a schema migration adds both, empty;
  - existing data and IDs are unchanged.
- **Commands:**
  - setting and clearing a project's or series' goal; both can be undone;
  - duplicating a project keeps its goal;
  - converting a task to a project starts with no goal, because single tasks hold none.

  A command is refused if its value is not a UUID.
- **Damaged values:** in an imported document, a value that is not a UUID string or null projects as no goal and is reported as the repair `goal-invalid:{id}`. It is never an error, so a damaged reference cannot stop projection.
- **Older versions:** they keep the register unchanged and ignore it ([#217](https://github.com/thestephenromano/matters/issues/217), ADR 0001 *Forward compatibility*).
- **Occurrence merge:** `goal` is added to the fields compared when identical generated occurrences merge (`visible_occurrence` in `crdt/projection.rs`), in the same change that adds the register (#212).

### What Matters shows

- **A local cache, not synced data:** Matters keeps the goal title and state it last read, per goal ID, in a table on this device. The table is a cache; it is never authoritative and never synced. Each device reads the Goals app for itself, and the Goals app syncs its own data across devices.
- **No automatic writes:** Matters never changes a reference because of something that happened in the Goals app. A goal that was achieved, abandoned or deleted is shown in that state, and the link stays until the user changes it. One app's state therefore never causes writes, or sync traffic, in the other.
- **Without the Goals app:** if it is not installed or cannot be reached, a linked project shows its goal as unavailable, with the cached title if there is one. Nothing else changes.

### Cross-app contract

Matters is a reader of the Goals app's local D-Bus interface (#223) and never writes to it. Matters needs exactly these methods from it, named here by purpose; #223 names and versions them:

| Purpose | Input | Output |
| --- | --- | --- |
| List goals for the picker | none | Active goals in the Goals app's order, each with ID, title, state (on track, at risk, late) and target date |
| Read linked goals | A list of goal IDs | For each ID, either *not found* or its title, status (active, achieved, abandoned), state and target date |
| Open a goal | A goal ID | The Goals app shows that goal |

It also needs one signal, *goals changed*, carrying the IDs of changed goals, so the cache can be refreshed without polling.

- **Versioning:** the version is part of the interface name, as in #203. Matters calls the highest version it knows, and treats a missing interface as *unavailable*.
- **Starting the Goals app:** reads for showing linked goals do not start the Goals app (D-Bus `NO_AUTO_START`). Matters uses its cache until the app is running. The picker and *open a goal* may start it, because the user asked for it.
- **Threads and time limits:** calls run off the UI thread with a short time limit. The limit is chosen and justified in the interface issue (#214). A call that times out counts as *unavailable*.
- **Permission:** if the Goals app's interface checks client tokens as Matters' does (#203), Matters is a Read-only client of it. How Matters is allowed in, without a manual token step for the user, is for #223 to decide.

## Merge cases

These extend ADR 0001's fixture table. Each runs from a shared genesis on replicas A and B, in both delivery orders, and compares projections.

| ID | Concurrent actions | Required oracle |
| --- | --- | --- |
| G01 | A links project P to goal g1; B links P to g2 | Exactly one of g1 and g2 wins, chosen by the pinned engine's scalar order; the same on both replicas. A causally later link always wins. |
| G02 | A links P to g1; B clears P's goal | One value wins by scalar order, either g1 or none; the same on both replicas. A causally later action always wins. |
| G03 | A moves P to the Trash; B links P to g1 | P stays in the Trash with the link kept; putting P back shows g1 (as C06). |
| G04 | A finishes P; B changes P's goal | Both survive: P is finished and serves B's goal. A finished project keeps its link. |
| G05 | A empties a Trash containing P; B links P | P is permanently absent from SQL, and its reference with it (as C09). |
| G06 | A completes series occurrence O1, creating O2; B changes O1's goal to g2 | O1 serves g2 and O2 serves the goal handed on when A created it, the same on both replicas. B's change lands on O1, which A no longer writes, so it does not reach O2; the user sees O2's goal and can change it. Handing on copies the goal to O2 and never writes O1's `goal`, so there is no concurrent write to race. |
| G07 | A and B generate the same occurrence of a series | `goal` is one of the fields compared for the occurrence merge (ADR 0001, *Generated occurrences*). Twins with the same reference merge as before; twins whose references differ stay conflict copies (C18). Unlike the rule, generating the next occurrence never rewrites `goal`, so twins differ only after a concurrent edit, and the user can see that difference. |
| G08 | A, on a build that knows goals, links P; B, on an older build, renames P | P is renamed and still serves the goal. B's edit leaves the register untouched (#217). |
| G09 | A imports a document whose P has a `goal` value that is not a UUID string | P projects with no goal, the repair `goal-invalid:{id}` is reported, and the rest of the document projects. |

## Consequences

- **Matters' document gains one register on two kinds of entity, and no new entity kind.** The sync work reviewed G01–G09 on 2026-11-21 (#232).
- **The Goals app's MVP includes the three methods and the signal above.** They are the first use of its D-Bus interface from outside the app.
- **No goal data in Matters.** Achieving, abandoning or deleting a goal happens in the Goals app only, and Matters only shows the result.
- **Matters works without the Goals app.** Linking is unavailable without it, but nothing else depends on it.

## Not decided here

- The Goals app's bus name, object path and interface version, which depend on its name (#223).
- The Goals app listing the Matters projects that serve a goal, which it would read from Matters' interface (#203). That comes after #223's MVP.
- Keyboard shortcuts and display details, which belong to #214.
