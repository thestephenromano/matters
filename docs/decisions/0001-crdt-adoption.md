# ADR 0001: adopt Loro for the local sync prototype

Date: 2026-11-18. Status: **approved for bounded prototype evaluation on 2026-11-18**. Tracks [#32](https://github.com/thestephenromano/matters/issues/32); implementation belongs to [#35](https://github.com/thestephenromano/matters/issues/35).

The maintainer approved Loro 1.18 and the proposed hybrid HPKE/ML-DSA stack for prototype evaluation in the project conversation on 2026-11-18. This authorizes implementation of the experiment; production adoption and issue closure still require the evidence below.

## Recommendation and evidence

Evaluate **Loro 1.18.0**, with **one document per personal collection**, backed by SQLite. Adopt its map, text and ordered-tree CRDTs. Build Matters commands, projection and persistence integration. Do not implement a CRDT or a generic engine framework. This recommendation is based on source/API applicability, not measured performance or a completed integration.

The [dependency evidence](../evidence/sync-dependencies-2026-11-18.json) records exact package checksums, source revisions, licenses, publication dates and inspected-file hashes. Direct version pins are candidates for a prototype; no transitive lockfile has been resolved. The [reproduction instructions](../evidence/README.md) distinguish source inspection from execution evidence.

| Candidate | Inspected version / license | Applicability and decision |
| --- | --- | --- |
| [Loro](https://docs.rs/loro/1.18.0/loro/) | 1.18.0 / MIT; published November 16 | Rust text and ordered-tree APIs cover notes, placement and sibling moves without inventing a distributed ordering scheme. First candidate. Validate pending-import persistence, tree conflict behavior and history growth. |
| [Automerge](https://docs.rs/automerge/0.12.0/automerge/) | 0.12.0 / MIT; package requires Rust 1.90 | Established document and text alternative. Its inspected API does not offer the same direct ordered-tree fit; an application move representation would need additional evaluation. Keep as fallback if Loro fails the fixtures or budgets. The package's reported VCS revision did not resolve through GitHub; its checksummed package is the inspected source. Resolve provenance before adoption. |
| [SQLite-Sync](https://github.com/sqliteai/sqlite-sync/releases/tag/1.2.3) | 1.2.3 / Apache-2.0 reported by repository; verify license at pin | Attractive SQL integration and Linux release assets. No evidence gathered here establishes an opaque, client-only-key deployment satisfying this application's encryption contract. Do not choose it solely to avoid changing SQL writes; request/verify that architecture before reconsidering. |

Recent releases establish recent publication, not maintenance guarantees. Before production, assess maintainer responsiveness, security reporting, release compatibility and a viable patch/fork process. None of these packages has been built in Matters. Initial validation target is `x86_64-unknown-linux-gnu` on Fedora 44 with both Qt and GTK; `aarch64-unknown-linux-gnu` is a second required pilot target on an actual host. SQLite-Sync advertises GNU/musl x86_64 and arm64 assets; that is not a compatibility test. Loro's packaged manifest does not declare a Rust minimum; establish one with the resolved dependency graph.

## Collection and document boundaries

The reusable service authorizes **collections** and stores opaque records. A document is an engine-level merge domain inside a collection. They remain different identifiers even when v0 has exactly one document per collection.

| Boundary | Benefit | Cost / decision |
| --- | --- | --- |
| Whole personal collection | Parent moves, ordering, deletion evidence and tags share one causal history; one bootstrap lineage. | Entire collection is loaded and shared. Choose for the first prototype; benchmark growth. |
| Per project | Smaller loading and prospective sharing units. | Inbox, project moves, area ordering and shared tags need cross-document coordination. Defer until a demonstrated workload warrants it. |
| Per task | Fine-grained loading and access. | Parent/order changes and subtree deletion span documents; many documents and indexes. Reject for initial scope. |

Initial sharing means granting another personal device the whole collection and retained history. Per-project permissions, partial replication and multiple users need a new decision and migration; they are not promised by this layout.

## Proposed domain representation

Use one ordered Loro tree for areas, projects, headings, tasks and checklist items. Store each existing application UUID and immutable entity kind in the node metadata. Keep a unique UUID-to-engine-node mapping established by the originating create operation; creation of an existing UUID is rejected. Task and project notes use a child text container created with the entity's original lineage. Initialize required containers once, rather than allowing each replica to independently invent a container for the same logical field. Project schedules and status use the same tuple rules as tasks below.

Allowed parents: area → root; project → root/area; heading → project; task → root/area/project/heading; checklist item → task. Derive SQL `area_id`, `project_id` and `heading_id` from this ancestry. Placement and order are one tree move, never separate writes to independent SQL fields. A typed command rejects invalid local moves. Imported state is resource-checked and projected through the same invariant checks; invalid typed ancestry is quarantined with the offending records retained, not partially applied to SQL. The tree prevents structural cycles; it does not establish application-level validity.

Store title, deadline and each other independent scalar in separate map slots. Store schedule `(start_date, time_of_day, is_someday)` as one immutable scalar value. Store `(status, completed_at)` together; an open task always carries a null completion timestamp. Use the engine's scalar conflict order (logical time, then peer identity), not wall-clock timestamps, for these registers. Record the winning operation in diagnostic history. This deliberately permits a concurrent reopen to win over completion; neither action has unconditional precedence. [Loro map semantics](https://loro.dev/docs/tutorial/map)

Explicit trash is a separate scalar `(trashed, trashed_at)`. Effective trash visibility is derived from the entity and its current ancestors. Restoring a project or area leaves independently trashed descendants trashed. A task moved to a live parent can become visible even when its former parent is trashed. Timestamps are display/audit values and never determine merge order. Preserve original `created_at`; derive `updated_at` as the maximum recorded timestamp of accepted mutations to that entity, not projection time. Clock skew can affect that display value but never conflict resolution.

Use Loro ordered-tree position APIs for moves and reordering, enabling fractional indexing and recording its configuration in the prototype fixtures. SQL `sort_order` becomes a rebuildable sibling rank; retain stable UUID tie-breaking for migration of equal legacy ranks. Do not merge floating-point positions independently. Tree deletion hides descendants and is insufficient to implement irreversible Empty Trash. [Ordered-tree API](https://docs.rs/loro/1.18.0/loro/struct.LoroTree.html)

## Deletion and tags

Maintain permanent deletion evidence keyed by immutable entity UUID, using engine map entries that only transition to a tombstone. Admission rules prohibit removing or rewriting tombstones. Empty Trash records the exact set of entity UUIDs visible to the issuing replica, including known descendants and checklist items, in one command. A concurrent move does not rescue an ID already in this set. New descendants unknown to that replica are not implicitly purged. Keep tombstones and original updates for the prototype; history pruning requires [#40](https://github.com/thestephenromano/matters/issues/40).

Projection filters tombstoned entities independently of the engine's tree visibility. Recover a surviving node whose parent was permanently deleted to the nearest surviving, type-compatible ancestor in the merged tree; otherwise place tasks in Inbox and projects at root. Preserve sibling order, with UUID as the fallback tie-breaker. A surviving heading with no live project, or checklist item with no live task, needs a visible recovery record/export rather than an invented invalid SQL parent. The adapter must prove it can enumerate the relevant hidden nodes; inability to recover them is an adoption failure. Do not delete engine nodes as an optimization before these fixtures pass.

Deleting a heading tombstones that heading alone and projects its surviving tasks into its project, matching today's `ON DELETE SET NULL` intent. Deleting a checklist item tombstones that item. Empty Trash does not delete global tag definitions. Permanent deletion wins over stale edits and restores; a user-directed recovery creates a **new UUID** with explicit provenance. It never clears a tombstone.

Tag definitions retain individual UUIDs. Names compare with the current SQLite binary, case-sensitive equality. For equal names, project one canonical row using the lexicographically smallest UUID, mapping memberships from all matching definitions to it. This is a derived view, not an irreversible identity union: renaming a definition recomputes groups and retains original memberships. Membership for each `(task UUID, original tag UUID)` is a boolean scalar with engine conflict order. A command removing a visible canonical tag clears all currently known matching memberships; an unseen concurrent membership can remain. State this behavior in the UI if tag editing is exposed. No tag write API currently exists; adding one must use this command boundary.

An authenticated malicious writer can still vandalize content. These rules provide deterministic application behavior for valid commands, not Byzantine consensus. Verify that each imported change belongs to its authenticated writer incarnation; engine peer IDs are not identities by themselves.

## Concurrency fixture specification

All fixtures begin from one shared genesis document with fixed UUIDs and fixed test-only peer IDs. Fork replicas A/B, execute the actions below without delivery, exchange both ways, then repeat with reversed delivery, duplicate batches, dependency-before/after permutations and a restart between import and projection. Compare canonical projections and causal frontiers, not raw snapshot byte equality. Retain the schedule/seed and history on failure. These are specifications; no fixture has run yet.

| ID | Concurrent actions | Required oracle |
| --- | --- | --- |
| C01 | A renames task; B changes deadline | Both intentional edits survive; schedule/status unchanged. |
| C02 | Notes initially `ab`; A inserts `X` before `a`; B inserts `Y` after `b` | Notes become `XabY`. Same-position insert variant retains both inserts in the engine-defined order at every replica. |
| C03 | A completes; B reopens | The operation winning the pinned engine's scalar order supplies the entire status/timestamp tuple. A causal reopen after completion always wins. Repeat with swapped peer IDs and skewed wall clocks. |
| C04 | A chooses Someday; B sets date/evening | Exactly one complete schedule tuple wins; no mixed Someday/date state. |
| C05 | A moves task P→Q; B reorders it within P | Exactly one task, one legal parent, one sibling position, and identical order on both replicas. Record the engine's deterministic winning move from the fixed operation history; require a subsequent causally later move to win. |
| C06 | A trashes P; B edits a child in P | Child remains effectively trashed, edit is retained, and restore P exposes that edit. |
| C07 | A trashes P; B moves child to live Q | Child is visible in Q unless independently trashed. |
| C08 | A restores P; B independently trashes child | P is live; child remains trashed. Concurrent trash/restore of the same entity follows scalar operation order. |
| C09 | A empties trash containing T; B edits, restores or moves T | T remains permanently absent from SQL; deletion evidence and recoverable original history remain. |
| C10 | A purges P; B creates previously unknown task T in P | P remains deleted; T survives in nearest legal live ancestor or Inbox. Known descendants included in A's purge remain deleted even if B moves them out. |
| C11 | A deletes heading H; B edits task T in H | T remains in H's surviving project with edit retained and null `heading_id`. |
| C12 | A and B create tags named `work` and assign them | One projected tag row, references retained, canonical UUID is minimum. `Work` stays separate. Rename one original definition and rebuild: memberships separate correctly. |
| C13 | Both set a checklist item checked; then one deletes it while the other unchecks | First merge is checked, not toggled twice. Final item is absent with tombstone. |
| C14 | Deliver dependent update before its prerequisite, snapshot, restart, deliver prerequisite | Original pending bytes survive; resulting document/projection equals complete ordered delivery. |
| C15 | Clone a database, create fresh writer incarnations, make simultaneous edits | No reused engine actor/counter ranges; both edits survive. Deliberate actor collision is quarantined, not silently deduplicated. |
| C16 | Invalid parent types or duplicate UUID create in an authenticated input | Reject/quarantine before projection commit; prior valid state and offending bytes remain available. |
| C17 | Reorder Today on A; move/reorder the task's home list on B | Preserve both view-specific ordering intents. Today order must not move the task's parent or overwrite its home position. Finalize the representation against the ordering work noted below. |

For C03/C05, capture fixed expected winners from the documented pinned engine order and independent small reference traces before writing adapter tests; a test that merely compares two calls to the adapter is insufficient. Randomized delivery tests supplement these semantic oracles.

## SQL write audit and intended commands

Audit baseline: `e6bcdbaa28a0d282fcbf3405a6181d0dfb530d7e`. At that committed baseline, all production SQL mutations found in [storage/db.rs](../../crates/matters-core/src/storage/db.rs) and [schema initialization/migrations](../../crates/matters-core/src/storage/schema.rs). Re-run the audit when implementing.

Concurrent, uncommitted task-ordering work appeared during this research: it adds a separate `today_order`, creation helpers and dedicated ordering mutations. It is outside this PR and outside the baseline audit. Reconcile its final committed API before closing #32 or implementing #35. Home-tree order cannot represent independent Today order. Evaluate Loro's movable list for a separate view-order index keyed by task UUID, retaining one logical entry per task and deriving Today membership from the task schedule. Its initialization, duplicate-entry handling and migration need a fixture-backed decision after the ordering change lands. C17 makes that compatibility requirement explicit; the table below must not be treated as an audit of those later edits.

| Existing boundary | Intended command / constraint |
| --- | --- |
| `init_schema`, migration transaction | Versioned local migration; not a replicated domain edit. |
| `create_area`, `create_project`, `create_heading`, `create_task`, `create_checklist_item` | Typed create with stable UUID, valid parent, initial containers and position in one engine transaction. |
| `update_area`, `update_project`, `update_task` | Replace public whole-entity saves with `Rename`, `SetSchedule`, `SetDeadline`, `SetStatus`, `Move`, and text edits as appropriate. Do not serialize unchanged fields as new edits. |
| `complete_task`, `uncomplete_task` | `SetStatus` writes one status/timestamp tuple. |
| `trash_*`, `restore_*` | `SetTrash` changes explicit state only; inherited visibility is derived. |
| `empty_trash` and SQL cascades | `PurgeKnownEntities` records the explicit observed set; projection SQL deletion follows that decision. |
| `delete_heading`, `delete_checklist_item` | Permanent tombstone with the recovery rules above. |
| `toggle_checklist_item` | `SetChecklistChecked(bool)` captures user intent. Never replicate a non-idempotent SQL toggle. |
| `with_conn` | Four current callers in [query/mod.rs](../../crates/matters-core/src/query/mod.rs) perform reads. Restrict production use to a read API so future writes cannot bypass the command boundary. |
| Missing tag and heading/checklist editing APIs | Add explicit typed commands when exposed; no generic SQL or entity-save escape hatch. |

The Qt [bridge](../../crates/matters-ui/src/bridge.rs) and GTK [UI](../../crates/matters-gtk/src/ui.rs) currently fetch a task, modify it and save the whole entity for notes/scheduling. Both must send intentional commands. Text edits must refer to the displayed text version/cursors and transform against subsequent changes; computing a replacement against the latest document can erase unseen remote edits. Deferred GTK note callbacks are a specific test case.

Persist authoritative engine changes, outgoing work and SQL projection atomically in SQLite before acknowledging a local save. On failed commit, discard/reload speculative in-memory state. Persist original incoming bytes before import; pending imports must survive independently of snapshots. [Loro persistence](https://loro.dev/docs/tutorial/persistence)

## Migration and evaluation gates

One device imports a backed-up SQL database into a new genesis lineage, preserving UUIDs, timestamps, views and existing ordering. Resolve contradictory legacy parent references through a report and explicit choice before conversion. Other devices bootstrap from this lineage; they do not independently convert matching SQL rows. Later independent collections require an explicit import/deduplication workflow. Clone/restore preserves queued edits but assigns a fresh engine writer incarnation before new edits.

Use synthetic deterministic workloads: 1,000 / 10,000 / 100,000 tasks, ten tasks per project, 1 KiB notes, five checklist items per task; variants add 64 KiB notes and a deeply nested valid hierarchy. These sizes are declared test inputs, not claims about customers. Exercise 2 and 5 writers; 100,000 field/text/move operations; 30-day offline backlog modeled by a fixed edit trace; 10% duplicates, reverse order, dropped-then-retried dependencies and crash boundaries. Save the generator revision, seed, operation trace and delivery schedule.

Measure on the same host against the current SQLite app: p50/p95/p99 durable local commit, process startup, full replay, incremental catch-up, projection rebuild, peak RSS, bytes per operation, snapshot size and total retained history. Record CPU model, RAM, OS/kernel, target, Rust/lockfile, build flags, storage/filesystem, fsync settings and both source commits. Run warmup plus repeated samples; publish distributions and workload sizes. The host recorded in the evidence JSON is only the research environment, not a measured benchmark host.

Proposed prototype budgets, subject to approval:

- Correctness: all C01–C17 invariants, existing domain/view tests and crash recovery pass; zero lost acknowledged edits or silent resurrection.
- UI: p95 synchronous UI-thread command dispatch below 16 ms (one frame at roughly 60 Hz); durable save acknowledgement below 100 ms p95 on the 10,000-task workload. Keep work off the UI thread where needed. These are chosen interaction budgets, not measured results.
- Memory: incremental peak RSS below 128 MiB over the existing app at 10,000 tasks. This allocates substantially more than the roughly 10 MiB raw notes to engine history and indexes while remaining bounded on desktop machines; revise only with recorded workload evidence.
- Replay: 10,000-task bootstrap below 5 s on the named development host; 100,000 operations of catch-up below 10 s with visible progress. These are prototype usability targets, not service SLAs.
- Retention: record all history/pending bytes; no destructive compaction. Measure storage growth and establish a quota before pilot. No storage-cost claim is justified yet.

If these gates fail, first determine whether the engine, adapter or workload budget owns the failure. Compare the same fixture/trace with Automerge before inventing infrastructure. Approval authorizes a bounded prototype, not production adoption. The prototype decision is approved; record the remaining evidence links here before closing #32.
