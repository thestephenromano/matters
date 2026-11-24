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
6. Its writer's lineage. If it is frozen here on evidence, the record waits as `frozen`, unless an admin's closing frontier has recovered the writer and the record lies on its closed prefix (below). If the chain was closed, by a handoff or an admin's closing frontier, at a head this device already holds (or closed empty), it waits as `after-close`. A second signed successor of a record this device holds freezes the lineage (below).
7. Its place in its writer's chain: its predecessor must be the chain's head here, or it waits for it. The first record of a replacement writer also waits, as `handoff`, until this device holds the closing head of the writer it replaced (below).
8. The engine checker must have decoded its engine update within its budget, in a child process (below, "Engine resources"); otherwise it is refused. The update must carry only its writer's admitted actor, and the engine must already hold every operation the update depends on, read from the update's metadata without importing it. Otherwise it waits for them.

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
| `frozen` | an admin's closing frontier for its writer | the membership's manifest has changed since it was last tried, or the walk of a recovered prefix reaches it |
| `handoff` | the closing head of an earlier writer of its lineage, `(incarnation, head)` | that head applies |
| `after-close` | nothing | never; kept as evidence |

A kept row is tried again only when what it waits for has arrived. Each record is authenticated when it arrives and at most once more when it can apply. The drain that runs after each page, control and checkpoint start checks the other rows by SQL and in memory only, with no cryptography. Each manifest's envelope is unwrapped once a page. A reversed chain of 2,002 records applies with at most two authentications per record (test below).

Records a checkpoint carried, and rows kept before schema v19, are received again once, as if fresh, by the next drain. Nothing is applied, or carried on in a checkpoint, without being authenticated on this device.

**Budgets.** Each is a chosen bound, and the oldest rows are evicted first:
- records kept without authentication (their manifest unknown): 64 rows and 8 MiB in all (`UNAUTHENTICATED_ROWS`, `UNAUTHENTICATED_BYTES`). An evicted record is read again from the relay (below);
- records kept as revoked: 256 rows and 16 MiB per removed author (`REVOKED_ROWS_PER_AUTHOR`, `REVOKED_BYTES_PER_AUTHOR`), since a removed device can sign without limit;
- refused records remembered in `sync_quarantine`: 1,024 rows, unsigned ones forgotten first, with up to 32 MiB of signed bytes kept as evidence; beyond that the oldest keep their reason without their bytes (`QUARANTINE_ROWS`, `QUARANTINE_BYTES`).

Authenticated waits for a key, a writer, a predecessor or a dependency are not budgeted here. Each is signed by a member of its epoch, and the relay's quota bounds what members store.

**Reading an evicted record again.** A record kept without authentication may be genuine: it names a manifest that a transition this device has not read yet will bring. So eviction is a re-read, not a loss. The client hands the receiver each page with its storage generation and each record's sequence in the relay's log (`LocalStore::receive_page`), and a kept row remembers where it was read (`sync_pending.origin`, schema v21). When rows are evicted, the earliest place among them is kept in `sync_rewind`, with the manifest current then; later evictions in the same generation keep the earlier place and the first manifest. Before it reads the record log, the client asks the receiver to rewind (`LocalStore::rewind_records`). Once the manifest has changed since that first eviction, the receiver moves the records cursor back to just before the remembered place, in the same generation and never forward, and forgets the place. The page read again holds the evicted records. A record its new manifest authenticates is kept authenticated or applies; records already held are duplicates, recognised by digest. A forgery is let go of again, and the place it leaves waits for the next change of manifest. So a sustained flood costs at most one re-read for each membership change. A generation change forgets the place and the rows' origins, since the cursor starts over anyway.

## Applying, and the projection

