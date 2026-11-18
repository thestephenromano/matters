# Bounded sync adoption prototype

Implements the experiment approved in [ADR 0001](../../docs/decisions/0001-crdt-adoption.md) and [ADR 0002](../../docs/decisions/0002-post-quantum-sync.md). Tracks [#32](https://github.com/thestephenromano/matters/issues/32), [#33](https://github.com/thestephenromano/matters/issues/33), [#35](https://github.com/thestephenromano/matters/issues/35) and [#36](https://github.com/thestephenromano/matters/issues/36).

This standalone Rust workspace is outside the application's dependency graph. It never opens the default Matters database and rejects an existing database without its prototype format marker. **Its record journal is plaintext and unauthenticated.** The crypto tests include a separate in-memory key-distribution flow; this is not an encrypted sync client, transport, service or migration tool.

## Run

From the repository root:

```sh
export CARGO_TARGET_DIR="$PWD/target/sync-prototype"
cargo test --manifest-path experiments/sync-prototype/Cargo.toml --locked --features crypto-probe,record-contract
cargo clippy --manifest-path experiments/sync-prototype/Cargo.toml --locked --all-targets --features crypto-probe,record-contract -- -D warnings
cargo fmt --manifest-path experiments/sync-prototype/Cargo.toml --check
```

The nested lockfile pins only the prototype graph. The optional baseline helper in `baseline/` is an independent workspace and executable; its local lockfile is regenerated against the app checkout selected for measurement. An app dependency bump does not participate in prototype dependency resolution. Its crypto feature enables only the approved PQ draft/provider configuration; it does not enable the incompatible legacy HPKE draft feature. The separate CI workflow checks formatting, Clippy and the tests on Linux x86_64. An aarch64 build is still required before pilot.

`process_crash_worker` is intentionally ignored in ordinary test discovery. Its parent test invokes it twice in subprocesses and checks the exact exit codes and recovered database. It therefore executes as part of the normal test suite without exposing an uncontrolled process-exit path to the application.

## What the experiment establishes

- An ordered Loro tree represents typed home placement and sibling order; a movable list holds independent Today references. One initial lineage creates each task and Today entry together. Duplicate UUIDs and invalid typed local parents fail before commit.
- Commands use the visible model: a tag removal clears all known same-name memberships for that task, while unseen concurrent additions remain mergeable. Home moves resolve positions among live projected siblings of the same kind, including children recovered through deleted containers. Today moves exclude permanently deleted references before mapping to engine positions.
- Independent scalar edits survive; schedule and completion values merge atomically; notes use the shared original text container. Fixed test peer IDs establish concrete winners for scalar and tree conflicts, with wall time excluded from merge priority.
- Explicit and inherited trash remain separate. Permanent UUID tombstones suppress stale edits, moves and restores; newly encountered children survive deletion of an ancestor. Heading deletion preserves tasks in their project. Orphan headings/checklists are retained with a recovery-required marker.
- Identically named tags share a deterministic projected row while original definitions/memberships survive a later rename.
- A caller retains a serializable `Request` before submission. Its ID, complete command batch and exact output record are committed atomically; retrying the same request returns that record, including after restart or acknowledgement loss. Reusing the ID for different commands is rejected. A newly generated ID means a new intent.
- A SQLite transaction stores the raw update, projection and outgoing flag before local success. A staged document replaces live memory only after commit. Actual SQL rejection and abrupt process exit exercise rollback and durable acknowledgement boundaries.
- Raw incoming records enter a durable inbox before import. Rejections retain original bytes and a reason in quarantine. `receive` returns Applied or Pending; `inbox()` exposes Received, Pending, Applied and Quarantined records. Storage failures retain Received for admission after restart. Quarantine is diagnostic; it does not authorize discarding dependencies or accepting invalid state.
- Checkpoints atomically store an ordinary snapshot, separately exported pending updates and the journal sequence they cover. Restart imports that checkpoint and replays only its journal tail. Original raw history and request receipts remain retained. `restore_stats()` makes the actual restoration path observable. Legacy prototype databases migrate transactionally; old snapshots with no known cutoff replay the retained journal. Imported records do not become outgoing records.
- A copied database retains its existing outbox, while each writable open creates a fresh engine peer. Same transport record ID with different bytes is rejected.

The semantic tests use two real SQLite-backed replicas, fixed test peer IDs, reversed delivery and duplicate replay. `C01`–`C17` names map the executed subset to the ADR scenarios; they are **not** a claim that every variant or acceptance criterion in those scenarios is complete. In particular, C15 does not authenticate actor ownership or quarantine conflicting engine counter ranges, and C16 does not implement authenticated import admission. Durable byte/domain quarantine is covered separately by the review regression tests. There is no network threat boundary in this crate.

## Crypto evidence and limits

The feature-gated probes compile the pinned HPKE/libcrux and ML-DSA candidates and check:

1. Receiver key derivation and decryption against the published MLKEM768-P256/HKDF-SHA256/**AES-128-GCM** draft vector. [Exact provenance and hashes](tests/vectors/README.md).
2. The selected MLKEM768-P256/HKDF-SHA256/**AES-256-GCM** suite's same-provider round trip, changed info/AAD/ciphertext, wrong recipient, truncated encapsulation and fresh encapsulation on repeated sealing.
3. Pure ML-DSA-65 signing/verification and rejection of a different author, message or domain context. Fixed public test seeds are confined to the test file.

The published vector uses a different AEAD from the proposed application suite. These results do not establish independent interoperability of the full selected suite, KEM/signature certification, constant-time behavior, secure application key storage, or the protocol's security. Canonical content/control messages and a first encrypted update are exercised in the bounded tests below. Durable membership/recovery and encrypted journal integration, usage bounds, cross-target execution and independent review remain open. No signing/encryption helper is exposed to production application code.

## Persistence and measurement limits

This adapter favors inspectable transaction semantics: it forks full engine history before a command and rebuilds the whole projection into one JSON SQL row. A shallow UUID/kind index resolves entity identity without converting notes; it is updated on local creates and rebuilt once after import. Receive validates and persists one newly computed projection against cached accepted state. Eight-byte fractional-index jitter is configured once per document instance, with peer-seeded engine randomness; explicit fixture initialization selects zero jitter. These are encoding/allocation choices, not cryptographic randomness guarantees. These costs are deliberate properties of the experiment and must be removed or budgeted before app integration. This JSON row is not the application's existing relational query projection. The content model omits production timestamp/date validation and the full UI's selection/filtering behavior; notes commands act on the current local text, not a stale displayed editor version.

Today retains one ordering entry per task throughout that task's lineage. The application must still derive Today membership and daytime/evening sections and validate moves within the displayed section. The committed ordering API at `47dcba5` adds `ordering::{place_task, move_task, place_project, place_area}`, `Database::add_*` and `with_conn_mut`; those paths must be captured by domain commands during integration. The independent Today/home intent is exercised here without changing that branch.

There is one intended writer process per prototype database. Multiple processes writing the same file are not a supported replica topology. Restored/cloned OS RNG state, hostile engine payload limits, authenticated admission and repair of quarantined causal streams, tombstone admission against malicious authors, stale-history compaction and document/collection identity binding need the later security/service work. Retaining all raw history is mandatory here; no garbage collection is implemented.

For an exploratory comparison against the unchanged Matters core, provide a directory on the filesystem being measured:

```sh
mkdir -p target/sync-prototype/measure
cargo build --release --manifest-path experiments/sync-prototype/baseline/Cargo.toml
cargo run --release --locked --manifest-path experiments/sync-prototype/Cargo.toml --example measure -- target/sync-prototype/measure 1000 "$CARGO_TARGET_DIR/release/matters-sync-baseline"
```

The program uses disposable databases under that directory and removes them on normal exit. It creates 1,000 flat tasks with 1 KiB notes, warms up five renames and alternates 100 measured renames against `matters-core::Database`. Both paths use WAL and `synchronous=FULL`. They run in separate processes, alternating requests; the baseline reports its internal save duration, excluding process startup and IPC. Record both lockfile hashes and the app source commit for each run. It emits all samples, percentiles, replay/rebuild cost and stored sizes. It compares complete save paths, including this adapter's full-history staging and projection work; it is not an isolated Loro benchmark. This small workload does not meet the full ADR's 10,000/100,000-task, multiwriter, long-offline, memory and history-growth acceptance plan. Use repeated named-host runs before setting production budgets.

[The historical first execution record](../../docs/evidence/sync-prototype-2026-11-18.json) remains bound to its original code revision. [The review execution record](evidence/review-2026-11-18.json) binds the corrected adapter, both dependency graphs and all refreshed samples. On `betelgeuse` (Intel i9-10920X, Fedora 44 kernel, NVMe/Btrfs), three 1,000-task review runs measured prototype save p95 at 23.86–26.59 ms and the separate app baseline at 1.11–2.13 ms. Full-history staging and projection rebuilding remain included. The host was shared, without CPU pinning, and the baseline process arrangement changed; these runs do not isolate the cause of timing differences. They do not establish the ADR's larger-workload, memory or service budgets.

## Next integration gates

Keep #32/#33/#35/#36 open. Reconcile the final ordering changes; complete missing semantic/admission variants; replace the diagnostic projection with existing SQL views and intentional UI commands; bind real writer identities; build aarch64; measure the full workloads; then implement the reviewed encrypted record and membership contracts. The existing application remains unchanged until migration and UI integration gates are satisfied.

## Review assurance

[The review record](REVIEW.md) maps the reported findings to their owning invariants, regression evidence and remaining limits. No production encryption or authenticated-device claim follows from these local persistence corrections.

## Content-record contract experiment

The opt-in `record-contract` feature exposes a bounded minicbor adapter for the [experimental content-record contract](../../docs/SYNC_RECORD_CONTRACT.md). It is not connected to the replica journal or the application. `wire::parse` returns structural validity only; membership, signature, epoch/actor/chain admission, AEAD and domain validation remain mandatory downstream gates. Snapshot bytes are defined, but authoritative snapshot import still requires authenticated coverage.

[Golden fixtures](tests/vectors/record-contract/README.md) compare exact CBOR transcripts with Python cbor2, validate the CDDL shape through cddl-cat/Ciborium, and cross-check HKDF/AES bytes with Python cryptography. Signature tests use the selected ML-DSA provider and public synthetic keys; they do not establish independent signature or hybrid HPKE interoperability.

## Key-distribution contract experiment

The [key-distribution contract](../../docs/SYNC_KEY_DISTRIBUTION.md) and [independent fixtures](tests/vectors/key-distribution/README.md) specify pinned-root genesis, device-key fingerprints, removal/rotation, writer-start and signed recipient envelopes. With both crypto features enabled, the integration tests distribute a real CEK between two devices through hybrid HPKE, then authenticate, decrypt and import the admitted actor's first Loro update. All authority and secrets remain in test memory. The support module is not an application API or a durable encrypted journal; enrollment, recovery, historical admission and crash-safe sealing remain open.

## Encrypted-outbox persistence experiment

The [outbox contract](../../docs/SYNC_ENCRYPTED_OUTBOX.md) and test-only SQLite component implement committed record-ID/header reservations, immutable signed ciphertext, writer-head commits and local ACK bookkeeping. The harness verifies a real encrypted Loro update and nine abrupt process-exit boundaries. Reopen fences older signing handles and allocates a fresh local writer. Already-sealed bytes remain deliverable; unsealed old-writer intents remain visible as recovery-required and cannot be signed from a potentially restored head. Delivery-only and read-only opens allocate no writer. This uses a separate rollback-journal database. Pending plaintext stays local and unencrypted at rest. Transport, secret storage, historical admission and domain-journal integration remain separate gates. The ignored `outbox_crash_worker` runs nine times through its parent test.
