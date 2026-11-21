# Integrating the sync service into another application

Status: prepared for [#46](https://github.com/thestephenromano/matters/issues/46). This is the plan and guide for a second application, not yet a record of one integrated.

The sync stack splits into a part any application reuses and a part that is Matters' own.

| Layer | Crate | Reusable as is | What it knows |
| --- | --- | --- | --- |
| Records, controls, crypto, checkpoints | `matters-sync` | yes | Nothing about the application: opaque payloads, routes, members, epochs |
| The relay | `matters-relay` | yes | Routes, opaque records, signed public authority objects |
| The client | `matters-sync-client` | yes | The `LocalStore`, `JoiningStore` and `RecoveringStore` traits; the relay's HTTP contract |
| Local persistence, engine, projection | `matters-core` storage | **no**: Matters' tasks. Its device side moves to a shared crate in #246 | Matters' schema, its Loro document and its commands |
| The app session | `matters-sync-app` | as a pattern | Matters' database, and its `MATTERS_APPLICATION` ID |

## What a second application writes

**After #246.** The device side moves out of `matters-core` into a shared crate, behind a small engine trait: apply an update, project, snapshot, the version vector and frontier in the canonical encodings `crdt::encode_version` and `encode_frontiers` define, and the actors an update carries. The shared crate does the security-critical parts:
- sealing inside the application's write transaction, through a call it provides;
- receive: authentication, chain order and pending records;
- membership, enrollment and recovery state;
- checkpoints.

The application then writes only:
1. **an application ID**, routing its collections apart from Matters' (16 bytes, like `MATTERS_APPLICATION`);
2. **the engine trait**, for its own document;
3. **its session wiring**, following `matters-sync-app`.

**Until #246 lands**, those parts exist only inside `matters-core`, written for Matters' document. A second application should wait for #246 rather than copy them. The places they live are listed below, for reviewing #246, not as a template:
- the `LocalStore` implementation in `matters-core/src/storage/sync_store.rs`, as `matters_sync::store` documents each method;
- sealing in the write's own transaction (`matters-core/src/storage/sealing.rs`);
- receive with the authority's checks (`matters-core/src/storage/receive.rs`, SYNC_DURABLE_RECEIVE.md);
- the joining and recovering stores (`Enrolling`, `Recovering`).

The relay needs no change: routes are application-scoped, and the relay never reads payloads.

## A candidate second application

The candidate is the Goals app (#223). It has its own data model (goals and their progress, no tasks), its own repository, and already a link to Matters through goal IDs (#212).

To show the integration works, it must:
- integrate through the crates above, after #246, with no task types and no change to the relay;
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
