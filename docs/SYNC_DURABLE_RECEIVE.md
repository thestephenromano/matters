# Durable authenticated receive experiment

Status: bounded integration-test support for [#35](https://github.com/thestephenromano/matters/issues/35) and [#36](https://github.com/thestephenromano/matters/issues/36). Neither issue is closed. No application database, network service or user key store uses this code.

## Adopted components and boundary

Extend the approved SQLite/rusqlite, Loro, minicbor and HPKE/ML-DSA dependencies. Adopt the transactional-inbox pattern: retain input and receipt progress together, then atomically publish application results. [SQLite transactions](https://www.sqlite.org/lang_transaction.html) own commit/rollback and single-writer serialization. [Loro persistence](https://loro.dev/docs/tutorial/persistence) requires pending input to survive independently of a normal snapshot. This experiment retains original encrypted input instead of using snapshots as a receipt ledger. A separate message broker or database engine provides no needed capability for this local transaction boundary. No new library, primitive, transport or storage service is selected.

The implementation is [encrypted_inbox.rs](../experiments/sync-prototype/tests/support/encrypted_inbox.rs), with [executable cases](../experiments/sync-prototype/tests/cases/encrypted_inbox.rs). It reuses the signature/context/epoch gate from [key distribution](SYNC_KEY_DISTRIBUTION.md), the full-record SHA-384 chain definition and the existing task projection. `Authority::authenticate_data` produces a read-only authenticated record handle; only that handle can decrypt. The earlier first-record helper retains its stricter null-predecessor and first-actor-range checks.

## Identity and trust

The caller supplies an independently trusted application/tenant/collection/document route, pinned root, signed genesis, ordered signed writer starts and a local receive-stream generation. Bootstrap and writer admission validate that bundle before opening storage. The exact bundle is bound to the database and cannot change on reopen. Membership and writer forks in configuration fail opening; changes, retired writers, unknown writers, epochs and snapshots require later lifecycle work.

The approved [account model in #34](https://github.com/thestephenromano/matters/issues/34) uses a random application-scoped tenant identifier without a user name, email address or profile. Identifiers do not grant access. Login/billing identity mapping stays outside the sync data plane; encrypted record validation uses independently trusted opaque IDs. This experiment has no login, billing, token or telemetry integration and cannot establish their privacy properties. Route IDs, device relationships, sizes, timing and network metadata are not anonymity guarantees.

One collection/document and one fixed membership epoch are supported per database. Keys are caller-supplied authenticated `EpochKey` handles and remain in memory. Their route/manifest/epoch must match before processing starts. Selecting the intended content key for that epoch remains the caller's responsibility; the receiver cannot distinguish a signed record encrypted with a wrong key from a wrong same-epoch key supplied locally. No key persistence or recovery ceremony is implemented.

## Transactions and observable states

| Boundary | Committed together | Retry/restart behavior |
| --- | --- | --- |
| Receipt | Exact input bytes, local stream position and `waiting/unprocessed` status | Retrying the same position and bytes is a no-op; different bytes at that position fail. |
| Application | Accepted replay order, quarantine/wait/fork classifications and complete task projection | Reconstruct accepted engine state from encrypted records in committed application order, then retry pending records. |

Receipt positions start at one and are contiguous within the caller-pinned generation. Missing positions, changed generations and oversized inputs fail without advancing progress. This is a bounded local stream contract, not a selected relay pagination API. In-range malformed records are retained before processing; receipt progress means bytes are recoverable, not that their contents were accepted. There is no network acknowledgement or durability promise to a remote peer.

Both transactions use `BEGIN IMMEDIATE`. Persistent opens require SQLite to return WAL mode and set `synchronous=FULL`; `:memory:` is rejected. A dedicated format marker and exact configuration binding reject other databases, including names such as `SQLiteCache`. Validation is repeated under the initialization transaction to protect competing openers. Busy/SQL errors roll back and can be retried. Capacity does not depend on opens or failed attempts.

All authoritative accepted state is the retained ciphertext plus its application order. The receiver has no long-lived in-memory document to get ahead of a failed commit. Processing replays accepted records and stages candidate imports in fresh documents. Projection and order updates cannot commit separately. No incoming path writes an outgoing queue. The stored projection contains plaintext and the database/WAL/backups are not encrypted at rest.

`entries()` exposes original bytes, sequence, classification, a payload-free reason and optional committed application ordinal. `projection()` returns the last committed accepted view. Callers must inspect classifications as well as the view: an observed fork can freeze a chain after earlier records were accepted. This experiment preserves that prior view and its history; it does not silently roll back or choose a fork winner.

## Admission and conflicts

1. Validate canonical content encoding and independently expected route, current membership/epoch, admitted active writer and original-byte ML-DSA signature. Unsupported or unauthenticated inputs are quarantined without contributing chain claims.
2. Gather signed record identities and `(incarnation, predecessor)` claims across retained records before decrypting or importing. Two different authenticated successors, including two null roots, freeze that incarnation. A same-ID/different-bytes authenticated conflict freezes both involved incarnations. Retain both byte strings. Exact-byte duplicates are not applied again.
3. Decrypt through the authenticated handle. A valid competing signature remains fork evidence even if its AEAD or CRDT payload is invalid. A forged competitor cannot freeze a chain.
4. Require the predecessor to equal the accepted head for that incarnation. Keep unresolved records waiting. A descendant of a retained quarantined/forked predecessor reports `blocked-by-rejected-predecessor`; deeper unresolved dependencies remain visible as waiting.
5. Admit only Loro Updates blobs with a positive range for the writer's single admitted actor. Reject overlapping ranges and snapshot blobs. Wait for missing actor counters or causal dependencies. After staged import, require no pending engine updates and exactly the expected full version vector; then rebuild the task projection.
6. Repeat the pending pass until no additional record can apply. Persist classifications and projection atomically. Application order can differ from delivery order.

Classification is recomputed from retained signed input on each pass. This preserves fork evidence without a second mutable authority cache. Rejected inputs can be inspected after restart; processing does not silently discard them. Local arbitrary database tampering, rollback by a hostile local operator and cryptographic hash collisions are outside this experiment's model.

## Budgets and evidence scope

Choose at most 32 retained deliveries per database and the existing content-record wire cap per delivery (roughly 32 MiB of input at the limit). This is an inspectable fixture budget, not a service quota. Duplicate deliveries at new positions also consume it; an exact retry at an existing position does not. At capacity, reopen, inspection, processing and existing-position retries remain available. There is no pruning.

Re-authentication/replay and fixed-point staging deliberately trade performance for a small auditable state machine. Full history, candidate plaintext, engine staging and SQLite pages require additional space. Neither the input cap nor the fixture count establishes a decompression/CPU/memory bound for hostile signed Loro updates. No latency or production throughput claim follows.

Tests cover reversed delivery, missing cross-writer causality, duplicate positions/records, rejected predecessors, forks before/after prior application, forged competitors, authenticated record-ID conflicts, signed undecryptable branches, actor overlap, malformed domain state, cross-tenant/unsupported snapshot inputs, wrong-context key handles, two database handles, real SQLite write contention, SQL trigger rejection, capacity, foreign storage and restart. Four child-process exits cover before/after receipt and application commits. Direct discovery of the ignored helper is a no-op only when all worker variables are absent.

The [active evidence](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) records commands, source hashes and deliberate guard-removal results. CI checks those bindings, independent fixtures, formatting, Clippy and both normal and ignored-inclusive suites. Process exits on the recorded local filesystem do not establish power-loss, media-failure or host-loss durability.

## Remaining gates

Collection-wide record-ID conflict indexing before supporting multiple documents; durable changing membership/writer authority, enrollment and revocation/rotation, historical closing frontiers, snapshots/checkpoints and safe pruning; full hostile-document/schema/resource admission; persistent key storage and recovery; atomic integration with local domain commands/outbox; a service API with restore generations and remote durability semantics; production retention and disk exhaustion handling; exact-suite independent interoperability, cross-target runs and independent security review. Fixed configuration is not transparent account recovery or support for old retained outbox writers.
