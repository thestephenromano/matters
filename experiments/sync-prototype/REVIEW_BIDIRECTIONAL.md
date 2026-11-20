# Bidirectional replica review follow-up

Review of #150 (bidirectional replicas and the two-device simulation) against its merge `dd63919`. Contract: [bidirectional](../../docs/SYNC_BIDIRECTIONAL.md). Each finding lists its root cause, the owning boundary and the regression witness. Every behavioral witness below failed against the #150 code (with the merge-conflict test seam added to it) and passes after the change.

| # | Finding | Root cause and owning boundary | Change | Witness |
|---|---|---|---|---|
| 1 | A shared merge conflict rolled back the pass and recurred on every later pass, with no record of why | The standalone inbox records a conflict instead of a projection; the shared replica has no state without a projection, so the shared path could only fail | The pass still rolls back. The conflict is recorded (`inbox_meta.conflict`), later passes report it before staging, and local commands continue on the last committed state. `Sender::merge_conflict` exposes it. Valid commands never reach this state (seeded merge property test), so `force_merge_conflict` is a test seam | `a_shared_merge_conflict_is_recorded_and_stops_application_without_restaging` |
| 2 | The shared receive budget was enforced on one side: remote receipt ignored local rows, and a full budget let new local edits block application | Local rows claimed their budget only when a pass copied them in, so neither receipt nor authoring was charged for them | Every consumer is charged where it is created. A new local intent is charged in its command transaction (each becomes one retained row), and receipt counts remote rows plus local intents. A full budget refuses the edit or the receipt; a pass never fails on capacity | `the_shared_receive_budget_counts_local_and_remote_rows_where_they_arrive` |
| 3 | Local writer announcements filled the writer budget during a pass, then blocked every pass | Announcements were copied into `writer_receipts` lazily, on each pass | An open retains this device's announcements in the transaction that signs the new one. A full budget refuses the new writer and leaves none behind; passes no longer read `writers` | `a_full_writer_budget_refuses_a_new_writer_and_never_blocks_application` |
| 4 | The pass read `writers.announcement` without skipping NULL | Same lazy copy | The pass no longer reads `writers`. The open-time retention skips NULL, as `Outbox::announcements` does. A shared store is always announced, so no NULL row can occur there | Structural |
| 5 | Every pass read the plaintext and sealed bytes of every sealed intent ever written | Same lazy copy, with a consistency check over all history | A pass inserts only sealed intents not yet retained (one `INSERT … SELECT`), and reads plaintext only for local rows not yet applied. Sealed bytes never change once written | Structural; the existing echo and recovery tests cover the rows still read |
| 6 | Capacity and freeze checks decoded every retained row | `rows(tx)?.len()` inside the loop, and a full scan for local forks | The capacity check moved to the charge points and counts with SQL. The freeze check is an `EXISTS` query | Structural |
| 7 | A retained local row copied the whole shared document | `stage` returned `Ready(doc.fork())` for a row it does not import | `Stage::Retained` keeps the current document | Structural |
| 8 | `BeforeApply` fired before the journal and projection writes in shared mode | The hook ran inside the prepare callback | `apply_authenticated` takes a `before_commit` callback run after every staged write, just before the commit | `a_failed_shared_write_never_reaches_the_before_apply_point` |
| 9 | Sender-only stores marked `/1` were refused | `ATOMIC_FORMAT` moved to `/2` for every integrated store | `ATOMIC_FORMAT` is `/1` again; `SHARED_ATOMIC_FORMAT` (`/2`) marks stores that also receive, chosen by `LocalOutbox::format`. Opens accept both, and the open still checks the store's receive mode. `/2` still keeps older code from opening a shared store | `a_sender_only_store_keeps_the_first_atomic_marker` |
| 10 | Duplicated schema probes and guards | Copied at each use | `has_shared_inbox` serves the open, the inbox and the outbox; `Sender::inbox`/`inbox_mut` hold the mode guard; `retain_writer` owns the control size check | Structural |

## Decisions

- **Budget charging (2, 3):** the documented shared budget is unchanged. Charging at creation means exhaustion refuses the action that would exceed it: an edit, a receipt or a new writer. A device whose writer budget is full can no longer open, which is the fixture budget's limit until compaction exists.
- **Merge conflict (1):** stopping application and recording the conflict keeps local work available without a projection-free replica state. A projection-free replica would be a larger contract change.

## Sibling search

- Lazy charging of a shared budget: the outbox's `MAX_INTENTS` counts at enqueue, under its own transaction; no other budget is charged lazily.
- Reads of nullable announcements: `Outbox::announcements` and the new open-time retention both skip NULL; no other reader remains.
- Hook points inside prepare callbacks: the standalone `Inbox::process` already fires `BeforeApply` just before its commit.

## Remaining

- The fixture budgets have no compaction. A device that fills its writer budget cannot open again.
- A real merge conflict, if one were ever reachable, stops application on that device until reconciliation exists.
