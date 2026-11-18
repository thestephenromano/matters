# Encrypted outbox persistence experiment

Status: bounded test-only implementation of the [content-record sealing contract](SYNC_RECORD_CONTRACT.md#local-sealing-state-machine-and-budgets), following the [key-distribution experiment](SYNC_KEY_DISTRIBUTION.md). Tracks [#36](https://github.com/thestephenromano/matters/issues/36) and [#35](https://github.com/thestephenromano/matters/issues/35); neither is closed by this experiment.

## Adoption and scope

Reuse the established transactional-outbox design and the existing rusqlite/SQLite dependency. The maintainer approved this next step after PR #93; no broker, transport, storage service or new dependency is introduced. [SQLite transactions](https://www.sqlite.org/lang_transaction.html) supply the atomic boundary; explicit reservation-before-crypto and stored-byte retry rules supply the application invariant. The approved content codec, HKDF/AES provider and ML-DSA helper are reused unchanged in cryptographic behavior.

The [implementation](../experiments/sync-prototype/tests/support/encrypted_outbox.rs) is integration-test support, outside the application and the earlier plaintext replica database. It opens only a dedicated `matters-encrypted-outbox/0` database and refuses other populated database formats. This is a persistence experiment, not a production encryption API, authenticated relay or task-command integration.

The bundled `libsqlite3-sys 0.40.0` amalgamation reports SQLite 3.54.1; the source ID and amalgamation hash match the [upstream release](https://www.sqlite.org/releaselog/3_54_1.html). The [SQLite upgrade record](SQLITE_UPGRADE.md) documents the WAL-reset fix provenance and three independently resolved runtime probes. This experiment continues to select `journal_mode=DELETE`, `synchronous=FULL`, foreign keys, and immediate write transactions. It verifies that rollback journaling was selected. The dependency gate in [#97](https://github.com/thestephenromano/matters/issues/97) is addressed by that upgrade. WAL was reassessed and is unnecessary for this experiment: its current writer-fencing and COMMIT-busy fixtures use rollback journaling. A future mode change needs its own contention/fault coverage and measured benefit. This configuration makes no performance claim. The [atomic-commit documentation](https://www.sqlite.org/atomiccommit.html) describes the underlying filesystem/hardware assumptions.

## Owning invariants

1. A sealing callback is invoked only after its fresh record ID and complete immutable header are committed.
2. Each attempt invokes that synchronous `FnOnce` callback at most once. There is no reservation-resume API. Any uncertain attempt is abandoned before another ID is reserved, including retries in the same process.
3. Completed ciphertext/signature bytes, attempt completion and writer-chain head advance commit atomically. No pending-upload API exposes uncommitted bytes.
4. A completed logical batch returns its original stored bytes before requesting entropy or invoking crypto. A changed payload under the same batch ID is a conflict.
5. Reopen/restore creates a fresh local writer incarnation and actor, fences older handles, and retains all existing intents, reservations, ciphertext and acknowledgements. Unsealed old-writer intents return `RecoveryRequired` before entropy or crypto; `recovery_required()` exposes their batch, original writer/actor and plaintext for later domain reconciliation. Local uniqueness checks include retained history.
6. Delivery acknowledgement is monotonic bookkeeping; it never deletes records or intent history.

The local database and the sealer callback are trusted. The outbox supplies a reserved header and retained plaintext to the callback, parses its result against the configured route and compares the entire header with the reservation. **It does not independently verify the callback's signature, key choice or ciphertext correctness.** The actual harness uses the approved crypto helpers, and receiver tests verify signatures/AEAD and import the decrypted Loro update. A production implementation must make that sealer/key-authority boundary concrete rather than accept arbitrary callbacks. No network upload occurs inside the callback.

Keys remain in the caller's memory. The outbox stores no CEK or private signing/KEM key. Its immutable binding contains application, tenant, collection, document, epoch, manifest and author. It rejects changes to any of those on reopen, before requesting entropy. Selecting the correct authenticated CEK and signer remains the caller's responsibility. Epoch migration, revoked-author handling and authorized resealing are not implemented.

## Durable states and operations

| State/action | Transaction and observable behavior | Failure/retry |
| --- | --- | --- |
| Enqueue | Persist batch ID, original writer and prepared plaintext together. | Before commit, nothing is accepted. After commit, the same batch/payload is idempotent; changed bytes conflict. Caller retains the batch ID and payload until acceptance is known. |
| Reserve | Caller uses `next_batch()` to find the oldest eligible current-writer batch, then passes its ID to `seal(batch, ...)`. Reject retired-writer intents and out-of-order current-writer batches; read the current writer head, abandon uncertain attempts, and insert a fresh random ID plus canonical header. | Transaction commits before crypto. RNG failure or a known ID collision returns an error without invoking the sealer. |
| Seal | Invoke trusted callback once using the reserved header. | Any error, panic or abrupt exit leaves an uncertain reservation. A same-session retry uses a different ID; existing IDs remain reserved in history. After reopen an unfinished intent requires recovery, even if crypto never started. |
| Store | In one transaction, insert complete signed record bytes, mark the attempt sealed and advance the active writer head to SHA-384 of those exact bytes. | Statement/commit failure leaves no uploadable row or advanced head. Lost local acknowledgement after commit returns original bytes on retry. |
| Upload retry | `pending()` returns immutable stored records without deleting them. | Lost remote ACK produces the same record ID, ciphertext and randomized signature. The harness simulates duplicate receipt; no service exists here. |
| ACK | Mark the known record acknowledged. | Unknown IDs fail; duplicate ACKs are idempotent. The original record remains retrievable after ACK or restart. |

The schema separates `intents`, `attempts`, `sealed`, `writers` and the format/binding/active-owner metadata. SHA-384 chain heads refer to full signed records. The queue seals in intent order per active writer within one bound document; retired intents cannot block fresh-writer work. This avoids concurrent pending predecessor selection without signing old-actor payloads under a new incarnation. An existing sealed batch can be retrieved regardless of later pending batches.

The caller must atomically integrate prepared CRDT updates with its domain journal in a later step. This experiment persists already-prepared payloads; it does not commit task state and outbox intent together, validate arbitrary Loro actor ownership, or construct domain commands. Snapshot sealing is unsupported here: new headers are always `data`.

## Reopen, contention and clone behavior

Every successful `Outbox::open` persists a fresh random 32-byte incarnation and a random 64-bit engine actor, then replaces the active owner. Duplicate local actors/incarnations fail the whole open transaction. Enqueue and both sealing transactions verify that owner inside their transaction. If another open occurs while crypto is running, the older handle cannot commit the result. Its consumed reservation and original intent remain available to the new owner for explicit recovery, never automatic signing.

SQLite write contention is returned immediately (`busy_timeout=0`); there is no internal retry loop or timeout-based takeover. A real reader-held lock is used to make the final outbox COMMIT fail in the tests. After the lock is released in the same session, the next seal uses a fresh ID and the unchanged committed predecessor. There is no lifetime attempt limit: 70 real COMMIT-busy failures followed by successful sealing are covered by a regression test. Owner checks and SQLite transactions cover cooperative handles using this component, not arbitrary SQL writers or hostile modification of the local database.

Retained intents keep their original writer, engine changes and logical batch. New edits use the fresh writer/actor. Clone tests use SQLite's [VACUUM INTO](https://www.sqlite.org/lang_vacuum.html) to obtain a consistent live snapshot; copying only an active database file is not a supported backup procedure. Existing ciphertext is identical after restore; uncertain reservations are abandoned. Separately resumed copies use independent OS entropy and local collision checks. This is probabilistic separation between isolated copies, not coordination or global uniqueness.

A backup cannot establish that its retained writer head is the latest signed head. Consequently **every `Outbox::open` retires old writers for new sealing**, including ordinary restart. `seal()` returns the typed `RecoveryRequired` error for old unsealed intents before reserving an ID or calling crypto. It never automatically rewrites their actor, batch or incarnation. `recovery_required()` exposes the retained work; `next_batch()` reports only work eligible for the active writer. Domain-command replay and authenticated history reconciliation remain under #36/#37/#40. A user-facing recovery flow must be designed before application integration; pending plaintext is preserved but is not automatically delivered after a restart.

**Preservation is not remote admission.** Existing signed bytes can still be delivered exactly as stored, but may be stale after a replacement writer is admitted. This experiment does not authorize both histories or claim transparent concurrent-clone synchronization. It prevents creating a new old-writer successor from a restored head; live process/OS snapshots and hostile database edits remain outside the model.

`Delivery::open(path, binding, read_only)` opens an existing database without allocating/fencing a writer, changing reservations, requiring keys, or requesting entropy. Read-only access provides `record()`/`pending()`; read-write delivery also provides `acknowledge()`. SQLite enforces the read-only flag. Neither mode creates a missing database. ACK bookkeeping is independent of writer ownership and never advances a chain head. Binding validation applies to both modes.

## Evidence and limits

The [tests](../experiments/sync-prototype/tests/cases/encrypted_outbox.rs) exercise:

- A real CEK envelope, admitted writer and encrypted Loro update, followed by exact-byte retrieval after restart without calling RNG or crypto again.
- Ordered updates, full-record hash chaining and rollback of head changes when ciphertext storage fails.
- Same-process uncertain retries, forced record/actor/incarnation collisions, entropy failure and mismatched route/epoch/authority bindings.
- A real SQL trigger rejection, a real COMMIT-busy failure, and a new owner arriving during an old owner's encryption callback.
- Simulated lost upload ACKs, idempotent receipt bookkeeping, retained records after ACK and consistent database cloning.
- Abrupt child-process exits before/after intent commit, before/after reservation commit, after crypto, before/after outbox commit and before/after ACK commit: nine child executions with exact exit-code and recovered-state assertions. The ignored child helper runs through its parent test; direct `--include-ignored` discovery is a no-op when all worker environment variables are absent. Partially configured workers still fail.
- Oversize input, changed sealer headers, foreign table names including `SQLiteCache`/`sqliteXmeta` and view-only databases, delivery-only/read-only access, more than 16 writable reopens and a new-work capacity limit.

The new-work budget is 16 retained intents per database, with the existing 1 MiB plaintext cap. Sixteen intents allow up to 16 MiB of retained plaintext plus at most 16 record payloads and framing; SQLite pages, journals and temporary allocations are additional. This chosen fixture capacity limits new enqueue operations; it does not prevent opening, sealing accepted current-writer work, reading recovery state, delivering bytes or ACK bookkeeping. Writer and reservation identity histories have no lifetime count cap and are never deleted or reused. They grow with restarts and failed attempts. Production disk quotas, retention/compaction and storage-exhaustion recovery remain an explicit design gate; this experiment claims no indefinite service availability.

The [active execution record](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) binds final source, documentation, fixtures and checks. CI checks those hashes, independent wire/symmetric-crypto fixtures, Python oracle guards, formatting, Clippy and the full Rust suite. Tests use temporary local databases, including RAM-backed temporary storage on the recorded host. Abrupt process termination is tested; power loss, media failure, arbitrary storage corruption, OS/RNG snapshots and host-loss recovery are not. Synthetic fixed credentials appear only in the crash harness so parent and child can use the same test authority without serializing secrets.

Pending plaintext is intentionally retained locally; SQLite, journals and backups are **not encrypted at rest** by this component. Secret storage, zeroization, durable membership/actor authority, full authenticated receive chains, epoch transitions, historical/snapshot admission, task-domain integration, network/service durability, exact-suite independent interoperability, cross-target execution and independent security review remain open.
