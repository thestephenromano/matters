# Self-hosting the sync relay

Status: [#44](https://github.com/thestephenromano/matters/issues/44), in steps.
- The image, the stack, health and metrics are in place.
- Automated restore drills, the upgrade compatibility matrix and dashboards follow. Until they land, the restore and upgrade procedures below are documented but not yet proven by an automated drill.

The relay stores only what clients have already encrypted and signed: records, authority objects, envelopes, and the indexes it needs to serve them. Content keys, device keys and recovery material never reach it (SYNC_SERVICE_CONTRACT.md). An operator can see routes, sizes and timing, but never content.

## What runs

`packaging/relay/compose.yaml` runs three containers on one host, each image pinned by digest.

| Service | Image | Role |
| --- | --- | --- |
| `postgres` | `postgres:18.7` | The relay's store: every acknowledgement is a committed transaction (ADR 0003). Data in the `postgres` volume. |
| `relay` | built from `packaging/relay/Containerfile` | The relay, distroless and unprivileged, on the private network only. API on 8484, metrics on 9484. |
| `caddy` | `caddy:2.11` | TLS in front, with certificates from a public CA, or its own CA with `internal`. It publishes 443 and 80, and forwards only `/v0/*`, `/healthz` and `/readyz`. |

The image build is reproducible from source:
- base images are pinned by digest;
- `cargo build --locked`;
- a fixed `SOURCE_DATE_EPOCH`;
- remapped paths.

CI's `self-hosted relay image` job (`ci/relay-image.sh`) runs whenever the relay, matters-sync or the packaging changes. It builds the image and brings the stack up, then checks that:
- the relay is ready over TLS through Caddy, on HTTP/2;
- metrics are not public, and are served on the private network.

## Setting up

From the repository root:

```sh
cp packaging/relay/relay.env.example packaging/relay/relay.env
openssl rand -hex 32 > packaging/relay/postgres_password   # and put it in relay.env's URL
chmod 600 packaging/relay/relay.env packaging/relay/postgres_password
docker compose -f packaging/relay/compose.yaml up -d --build
```

`relay.env` sets:
- the public name (`MATTERS_RELAY_DOMAIN`);
- the ACME contact address (`MATTERS_RELAY_TLS`);
- the database URL;
- optionally the rate limit (`MATTERS_RELAY_RATE`) and the export credential (`MATTERS_RELAY_OPERATOR_TOKEN`).

Neither secrets file is committed (`packaging/relay/.gitignore`). `MATTERS_RELAY_HTTPS_PORT` and `MATTERS_RELAY_HTTP_PORT` change the published ports.

Provision each collection with the root key and genesis its admin device exports:

```sh
docker compose -f packaging/relay/compose.yaml exec relay \
  matters-relay provision <route-hex> <root-key-file> <genesis-file>
```

## Health and metrics

- `GET /healthz`: the process is up.
- `GET /readyz`: its database answers too. The container's health check runs `matters-relay probe`, which needs no shell.
- `GET /metrics` on port 9484, reachable only on the stack's network: OpenMetrics text for Prometheus (crates/matters-relay/README.md). Labels are path templates and statuses only. No route, tenant, device or record identifier ever appears in them.

Signs of degraded sync:

| Signal | Meaning |
| --- | --- |
| `/readyz` answering `503` | The database is away: clients keep their work queued and retry. |
| Rising `requests_total{status="503"}` or `"429"` | Unavailability, or rate limiting. |
| `"507"` | Routes at their quota: clients keep their work locally (`quota-exhausted`). |
| `"409"` on record uploads | Conflicting record IDs: evidence worth keeping, never retried as success. |
| `request_duration_seconds` on `/v0/routes/{route}/records` | Catch-up cost. |

## Backups and restore

A backup must hold the records, authority log, envelopes and indexes **together and consistently**. Take it from PostgreSQL itself:

```sh
docker compose -f packaging/relay/compose.yaml exec -T postgres \
  pg_dump -U relay -Fc relay > relay-$(date -u +%Y%m%dT%H%M%SZ).dump
```

`pg_dump` reads one consistent snapshot. Records acknowledged after the snapshot are not in it. Restoring an older backup therefore **loses acknowledged records**. This is not zero data loss: clients re-upload what they still hold, and the relay deduplicates by record ID.

To restore:
1. stop the relay;
2. restore the dump into an empty database (`pg_restore -U relay -d relay --clean`);
3. **start a new storage generation**: `matters-relay new-generation --all`;
4. start the relay.

The new generation makes every client's cursor invalid (`410 generation-changed`). Each client then forgets its cursors, re-queues everything it wrote, and reads the log again. Record identities, membership and epochs travel inside the signed bytes and do not change.

`matters-relay export` and `import` move one route between deployments with every byte unchanged, under a new generation (SYNC_SERVICE_CONTRACT.md).

## Upgrades

- **Schema.** The relay's schema migrations run forward at start, and older relays do not run against a newer schema.
- **Before upgrading.** Take a backup.
- **Rolling back.** Restore that backup with a new generation, as above, so clients re-upload whatever arrived after it.
- **Clients and the API.** The API is versioned (`/v0/`). Clients refuse unknown versions and suites, and a relay answers an unknown version with `unsupported-version`.

## Credentials

| Credential | How to rotate it | Effect |
| --- | --- | --- |
| Database password | `ALTER ROLE relay PASSWORD …`, then update `postgres_password` and `relay.env`, then restart the relay | Clients see a brief unavailability and retry. |
| Operator export token | Replace `MATTERS_RELAY_OPERATOR_TOKEN` and restart | The old token stops working at once. |
| Device sessions | Last 15 minutes and are bound to current membership | A removed device loses access at its next request. |
| TLS certificates | Renewed by Caddy | — |

## Quotas

```sh
matters-relay quota <route-hex> bytes <n>     # or records <n>, or none
```

At its quota, a route refuses new records with `quota-exhausted`. Nothing stored is deleted, and clients keep the refused work locally.

## Ownership and retention

The operator owns the host, its backups and their retention. The relay never deletes history in this version (SYNC_CHECKPOINTS.md, "Retention and quotas"). Storage grows with every record and checkpoint, about 4.8 KB per edit on the workloads measured for #40.

Deleting a route's data is an operator action on the database. It does not remove the copies every device holds.
