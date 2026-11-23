# Self-hosting the sync relay

Status: [#44](https://github.com/thestephenromano/matters/issues/44).
- The image, the stack, health and metrics are in place.
- A restore drill and the upgrade rules below are checked automatically on every change to the relay or the client.

The relay stores only what clients have already encrypted and signed: records, authority objects, envelopes, and the indexes it needs to serve them. Content keys, device keys and recovery material never reach it (SYNC_SERVICE_CONTRACT.md). An operator can see routes, sizes and timing, but never content.

## What runs

`packaging/relay/compose.yaml` runs three containers on one host, each image pinned by digest.

| Service | Image | Role |
| --- | --- | --- |
| `postgres` | `postgres:18.7` | The relay's store: every acknowledgement is a committed transaction (ADR 0003). Data in the `postgres` volume. |
| `relay` | built from `packaging/relay/Containerfile`, or a released image (*Using a released image* below) | The relay, distroless and unprivileged, on the private network only. API on 8484, metrics on 9484. |
| `caddy` | `caddy:2.11` | TLS in front, with certificates from a public CA, or its own CA with `internal`. It publishes 443 and 80, and forwards only `/v0/*`, `/healthz` and `/readyz`, except export (`/v0/routes/*/export`), which it answers `404`. |

The image build pins its inputs, so that the same source can build the same binary:
- base images are pinned by digest;
- `cargo build --locked`;
- a fixed `SOURCE_DATE_EPOCH`;
- remapped paths.

No gate yet builds the image twice and compares the results, so reproducibility is the aim of these pins, not a checked property.

CI's `self-hosted relay image` job (`ci/relay-image.sh`) runs whenever the relay, matters-sync or the packaging changes. It builds the image and brings the stack up, then checks that:
- the relay is ready over TLS through Caddy, on HTTP/2;
- metrics are not public, and are served on the private network;
- export is not reachable through Caddy.

## Setting up

From the repository root:

```sh
cp packaging/relay/relay.env.example packaging/relay/relay.env
cp packaging/relay/caddy.env.example packaging/relay/caddy.env
openssl rand -hex 32 > packaging/relay/postgres_password
chmod 600 packaging/relay/relay.env
# The relay runs as the image's unprivileged user (uid and gid 65532) and
# reads the password as a secret file, so that group may read it.
chgrp 65532 packaging/relay/postgres_password
chmod 640 packaging/relay/postgres_password
docker compose -f packaging/relay/compose.yaml up -d --build
```

Each service gets only what it needs:
- `caddy.env`, Caddy's only configuration: the public name (`MATTERS_RELAY_DOMAIN`) and the ACME contact address (`MATTERS_RELAY_TLS`).
- `relay.env`, the relay's only: the database URL, without a password, and optionally the limits (*Limits* below) and the export credential (`MATTERS_RELAY_OPERATOR_TOKEN`).
- `postgres_password`, a compose secret mounted into PostgreSQL (which is initialized from it) and the relay (`MATTERS_RELAY_DATABASE_PASSWORD_FILE`), so the password is in no environment and in no file but this one.

With rootless Docker or Podman, give the file to the uid that 65532 maps to on the host instead. None of these files is committed (`packaging/relay/.gitignore`), and none, nor any `*.dump`, is sent to an image build (`.dockerignore`). `MATTERS_RELAY_HTTPS_PORT` and `MATTERS_RELAY_HTTP_PORT` change the published ports.

### Using a released image

Each release publishes the relay's image as `ghcr.io/thestephenromano/matters-relay:<tag>`, for linux/amd64 and linux/arm64, built from the same Containerfile by the release workflow. Its digest is in the release notes, and it carries a build provenance attestation ([RELEASING.md](RELEASING.md)). Check it before you run it:

```sh
gh attestation verify oci://ghcr.io/thestephenromano/matters-relay@sha256:<digest> \
  --repo thestephenromano/matters
```

Then run that digest instead of building. Next to `compose.yaml`, write `packaging/relay/compose.release.yaml`:

```yaml
services:
  relay:
    build: !reset null
    image: ghcr.io/thestephenromano/matters-relay@sha256:<digest>
```

and start the stack with both files (Docker Compose 2.24.4 or later reads `!reset`):

```sh
docker compose -f packaging/relay/compose.yaml -f packaging/relay/compose.release.yaml up -d
```

Name the image by digest, not by tag: a digest names exactly the image that was verified. To upgrade, verify the new release's digest and replace it in that file (*Upgrades* below).

**A database elsewhere.** The stack's PostgreSQL is on the stack's private network, so its URL needs no TLS. A database on any other host must be reached with `sslmode=verify-full`, which encrypts the connection and checks the server's certificate and name: `postgres://relay@db.example.org:5432/relay?sslmode=verify-full`. The relay trusts the operating system's roots; for a private CA, add `&sslrootcert=/path/to/ca.pem` and mount that file. `sslmode=require` encrypts without checking who answers, and `prefer` (the default) silently falls back to cleartext, so use neither across a network.

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

A dump holds every stored byte. Keep it with the same care as the database, outside the repository checkout if you can; inside it, `*.dump` is ignored by git and by image builds.

`pg_dump` reads one consistent snapshot. Records acknowledged after the snapshot are not in it. Restoring an older backup therefore **loses acknowledged records**. This is not zero data loss: clients re-upload what they still hold, and the relay deduplicates by record ID.

To restore:
1. stop the relay;
2. restore the dump into an empty database (`pg_restore -U relay -d relay --clean`);
3. **start a new storage generation**: `matters-relay new-generation --all`;
4. start the relay.

The new generation makes every client's cursor invalid (`410 generation-changed`). Each client then forgets its cursors, re-queues everything it wrote, and reads the log again. Record identities, membership and epochs travel inside the signed bytes and do not change.

**The drill.** `a_relay_restored_from_a_backup_loses_nothing_its_devices_still_hold` (`crates/matters-sync-client/tests/client_tests.rs`) runs this procedure against the real relay and PostgreSQL:
1. two devices sync;
2. a consistent copy of the relay's database is taken;
3. the relay acknowledges another edit;
4. the database is lost, the copy is restored, and a new generation starts.

The devices re-send what the copy lacked and agree again. The re-sent record is the same record, with the same ID and bytes, and nothing is stored twice. Membership and epochs are unchanged.

The drill shows what the recovery point means in practice. An edit acknowledged after the backup survives *because a device still held it*. If every device that held it is lost too, it is lost.

`matters-relay export` and `import` move one route between deployments with every byte unchanged, under a new generation (SYNC_SERVICE_CONTRACT.md).

Export is operator-only. Run `matters-relay export` in the relay's container, or call `GET /v0/routes/{route}/export` with `MATTERS_RELAY_OPERATOR_TOKEN` on the relay's own listener (port 8484 on the stack's private network). Caddy refuses the path on the public name, so a leaked token alone cannot read a route from outside.

## Upgrades

- **Schema.** The relay's schema migrations run forward at start. An older relay refuses a database a newer one migrated (`an_older_relay_refuses_a_database_a_newer_one_migrated`), so it never serves tables it does not know.
- **Before upgrading.** Take a backup.
- **Rolling back.** Restore that backup with a new generation, as above, so clients re-upload whatever arrived after it.
- **Clients and the API.** The API is versioned (`/v0/`). Clients refuse unknown versions and suites, and a relay answers an unknown version with `unsupported-version`.

| | Relay at the same `/v0` | Relay rolled back to an older build |
| --- | --- | --- |
| **Client, same `/v0`** | Works. Additive endpoints, such as `/snapshots`, `/enrollments` and `/invitations`, are used where present. A client from before `/invitations` cannot add devices through a relay that requires them (`404 unknown-invitation`) | The older relay refuses the newer database (above). Restore the backup taken before the upgrade, with a new generation |
| **Client too old for the relay's `/v0` additions** | Works without them. A device that can't start from a checkpoint reads the whole log | Works |
| **A future `/v1`** | Unknown paths answer `unsupported-version`, and the client asks for attention | — |

On the device, Matters refuses a database written by a newer Matters (`MattersError::NewerSchema`), and leaves it untouched.

## Credentials

| Credential | How to rotate it | Effect |
| --- | --- | --- |
| Database password | `ALTER ROLE relay PASSWORD …`, then update `postgres_password`, then restart the relay | Clients see a brief unavailability and retry. |
| Operator export token | Replace `MATTERS_RELAY_OPERATOR_TOKEN` and restart | The old token stops working at once. |
| Device sessions | Last 15 minutes and are bound to current membership | A removed device loses access at its next request. |
| TLS certificates | Renewed by Caddy | — |

## Limits

Requests without a session can come from anyone, so they are limited by default. Behind Caddy every peer address is Caddy's, and the relay trusts no forwarded header, so these limits are per route:

| Setting | Default | Limits |
| --- | --- | --- |
| `MATTERS_RELAY_PUBLIC_RATE` | `10/60` | Requests without a session per route: challenges, session answers, mailbox uploads, session-free transitions |
| `MATTERS_RELAY_CHALLENGE_RATE` | `1/10` | Session challenges per route and device |
| `MATTERS_RELAY_RATE` | off | Authenticated requests per route and device |

Each is `<per-second>/<burst>`, or `off`. A client over a limit gets `429 rate-limited` and retries after the time it is given.

The server bounds slow and long requests and how many run at once:

| Setting | Default | Bounds |
| --- | --- | --- |
| `MATTERS_RELAY_HEADER_TIMEOUT` | `10` (seconds) | How long a connection may take to send its first bytes and each request's headers; then it is closed |
| `MATTERS_RELAY_REQUEST_TIMEOUT` | `60` (seconds) | How long a request may take to be answered, its body included; then `503 unavailable` |
| `MATTERS_RELAY_MAX_REQUESTS` | `256` | Requests handled at once; past it a request is shed at once with `503 unavailable`, never queued. `/healthz` and `/readyz` are exempt |

Clients retry `503` with backoff, and an upload cut off this way is "not known to be stored", as after any failure. Caddy has its own timeouts in front; these hold for anything that reaches the relay directly.

## Quotas

```sh
matters-relay quota <route-hex> bytes <n>     # or records <n>, or none
```

The byte quota counts records and authority objects. At its quota, a route refuses new records, writer claims and key envelopes with `quota-exhausted`. Membership transitions are still stored, so the admin can remove a device from a full route. They have their own bound instead: at most 2,048 transitions and recoveries per route (about 128 MiB at most), past which they too get `quota-exhausted`. Nothing stored is deleted, and clients keep the refused work locally.

A route's authority log grows with use. It never shrinks, because the relay deletes nothing:
- **Every app open** stores one signed writer claim, about 3.7 KB (a `writer_handoff` is 3,695 bytes, almost all of it the ML-DSA-65 signature).
- **Every membership change** stores the transition (up to 64 KiB with a full roster), plus a key envelope of about 4.9 KB for each member and one for the recovery key.

Writer claims and key envelopes have no count limit. The byte quota is what bounds them, as it bounds records. Set a byte quota on every route you serve (`matters-relay quota <route-hex> bytes <n>`). Without one, nothing bounds a route's storage, and a member that uploads without end fills the disk.

## Ownership and retention

The operator owns the host, its backups and their retention. The relay never deletes history in this version (SYNC_CHECKPOINTS.md, "Retention and quotas"). Storage grows with every record and checkpoint, about 4.8 KB per edit on the workloads measured for #40.

Deleting a route's data is an operator action on the database. It does not remove the copies every device holds.
