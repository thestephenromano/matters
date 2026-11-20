# ADR 0003: relay storage, transport and session authentication

Date: 2026-11-20. Status: **approved by the maintainer on 2026-11-20 for the v0 relay**. Tracks [#34](https://github.com/thestephenromano/matters/issues/34); the relay is built in [#38](https://github.com/thestephenromano/matters/issues/38) and the client side in [#39](https://github.com/thestephenromano/matters/issues/39). The contract these components implement is [SYNC_SERVICE_CONTRACT.md](../SYNC_SERVICE_CONTRACT.md).

## Decision

Build a thin relay from established components; no existing sync server meets the contract.

| Concern | Choice | Pin for v0 |
| --- | --- | --- |
| Record and authority storage | [PostgreSQL](https://www.postgresql.org/support/versioning/), synchronous commit | 18.x (18.7 current); not 19 until its first minor releases |
| Database client | [sqlx](https://crates.io/crates/sqlx) (MIT/Apache-2.0) | 0.9.1, Postgres driver, Tokio runtime |
| HTTP API | [axum](https://docs.rs/axum/0.8.9/axum/) (MIT) on hyper 1, HTTP/2 | 0.8.9 with the `ws` and `http2` features |
| Commit hints | WebSocket through axum `ws`; payload only "route reached sequence N" | as above |
| Message bodies | CBOR through [minicbor](https://docs.rs/minicbor/2.3.1/minicbor/), already adopted for records | 2.3.1 |
| Device sessions | Challenge signed with the device's enrolled ML-DSA-65 key, verified against the stored roster; short-lived route-scoped bearer token | the ML-DSA implementation approved in [ADR 0002](0002-post-quantum-sync.md) |

The acknowledged failure domain is **one host**: an acknowledgement means the record, its identity and its receipt are committed and flushed on the relay's primary (`synchronous_commit = on`). Losing that host can lose the WAL archive gap since the last archived segment. A synchronous standby (`synchronous_standby_names`) is a documented deployment option that widens the domain; it is not required. Availability has no service-level target in v0. The contract states what clients observe in each case.

Exact versions are resolved and locked in #38's `Cargo.lock`, against these pins.

## Alternatives considered

Facts below were checked against the cited sources on 2026-11-19/20.

| Candidate | Why not chosen |
| --- | --- |
| [NATS JetStream](https://docs.nats.io/nats-concepts/jetstream/streams) 2.15 | `Nats-Msg-Id` deduplicates only within a window (2 minutes by default), so a permanent same-ID conflict needs a subject per record and a separate byte comparison. The default fsync interval acknowledges before data is on disk unless `sync_interval: always` is set. It adds a broker the contract does not need. |
| SQLite with [Litestream](https://litestream.io/tips/) | Simplest to self-host and gives gapless per-route sequences, but replication is asynchronous (about 1 s), so the acknowledgement covers one local disk only, and managed hosting has no shared high-availability path. Remains a possible second storage adapter behind the same contract. |
| S3-compatible storage with conditional writes | AWS S3 refuses an existing key with `If-None-Match`, but self-hostable servers differ: Garage was reported to overwrite silently, and MinIO's community repository is archived. Not a log: ordered per-route cursors and notifications need another component. Possible later as a blob tier behind an SQL index. |
| [FoundationDB](https://apple.github.io/foundationdb/known-limitations.html) 7.4 | 100 kB value limit (records reach about 1 MiB) and a heavy self-hosting burden. |
| [tonic](https://docs.rs/tonic/0.14.9/tonic/) gRPC 0.14 | Adds protobuf as a second codec, is harder to proxy, and browsers would need grpc-web. No contract benefit over HTTP with CBOR. |
| [Loro Protocol](https://loro.dev/blog/loro-protocol) | Its end-to-end encrypted mode is documented as experimental; its encrypted server indexes plaintext headers and replaces stored spans, which conflicts with immutable records and permanent conflicts. Its records carry no signature binding. Useful reading only. |
| Whole sync servers: y-sweet, automerge-repo sync server, ElectricSQL, PowerSync | y-sweet and the automerge-repo server merge plaintext documents. Electric does not sync writes; PowerSync routes writes through an application backend. None stores opaque client-encrypted records with permanent conflict detection and cursor generations. |
| OAuth/OIDC bearer tokens | A standard login would need an identity provider for every self-hosted relay and a trust root separate from the device keys the protocol already authenticates. Accounts may be added later for billing, without becoming a decryption or enrollment authority. |

## Consequences

- The relay is built, not adopted: authentication, quotas, cursors and the API are Matters code, held to the contract's conformance fixtures. Postgres supplies durability, uniqueness and backup.
- Per-route sequence numbers come from a per-route counter row updated in the record's transaction, so writers to one route serialize. That is cheap for personal collections; shared collections must be measured before a pilot.
- `NOTIFY` is delivered only on commit, and only as a hint; clients always fetch by cursor, so a lost hint costs a later fetch.
- Point-in-time recovery does not change the storage generation by itself: the restore procedure must, and #44's drills check it. A malicious operator can still hide a reset; clients keep their own receipts (contract, *Restore*).
- PostgreSQL, sqlx, axum and minicbor licenses are permissive. Distribution review of the full dependency graph happens with #38's lockfile.