An update applies only when the engine holds every operation it depends on, so the engine never holds operations aside for missing dependencies (Loro's pending state). If the engine still holds an update aside after importing it, the record is refused and the engine is rebuilt without it. A device whose engine holds operations aside makes no checkpoint (`Document::has_pending`).

The page's updates are merged unindexed. At the page's end the document is reindexed and projected once. If that fails, the page is rolled back and received again with each update's effect on the document checked on its own. The update that leaves the document unreadable is refused, so the engine and the rows stay in step. That second pass costs one projection per applied record of the page. A document that cannot be projected before the page is reported as the page's failure.

Engine work on member-authored bytes runs under `catch_unwind`: decoding an update's metadata, importing it, reading a snapshot's version, reindexing and projecting. A panic there refuses the record, and the engine is rebuilt from the journal, keeping its engine actor. An engine that panicked can hold poisoned locks, on which Loro's own drop panics again, so a document's engine is dropped under `catch_unwind` too (`Document`'s `Drop`). A panic elsewhere, while the database's locks are held, poisons them. The next call takes them back: it rolls back any transaction the panicking call left open, clears `query_only`, and rebuilds the engine from the journal. The sealing state is kept as last set; a membership the panicking call had taken out reads as sync not resumed until the next resume.

## Engine resources

Loro bounds neither the memory nor the processor time of a decode, and no input cap supplies a bound: a member-signed update under 100 bytes can decode to gigabytes ([checkpoints](SYNC_CHECKPOINTS.md#limits) gives the measurements). A bound cannot be kept inside the process, since a failed allocation aborts it on stable Rust, and an out-of-memory kill records nothing: the device would meet the same record on every start.

So nothing member-authored is decoded in the app before the **engine checker** has decoded it (`crdt::isolation`, #45). Matters' binaries (`matters-ui`, `matters-syncd`) install one at startup that runs the binary itself (`/proc/self/exe`) again in check mode: `main` enters it first, before Qt or the async runtime starts, so check mode never starts the interface. The child limits its own address space (`RLIMIT_AS`, on top of what it already uses) and processor time (`RLIMIT_CPU`) for each blob, writes no core file, follows the app if it dies, and is first in line for the out-of-memory killer; the app kills it at a wall-clock deadline. For an update the child decodes its metadata (as the actor and dependency checks do) and imports it into a scratch document (as merging does). It reports each blob as it finishes, so when one stops it, the blobs before it have passed and the culprit is known: the rest go on in a new child.

- **One child for a page.** Before the first record's turn, each fresh data record of the page is authenticated and opened once, keeping what that found for its turn, and the checker decodes all their updates in one child. A record kept waiting meets its verdict again when it applies. Verdicts are kept by the hash of the bytes for the life of the process (the latest 4,096), so a refused record delivered again is refused without a child, and records kept waiting across pages need none.
- **A verdict is final.** An update that ran out of memory or processor time, outlived its deadline or stopped the decoder is refused and quarantined, with the reason "the engine could not decode it within its budget" and the cause. The rest of the page goes on.
- **No verdict is no refusal.** A child that cannot be started, that does not greet in check mode, that cannot limit itself, or that something else kills (the system's out-of-memory killer, say) judges nothing: the page fails as a store failure, refusing nothing, and is received again later. With no checker installed, receiving fails the same way, so a binary that installs none cannot decode untrusted bytes without limits. Only builds with the `test-support` feature, which no application enables, decode in-process when none is installed.

**The update budget** is 256 MiB of address space and 10 s of processor time, never more than half the machine's memory; the wall clock allows 20 s to start, and each update 20 s beyond twice its processor time. An update is at most 1 MiB (`MAX_PLAINTEXT_BYTES`). The largest honest updates, measured on one shared development host ([evidence record](evidence/sync-engine-budget-2026-11-23.json): i9-10920X, release build, one run each), took:

| One commit of 1 MiB | Address space | Processor time |
| --- | ---: | ---: |
| creating tasks, titles only | 32.0 MiB | 0.20 s |
| creating tasks and moving each | 32.5 MiB | 0.21 s |
| renaming one task again and again | 48.5 MiB | 0.42 s |
| one task's notes | 2.0 MiB | 0.00 s |

The budget allows the most of these 5.3 times its memory and 24 times its time. The hostile update above reaches the memory budget at about 2×10^6 ops, in about a second.

## Forks, closing heads and duplicates

`sync_received` keeps each applied record's predecessor and chain position (schema v19), and `sync_covered` the same for records met again past a checkpoint. A record whose predecessor this device holds, while another record already follows that predecessor, is a second signed successor. The author's writer lineage is then frozen here (`sync_frozen`, with the contradicting record's bytes and the held record's digest), and that author's later records, of any incarnation, wait as `frozen`. A root record competing with a held root is treated the same. What was applied before the fork became visible stays applied; nothing is rolled back.

A writer whose chain was closed at head `c`, by its own handoff or by an admin's closing frontier, applies nothing new once this device holds `c`: as its chain head, among its applied records, or as a checkpoint's covered head. A chain closed empty applies nothing at all. Such records wait as `after-close`. A removed device may have signed on before it learnt of its removal, so this alone freezes nothing. A branch that forked before `c` and applied here first freezes the lineage when the closed chain's own record at that position arrives.

Duplicates are recognised by digest, not by record ID alone. Other bytes under a record ID this device holds are refused. If a member signed them, they are kept in quarantine as evidence.

## Recovering a frozen lineage

A frozen lineage is recovered by an admin's decision, through the closing frontier of [revocation](SYNC_KEY_DISTRIBUTION.md#revocation). The admin removes the device. The transition closes each of its writers at the head of the chain the admin holds, so an admin closes only what it has seen. The removed device can be invited again as a new member, under new keys.

A frozen record waits tagged with the manifest it was last tried under, so a membership change tries it again, whole. Its writer is recovered when a signed closing prefix ends it at a head `c` and its device is no longer a member: a frontier entry, or a handoff its writer signed whose chain of handoffs leads to a writer a frontier names. A member's own handoff decides nothing about its fork. For a lineage frozen on competing handoffs, the authority admits a frozen device's record only when its writer is admitted here within such a prefix (`authenticate_at`); otherwise it refuses it as before.

The receiver then walks the chain back from `c`, by hash, through the records it keeps waiting:
- If the walk reaches this device's head of the chain (or the chain's start, when it holds none of it), the records met are the closed prefix after what it applied. Each applies in its chain's order. Every other record of the writer waits as `after-close`, and nothing past `c` applies.
- If the walk reaches another record this device holds, the closed prefix does not lead on from what it applied: it applied another branch first. Nothing more of the writer applies, and its records wait as `after-close`. What was applied stays.
- If the walk reaches a record not kept here yet, the records it met wait for their predecessors, and the others stay frozen. When the missing record arrives, the walk goes on through it, and each frozen record it meets is tried again.

Within a page, the walk is made once for each writer and goes on from where it stopped. The fork evidence in `sync_frozen` is kept.

## Replacement writers

A writer's first record waits, as `handoff`, until this device holds the closing head of the writer it replaced: applied, or a checkpoint's covered head. A writer closed empty is passed over, so an empty intermediate writer does not bypass an earlier nonempty closing head. Only the nearest nonempty closing head is checked: the earlier ones were waited for when that head's own chain began. The wait applies whether or not the new content depends on the old in the engine, and it is drained when the head applies. If a record of the closed prefix is refused, or frozen, the head never applies, and the replacement's content waits for good. A legacy replacement (`Boundary::Unbounded`) signed no head, and nothing waits for it.

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

`crates/matters-core/tests/crdt_tests.rs` covers a 90-byte update on which Loro asserts being refused, and the document it poisoned dropping without a panic.

`crates/matters-core/tests/revocation_tests.rs` covers records within and past an admin's closing frontier.

`crates/matters-core/tests/engine_isolation.rs` is its own engine checker: its `main` enters check mode first, as Matters' binaries do. Against a real child, with deliberately low budgets, it covers:
- a hostile update (one change whose op columns hold 10^6 zero-length map deletions, under 100 bytes) refused for its memory, while honest updates before and after it in the same child pass;
- the running binary, with the default budgets, passing an honest update of 2,000 created tasks and refusing the hostile one;
- processor time and the wall-clock deadline as budgets, and a blob's reading kept out of its budget;
- the child's own limits, read from `/proc`: no core file, first for the out-of-memory killer, and each blob's address space and processor time;
- a child that does not greet, is killed from outside, cannot limit itself, or is no program, judging nothing; one that quits early refusing its blob;
- a page holding the hostile update: refused and quarantined, the page's genuine record applied, its updates checked in one child, a second delivery refused on the verdict kept, and the database reopening on what it took;
- records met twice in a page, or held already, not authenticated ahead;
- records kept waiting applying on the verdicts reached when they arrived;
- the same hostile update, with no isolation installed, waiting instead (what the tests above rely on);
- the hostile update handed to `receive_update`, not taken;
- a snapshot past its budget refused as a checkpoint ([checkpoints](SYNC_CHECKPOINTS.md)).

`crates/matters-core/tests/receive_limits_tests.rs` covers:
- a genuine record a flood evicted, read again after its rotation arrives, once for each change of manifest;
- the rewind kept to the earliest place of a generation, never moving the cursor forward or into another generation, and forgotten on a generation change;
- a frozen lineage recovered by an admin's removal: the closed prefix applies, the fork and another branch wait `after-close`, a member's own handoff recovers nothing, and the device rejoins as a new member;
- a closed prefix that does not lead on from what was applied, applying nothing;
- a recovered chain received out of order, applying once its gap fills;
- a lineage frozen on competing handoffs, recovered through the handoff both devices admitted;
- a replacement's first record waiting for the closing head, not tried again until it applies; an empty intermediate writer not bypassing it; a refused record in the closed prefix blocking it for good.

`crates/matters-sync-client/tests/hostile_relay_tests.rs` covers the client rewinding: a record served before its rotation and pushed out by a flood applies in the round after the rotation, and the next round reads on from the end.

The `receive` fuzz target drives a joined database through fuzzed sequences of records and controls. It seals some as an admitted member's writer around fuzzed plaintext or a fuzzed engine edit, so it reaches the checks after authentication. It asserts that receiving never fails as a store, that a refusal is the only error, that the rows rebuild from the engine, and that a local write still works. The `engine_import` target gives fuzzed bytes to every engine decode and import above. Both are in `fuzz/`, and their corpora are replayed on stable by `crates/matters-core/tests/fuzz_replay_tests.rs`. libFuzzer aborts on any panic, so it reports an engine panic that the receiver itself would catch.

## Limits

- **Unauthenticated waits.** A genuine record that names a manifest this device does not know yet is kept within the unauthenticated budget, and a flood of forged records can evict it. It is read again from the relay once the manifest changes. A flood that goes on past that change evicts it again if its manifest is still unknown, and the next re-read waits for the next change. Only records received with their place in the log are remembered: an eviction while a checkpoint's carried records are received again remembers nothing. A rewind reads again everything after the evicted record.
- **Revoked retries** run when the membership's manifest changes, which is when a closing frontier can arrive.
- **The checkpoint region.** In a chain this device knows only by a checkpoint's length and head, a fork is not detected, and a record naming a predecessor inside it waits until a relay restore brings the covered records again.
- **Rows from before v19** have no predecessor or position, and take no part in fork detection or in the position check of checkpoints.
- **Fork evidence** keeps the contradicting record's signed bytes, and only the digest of the record already applied: signed bytes are not retained once applied.
- **Recovering a frozen lineage** needs the admin to remove the device. A device that applied another branch before the fork became visible keeps it, and applies nothing more of that writer. Of a lineage frozen on competing handoffs, only writers this device admitted are recovered: the records of a writer whose claim it refused stay frozen. A record whose place was unknown when it was tried stays frozen until the next membership change, unless the walk reaches it first.
- **Replacement writers.** A refused or frozen record in a closed prefix blocks the replacement's content for good, as in the prototype.
- **Engine resources.** The engine checker bounds a member's update only within its budget (above): a hostile update may take 256 MiB and 10 s of processor time in the child, once, before it is refused. An update that passed is decoded again in the app, which takes about what it took in the child, so the app's own decoding of a page is bounded by the page's updates' budgets. What the child cannot reproduce is merging: it imports the update into an empty document, where its operations wait for their dependencies, while the app applies them to its state. So the state that applying an update's operations builds is not bounded by the checker, only their decoding is. The checker runs while the database is locked, so a page holds other writes for as long as its child runs: a debug build of `matters-ui` started in check mode in 0.8 s and checked a page of 256 small honest updates in 0.9 s, on the host above. Linux only: the limits are `RLIMIT_AS` and `RLIMIT_CPU`, and the binary is `/proc/self/exe`.

## The earlier experiment

`experiments/sync-prototype/tests/support/encrypted_inbox.rs` was the design experiment for this receiver: a fixed-epoch, 32-delivery transactional inbox with causal-past domain verdicts, and its own evidence record. Its tests and evidence describe that experiment, not this implementation.
