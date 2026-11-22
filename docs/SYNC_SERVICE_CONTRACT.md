# Sync service contract, v0

Date: 2026-11-20. Tracks [#34](https://github.com/thestephenromano/matters/issues/34). Components: [ADR 0003](decisions/0003-relay-components.md). Record bytes: [content-record contract](SYNC_RECORD_CONTRACT.md); authority objects: [key distribution](SYNC_KEY_DISTRIBUTION.md). Message shapes: [CDDL](protocol/sync-relay-v0.cddl). Conformance scenarios: [fixtures](protocol/sync-relay-v0-conformance.json).

This is the one contract between a sync client and a relay, for managed and self-hosted deployments alike. The relay stores and serves opaque signed records; it never holds content keys and knows nothing about tasks. It implements the relay obligations of the content-record contract (*Receiver and relay obligations*) and makes them concrete. Implementations: the relay in [#38](https://github.com/thestephenromano/matters/issues/38), the client in [#39](https://github.com/thestephenromano/matters/issues/39).

## Identities and what the relay may see

| Identity | Form | Scope |
| --- | --- | --- |
| Application | 16 bytes | Provisioned namespace; part of every signed record. |
| Tenant | 16 bytes | One customer or user within an application. |
| Collection | 16 bytes | The unit of authorization and of the record log. A **route** is `(application, tenant, collection)`. |
| Document | 16 bytes | Inside a record header only; the relay neither indexes nor checks it. |
| Device | 48 bytes (SHA-384 of its signing and KEM key fingerprints) | Authenticated by the stored membership roster. |
| Record | 32 bytes, from the signed header | Unique per route for ever. |
| Authority object | SHA-384 of its complete signed bytes | Genesis, membership, writer start/handoff, key envelope. |
| Sequence | Unsigned 64-bit, per route, from 1, gapless in commit order | Assigned by the relay; a transport index, not authenticated. |
| Generation | 16 random bytes per route | Changes whenever stored history may have changed outside normal appends (a restore). |

The relay may read the routing fields of a record (application, tenant, collection, signer, record ID, kind), its size and timing, and the public parts of authority objects it needs for authorization (roster device IDs and signing keys, epochs). It must not need, accept or log content keys, plaintext or decrypted structure. Ciphertext and envelope bytes are stored and returned exactly.

## Versioning and compatibility

Every path starts with `/v0/`. Bodies are RFC 8949 core deterministic CBOR (`Content-Type: application/cbor`) except record and authority bytes, which travel exactly as signed (`application/vnd.matters.sync-record`, `application/vnd.matters.sync-authority`). A relay answers an unknown version prefix with `404` and error code `unsupported-version`; a client never retries with another version or suite. Records with a suite other than `matters-sync-pq-v0` are refused (`unsupported-suite`, permanent).

| Client | Relay v0 | Future relay v1 |
| --- | --- | --- |
| v0 | Supported | Must keep serving `/v0/` or refuse with `unsupported-version`; never reinterpret v0 bytes |
| v1 | Refused (`unsupported-version`) | Supported |

A protocol or suite change is a new version, never a negotiated downgrade. Exported data (below) is version-tagged and imported only by a relay that supports that version.

## Authentication and authorization

**Route provisioning** is an operator action outside this API (for example, the self-hosting admin CLI in #44). It stores the route, the independently obtained root signing key and the signed genesis. The relay verifies the genesis against that root key before accepting the route.

**Device sessions.** A device proves possession of its enrolled ML-DSA-65 signing key:

```text
POST /v0/session/challenge   {"route": route, "device": device48}
→ 200 {"challenge": bytes32, "expires": unix-ms}

POST /v0/session             {"route": route, "device": device48, "challenge": bytes32,
                              "signature": bytes3309}
→ 200 {"token": bytes32, "expires": unix-ms, "generation": bytes16}
```

The signature covers `CBOR(["matters-sync/v0/relay-session", application, tenant, collection, device48, challenge])` with the device's signing key. The relay accepts it only if the device is in the roster of the route's latest accepted membership, verified from the stored genesis and transitions with the rules of the key-distribution contract. A challenge is single-use and expires after 60 seconds. A token lasts at most 15 minutes, is scoped to one route and one device, is sent as `Authorization: Bearer`, and is stored only as a hash. On `401 session-expired` the client starts a new session; its local work is untouched.

**The root's session (#42).** A device recovering a collection after every device was lost is no member. It signs the same challenge as the collection's root, with the root key its recovery words derive, using the root's ID as `device`. The relay verifies it with the root key the route was provisioned with. That session may read the authority log, and publish only a `root_recovery` and `key_envelope`s. Every other request gets `403 not-a-member`. Once the relay accepts the recovery, the recovering device is the new generation's admin and signs in as itself.

A removed device's sessions end when the relay accepts the transition that removes it. The relay enforces upload rights (current roster, signer equals the session's device) independently of the end-to-end checks every client performs; it does not replace them.

## Operations

`{route}` in a path is the 96-character lowercase hex of application, tenant and collection. Examples use CBOR diagnostic notation with shortened byte strings.

### Append a record

```text
PUT /v0/routes/{route}/records/{record-id-hex}
Content-Type: application/vnd.matters.sync-record
<exact record bytes, at most 1,052,451 bytes (the record contract's wire cap)>
```

Before storing, the relay parses the record's structure (not its ciphertext) and requires: application, tenant and collection equal to the path's route; record ID equal to the path; signer equal to the session's device; a supported suite and kind. It does not verify the signature: clients do (it may later, as abuse control).

| Outcome | Status | Body |
| --- | --- | --- |
| Stored now | `201` | `{"sequence": 42, "generation": h'…', "digest": h'…48'}` |
| Same record ID, identical bytes already stored | `200` | The original receipt; no second append, no hint |
| Same record ID, different bytes | `409 record-conflict` | `{"code": "record-conflict", "retry": false, "digest": h'…'}` (the stored digest) |
| Routing fields do not match the path or session | `422 context-mismatch` | permanent |

**A `201` or `200` is the durable acknowledgement.** It is sent only after the transaction that stores the bytes, the record ID's uniqueness, the digest and the sequence has committed and been flushed on the relay's primary host (ADR 0003). Any other response, a timeout or a dropped connection means "not known to be stored": the client keeps the record queued and retries the same bytes; the retry returns `200` with the original receipt if the first attempt had committed. Hints for a sequence are published only after its commit.

### Fetch records

```text
GET /v0/routes/{route}/records?after={cursor}&limit={1..256}
→ 200 {"generation": h'…', "high-water": 57, "records": [[43, h'<record>'], …],
       "next": h'<cursor>', "complete": false}
```

The relay captures the route's high-water mark at the start of the request and returns records in sequence order up to it; records appended meanwhile belong to the next fetch. A page holds at most `limit` entries and at most 16 MiB (16,777,216 bytes) of entry bytes, except that its first entry is always returned, so every page makes progress. A shorter page than `limit` is therefore not the end: `complete` is true when the page reached the high-water mark. The cursor is opaque to clients: `CBOR([generation, last-sequence])`, base64url in the query. Omitting `after` starts from sequence 1. Records are immutable, so a client may retry a page and must deduplicate by record ID. A client advances its stored cursor only after it has durably stored the page's records locally (the receive commit of #35/#39).

A cursor from another generation gets `410 generation-changed` with the current generation (see *Restore*). So does a cursor of the current generation whose last sequence is past the route's high-water mark: history shrank without a new generation (a restore that skipped that step), and an empty, complete page would let the client wait for sequences that were lost. The same holds for the authority log's cursors. An empty page is not evidence that history never existed; a missing predecessor stays pending on the client.

### Enrollment mailbox

```text
PUT /v0/routes/{route}/enrollments/{lookup-hex}     <exact signed enrollment_request, at most 65,536 bytes>
GET /v0/routes/{route}/enrollments/{lookup-hex}     → 200 <the request bytes> | 404
```

A device joining through an invitation is not a member yet, so it has no session. It leaves its signed `enrollment_request` (SYNC_KEY_DISTRIBUTION.md) under the invitation's 16-byte lookup ID, which is derived from the invitation secret the relay never sees. The `PUT` needs no session.

The relay checks only that the bytes are an enrollment request for this route. The same bytes again return `200`, and other bytes under the same lookup get `409 record-conflict`. A route keeps at most 16 unexpired requests, each for an hour; a seventeenth gets `507 quota-exhausted`.

The `GET` needs a session, and only the route's current admin is given the request; anyone else, and an expired or absent request, gets `404`. The admin's device verifies the request against its invitation before signing anything. The joining device then becomes a member through the published transition and reads the authority log as any device does.

### Checkpoint parts

```text
GET /v0/routes/{route}/snapshots?limit={1..256}
→ 200 {"generation": h'…', "records": [[57, h'<record>'], [41, h'<record>'], …]}
```

This returns the route's newest `snapshot` records, newest first, with their sequences, under the same limits as a page: at most `limit` entries and 16 MiB of entry bytes beyond the first. A new device uses it to start from the newest checkpoint instead of reading the whole log (SYNC_CHECKPOINTS.md).

The relay tells these records apart only by their clear kind. It cannot see which checkpoint a part belongs to, or whether a checkpoint is complete. The client groups the parts and checks them. The same records also appear in the ordinary record log. An empty list means the relay holds no checkpoint, not that none was ever made.

### Authority objects

```text
PUT /v0/routes/{route}/authority/{sha384-hex}      <exact signed bytes, at most 65,536>
GET /v0/routes/{route}/authority?after={cursor}&limit={1..256}
```

Authority objects (membership transitions, writer starts and handoffs, key envelopes) are immutable and addressed by the SHA-384 of their bytes; the path digest must match. Storage rules match records: idempotent for identical bytes, and the address makes a conflict impossible. The relay verifies membership transitions before using them for authorization; it stores the other kinds after a structural check, for clients to verify. They are fetched with their own cursor and pagination, as above.

### Commit hints

```text
GET /v0/routes/{route}/hints   (WebSocket upgrade, bearer token)
server → client: {"generation": h'…', "sequence": 57}
```

A hint says only that the route has committed records up to that sequence. It may be late, coalesced or lost; it never precedes the commit. Clients fetch by cursor on each hint, on reconnect and periodically, so correctness never depends on hints.

### Export and import

```text
GET /v0/routes/{route}/export   (operator credential)
→ 200 application/cbor: ["matters-sync-export", 0, route, generation,
                         [authority-bytes…], [[sequence, record-bytes]…]]
```

Export preserves every authority object and record byte for byte, with its sequence. Import (an operator action on the target relay) stores the same bytes and assigns a **new generation**; sequences may be renumbered but keep the exported order. Authenticated bytes and identities never change, so clients move between managed and self-hosted relays by switching the endpoint and resynchronizing. Nothing in either format is specific to Matters' task model.

## Errors, quotas and retries

Every error body is `{"code": text, "retry": bool, ? "retry-after-ms": uint}`.

| Status | Code | Retry | Client action |
| --- | --- | --- | --- |
| 400 | `malformed` | no | Bug or tampering: keep the local intent, report it |
| 401 | `session-expired`, `unauthenticated` | after a new session | Re-authenticate; local work untouched |
| 403 | `not-a-member`, `revoked` | no | Stop uploading to this route; keep local work for recovery (#37) |
| 404 | `unknown-route`, `unsupported-version` | no | Configuration error |
| 409 | `record-conflict` | no | Retain both as evidence; never overwrite or re-sign |
| 410 | `generation-changed` | after resync | See *Restore* |
| 413 | `too-large` | no | Cannot happen for valid records; report |
| 422 | `context-mismatch`, `unsupported-suite` | no | Keep local intent, report |
| 429 | `rate-limited` | yes, after `retry-after-ms` | Back off |
| 503 | `unavailable` | yes | Back off with jitter |
| 507 | `quota-exhausted` | no, until the operator raises it | Keep everything queued; show a recoverable condition |

No error deletes, rewrites or acknowledges a record. A quota is refused before anything is stored; a request is never partially stored.

## Durability, restore and failure domains

| Event | What the relay guarantees | What the client observes and does |
| --- | --- | --- |
| Relay process crash | Every acknowledged record survives (committed and flushed) | Retries unacknowledged uploads; gets `200` for any that had committed |
| Primary host loss, no standby | Records acknowledged since the last archived WAL segment may be lost | After the operator restores, `410 generation-changed`; the client re-uploads everything it holds, which restores lost records unless every holder lost them too |
| Host loss with a synchronous standby | Every acknowledged record survives | No generation change if failover keeps the history |
| Point-in-time or backup restore | The restore procedure installs a new random generation before serving | `410` on the old cursor; the client refetches from sequence 1, compares with its retained receipts, re-uploads its records and authority objects, and deduplicates by record ID |

A malicious or careless operator can hide a reset by keeping the generation; clients detect contradictions only through their own retained receipts and authenticated history (record contract, *Restore / rollback*). Uploads from a client never depend on the relay being fresh.

## Conformance

[The fixture scenarios](protocol/sync-relay-v0-conformance.json) are transport-level scripts with expected statuses and receipts. #38's relay and any other implementation must pass them against a real PostgreSQL instance, including crashes before and after commit; #39's client must handle each documented response. They cover duplicate uploads, same-ID conflicts, context mismatch, reconnect and cursor resume, pagination with concurrent appends, generation change after a restore, session expiry, revoked devices and quota errors. The relay's runner is [`crates/matters-relay/tests/conformance.rs`](../crates/matters-relay/tests/conformance.rs). It reads this file, drives the real binary, and runs in CI's "relay against PostgreSQL" job.

Resource limits (body sizes above, page limits of 256 entries and 16 MiB, challenge and token lifetimes; the fixture file's `limits` lists them and the runner checks the relay's against it) are chosen v0 values, measured under #43's workloads before a pilot. Rate limits and quotas are operator configuration, reported through `429` and `507`.

## Out of scope for v0

Multiple documents per collection beyond routing, peer-to-peer transport, server-side signature verification of content records, history pruning (#40), accounts and billing, and any server knowledge of task semantics.
