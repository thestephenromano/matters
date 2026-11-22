# Durable receive

Status: implemented in `matters_core::storage::receive` (#39), with the receiver obligations of the [record contract](SYNC_RECORD_CONTRACT.md) and the bounds set after the internal review (#45). Checkpoint parts arriving through it are described in [checkpoints](SYNC_CHECKPOINTS.md); writer handoffs and closing prefixes in [writer lifecycle](SYNC_WRITER_LIFECYCLE.md); membership, epochs and revocation in [key distribution](SYNC_KEY_DISTRIBUTION.md).

## Boundary

A sync client gives `Database::receive_records` one page of records from the relay's log. The page is received in one SQLite transaction, and the application's rows are rebuilt once, at its end (#241). The client saves the page's cursor only after that transaction commits.

Each record has exactly one outcome, and it is final for that delivery:

| Outcome | Meaning |
| --- | --- |
| `Applied` | Merged into the engine, journalled, and recorded with its predecessor, chain position and digest. |
| `Duplicate` | These exact bytes are held already: applied, met again past a checkpoint, or this device's own. |
| `Pending(reason)` | Kept in `sync_pending`, tried again only when what it waits for arrives (below). |
| `Checkpoint(..)` | A checkpoint part, and what became of its checkpoint. |
| `Err(record refused: …)` | Refused. The record's ID, digest and reason are quarantined; the rest of the page goes on. |

The outer `Err` of `receive_records` is a failure of the store itself: an SQLite or I/O error. The page is rolled back, the engine is rebuilt from the journal, and the client fetches the page again. Nothing a member or the relay can put in a record produces it. A record refused only when a parked copy of it is retried, a checkpoint part that does not parse, and an update the document cannot be projected with are each that record's refusal, not the page's failure.

## The checks, in order

1. The record parses for this collection's document.
2. Its record ID and full-record digest (SHA-384). The same bytes held already are a duplicate. Other bytes under a held record ID are authenticated and refused, and kept as evidence if a member signed them.
3. A kept record that is already authenticated and still out of its chain's order waits on, with no cryptography.
4. The key of the epoch it was sealed in. Without a usable envelope of it, the record's signature is checked against the roster of the manifest it names (`Authority::roster_at`), if this device knows that manifest, and the record waits for its key, authenticated. If the manifest is unknown, the record is kept unauthenticated, within the budget below.
5. Its signature, its author's membership in that epoch and its writer's admission (`Authority::authenticate_at`, `authenticate_snapshot_at`). A record a removed author signed outside every signed closing prefix waits as `revoked`. Every other refusal there is final.
6. Its writer's lineage. If it is frozen here on evidence, the record waits as `frozen`. If the chain was closed, by a handoff or an admin's closing frontier, at a head this device already holds (or closed empty), it waits as `after-close`. A second signed successor of a record this device holds freezes the lineage (below).
7. Its place in its writer's chain: its predecessor must be the chain's head here, or it waits for it.
8. Its engine update must carry only its writer's admitted actor, and the engine must already hold every operation the update depends on, read from the update's metadata without importing it. Otherwise it waits for them.

The record then applies.

## Pending records

`sync_pending` is keyed by record ID and digest, so bytes under a genuine record's ID never displace it. It is indexed by what each row waits for:

| Reason | Waits for | Tried again when |
| --- | --- | --- |
| `key` | this device's envelope of its epoch's key, or the manifest it names | a drain finds the key, or, for an unauthenticated row, the manifest's roster |
| `writer` | its writer's admission | a drain finds the writer admitted |
| `predecessor` | the record before it | that record applies, or a checkpoint sets the chain's head to it |
| `dependencies` | one engine operation, `(actor, counter)` | a record of that actor applies and the engine then holds the operation |
| `revoked` | a transition naming its writer's closing prefix | the membership's manifest has changed since it was last tried |
| `after-close`, `frozen` | nothing | never; kept as evidence |

A kept row is tried again only when what it waits for has arrived. Each record is authenticated when it arrives and at most once more when it can apply. The drain that runs after each page, control and checkpoint start checks the other rows by SQL and in memory only, with no cryptography. Each manifest's envelope is unwrapped once a page. A reversed chain of 2,002 records applies with at most two authentications per record (test below).

Records a checkpoint carried, and rows kept before schema v19, are received again once, as if fresh, by the next drain. Nothing is applied, or carried on in a checkpoint, without being authenticated on this device.

**Budgets.** Each is a chosen bound, and the oldest rows are evicted first:
- records kept without authentication (their manifest unknown): 64 rows and 8 MiB in all (`UNAUTHENTICATED_ROWS`, `UNAUTHENTICATED_BYTES`);
- records kept as revoked: 256 rows and 16 MiB per removed author (`REVOKED_ROWS_PER_AUTHOR`, `REVOKED_BYTES_PER_AUTHOR`), since a removed device can sign without limit;
- refused records remembered in `sync_quarantine`: 1,024 rows, unsigned ones forgotten first, with up to 32 MiB of signed bytes kept as evidence; beyond that the oldest keep their reason without their bytes (`QUARANTINE_ROWS`, `QUARANTINE_BYTES`).

Authenticated waits for a key, a writer, a predecessor or a dependency are not budgeted here. Each is signed by a member of its epoch, and the relay's quota bounds what members store.

## Applying, and the projection

An update applies only when the engine holds every operation it depends on, so the engine never holds operations aside for missing dependencies (Loro's pending state). If the engine still holds an update aside after importing it, the record is refused and the engine is rebuilt without it. A device whose engine holds operations aside makes no checkpoint (`Document::has_pending`).

The page's updates are merged unindexed. At the page's end the document is reindexed and projected once. If that fails, the page is rolled back and received again with each update's effect on the document checked on its own. The update that leaves the document unreadable is refused, so the engine and the rows stay in step. That second pass costs one projection per applied record of the page. A document that cannot be projected before the page is reported as the page's failure.

Engine work on member-authored bytes runs under `catch_unwind`: decoding an update's metadata, importing it, reading a snapshot's version, reindexing and projecting. A panic there refuses the record, and the engine is rebuilt from the journal, keeping its engine actor. A panic elsewhere, while the database's locks are held, poisons them. The next call takes them back: it rolls back any transaction the panicking call left open, clears `query_only`, and rebuilds the engine from the journal. The sealing state is kept as last set; a membership the panicking call had taken out reads as sync not resumed until the next resume.

## Forks, closing heads and duplicates

`sync_received` keeps each applied record's predecessor and chain position (schema v19), and `sync_covered` the same for records met again past a checkpoint. A record whose predecessor this device holds, while another record already follows that predecessor, is a second signed successor. The author's writer lineage is then frozen here (`sync_frozen`, with the contradicting record's bytes and the held record's digest), and that author's later records, of any incarnation, wait as `frozen`. A root record competing with a held root is treated the same. What was applied before the fork became visible stays applied; nothing is rolled back.

A writer whose chain was closed at head `c`, by its own handoff or by an admin's closing frontier, applies nothing new once this device holds `c`: as its chain head, among its applied records, or as a checkpoint's covered head. A chain closed empty applies nothing at all. Such records wait as `after-close`. A removed device may have signed on before it learnt of its removal, so this alone freezes nothing. A branch that forked before `c` and applied here first freezes the lineage when the closed chain's own record at that position arrives.

Duplicates are recognised by digest, not by record ID alone. Other bytes under a record ID this device holds are refused. If a member signed them, they are kept in quarantine as evidence.

## Evidence

`crates/matters-core/tests/receive_tests.rs` covers two devices converging, out-of-order chains across a restart, waiting for an epoch's key and for a writer's claim, forgeries, foreign actors, snapshots in place of updates, records after a signed close, cloned devices, a page with its own outcome per record, and a crash before a page commits.

`crates/matters-core/tests/receive_review_tests.rs` covers:
- a page with a poisoned checkpoint part, a bad successor delivered before its predecessor, and an update that breaks the projection: each applies the page's other records, and later pages go on;
- a store failure aborting the page and refusing nothing;
- 10,000 forged records naming an unknown manifest keeping at most 64 rows, none of which travels in a checkpoint;
- junk under a genuine record's ID not displacing it;
- a forged record under the next epoch's manifest, served before the rotation, refused when the rotation arrives, with the device receiving on;
- a record naming a known manifest checked against its roster before it waits for its key;
- a record whose dependency is withheld (three devices) waiting, carried pending by a checkpoint, and applying once the dependency arrives;
- a second signed successor, and a branch met by a closed chain, freezing the lineage;
- 2,002 records delivered in reverse applying with at most two authentications each;
- checkpoint grouping, caps, supersession and unverifiable coverage ([checkpoints](SYNC_CHECKPOINTS.md));
- other bytes under an applied record's ID kept as evidence;
- a panic while every lock is held leaving the database working, with its writer's actor;
- a removed device's records kept as revoked within their budget.

`crates/matters-core/tests/revocation_tests.rs` covers records within and past an admin's closing frontier.

The `receive` fuzz target drives a joined database through fuzzed sequences of records and controls. It seals some as an admitted member's writer around fuzzed plaintext or a fuzzed engine edit, so it reaches the checks after authentication. It asserts that receiving never fails as a store, that a refusal is the only error, that the rows rebuild from the engine, and that a local write still works. The `engine_import` target gives fuzzed bytes to every engine decode and import above. Both are in `fuzz/`, and their corpora are replayed on stable by `crates/matters-core/tests/fuzz_replay_tests.rs`. libFuzzer aborts on any panic, so it reports an engine panic that the receiver itself would catch.

## Limits

- **Unauthenticated waits.** A genuine record that names a manifest this device does not know yet is kept within the unauthenticated budget. A flood of forged records can evict it first. The relay's log still holds it, but this device reads it again only after its cursor is reset (a relay restore). The client pulls the authority log before records, so this needs a rotation between the two pulls.
- **Revoked retries** run when the membership's manifest changes, which is when a closing frontier can arrive.
- **The checkpoint region.** In a chain this device knows only by a checkpoint's length and head, a fork is not detected, and a record naming a predecessor inside it waits until a relay restore brings the covered records again.
- **Rows from before v19** have no predecessor or position, and take no part in fork detection or in the position check of checkpoints.
- **Fork evidence** keeps the contradicting record's signed bytes, and only the digest of the record already applied: signed bytes are not retained once applied. Recovering a frozen lineage is not implemented.
- **Replacement writers.** A handoff successor's records apply without waiting for the closed chain's head to be applied here. [Writer lifecycle](SYNC_WRITER_LIFECYCLE.md) describes that gate for the prototype.
- **Engine resources.** Decompression and memory during a Loro import are not bounded by the engine; see [checkpoints](SYNC_CHECKPOINTS.md#limits).

## The earlier experiment

`experiments/sync-prototype/tests/support/encrypted_inbox.rs` was the design experiment for this receiver: a fixed-epoch, 32-delivery transactional inbox with causal-past domain verdicts, and its own evidence record. Its tests and evidence describe that experiment, not this implementation.
