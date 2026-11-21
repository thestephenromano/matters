# matters-relay

The durable encrypted sync relay (#38). It implements [the sync service contract](../../docs/SYNC_SERVICE_CONTRACT.md) on PostgreSQL 18 through sqlx, with the components chosen in [ADR 0003](../../docs/decisions/0003-relay-components.md). It stores and serves opaque signed records and authority objects: no content keys, no plaintext, nothing about tasks.

**This crate so far** holds the store:
- route provisioning, which verifies the genesis against the root;
- durable, idempotent record appends, with permanent same-ID conflicts;
- routing and membership checks;
- gapless per-route sequences;
- cursor pages under a storage generation;
- authority objects, with membership transitions verified before they authorize uploads;
- operator quotas.

**The v0 HTTP API** (`api::router`, on axum over HTTP/1.1 and cleartext HTTP/2) has:
- device sessions: a single-use 60-second challenge answered with the enrolled ML-DSA-65 key, and a 15-minute bearer token stored as its SHA-384, with membership checked again on every request;
- record and authority uploads and pages, with deterministic CBOR bodies and every error in the contract's code and status.

The `matters-relay` binary serves it. Set `MATTERS_RELAY_DATABASE_URL` and optionally `MATTERS_RELAY_LISTEN` (default `127.0.0.1:8484`). TLS terminates at a proxy in front (#44).

Commit hints, export and import, and the conformance runner follow in a later #38 pull request.

## Development

```sh
docker compose -f crates/matters-relay/compose.yaml -p matters-relay up -d
export MATTERS_RELAY_TEST_DATABASE_URL=postgres://postgres:relay@localhost:55432/postgres
cargo test -p matters-relay
```

- **Databases.** Each test creates and drops its own database. CI runs the same tests (`ci/relay-tests.sh`) against a PostgreSQL 18.7 service container.
- **Without a database.** If `MATTERS_RELAY_TEST_DATABASE_URL` is unset, the tests are skipped with a note. With `MATTERS_RELAY_REQUIRE_DB` set, as in CI, a missing database is a failure instead.
