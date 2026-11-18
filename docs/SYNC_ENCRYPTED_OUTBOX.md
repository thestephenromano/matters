# Encrypted outbox persistence experiment

Status: bounded test-only implementation of the [content-record sealing contract](SYNC_RECORD_CONTRACT.md#local-sealing-state-machine-and-budgets), following the [key-distribution experiment](SYNC_KEY_DISTRIBUTION.md). Tracks [#36](https://github.com/thestephenromano/matters/issues/36) and [#35](https://github.com/thestephenromano/matters/issues/35); neither is closed by this experiment.

## Adoption and scope

Reuse the established transactional-outbox design and the existing rusqlite/SQLite dependency. The maintainer approved this next step after PR #93; no broker, transport, storage service or new dependency is introduced. [SQLite transactions](https://www.sqlite.org/lang_transaction.html) supply the atomic boundary; explicit reservation-before-crypto and stored-byte retry rules supply the application invariant. The approved content codec, HKDF/AES provider and ML-DSA helper are reused unchanged in cryptographic behavior.

The [implementation](../experiments/sync-prototype/tests/support/encrypted_outbox.rs) is integration-test support, outside the application and the earlier plaintext replica database. It opens only a dedicated `matters-encrypted-outbox/0` database and refuses other populated database formats. This is a persistence experiment, not a production encryption API, authenticated relay or task-command integration.

The bundled `libsqlite3-sys 0.30.1` amalgamation reports SQLite 3.46.0. [Upstream documents a WAL-reset race](https://www.sqlite.org/wal.html) in that version range. This experiment therefore selects `journal_mode=DELETE`, `synchronous=FULL`, foreign keys, and immediate write transactions. It verifies that rollback journaling was selected. [#97](https://github.com/thestephenromano/matters/issues/97) tracks a fixed SQLite dependency and WAL reassessment for the app and earlier replica experiment. This configuration is chosen for a small inspectable experiment; it makes no performance claim. The [atomic-commit documentation](https://www.sqlite.org/atomiccommit.html) describes the underlying filesystem/hardware assumptions.

## Owning invariants

1. A sealing callback is invoked only after its fresh record ID and complete immutable header are committed.
2. Each attempt invokes that synchronous `FnOnce` callback at most once. There is no reservation-resume API. Any uncertain attempt is abandoned before another ID is reserved, including retries in the same process.
3. Completed ciphertext/signature bytes, attempt completion and writer-chain head advance commit atomically. No pending-upload API exposes uncommitted bytes.
4. A completed logical batch returns its original stored bytes before requesting entropy or invoking crypto. A changed payload under the same batch ID is a conflict.
5. Reopen/restore creates a fresh local writer incarnation and actor, fences older handles, and retains all existing intents, reservations, ciphertext and acknowledgements. Local uniqueness checks include retained history.
6. Delivery acknowledgement is monotonic bookkeeping; it never deletes records or intent history.

The local database and the sealer callback are trusted. The outbox supplies a reserved header and retained plaintext to the callback, parses its result against the configured route and compares the entire header with the reservation. **It does not independently verify the callback's signature, key choice or ciphertext correctness.** The actual harness uses the approved crypto helpers, and receiver tests verify signatures/AEAD and import the decrypted Loro update. A production implementation must make that sealer/key-authority boundary concrete rather than accept arbitrary callbacks. No network upload occurs inside the callback.

Keys remain in the caller's memory. The outbox stores no CEK or private signing/KEM key. Its immutable binding contains application, tenant, collection, document, epoch, manifest and author. It rejects changes to any of those on reopen, before requesting entropy. Selecting the correct authenticated CEK and signer remains the caller's responsibility. Epoch migration, revoked-author handling and authorized resealing are not implemented.

## Durable states and operations

| State/action | Transaction and observable behavior | Failure/retry |
| --- | --- | --- |
| Enqueue | Persist batch ID, original writer and prepared plaintext together. | Before commit, nothing is accepted. After commit, the same batch/payload is idempotent; changed bytes conflict. Caller retains the batch ID and payload until acceptance is known. |
| Reserve | Select the oldest unsealed intent, read its writer's committed head, abandon uncertain attempts, insert a fresh random ID plus canonical header. | Transaction commits before crypto. RNG failure or a known ID collision returns an error without invoking the sealer. |
| Seal | Invoke trusted callback once using the reserved header. | Any error, panic or abrupt exit leaves an uncertain reservation. The next attempt uses a different ID; existing IDs remain reserved in history. |
| Store | In one transaction, insert complete signed record bytes, mark the attempt sealed and advance the original writer head to SHA-384 of those exact bytes. | Statement/commit failure leaves no uploadable row or advanced head. Lost local acknowledgement after commit returns original bytes on retry. |
| Upload retry | `pending()` returns immutable stored records without deleting them. | Lost remote ACK produces the same record ID, ciphertext and randomized signature. The harness simulates duplicate receipt; no service exists here. |
| ACK | Mark the known record acknowledged. | Unknown IDs fail; duplicate ACKs are idempotent. The original record remains retrievable after ACK or restart. |

The schema separates `intents`, `attempts`, `sealed`, `writers` and the format/binding/active-owner metadata. SHA-384 chain heads refer to full signed records. The queue seals in global intent order within one bound document; this intentionally avoids concurrent pending predecessor selection. An existing sealed batch can be retrieved regardless of later pending batches.

The caller must atomically integrate prepared CRDT updates with its domain journal in a later step. This experiment persists already-prepared payloads; it does not commit task state and outbox intent together, validate arbitrary Loro actor ownership, or construct domain commands. Snapshot sealing is unsupported here: new headers are always `data`.

## Reopen, contention and clone behavior

Every successful writable open persists a fresh random 32-byte incarnation and a random 64-bit engine actor, then replaces the active owner. Duplicate local actors/incarnations fail the whole open transaction. Every mutating operation verifies that owner inside its transaction. If another open occurs while crypto is running, the older handle cannot commit the result. Its consumed reservation and original intent remain available to the new owner.

SQLite write contention is returned immediately (`busy_timeout=0`); there is no internal retry loop or timeout-based takeover. A real reader-held lock is used to make the final outbox COMMIT fail in the tests. After the lock is released, the next seal uses a fresh ID and the unchanged committed predecessor. Owner checks and SQLite transactions cover cooperative handles using this component, not arbitrary SQL writers or hostile modification of the local database.

Retained intents keep their original writer, engine changes and logical batch. New edits use the fresh writer/actor. Clone tests use SQLite's [VACUUM INTO](https://www.sqlite.org/lang_vacuum.html) to obtain a consistent live snapshot; copying only an active database file is not a supported backup procedure. Existing ciphertext is identical after restore; uncertain reservations are abandoned. Separately resumed copies use independent OS entropy and local collision checks. This is probabilistic separation between isolated copies, not coordination or global uniqueness.

**Preservation is not remote admission.** Retained old-writer records may be stale once a replacement writer is admitted. Two isolated copies resealing the same unfinished old-writer intent can create distinct signed successors; later receive-side fork/frontier rules must preserve and reconcile that ambiguity. This experiment does not authorize both histories or claim transparent concurrent-clone synchronization. Those rules remain under #36/#37/#40.

## Evidence and limits

The [tests](../experiments/sync-prototype/tests/cases/encrypted_outbox.rs) exercise:

- A real CEK envelope, admitted writer and encrypted Loro update, followed by exact-byte retrieval after restart without calling RNG or crypto again.
- Ordered updates, full-record hash chaining and rollback of head changes when ciphertext storage fails.
- Same-process uncertain retries, forced record/actor/incarnation collisions, entropy failure and mismatched route/epoch/authority bindings.
- A real SQL trigger rejection, a real COMMIT-busy failure, and a new owner arriving during an old owner's encryption callback.
- Simulated lost upload ACKs, idempotent receipt bookkeeping, retained records after ACK and consistent database cloning.
- Abrupt child-process exits before/after intent commit, before/after reservation commit, after crypto, before/after outbox commit and before/after ACK commit: nine child executions with exact exit-code and recovered-state assertions. The ignored child helper runs through its parent test.
- Oversize input, changed sealer headers and explicit retained-history capacity limits.

The budgets are 16 intents, 64 reservation attempts and 16 writable opens per database, with the existing 1 MiB plaintext cap. Sixteen intents allow up to 16 MiB of retained plaintext plus at most 16 record payloads and framing; SQLite pages, journals and temporary allocations are additional. Four attempts per intent on average and sixteen sessions keep the fault experiment finite while retaining every used ID. These are chosen experiment capacities, not production quotas or performance measurements. Exhaustion preserves history and fails closed; there is no GC, reset or automatic growth policy.

The [active execution record](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) binds final source, documentation, fixtures and checks. CI checks those hashes, independent wire/symmetric-crypto fixtures, Python oracle guards, formatting, Clippy and the full Rust suite. Tests use temporary local databases, including RAM-backed temporary storage on the recorded host. Abrupt process termination is tested; power loss, media failure, arbitrary storage corruption, OS/RNG snapshots and host-loss recovery are not. Synthetic fixed credentials appear only in the crash harness so parent and child can use the same test authority without serializing secrets.

Pending plaintext is intentionally retained locally; SQLite, journals and backups are **not encrypted at rest** by this component. Secret storage, zeroization, durable membership/actor authority, full authenticated receive chains, epoch transitions, historical/snapshot admission, task-domain integration, network/service durability, exact-suite independent interoperability, cross-target execution and independent security review remain open.
