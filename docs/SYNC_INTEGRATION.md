# Integrating the sync service into another application

Status: prepared for [#46](https://github.com/thestephenromano/matters/issues/46). This is the plan and guide for a second application, not yet a record of one integrated.

The sync stack splits into a part any application reuses and a part that is Matters' own.

| Layer | Crate | Reusable as is | What it knows |
| --- | --- | --- | --- |
| Records, controls, crypto, checkpoints | `matters-sync` | yes | Nothing about the application: opaque payloads, routes, members, epochs |
| The relay | `matters-relay` | yes | Routes, opaque records, signed public authority objects |
| The client | `matters-sync-client` | yes | The `LocalStore`, `JoiningStore` and `RecoveringStore` traits; the relay's HTTP contract |
| Local persistence, engine, projection | `matters-core` storage | **no**: Matters' tasks | Matters' schema, its Loro document and its commands |
| The app session | `matters-sync-app` | as a pattern | Matters' database, and its `MATTERS_APPLICATION` ID |

## What a second application writes

1. **An application ID.** It routes the application's collections apart from Matters' (16 bytes, like `MATTERS_APPLICATION`).
2. **A store implementing `LocalStore`.** It must:
   - persist outgoing records and controls until acknowledged, and resend the same bytes;
   - apply received controls and records durably and idempotently;
   - keep two cursors, and requeue everything on a generation change.

   `matters-core`'s `sync_store.rs` is the worked example, and `matters_sync::store` documents every method.
3. **Sealing in the write's own transaction.** It seals each recorded change with `matters_sync::seal` as the next record of this open's writer (`matters-core/src/storage/sealing.rs`).
4. **Receive with the authority's checks.** It uses `Authority::authenticate_at` and the same chain rules (SYNC_DURABLE_RECEIVE.md, `matters-core/src/storage/receive.rs`).
5. **Optionally, `JoiningStore` and `RecoveringStore`.** They let devices join through invitations and recover from the words, as `Enrolling` and `Recovering` do.

The relay needs no change: routes are application-scoped, and the relay never reads payloads.

## A candidate second application

The candidate is the Goals app (#223). It has its own data model (goals and their progress, no tasks), its own repository, and already a link to Matters through goal IDs (#212).

To show the integration works, it must:
- integrate through the crates above with no task types, and no change to the relay;
- cover local persistence, joining a second device, sync, recovery, and moving between a self-hosted and a hosted relay through export and import (SYNC_SERVICE_CONTRACT.md);
- report the integration effort in changed lines and in days, plus the questions it raised. Each question is a gap in this guide.

## Measuring cost per active collection

The relay's metrics (SELF_HOSTING.md) and the checkpoint example give the inputs. Each is measured on a named host and workload.

| Input | Source | Measured so far |
| --- | --- | --- |
| Retained bytes per edit | `examples/checkpoint_cost.rs` | about 4.8 KB per edit on Matters' workloads (`docs/evidence/sync-checkpoint-cost-2026-11-21.json`) |
| Checkpoint size | same | about 4.7% of retained history |
| Bootstrap time | same | 23–208 ms from a checkpoint at 400–3,200 records (betelgeuse) |
| Requests and latency per route | relay metrics | to measure in a pilot |
| Connections (hint sockets) | relay metrics | to measure |
| Recovery and support time | pilot log | to record per incident |

See [PILOT_PLAN.md](saas/PILOT_PLAN.md) for the cost model and the design-partner plan.
