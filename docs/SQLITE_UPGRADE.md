# SQLite dependency upgrade and verification

Tracks [#97](https://github.com/thestephenromano/matters/issues/97). The app, standalone sync prototype and optional measurement baseline use the existing rusqlite/SQLite stack. No storage engine, broker or custom SQLite patch is introduced.

## Selected release and provenance

Adopt the maintained upstream release **rusqlite 0.43.0 / libsqlite3-sys 0.40.0**, bundling **SQLite 3.54.1**. The maintainer approved the dependency-upgrade work after the encrypted-outbox experiment. Both checked-in manifests pin rusqlite exactly; both lockfiles pin the FFI crate and archive checksums. The measurement helper independently resolves the selected app checkout and inherits its exact rusqlite pin. Its local lockfile remains untracked and its graph stays outside the sync prototype.

[SQLite's WAL-reset report](https://www.sqlite.org/wal.html#walresetbug) identifies the affected concurrent write/checkpoint path and fixes in 3.51.3 or later, with backports to 3.44.6 and 3.50.7. [SQLite 3.51.3 release notes](https://www.sqlite.org/releaselog/3_51_3.html) establish the fix. The selected [3.54.1 release](https://www.sqlite.org/releaselog/3_54_1.html) includes subsequent changes and publishes the source identity below. [rusqlite 0.42.0](https://github.com/rusqlite/rusqlite/releases/tag/v0.42.0) introduced that bundled SQLite; [0.43.0](https://github.com/rusqlite/rusqlite/releases/tag/v0.43.0) includes later wrapper corrections. Selecting the current wrapper avoids stopping at the first WAL-fixed release while omitting those later corrections.

| Component | Exact version | License | crates.io archive SHA-256 |
| --- | --- | --- | --- |
| rusqlite | 0.43.0 | MIT | `f172c4812a19b26d1c23cdb0070e69db335503dfa378b910eb857386e5b8beba` |
| libsqlite3-sys | 0.40.0 | MIT | `b06e69070b7e12262b0cd90832f0a7396f142a2b330181255562decf9f0c6215` |

SQLite itself is [public domain](https://www.sqlite.org/copyright.html). rusqlite declares Rust **1.89.0** as its minimum; this execution uses Rust 1.99.0. Features preserve bundled native SQLite and statement caching, plus `chrono`/`uuid` in the app. Default features are disabled explicitly so this native dependency upgrade does not select the new optional WebAssembly backend.

The bundled `sqlite3.c` SHA3-256 is:

```text
e2b869d3003b33c942b7dd7063604fbc53553e1b66609570df39b9b7b558ee81
```

It equals the hash published for SQLite 3.54.1. All three actual linked runtime probes return:

```text
sqlite_version(): 3.54.1
sqlite_source_id(): 2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76
```

Before the upgrade, the same probes reported SQLite 3.46.0 and source ID `2024-05-23 13:25:27 96c92aba00c8375bc32fafcdf12429c58bd8aabfcadab6683e35bbb9cdebf19e`. The new exact identity gate failed in each graph before the upgrade and passes afterward. `ci/sqlite_runtime.rs` is test support shared by source inclusion, not a shared Cargo workspace. A future SQLite upgrade must deliberately update that gate and its provenance.

## Integration changes and coverage

The dependency update required reading SQLite `count(*)` results as `i64`, with a checked conversion for the public `usize` journal length. Table counts used only for comparisons remain `i64`. No schema migration or application write semantics change is introduced.

`ci/sqlite_wal_tests.rs` executes independently against the app and prototype dependency graphs:

- A writer commits 32 generations across eight rows with 2 KiB payloads while another connection attempts PASSIVE, FULL, RESTART and TRUNCATE checkpoints. Barriers overlap each pair of attempts; verification checks every final row and payload, integrity and reopen state. These finite fixture sizes provide eight rounds per checkpoint mode; they are not production limits or performance budgets.
- A retained reader snapshot continues to see the old generation after a writer commits. TRUNCATE reports busy while that reader holds the snapshot and completes after it releases it; the reader then sees the committed generation.
- Two child processes exit abruptly before and after a WAL commit. Reopen preserves exactly the expected committed rows. A consistent `VACUUM INTO` backup is reopened and checked for the same contents and integrity. The after-commit case verifies a nonempty WAL remains when the child exits without closing its connection.

The app's ordinary CI runs these tests and the independent baseline runtime probe. Prototype CI runs the shared tests plus its existing replica/outbox crash tests, conformance vectors and both normal and ignored-inclusive discovery. Inactive crash workers return normally during direct discovery, but configured child runs require their exact exit codes.

The [execution record](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) captures before/after probe output, dependency trees, archive/source hashes and final app/prototype commands. Its active source manifest binds prototype inputs. The separately labeled app/baseline hashes and baseline lock contents are revision-bound inventory, avoiding an app dependency-resolution dependency in prototype CI. Historical timing and mutation artifacts remain tied to their recorded versions; no new performance measurement is claimed.

## Journal-mode decision and limits

Keep WAL for the app and earlier replica, now linked to the upstream-fixed release. Keep the encrypted outbox on DELETE/FULL: its bounded contract, contention tests and writer-fencing tests already use rollback journaling, and no measured benefit justifies a mode change in this dependency PR. A later WAL proposal must exercise the outbox's specific commit/contention boundaries and document its durability/concurrency scope.

The integration tests exercise supported APIs and selected schedules on Linux x86_64. They do **not** reproduce SQLite's internal WAL-reset race, prove all interleavings, or establish power-loss, faulty-media, remote-filesystem or cross-target durability. The race-fix claim rests on the upstream-fixed source and its verified identity, independently of these local tests. No occurrence of the old race in Matters has been observed or claimed.
