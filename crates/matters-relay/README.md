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

**Self-hosting** is described in [docs/SELF_HOSTING.md](../../docs/SELF_HOSTING.md): the image (`packaging/relay/Containerfile`), and a stack of PostgreSQL, the relay and Caddy (`packaging/relay/compose.yaml`).

**Health and metrics (#44).**
- `GET /healthz` answers `200` while the process is up.
- `GET /readyz` answers `200` while its database answers within a second too, and `503` otherwise.
- With `MATTERS_RELAY_METRICS_LISTEN` set (for example `127.0.0.1:9484`), a second listener serves `GET /metrics` in OpenMetrics text for Prometheus.

The metrics are:
- `matters_relay_requests_total`, by matched path template and status. Durable acknowledgements are `PUT /v0/routes/{route}/records/{record}` answered `201`; replays are `200`; refusals and backpressure have their own statuses.
- `matters_relay_request_duration_seconds`, by template.
- `matters_relay_routes`, `matters_relay_stored_records` and `matters_relay_stored_bytes`, across every route.

Labels come only from the API's path templates and HTTP statuses. No route, tenant, device, record ID, digest or content ever appears in them, and the tests check this. Keep the metrics listener on a private interface.

**Operations:**
- **Commit hints.** `GET /v0/routes/{route}/hints` is a WebSocket. An append's `pg_notify` is delivered only when its transaction commits, so a hint never precedes its record, and duplicates send none.
- **Limits.** A per-device rate limit gives `429` with `retry-after-ms`. Operator quotas on bytes and on record count give `507`.
- **Export and import.** Both move a route byte for byte, and import installs a new generation. The HTTP export needs the operator credential.

**Operator commands:**

```text
matters-relay [serve]
matters-relay provision <route-hex> <root-key-file> <genesis-file>
matters-relay export <route-hex> <file>
matters-relay import <file>
matters-relay new-generation (<route-hex> | --all)    # --all: after restoring a backup
matters-relay quota <route-hex> (bytes | records) (<n> | none)
```

`serve` reads `MATTERS_RELAY_RATE` (`<per-second>/<burst>`) and `MATTERS_RELAY_OPERATOR_TOKEN` (64 hex digits).

**Conformance.** `tests/conformance.rs` runs every scenario of [the contract's fixtures](../../docs/protocol/sync-relay-v0-conformance.json) against the real binary, as a child process:
- restarts kill the process;
- crashes use the `fault-injection` crash points before and after an append's commit;
- backups and restores copy the database and run `new-generation --all`.

It also:
- takes the database away, which must answer `503 unavailable` and acknowledge nothing;
- checks every table and every log line for plaintext and session tokens.

## Development

```sh
docker compose -f crates/matters-relay/compose.yaml -p matters-relay up -d
export MATTERS_RELAY_TEST_DATABASE_URL=postgres://postgres:relay@localhost:55432/postgres
cargo test -p matters-relay
```

- **Databases.** Each test creates and drops its own database. CI runs the same tests (`ci/relay-tests.sh`) against a PostgreSQL 18.7 service container.
- **Without a database.** If `MATTERS_RELAY_TEST_DATABASE_URL` is unset, the tests are skipped with a note. With `MATTERS_RELAY_REQUIRE_DB` set, as in CI, a missing database is a failure instead.
