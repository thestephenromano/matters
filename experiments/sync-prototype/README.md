# Bounded sync adoption prototype

Implements the experiment approved in [ADR 0001](../../docs/decisions/0001-crdt-adoption.md) and [ADR 0002](../../docs/decisions/0002-post-quantum-sync.md). Tracks [#32](https://github.com/thestephenromano/matters/issues/32), [#33](https://github.com/thestephenromano/matters/issues/33), [#35](https://github.com/thestephenromano/matters/issues/35) and [#36](https://github.com/thestephenromano/matters/issues/36).

This standalone Rust workspace is outside the application's dependency graph. It never opens the default Matters database and rejects an existing database without its prototype format marker. **Its record journal is plaintext and unauthenticated.** The crypto tests exercise primitives separately; this is not an encrypted sync client, transport, service or migration tool.

## Run

From the repository root:

```sh
export CARGO_TARGET_DIR="$PWD/target/sync-prototype"
cargo test --manifest-path experiments/sync-prototype/Cargo.toml --locked --features crypto-probe
cargo clippy --manifest-path experiments/sync-prototype/Cargo.toml --locked --all-targets --features crypto-probe -- -D warnings
cargo fmt --manifest-path experiments/sync-prototype/Cargo.toml --check
```

The nested lockfile pins the resolved graph. Its crypto feature enables only the approved PQ draft/provider configuration; it does not enable the incompatible legacy HPKE draft feature. The separate CI workflow checks formatting, Clippy and the tests on Linux x86_64. An aarch64 build is still required before pilot.

`process_crash_worker` is intentionally ignored in ordinary test discovery. Its parent test invokes it twice in subprocesses and checks the exact exit codes and recovered database. It therefore executes as part of the normal test suite without exposing an uncontrolled process-exit path to the application.

## What the experiment establishes

- An ordered Loro tree represents typed home placement and sibling order; a movable list holds independent Today references. One initial lineage creates each task and Today entry together. Duplicate UUIDs and invalid typed local parents fail before commit.
- Independent scalar edits survive; schedule and completion values merge atomically; notes use the shared original text container. Fixed test peer IDs establish concrete winners for scalar and tree conflicts, with wall time excluded from merge priority.
- Explicit and inherited trash remain separate. Permanent UUID tombstones suppress stale edits, moves and restores; newly encountered children survive deletion of an ancestor. Heading deletion preserves tasks in their project. Orphan headings/checklists are retained with a recovery-required marker.
- Identically named tags share a deterministic projected row while original definitions/memberships survive a later rename.
- A SQLite transaction stores the raw update, projection and outgoing flag before local success. A staged document replaces live memory only after commit. Actual SQL rejection and abrupt process exit exercise rollback and durable acknowledgement boundaries.
- Raw incoming records are retained even when their causal dependencies are absent. Restart replays the complete journal; local staging explicitly carries pending imports across a fork. A saved ordinary snapshot never replaces the raw journal. Imported records do not become outgoing records.
- A copied database retains its existing outbox, while each writable open creates a fresh engine peer. Same transport record ID with different bytes is rejected.

The semantic tests use two real SQLite-backed replicas, fixed test peer IDs, reversed delivery and duplicate replay. `C01`–`C17` names map the executed subset to the ADR scenarios; they are **not** a claim that every variant or acceptance criterion in those scenarios is complete. In particular, C15 does not authenticate actor ownership or quarantine conflicting engine counter ranges, and C16 does not implement authenticated import admission. There is no network threat boundary in this crate.

## Crypto evidence and limits

The feature-gated probes compile the pinned HPKE/libcrux and ML-DSA candidates and check:

1. Receiver key derivation and decryption against the published MLKEM768-P256/HKDF-SHA256/**AES-128-GCM** draft vector. [Exact provenance and hashes](tests/vectors/README.md).
2. The selected MLKEM768-P256/HKDF-SHA256/**AES-256-GCM** suite's same-provider round trip, changed info/AAD/ciphertext, wrong recipient, truncated encapsulation and fresh encapsulation on repeated sealing.
3. Pure ML-DSA-65 signing/verification and rejection of a different author, message or domain context. Fixed public test seeds are confined to the test file.

The published vector uses a different AEAD from the proposed application suite. These results do not establish independent interoperability of the full selected suite, KEM/signature certification, constant-time behavior, secure application key storage, or the protocol's security. Canonical signed messages, membership/recovery, record encryption, usage bounds, cross-target execution and independent review remain open. No signing/encryption helper is exposed to production application code.

## Persistence and measurement limits

This adapter favors inspectable transaction semantics: it forks full engine history before a command and rebuilds the whole projection into one JSON SQL row. It scans entities to resolve IDs. These costs are deliberate properties of the experiment and must be removed or budgeted before app integration. This JSON row is not the application's existing relational query projection. The content model omits production timestamp/date validation and the full UI's selection/filtering behavior; notes commands act on the current local text, not a stale displayed editor version.

Today retains one ordering entry per task throughout that task's lineage. The application must still derive Today membership and daytime/evening sections and validate moves within the displayed section. The committed ordering API at `47dcba5` adds `ordering::{place_task, move_task, place_project, place_area}`, `Database::add_*` and `with_conn_mut`; those paths must be captured by domain commands during integration. The independent Today/home intent is exercised here without changing that branch.

There is one intended writer process per prototype database. Multiple processes writing the same file are not a supported replica topology. Restored/cloned OS RNG state, hostile engine payload limits, import quarantine, tombstone admission against malicious authors, stale-history compaction and document/collection identity binding need the later security/service work. Retaining all raw history is mandatory here; no garbage collection is implemented.

For an exploratory comparison against the unchanged Matters core, provide a directory on the filesystem being measured:

```sh
mkdir -p target/sync-prototype/measure
cargo run --release --locked --manifest-path experiments/sync-prototype/Cargo.toml --example measure -- target/sync-prototype/measure 1000
```

The program uses disposable databases under that directory and removes them on normal exit. It creates 1,000 flat tasks with 1 KiB notes, warms up five renames and alternates 100 measured renames against `matters-core::Database`. Both paths use WAL and `synchronous=FULL`. It emits all samples, percentiles, replay/rebuild cost and stored sizes. It compares complete save paths, including this adapter's full-history staging and projection work; it is not an isolated Loro benchmark. This small workload does not meet the full ADR's 10,000/100,000-task, multiwriter, long-offline, memory and history-growth acceptance plan. Use repeated named-host runs before setting production budgets.

[The first execution record](../../docs/evidence/sync-prototype-2026-11-18.json) binds the code revision, lockfile hash, host, workload and all samples. On `betelgeuse` (Intel i9-10920X, Fedora 44 kernel, NVMe/Btrfs), three 1,000-task runs measured prototype save p95 at 27.37–28.83 ms and the existing core path at 0.99–1.62 ms. Prototype replay/reopen was 27.47–29.43 ms. The full-history staging and projection budget buys an inspectable atomic-save boundary for this experiment; incremental integration and larger measurements remain necessary. The development host was shared, without CPU pinning. These numbers do not establish the ADR's 10,000-task budgets, an incremental memory budget or a service SLA.

## Next integration gates

Keep #32/#33/#35/#36 open. Reconcile the final ordering changes; complete missing semantic/admission variants; replace the diagnostic projection with existing SQL views and intentional UI commands; bind real writer identities; build aarch64; measure the full workloads; then implement the reviewed encrypted record and membership contracts. The existing application remains unchanged until migration and UI integration gates are satisfied.
