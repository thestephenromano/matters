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

The HTTP API, device sessions, commit hints, export and import, and the conformance runner follow in later #38 pull requests.

## Development

```sh
docker compose -f crates/matters-relay/compose.yaml -p matters-relay up -d
export MATTERS_RELAY_TEST_DATABASE_URL=postgres://postgres:relay@localhost:55432/postgres
cargo test -p matters-relay
```

- **Databases.** Each test creates and drops its own database. CI runs the same tests (`ci/relay-tests.sh`) against a PostgreSQL 18.7 service container.
- **Without a database.** If `MATTERS_RELAY_TEST_DATABASE_URL` is unset, the tests are skipped with a note. With `MATTERS_RELAY_REQUIRE_DB` set, as in CI, a missing database is a failure instead.
