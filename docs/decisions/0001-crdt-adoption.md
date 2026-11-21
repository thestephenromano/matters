# ADR 0001: adopt Loro for the local sync prototype

Date: 2026-11-18. Status: **approved for bounded prototype evaluation on 2026-11-18; adopted for application integration on 2026-11-20**. Tracks [#32](https://github.com/thestephenromano/matters/issues/32); implementation belongs to [#35](https://github.com/thestephenromano/matters/issues/35).

The maintainer approved Loro 1.18 and the proposed hybrid HPKE/ML-DSA stack for prototype evaluation in the project conversation on 2026-11-18. On 2026-11-20, on the evidence recorded under [Closure evidence](#closure-evidence), the maintainer adopted Loro 1.18 for integration into `matters-core` ([#35](https://github.com/thestephenromano/matters/issues/35)), with sync off by default. Adoption selects the engine and this document model. The acceptance budgets below become #35's gates, and the post-quantum stack keeps its own status in [ADR 0002](0002-post-quantum-sync.md).

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

All fixtures begin from one shared genesis document with fixed UUIDs and fixed test-only peer IDs. Fork replicas A/B, execute the actions below without delivery, exchange both ways, then repeat with reversed delivery, duplicate batches, dependency-before/after permutations and a restart between import and projection. Compare canonical projections and causal frontiers, not raw snapshot byte equality. Retain the schedule/seed and history on failure. The prototype executes C01–C21 (see [Closure evidence](#closure-evidence)) with the limits stated there.

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
| C16 | Invalid parent types or duplicate UUID create in an authenticated input (within that input's own causal past) | Reject/quarantine before projection commit; prior valid state and offending bytes remain available. |
| C17 | Reorder Today on A; move/reorder the task's home list on B | Preserve both view-specific ordering intents. Today order must not move the task's parent or overwrite its home position. Finalize the representation against the ordering work noted below. |
| C18 | A and B concurrently create entities under one UUID (duplicate import or faulty writer) | Neither creation is rejected and no edit disappears. The node with the smaller Loro tree ID keeps the UUID; each other node becomes a conflict copy with a derived UUIDv8 identity, keeping its own fields, kind, children and position. References by UUID resolve to the original. Identical on every replica in every delivery order ([#113](https://github.com/thestephenromano/matters/issues/113)). |
| C19 | Concurrent creations under one UUID add duplicate or non-task Today references | The first reference in the list's CRDT order wins; references naming a non-task are skipped. Each repair is reported in the projection. |

For C03/C05, capture fixed expected winners from the documented pinned engine order and independent small reference traces before writing adapter tests; a test that merely compares two calls to the adapter is insufficient. Randomized delivery tests supplement these semantic oracles.

## SQL write audit and intended commands

Audit baseline: `e6bcdbaa28a0d282fcbf3405a6181d0dfb530d7e`. At that committed baseline, all production SQL mutations found in [storage/db.rs](../../crates/matters-core/src/storage/db.rs) and [schema initialization/migrations](../../crates/matters-core/src/storage/schema.rs). Re-run the audit when implementing.

Concurrent, uncommitted task-ordering work appeared during this research: it adds a separate `today_order`, creation helpers and dedicated ordering mutations. It is outside this PR and outside the baseline audit. Reconcile its final committed API before closing #32 or implementing #35. Home-tree order cannot represent independent Today order. Evaluate Loro's movable list for a separate view-order index keyed by task UUID, retaining one logical entry per task and deriving Today membership from the task schedule. Its initialization, duplicate-entry handling and migration need a fixture-backed decision after the ordering change lands. C17 makes that compatibility requirement explicit; the table below must not be treated as an audit of those later edits.

Prototype follow-up: that ordering work is now committed at `47dcba5`. Its additional mutation boundary is `ordering::{place_task, move_task, place_project, place_area}`, with `Database::add_*` and `with_conn_mut`. The [evaluation harness](../../experiments/sync-prototype/README.md) creates one Today movable-list entry alongside each original task create, retains those entries and filters tombstones in projection. C17 demonstrates independent Today/home intent. Production view membership, evening sections and SQL/UI command routing still require integration against the final ordering branch.

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

### Re-audit for integration (#35, 2026-11-20)

Before the engine is wired in, every write in `matters-core` goes through a typed operation, and the raw connection is read-only:

- Whole-entity saves are gone. `update_task`, `update_project` and `update_area` are replaced by field operations: `set_task_notes`, `set_task_schedule`, `set_task_deadline`, `cancel_task`, `set_project_notes`, `set_project_schedule` and `set_project_deadline`, alongside the existing renames, `complete_task`/`uncomplete_task`, `finish_project`/`reopen_project` and `set_task_home`/`set_project_area`. Moving a Today task between sections writes only its schedule tuple. Both frontends and the measurement baseline use these operations.
- `Database::with_conn` sets `PRAGMA query_only` for its duration, so SQLite refuses any write through it. Every write goes through the crate-private `Database::write`, which records the write's engine commands in the same SQLite transaction when sync is on. The audit found one write on the read path (`remove_tag_from`), which now uses the write path. Tests that must stage states the operations never produce use `with_raw_conn`, which exists only with the `test-support` feature.
- The public write surface is the `Database` operations for areas, projects, headings, tasks, checklist items and tags, plus `crate::ordering`. Each is one intent with its own validation, so the engine adapter maps operations to commands one for one.

### Application document model (#35, 2026-11-20)

`matters_core::crdt` implements this ADR's representation for the application's current domain. It is not yet wired into `Database`. Three rules differ from the prototype, because the application gained behavior after this ADR was written:

- **Deleting a heading or checklist item can be undone.** The app offers Undo for both, so deletion is a `removed` register, not a tombstone. A removed heading's tasks project into its project with no heading, which C11 required. Undo brings the heading back with every task still under it, and with concurrent edits kept. Only Empty Trash writes permanent tombstones.
- **Tags nest, and their names are unique ignoring case.** Tags live in a second ordered tree. Deleting a tag tombstones it, and its children project under the nearest surviving ancestor, as `delete_tag` does. Concurrent tags whose names differ only in case merge into the one with the smallest UUID. Their assignments move to it, and the merge is listed as a repair. This replaces C12's case-sensitive grouping.
- **Projects, areas and tasks take tags.** An assignment is a boolean register per (item, tag).

Status, finish time and the "closed by its project" mark share one register, and an open item never carries a finish time or the mark. Every command stamps the entity's `updated_at`. Order keys in the projected rows are ranks, so only their relative order is meaningful.

### Recorded writes (#35)

With sync on, every typed write records its engine commands in the transaction that changes the rows, and the engine is rebuilt from the journal if that transaction fails. Only schema migrations write outside `Database::write`. Ordering records each changed key as a placement after its predecessor among its siblings, and Today anchors within the task's section. Empty Trash records the exact set of entities it deletes, cascades included. Converting a task into a project purges the task and its checklist, so a concurrent edit to the old task elsewhere is dropped, as with Empty Trash.

### Generated occurrences (#61, 2026-11-21)

Repeating tasks derive each occurrence's ID from its series and date. Two devices that generate the same occurrence therefore create two nodes under one UUID, which C18 would show as a conflict copy. The refinement:
- **Merge when identical.** When every node under that UUID is a generated occurrence matching in everything a user can see, they merge into the node with the smallest tree ID, and the merge is listed as an `occurrence-merge` repair. "Everything a user can see" means title, notes, When, deadline, status, trash state, reminder, series and date, and the checklist's titles and ticks in order. The other nodes, and their checklists, are hidden and can't be addressed.
- **Otherwise keep C18.** Nodes that differ in anything visible stay conflict copies, as C18 says, so no edit is lost.
- **Not compared:** the repeat rule and timestamps. A device hands the rule on and stamps the occurrence when it generates the next one.

### Forward compatibility (#217, 2026-11-21)

A newer version may add entity kinds (goals, #202) and fields that an older version syncs before it can read them. The rule:
- **Unknown kinds.** A node of a kind this build doesn't know is kept and merged like any other, but it is neither projected nor addressable. A command naming its ID, or creating that ID, is refused.
- **Their children.** A known child of such a node projects under its nearest ancestor that may host it, as under a removed heading. That may be the top level, ranked among the top level's own rows. If no ancestor fits, it goes to recovery. The tree keeps the newer placement until someone moves the child.
- **Unknown fields** on a known kind are ignored by projection. Edits never remove them, because commands write their own keys only.
- **Damage.** A `kind` that is not a JSON string is still a damaged document and an error.

### Measured cost (#35, 2026-11-20)

[The engine cost record](../evidence/sync-engine-cost-2026-11-20.json) compares sync on with the unchanged write path on `betelgeuse` (i9-10920X, NVMe/Btrfs, Fedora 44), at 1,000 and 10,000 tasks with 1 KiB notes:
- **Saves meet the budget.** Durable save p95 is 3.16 and 3.76 ms with sync on, against 3.77 and 8.52 ms with sync off.
- **Reopen meets the budget.** It replays the journal: 87.5 ms and 605.5 ms, against the 5 s bootstrap budget.
- **Creating a task costs more and grows.** About 41 ms per task at 10,000 tasks, against 7.5 ms with sync off, growing with the task count because predecessors are found by unindexed key lookups. That is a scaling cost to remove before a pilot.
- **Not measured:** peak RSS, and receiving or rebuilding at these sizes.

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

If these gates fail, first determine whether the engine, adapter or workload budget owns the failure. Compare the same fixture/trace with Automerge before inventing infrastructure. The prototype approval authorized a bounded experiment. Application integration was approved on 2026-11-20; these budgets are #35's acceptance gates, measured there against the unchanged SQLite baseline.

Initial execution is recorded in [the prototype evidence](../evidence/sync-prototype-2026-11-18.json). The focused cases pass on Linux x86_64, with explicit coverage limits for actor authentication, hostile imports and UI behavior. The 1,000-task comparison is exploratory; no full-workload acceptance budget is marked passed.

## Closure evidence

#32's deliverables, with the artifacts that establish each:

- **Adoption record:** this ADR names Loro 1.18.0, one document per personal collection, the rejected alternatives and their reasons. Exact checksums and source revisions are in [the dependency evidence](../evidence/sync-dependencies-2026-11-18.json); the prototype's lockfile pins the resolved graph ([`experiments/sync-prototype/Cargo.lock`](../../experiments/sync-prototype/Cargo.lock)).
- **Concurrency cases:** [`tests/convergence.rs`](../../experiments/sync-prototype/tests/convergence.rs) runs C01–C19 against two SQLite-backed replicas, with reversed and duplicate delivery and fixed test peer IDs. C20 guards the reserved derived-identity namespace, and C21 is a seeded property test over merged concurrent histories. The [prototype README](../../experiments/sync-prototype/README.md#what-the-experiment-establishes) states which variants each name covers. Writer-actor ownership (C15) and authenticated import admission (C16) are exercised by the [durable receive](../SYNC_DURABLE_RECEIVE.md) and [writer lifecycle](../SYNC_WRITER_LIFECYCLE.md) experiments, not by the convergence suite.
- **Migration model:** [Migration and evaluation gates](#migration-and-evaluation-gates) fixes one initial lineage per collection, created by one device with the existing UUIDs. Other devices bootstrap from it, and independent imports go through an explicit import workflow. Implementation belongs to [#41](https://github.com/thestephenromano/matters/issues/41).
- **Evaluation plan:** the workloads, measurements and budgets above. The [exploratory 1,000-task run](../evidence/sync-prototype-2026-11-18.json) names its host and configuration. No full-workload budget is marked passed; #35 measures them.
- **Canonical design:** [SYNC_DESIGN.md](../SYNC_DESIGN.md), linked from the [specification](../SPEC.md#54-planned-encrypted-synchronization), the README and epic [#31](https://github.com/thestephenromano/matters/issues/31).
- **SQL write audit:** the table above audits the baseline commit and names the later ordering boundary. #35 re-runs the audit against the code it integrates, including the later headings, move and project-completion paths, before routing writes through commands.
