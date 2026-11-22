# How encrypted sync is verified

Status: the verification map for [#43](https://github.com/thestephenromano/matters/issues/43).

This document lists each property sync claims, the gate that checks it, and the command that reproduces it. It ends with what is **not** claimed.

Every gate runs on each change in CI (`.github/workflows/ci.yml`). `ci/check.sh` runs them all in order, as one command. A broader suite runs weekly (`.github/workflows/weekly.yml`), at a length too long for every change.

## Convergence and no lost edits

`crates/matters-sync-client/tests/convergence_tests.rs` runs three Matters databases through the real relay on PostgreSQL. Each run follows a recorded schedule fixed by its seed, so rerunning a seed replays it exactly. The steps are:
- offline edits, including concurrent edits of one task;
- trashing and emptying the trash;
- syncing;
- lost acknowledgements;
- process restarts;
- checkpoints;
- relay restores into a new storage generation.

After a final round, the run fails if:
- any two devices differ in any task's title, notes, status or completion;
- a task no device purged is missing everywhere (a lost acknowledged edit);
- a task some device purged is back anywhere (unintended resurrection: ADR 0001, case C09);
- any record is left pending;
- an honest checkpoint was refused.

| Run | Seeds | Command |
| --- | --- | --- |
| Every change (`relay against PostgreSQL`) | 8 recorded seeds × 60 steps | `ci/relay-tests.sh` |
| Weekly | seeds 1–48 × 60 steps | `MATTERS_CONVERGENCE_SEEDS=48 cargo test --release -p matters-sync-client --test convergence_tests` |

The expected outcome is defined by the domain, independently of the engine: the same projection on every device, purged tasks absent, nothing else lost. The test reads tasks through `Database::get_task`, never through Loro.

## Authorization

Records and controls a device has no right to apply are refused before anything changes:
- forged and malformed records, another actor's update under a writer's signature, a snapshot posing as an update (`matters-core/tests/receive_tests.rs`);
- records after a writer's signed close, and a cloned device's frozen lineage (also `receive_tests.rs`);
- outsiders' writer claims, and claims under unknown manifests (`matters-sync/tests/authority_tests.rs`);
- dishonest and wrong-epoch checkpoints (`matters-core/tests/checkpoint_tests.rs`).

At the relay:
- uploads need the current roster (`matters-relay/tests/store_tests.rs`, `membership_transitions_authorize_uploads_and_are_verified_first`);
- a removed device's sessions end;
- the root's recovery session is confined to the authority log (`api_tests.rs`);
- the enrollment mailbox serves only the admin.

## Crashes, restarts and restores

Each crash test kills a child process at a named crash point, then reopens the database. None relies on wall-clock sleeps.

| Boundary | Test |
| --- | --- |
| A recorded write's commit | `sync_crash_tests.rs`, `sealing_tests.rs` (`a_crash_at_commit_leaves_the_record_whole_or_absent_and_ids_unique`) |
| Migration: after the backup, before and after the commit | `migrate_tests.rs` (`a_crash_at_each_migration_boundary_…`) |
| Creating a checkpoint | `checkpoint_tests.rs` (`a_crash_while_creating_a_checkpoint_…`) |
| An admission, a recovery | `enrollment_tests.rs`, `membership_tests.rs` |
| The relay before and after its commit | `matters-relay/tests/conformance.rs`, `every_conformance_scenario_passes_against_the_real_binary` (the real binary, killed at its fault points) |
| A relay restore into a new generation | `client_tests.rs` and the convergence schedules; `checkpoint_tests.rs` (covered records met again) |
| A database outage | `conformance.rs`, `a_database_outage_answers_unavailable_and_acknowledges_nothing` |
| A relay restored from a backup that lacks acknowledged edits | `client_tests.rs`, `a_relay_restored_from_a_backup_loses_nothing_its_devices_still_hold` (identities, membership and epochs unchanged) |
| A relay rolled back to an older build | `store_tests.rs`, `an_older_relay_refuses_a_database_a_newer_one_migrated` |

## Identity, nonces and malformed input

- **Fresh record IDs.**
  - Every seal draws a fresh record ID, so no key and nonce are ever reused.
  - Copies, reopens and concurrent handles never share one (`sealing_tests.rs`).
  - Each open is a new writer incarnation.
- **Parsers under fuzzing.** Every parser of outside input has a cargo-fuzz target: records, controls, invitations, typed words and codes, and checkpoint parts. The targets are seeded from independent golden vectors, and the corpora are replayed on stable in the ordinary suite (`fuzz_replay.rs`). CI fuzzes any change to matters-sync for 60 s per target, and the weekly campaign for 600 s.
- **Independent oracles.** Python standard-library encoders check the record contract, invitations and checkpoints byte for byte, as do the golden vectors.
- **Isolation.** Uploads must match their route and session (`store_tests.rs`, `appends_are_idempotent_conflicts_permanent_and_routing_checked`). Cross-route reads and writes are refused (the conformance scenarios), and export and import keep routes apart.

## Performance

Measurements name their host and workload, and live in `docs/evidence/`:

| Record | What |
| --- | --- |
| `sync-engine-cost-2026-11-20.json` | Recording writes for sync, against the write path with sync off |
| `sync-checkpoint-cost-2026-11-21.json` | Checkpoint size and creation, and starting from a checkpoint against receiving every record |

The weekly `checkpoint cost, measured` job runs the same example on a CI runner and keeps its output, to show the trend. A runner is not the evidence host, so no budget is set from it. Budgets on bootstrap and catch-up wait for #241, the per-record projection rebuild.

## One command

`ci/check.sh` runs every gate above that runs per change. Run the PostgreSQL-backed parts on their own with:

```sh
docker compose -f crates/matters-relay/compose.yaml -p matters-relay up -d
MATTERS_RELAY_TEST_DATABASE_URL=postgres://postgres:relay@localhost:55432/postgres \
  MATTERS_RELAY_REQUIRE_DB=1 ci/relay-tests.sh
```

## What is not claimed

These properties are outside what the gates above establish, and nothing should be read as claiming them.

- **Freshness against a withholding relay.** A device can tell that what it received is authentic. It cannot tell that nothing newer exists. A fresh device starting from a checkpoint trusts the relay to serve the newest one (SYNC_CHECKPOINTS.md).
- **Honest authorized members.** A member can sign dishonest content, a dishonest checkpoint included. A device with history detects contradictions with what it holds; a fresh device cannot.
- **Endpoint compromise.** Malware on a device holds its keys and reads its tasks.
- **Availability.** The relay can refuse or delay. Devices keep working and keep their changes, but nothing is synced meanwhile.
- **Metadata.** The relay sees sizes, timing, device counts and addresses (SYNCING.md).
- **Independent review.** The protocol has not had an independent review, which #45 gates release on.
