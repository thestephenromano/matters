# Atomic local edit and encrypted-outbox experiment

Status: bounded implementation under #35/#36, following the approved next slice after #132/#135. This connects the existing prototype command boundary to the existing outbox in one SQLite file. The production app, migration and authenticated incoming-write integration remain separate gates.

## Decision

Adopt the established transactional-outbox pattern with the already selected SQLite/rusqlite and Loro implementations. Reuse the existing reservation, encryption, signing, writer-handoff and exact-byte retry code. No new dependency, queue, transport or cryptographic construction is introduced. The alternative of coordinating two databases would add a recovery protocol: [SQLite WAL transactions](https://www.sqlite.org/wal.html) are atomic within each database, not across a set of attached databases. One file provides the required local boundary.

The combined store uses the prototype's WAL mode, FULL synchronization and foreign keys. Opens assert WAL, and writes use [immediate transactions](https://www.sqlite.org/lang_transaction.html). The existing linked SQLite version/source gate applies. The standalone outbox retains its DELETE/FULL behavior and its existing contention tests. The combined-store tests cover a held WAL reader, a competing writer, SQL rejection and process exits; this mode choice makes no performance claim.

## Local save invariant

For each accepted request that changes the CRDT, these rows commit together:

- the exact request identity/body and exported engine update in `journal`;
- the task projection derived from that staged state;
- the corresponding outbox intent, carrying those exact update bytes and the active writer incarnation.

`Replica` stages commands in a fork and invokes a mandatory `LocalOutbox` participant inside its write transaction. The participant checks the pinned binding, active owner and recovery state, then calls the existing intent insertion routine with that same transaction. In-memory document, projection and identity index advance only after commit. A rejected command, oversized export, exhausted intent capacity, SQL failure or failed commit leaves all three unchanged.

The participant and signing/sealing callbacks are trusted test support, reachable only with the crate's `local-outbox` feature (enabled by the key-distribution probe that composes them). This is not a plugin sandbox or protection against arbitrary SQL access. `Sender` keeps its replica and outbox private and exposes the composed operations. Standalone writable APIs refuse the combined format, so ordinary supported opens cannot omit capture or change its journal mode. The plaintext replica's receive API and test-actor override are disabled in integrated mode; the authenticated inbox remains a separate receiving store for this slice.

The caller retains a canonical `Request` UUID and command body before submission. The retry lookup runs under the same write lock as the save, so two handles can never both save one request. Exact retries return the saved journal result, including after an uncertain acknowledgement or reopen; the only change is a request re-issued on reopen (below), whose original operations were never sent. Reuse with a different command body fails. A retry of an already saved request reports its local result; it does not claim remote delivery or resolve recovery-required work. The logical batch is an injective encoding of the UUID's 16 bytes followed by 16 zero bytes. This stable routing identity is not a secret, a fresh record ID, a nonce or a key. The existing full-width random record/incarnation IDs remain unchanged.

A request producing no new engine operations retains its journal receipt but creates no transport intent. Empty authenticated engine updates would otherwise be refused by the receive actor-range rule. Multi-command requests stage and commit as a single unit; a later invalid command discards the entire staged batch.

## Opening, sealing and restart

The dedicated `matters-sync-atomic-outbox/1` marker distinguishes this composition from earlier standalone stores (a store that also receives uses `/2`, see [bidirectional](SYNC_BIDIRECTIONAL.md)), and `outbox_meta` records it as `atomic`, so a standalone open that raced the creation still cannot take the writer. Initial schema, outbox binding, fresh writer and signed root/handoff commit in one transaction; the `AfterWriterCommit` point follows that commit directly, before the rest of the open can fail. A writer orphaned by such a failure is an empty link in the signed handoff chain. The existing `matters-encrypted-outbox/3` tables remain within that file. Existing standalone, app and foreign databases are refused; this is not a migration.

The Loro document uses the actor allocated with the signed writer. Reopen restores the domain journal/checkpoint and allocates a fresh writer through the reviewed handoff path. Older handles are fenced before local commits, sealing, projection rebuilds and checkpoints. Retrying a previously accepted request can still return its retained receipt without performing a write.

Sealing happens after the local edit commit. A second SQLite connection to the same file performs the existing reservation-before-crypto and ciphertext/head transaction. It cannot enqueue domain work or allocate another writer through the `Sender` API. `next_batch` selects the oldest eligible intent. Uncertain sealing consumes the reservation; retry uses a new record ID, while committed ciphertext returns unchanged before entropy or crypto. Delivery and ACK bookkeeping operate on committed exact bytes. No network or authenticated remote ACK protocol is added.

An unsealed intent from a retired writer, such as an edit saved just before a crash, can never be sent as it is: its operations carry the old writer's actor, which receivers accept from no other writer, and the old writer's handoff has closed its chain. Re-sealing it under the old writer is refused (a restored copy must never sign a competing record). Instead **opening re-issues it**: in the open's write transaction, the document is rebuilt without the stranded operations (ignoring a checkpoint that holds them), each stranded request's saved command body is re-run as the new writer in journal order, its journal row takes the new operations, and its intent moves to the new writer in its original queue position. Only stranded requests and their no-op neighbours can follow the first stranded row (sealing is in order, and edits waited for re-issue), so each request re-runs on the state it first ran on. A failure commits nothing and the next open retries. Editing then continues; `recovery_required()` stays empty unless re-issue failed, and new state-changing commands still refuse to build on unsendable operations in that case.

A consistent backup can omit a ciphertext commit that happened later on the original device. The restored copy re-issues that work as its own new writer, whose handoff closes the old writer at the backed-up head; the original's later record contradicts it. The existing receiver detects that contradiction, freezes the device and retains fork evidence; re-issue never signs a competing record under the old writer. No copied local database proves global freshness. Backup tests use `VACUUM INTO`, not an unsynchronized copy of an active WAL/database pair.

## Evidence and limits

The integration suite covers saved edits through encryption and receiver projection, reversed/duplicate delivery, checkpoint restart, immutable retries, SQL failure at all three local write locations, a deferred-foreign-key COMMIT failure, request conflicts, invalid/no-op batches, capacity and payload bounds, fencing, incompatible opens, WAL contention and restored unsealed work. Eight additional child-process exits cover before/after initialization commit, before/after local edit commit, after reservation, after crypto and before/after ciphertext commit. The active execution record binds the source and records executed tests and targeted guard-removal checks.

Retained plaintext, request bodies, projections, journals and backups are unencrypted at rest. This keeps the existing 16 in-flight intent fixture capacity (acknowledged intents do not count) and 1 MiB exported-payload limit. Connections to the combined file wait up to one second for another short write (a seal or acknowledgement) instead of failing at once; it does not bound staging memory, arbitrary commands, retained request receipts or lifetime storage growth. Tests establish process-crash behavior on the recorded host, not power/media/host-loss durability, hostile-import safety or production security.

Membership/key epochs and the document stay fixed. Dynamic membership, secret storage, reconciliation, bidirectional authenticated writes in the same local database, production retention, UI command capture, migration, service APIs, cross-target execution, performance measurement and independent security review remain open.

## Bidirectional composition

[Bidirectional single-store receive](SYNC_BIDIRECTIONAL.md) now composes the authenticated inbox with the same journal and projection. It uses a separately pinned receive mode. Local intents must be sealed before incoming application; raw receipt can proceed while they wait. The sender-only mode and the disabled plaintext receive path remain available as described above. Production migration and UI integration remain open.
