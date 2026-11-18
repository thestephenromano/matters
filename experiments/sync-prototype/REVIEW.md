# PR #82 review evidence

This record covers the ten findings reported against `4600cf1`. The corrections were made in an isolated worktree after inspecting the contracts and running regression witnesses. It is an implementation/evidence record; no production security or performance certification is implied.

## Diagnosis and owning boundaries

| Finding | Evidence and invariant | Correction / regression coverage |
| --- | --- | --- |
| 1. Merged tag removal | Two definitions named `work`, both assigned to the same task, still projected a membership after removing the canonical ID. Commands must operate on the visible equivalence class. | Removal clears every currently known matching membership for that task. Tests retain other tasks, case-distinct names and unseen concurrent additions. |
| 2. Home positions | With a tombstoned first item, moving visible item 0 to 1 remained a no-op. A command position belongs to the visible sibling list, not the physical tree. | Resolve same-kind visible siblings to engine before/after anchors. Cover prefix/middle/suffix deletions, all valid move slots and children projected through a deleted heading. |
| 3. Today positions | The same tombstone-prefix witness failed in the retained Today reference list. | Translate the visible insertion slot into the raw list slot after removing the moving reference. Out-of-range visible positions reject without persisting changes. |
| 4. Retrying writes | Injected post-commit ACK loss followed by a retry inserted `onceonce`. Identical commands alone cannot distinguish a retry from another user intent. | Callers retain a serializable Request ID and command batch before submitting. The journal stores the intent and exact receipt atomically. Same-ID retries return that receipt; changed intent rejects. Cover create, notes, batch, rollback, restart, and process exit on both sides of commit. |
| 5. Ordering configuration | Every command set zero jitter. Loro 1.18 also defaults to zero; the existing code had no explicit runtime/fixture policy. Source inspection shows equal positions can require sibling position rewrites. | Configure each document instance once. Runtime uses an explicit eight-byte position allocation budget with peer-seeded engine RNG; fixture initialization selects zero. No measured history-growth or cryptographic randomness claim is made. |
| 6. Receive projection work | Receive projected the accepted document, candidate document, then candidate again inside persistence. The accepted projection already belongs to committed state. | Cache accepted state, compute candidate projection once, and persist that exact value. An instrumented unit test checks one projection and compares persisted/cache contents. |
| 7. Identity lookup | Every lookup converted complete metadata, including notes, for each scanned node. Batch creation compounds the repeated scans. | Maintain a shallow UUID/kind index. A 1 MiB notes fixture verifies that index construction and scalar-command lookup perform no deep metadata reads. Full-history staging/full projection costs remain explicit. |
| 8. Unused snapshots | Corrupting stored snapshot bytes did not affect open, proving the snapshot was never consumed. | Save snapshot, pending blob and journal cutoff atomically. Open imports that checkpoint and replays its tail. Tests assert restoration statistics, resolve pending updates after reopen, reject corrupt snapshots and migrate legacy checkpoints without losing pending data. |
| 9. App dependency coupling | Temporarily changing the app's rusqlite requirement from 0.32 to 0.33 made prototype resolution fail on competing `sqlite3` native links, not merely a stale lockfile. | Remove the app from the prototype graph. The optional baseline is a separate workspace/process. Repeating the temporary bump with `cargo metadata --locked` succeeds and shows no matters-core package in the prototype graph. The app source was restored after both probes. |
| 10. Missing quarantine | The receive error was returned to the caller, but the rejected bytes had no durable record. Dependent imports had no public pending-state diagnostics. | Store incoming bytes before import. Quarantine failures with their reason; expose Pending/Applied outcomes and persistent inbox entries. Resume Received entries after interrupted admission. Tests cover malformed bytes, invalid domain state, SQL failure, ID conflicts, restart and dependent-stream recovery. |

Six regression witnesses failed against the original code for their intended reasons: tag removal, home ordering, Today ordering, post-commit note retry, snapshot consumption, and durable rejected-record storage. The implementation changes followed those failures. The dependency-bump experiment independently reproduced finding 9. Findings 5–7 were traced through the pinned engine/configuration and materialization paths; their performance impact is not inferred from source alone.

## Assurance

- Full local suite with `--locked --features crypto-probe`: **46 passed**. One child-process helper is intentionally ignored during discovery and exercised twice by its parent test.
- Clippy with `--locked --all-targets --features crypto-probe -- -D warnings`: passed.
- Focused mutation testing with cargo-mutants 27.1.0 over `remove_tag` and `raw_move_slot`: **15 tested, 15 caught**, with the unmutated baseline passing. This is targeted evidence, not a whole-program mutation score.
- Sibling-path search covered create and batch commands, moves across parents and recovered containers, Today ordering, tag alias/name behavior, local and received journal entries, retry and subprocess exit, snapshot/pending/tail restoration, and the optional comparison executable. Identity/deep-value reads now remain in projection; fractional-index configuration lives at document construction rather than command dispatch. No applicable formal proof harness was present in the repository.

Reproduce the main checks using the commands in [README](README.md). The mutation command, run only in an isolated worktree, is:

```sh
cargo mutants --manifest-path experiments/sync-prototype/Cargo.toml --in-place --re 'remove_tag|raw_move_slot' --timeout 30 --build-timeout 60 --output target/sync-review-mutations
```

The [review execution record](evidence/review-2026-11-18.json) binds source, both dependency graphs, mutation outcomes and refreshed samples. The [original timing artifact](../../docs/evidence/sync-prototype-2026-11-18.json) remains historical evidence for its original revision; it is not a measurement of the corrected adapter. The optional app baseline now runs in a separate process and reports internal save duration excluding IPC.

## Remaining limits

The journal and inbox remain local, plaintext and unauthenticated. Quarantine records evidence and exposes dependency waits; it cannot automatically repair an invalid causal predecessor, authenticate its author or prove which rejected bytes caused a wait. Recovery uses an explicit valid re-delivery with a new transport ID; a rejected ID cannot be silently replaced with different bytes. There is no automatic dependency skipping or history deletion.

The prototype remains single-writer per local database. It retains all raw history, request receipts, pending blobs and quarantine data; quotas, authenticated membership, actor admission, encryption, full application views, large workloads and cross-target validation remain separate gates. The old prototype format migrates to version 1; pre-migration writes have no request receipt because the old API supplied no stable intent identity. No compatibility promise is made for running an older prototype binary against the upgraded format.
