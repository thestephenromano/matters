# Checkpoints

Status: design for [#40](https://github.com/thestephenromano/matters/issues/40), delivered in steps.
- The encoding, assembly and comparison rules are implemented in `matters_sync::checkpoint`.
- Creating, receiving, judging and bootstrapping from checkpoints are implemented in `matters_core::storage::checkpoint`.
- The relay's snapshot index and the client's use of it follow.

A device with history never imports a checkpoint. Only a device that holds nothing yet imports one, through `Database::start_from_checkpoint`, after the checks below.

## Adopted components

| Need | Adopted | Why |
| --- | --- | --- |
| The snapshot | Loro's full-history snapshot (`ExportMode::Snapshot`), the engine already adopted (ADR 0001) | It holds the whole operation log as well as the state, so later updates from any writer still merge. [Loro export modes](https://loro.dev/docs/tutorial/encoding) |
| Carrier, sealing, signatures | The record contract's `snapshot` kind, sealed under the current epoch with the author's writer key | Already specified with its own signature context. No second cipher or framing. |
| Encoding | Deterministic CBOR through minicbor, as for every other sync object | One canonical encoding, checked by re-encoding. |
| Relay storage | The existing record log, plus one index of snapshot records (`GET …/snapshots`, SYNC_SERVICE_CONTRACT.md) | The relay can already read a record's kind. It never sees inside. |

Shallow snapshots (`ExportMode::ShallowSnapshot`) are not used. They drop history that an old device's edits may still depend on (SYNC_DESIGN.md, "Checkpoints and retention").

## What a checkpoint is

A checkpoint is a snapshot of the collection's document together with a **coverage statement**, cut into parts of one `snapshot` record each (`sync-checkpoint-v0.cddl`).

The coverage statement names:
- the engine format and domain schema;
- the epoch;
- every writer chain the snapshot includes, as its author, incarnation, number of records and head record hash;
- the engine's version vector and frontier;
- the record hashes of pending records the checkpoint carries;
- the author's relay cursor;
- the payload's SHA-384 and length.

The checkpoint's ID is the SHA-384 of the canonical statement.

The payload is the snapshot followed by the pending records' original bytes. These are records the author has received but could not apply yet, because a dependency is missing or they wait for a key or an admission. Loro's snapshots leave out imports that are still pending, so the checkpoint carries them itself. [Loro persistence](https://loro.dev/docs/tutorial/persistence)

**Snapshot records stand outside the data chains.** Their `previous-record-hash` is null and no data record names one as its predecessor. A receiver dispatches them by kind before any chain check. A device can therefore publish a checkpoint at any time without changing what any writer's next data record must name.

**One encoding.** Chunks are exactly 512 KiB except the last. Writers are ascending by incarnation and pending records by hash. Every part names the checkpoint's ID, and only part 0 carries the statement.

## When a device makes one

Only a fully synced member makes a checkpoint, in one SQLite transaction. "Fully synced" means:
- every record it wrote is acknowledged by the relay;
- its last pull reached the relay's high-water mark;
- nothing has been written locally since.

Every record at or before its cursor's sequence is then either in the snapshot or carried pending. Everything after the cursor is its tail. This is what lets a fresh device start reading at the cursor.

It carries only pending records whose signature it checked (`sync_pending.authenticated`); records kept unauthenticated never travel. It makes none, and returns `None`, while:
- its engine holds operations aside for missing dependencies, which no snapshot's version would show;
- more authenticated records wait than a checkpoint carries (`MAX_PENDING`, 1,024);
- another open of the same database has replaced its writer (`sealing::fence`, an error).

The parts are kept, and resent byte for byte, until the relay acknowledges them, as data records are. A restart during creation leaves no parts, or all of them, because the transaction commits as a whole. A restart during publication resends the same bytes. Records written concurrently are simply later than the cursor and arrive through the log as usual. Nothing already acknowledged is ever removed.

## Receiving one

A receiver authenticates each part as it does a data record:
- the author is a writer admitted under that incarnation;
- the signature verifies;
- the epoch key opens it.

It then keeps the part until all parts are present.

**Parts are grouped by checkpoint and author.** One member's parts never spoil another member's parts of the same checkpoint. An author's parts are held to one checkpoint's worth (`MAX_PARTS`, 512); more refuses the part and drops the author's parts of that checkpoint. A part of an author's newer checkpoint drops that author's parts of earlier, undecided ones, and a refused checkpoint's parts are kept until then, as evidence. A checkpoint is assembled once, when its last part arrives (part 0 states how many there are).

`assemble` refuses:
- parts that disagree on the checkpoint, the writer, the epoch or the chunking (`Inconsistent`);
- a statement whose epoch is not the records' (`Epoch`);
- a payload that does not hash to the statement, or that carries other pending records than it lists (`Digest`).

Those refusals are the author's sealing, not the checkpoint's content: the parts' records are quarantined and nothing is recorded under the checkpoint's ID.

A device with history then checks the statement against what it holds:
1. **Each listed writer** must be admitted here, under its listed author. A writer not admitted yet makes the checkpoint wait, before its snapshot is imported. The wait is recorded (`sync_checkpoint_waits`), and the checkpoint is judged again only once that writer is admitted.
2. **The snapshot.** Imported into an empty engine, it must have exactly the stated version vector and frontier, and every actor in the version must be a listed writer's.
3. **Each listed chain.** Where the device holds at least `records` records of that chain, the record at position `records` must be `head` (positions are kept since schema v19). A different head is evidence of a fork, and the checkpoint is refused.
4. **Each writer's counter** must agree with the chain lengths: equal where the device holds the chain as far, lower where it holds more, higher where it holds less.
5. **The previous checkpoint.** The statement must cover at least the latest checkpoint the device accepted and verified (`Coverage::follows`): the same or a later epoch, every writer with at least as many records, the same head where the counts are equal, and no earlier cursor in the same relay generation. A relay that offers an older checkpoint instead of a newer one gets it superseded (`Behind`), and one that contradicts it is refused.

A checkpoint that lists some chain further than this device holds it passes checks 3 and 4 on claims nothing here can refute. It is accepted **unverified**: it never becomes the latest verified checkpoint, so it never supersedes or refutes a checkpoint this device can verify. `latest_checkpoint` reports the latest verified checkpoint, or, while there is none, the latest accepted.

A device that already holds everything a checkpoint covers records it as accepted and imports nothing. Its history is a superset.

Refused, superseded and unverified checkpoints are kept, newest first, up to `KEEP_DECIDED` (32), and as many verified accepted ones besides; older ones are pruned.

## Bootstrapping a fresh device

A device that has just joined asks the relay for the newest snapshot records (`GET …/snapshots`). It assembles the newest complete checkpoint that passes the checks it can make, then commits in one transaction:
- the imported snapshot and its projection;
- each listed writer's head and record count, as its receive head;
- the carried pending records, as pending input to receive again;
- the relay cursor, when its generation is the relay's current one.

The drain that follows receives each carried record as if fresh, so it is authenticated on this device before it applies or waits.

It then reads the log from the cursor. Every record after it is tail: it either names a covered head as its predecessor, or it starts a later chain.

**Limit.** A fresh device holds no history, so the relay is its only source of freshness. It can check a checkpoint's structure, signatures and internal consistency. It cannot tell whether a newer checkpoint, or later records, are being withheld. Only an authorized writer can author a checkpoint. Its signature identifies that writer, but does not prove the snapshot matches the records it lists. A malicious member can author a dishonest checkpoint (SYNC_DESIGN.md threat model). Devices with history detect a contradiction at check 2 or 3. A fresh device cannot.

When the cursor's generation is not the relay's current one (the relay was restored), the device does not use the checkpoint. It reads the whole log instead.

A device that started from a checkpoint knows each covered chain only by its length and head. If it meets covered records again, for example after a later relay restore, it recognises them by walking each chain back from its covered head, by hash, through its pending records, looked up by digest. Each one found is recorded as held, with its predecessor and position, and is no longer pending.

## Old offline devices

History is never pruned in this version. The relay keeps every original record and every checkpoint, so an old device with unsent edits merges normally:
- it pushes its records;
- it pulls everything after its own cursor.

If it was removed or its lineage frozen, its writes stay local and unsent, and it keeps the export and recovery path (SYNC_KEY_DISTRIBUTION.md).

## Retention and quotas

Checkpoints add to storage. They do not replace anything, and their records count against the route's record and byte quotas like any other. When a quota is exhausted, the relay refuses the parts with `quota-exhausted` (SYNC_SERVICE_CONTRACT.md). The device keeps them queued and keeps working locally. Nothing is deleted on either side to make room.

Pruning history needs its own approved policy, which must name:
- which devices must have accepted a checkpoint first;
- how deletion evidence survives;
- how a device older than the retained history recovers.

The policy must be based on measured checkpoint size, bootstrap latency and history growth for named workloads (#40, #43).

## Limits

- **Engine resources.** Judging a complete checkpoint imports its snapshot into an empty Loro document, and starting from one imports it into the device's engine. Receiving an update decodes its metadata, then imports it. Loro 1.18 bounds neither the memory nor the CPU time of a decode, and has no setting for either. Two expansions were measured with Loro 1.18.0 (x86-64 Linux, release build):
  - **LZ4.** `loro-kv-store` decompresses each snapshot block's LZ4 frame into a growing buffer (`compress.rs`, `decompress`). Loro writes no decompressed size, and checks a normal block's size only after decompressing it. LZ4's ratio is at most about 255:1 (254.8:1 measured on 128 MiB of zeros), so the checkpoint payload limit (`MAX_PARTS` × `CHUNK_BYTES`, 256 MiB) bounds this stage at about 64 GiB.
  - **Run-length op columns.** A change block stores its ops as run-length encoded columns (`loro-internal` `block_encode.rs`, `decode_block`). One run holds up to 10^9 ops, an op may have length 0, and a map deletion reads no value bytes. A 98-byte update holding one run of 3×10^7 such ops took 3.9 GiB and 12 s to decode its metadata alone; that is about 130 bytes and 0.4 µs per op. Input size does not bound this stage, so it applies to every update record (`MAX_PLAINTEXT_BYTES`, 1 MiB) as well as to checkpoints.

  Bounding the second stage from outside needs either a re-implementation of Loro's private block format, kept in step with each release, or the decode run in a process with an address-space and CPU limit (#45). The `engine_import` fuzz target runs under libFuzzer's 2 GiB memory limit. A panic in the import refuses the checkpoint rather than taking the application down.
- **A fresh device** cannot verify a checkpoint against history it does not hold (above).
- **Retention.** Pruning keeps the latest decided checkpoints only; a re-delivered pruned one is judged again.

## Evidence

`crates/matters-core/tests/checkpoint_tests.rs` covers:
- a checkpoint waiting for the device's own records to be acknowledged, and not made twice for the same state;
- a member with history accepting one and importing nothing;
- a fresh device starting from one and following the tail from both writers;
- a checkpoint from another relay generation starting nothing;
- pending records travelling with a checkpoint and applying later;
- a deletion travelling with a checkpoint, with a stale edit from another device after it merging the same on the source and on the device that started from it;
- covered records met again after a relay restore, recognised by their chains;
- dishonest checkpoints refused and kept as evidence: another head, a head held at another position, a version the snapshot does not hold, another author, a counter that disagrees; and parts sealed under another epoch quarantined;
- an older checkpoint never replacing a newer one the device verified;
- publication surviving a restart and a relay restore with the same bytes;
- a crash while creating a checkpoint leaving nothing and losing nothing.

`crates/matters-core/tests/receive_review_tests.rs` covers another member's parts leaving an honest checkpoint to be judged on its own, an author's parts capped and superseded by its newer checkpoint, an unverifiable checkpoint never overriding one the device can verify, a dependency-waiting record carried pending, and unauthenticated records never carried.

`authority_tests.rs` checks that snapshot records authenticate only as snapshots, and only without a predecessor.

`crates/matters-sync/tests/checkpoint_tests.rs` covers:
- one-part and multi-part round trips, in any delivery order and with duplicates;
- refusal of parts that disagree, tampered chunks and statements, and wrong epochs;
- rejection of non-canonical and out-of-limit encodings;
- each `Behind` and `Fork` case of the comparison;
- byte-exact agreement with an independent encoder (`tests/vectors/checkpoint_kat.py`, Python standard library).

The `checkpoint` fuzz target runs the part and statement parsers, and assembly. It is seeded from that encoder, and its corpus is replayed on stable by `tests/fuzz_replay.rs`. The `built_checkpoint` target builds checkpoints from fuzzed fields and checks that each assembles from its own parts into itself, follows itself and is refused under another epoch; `signed_record` seals fuzzed parts as the fixture's admitted writer, so assembly runs after authentication.

## Measured

Measured on one shared development host ([evidence record](evidence/sync-checkpoint-cost-2026-11-21.json): i9-10920X, btrfs on NVMe, release build, one run each). Each workload has *n* tasks with 1 KiB notes, a rename of every second task and a completion of every tenth, each edit its own sealed record. There is one writer and nothing is pruned.

| Tasks | Records | Retained history | Checkpoint | Create | Start from it | Receive every record |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 250 | 400 | 1.9 MB | 92 KB, 1 part | 22 ms | 23 ms | 8.5 s |
| 1,000 | 1,600 | 7.7 MB | 354 KB, 1 part | 57 ms | 99 ms | 104 s |
| 2,000 | 3,200 | 15.4 MB | 708 KB, 2 parts | 106 ms | 208 ms | 422 s |

- **Size.** A checkpoint is about 4.7% of the retained history at each size.
- **History growth.** History grows by about 4.8 KB per edit, mostly the record's ML-DSA-65 signature.
- **Starting a device.** Starting from a checkpoint grows with the collection's size. Receiving every record grew quadratically in this measurement. Since #241 a device receives a page at a time, with one rebuild of the rows and a lazily rebuilt index, and the same workloads take 0.65 s, 2.6 s and 6.1 s ([record](evidence/sync-receive-cost-2026-11-22.json)).
- **Retention budget.** None is set. Pruning, and any budget on bootstrap time, needs its own approval against these figures.
