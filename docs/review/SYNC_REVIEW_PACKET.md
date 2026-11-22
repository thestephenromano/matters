# Encrypted sync: independent review packet

Status: prepared for [#45](https://github.com/thestephenromano/matters/issues/45). A draft request to a reviewer is in [REVIEW_REQUEST.md](REVIEW_REQUEST.md). The packet is a draft for the maintainer to send to a reviewer they choose. Nothing in it records a review, a finding or a release decision.

This packet gives an independent reviewer what to review, where it is, and what we need answered. The review must assess the **pinned** protocol and implementation at one source revision, which the maintainer fixes when the review starts.

## 1. What the reviewer gets

| Item | Where |
| --- | --- |
| Source revision under review | to be fixed at the start (a tag on `main`) |
| Design and threat model | `docs/SYNC_DESIGN.md` |
| Post-quantum suite decision | `docs/decisions/0002-post-quantum-sync.md` |
| CRDT engine and merge semantics | `docs/decisions/0001-crdt-adoption.md` |
| Relay storage and transport | `docs/decisions/0003-relay-components.md` |
| Content-record contract | `docs/SYNC_RECORD_CONTRACT.md`, `docs/protocol/sync-record-v0.cddl` |
| Key distribution, enrollment, removal, recovery | `docs/SYNC_KEY_DISTRIBUTION.md`, `docs/protocol/sync-key-distribution-v0.cddl` |
| Writer lifecycle and durable receive | `docs/SYNC_WRITER_LIFECYCLE.md`, `docs/SYNC_DURABLE_RECEIVE.md` |
| Checkpoints | `docs/SYNC_CHECKPOINTS.md`, `docs/protocol/sync-checkpoint-v0.cddl` |
| Relay service contract | `docs/SYNC_SERVICE_CONTRACT.md`, `docs/protocol/sync-relay-v0.cddl`, `docs/protocol/sync-relay-v0-conformance.json` |
| Self-hosting and operations | `docs/SELF_HOSTING.md`, `packaging/relay/` |
| User-facing guidance | `docs/SYNCING.md` |
| Verification map | `docs/SYNC_VERIFICATION.md` |
| Evidence records | `docs/evidence/`, `experiments/sync-prototype/evidence/` |
| Implementation | `crates/matters-sync` (codecs, crypto, authority), `crates/matters-core/src/storage` (sealing, receive, membership, checkpoints, migration), `crates/matters-sync-client`, `crates/matters-relay`, `crates/matters-sync-app` |
| Dependency pins and licence policy | `Cargo.lock`, `deny.toml`, the `Cargo.toml` comments naming each decision |

To reproduce every gate, run `ci/check.sh`. The PostgreSQL-backed parts are in `docs/SYNC_VERIFICATION.md`.

## 2. Questions for the reviewer

**Protocol composition**
1. Does the record transcript bind everything a receiver relies on? Header fields, kind, epoch, manifest, author, incarnation and predecessor are each signed with ML-DSA-65 and authenticated by AES-256-GCM, under a per-record HKDF key.
2. Does the derivation of a fresh record ID and key per seal rule out key and nonce reuse? Consider crashes, copied databases, restores from backup and concurrent handles.
3. Are the signature contexts (`matters-sync/v0/<kind>`) and HKDF labels separated enough across every object type?

**Membership and authority**

4. Can a transition, writer claim, envelope or root recovery ever be admitted by a party without the authority the contract states? Include replays, stale manifests and forks.
5. Is the enrollment binding (HMAC under the invitation secret) sound against a relay that substitutes keys, replays requests or withholds the mailbox?
6. Root recovery makes a new generation, signed with the root key derived from the words and the route. Are its freeze-clearing rules and the root session's narrow scope at the relay safe?

**Writers and receive**

7. Can a revoked or stale writer's records, or a frozen lineage's, ever become applied state?
8. Can a malicious member's checkpoint make an honest device with history apply, or lose, anything? A fresh device's limit is stated in SYNC_CHECKPOINTS.md.

**Service boundary and metadata**

9. Does the relay ever need, receive or log content keys or plaintext? Check what it parses: route, kind, record ID, signer, sizes, and the public parts of authority objects.
10. Is the metadata it observes (SYNCING.md) complete and accurately stated?

**Inputs, deployment and updates**

11. Parsers, fuzz targets and limits: what remains unbounded?
12. Self-hosted deployment: secret handling, TLS at the proxy, the metrics' redaction, and backup and restore semantics.
13. Software and update trust: what must change before any claim about resisting future impersonation? Consider package signing and the provenance of updates.

## 3. How findings are recorded

Each finding becomes a GitHub issue that links the report section. It names its severity, an owner, and the evidence that will show it fixed (a test, a gate or a document change).

A release-blocking finding stays open until it is fixed and that evidence passes. A change to the protocol after the review goes back to the reviewer for the changed part.

## 4. Release checklist (draft)

Before real users' data reaches a hosted pilot:
- [ ] The review report covers the pinned revision, and its scope is recorded.
- [ ] No open finding contradicts the security or durability contract.
- [ ] Every accepted residual risk is listed in §5. None waives a core requirement.
- [ ] Every gate in SYNC_VERIFICATION.md passes on the release candidate, the weekly suite included.
- [ ] Dependency versions, supported platforms, build configuration and update-trust assumptions are recorded with the evidence.
- [ ] A rollback plan exists: the previous client release keeps working against the relay (versioned API), and the relay is restored from backup with a new generation (SELF_HOSTING.md).
- [ ] Public claims match §6.
- [ ] The maintainer records release readiness and the pilot's constraints.

## 5. Residual risks to state

- **Compromised endpoints.** A device's malware has its keys and its tasks.
- **A withholding relay.** It can delay or drop. Devices keep their changes, but cannot prove they have everything.
- **Fresh-device freshness.** A device starting from a checkpoint trusts the relay to offer the newest.
- **Honest members.** A member can author dishonest content. Devices with history detect contradictions with what they hold.
- **Historical keys.** A removed device keeps the keys of epochs it was in, and anything it already held.
- **Metadata.** The relay sees each record's clear header and size: who wrote it, when, its writer's chain, its batch and its kind. It also sees membership history, invitation use and the joining device's address and public keys, when each device is connected, device counts and network addresses, and keeps all of it as long as it keeps the collection. `docs/SYNCING.md` lists it in full. Records are not padded.
- **Hostile engine payloads.** A member's signed update or checkpoint is decoded by Loro. Decoding failures and panics are contained to the record, but no CPU or memory bound is established for a hostile signed payload inside the engine.
- **Log growth.** The authority log is not compacted: it grows with every app open and every membership change. A joining or recovering device holds it in memory within the client's caps (`docs/SYNC_KEY_DISTRIBUTION.md` gives the arithmetic), and the relay bounds it with the route's byte quota.
- **Waiting records.** A device keeps a bounded number of records it cannot authenticate yet. A flood of forgeries can push out a genuine one, which the device reads again only after a cursor reset. In effect, that is the relay withholding it.
- **Update trust.** A compromised update channel is a compromised endpoint. No release is signed or carries build provenance yet, so no claim about resisting future impersonation through updates can be made.

## 6. Claims, bounded by evidence (draft)

Claims that may be made once the checklist passes, each with its evidence:
- **Encryption.** Tasks are encrypted and signed on the device with ML-KEM-768 hybrid key distribution, ML-DSA-65 signatures and AES-256-GCM (ADR 0002; the record contract's independent vectors).
- **What the relay holds.** It stores only ciphertext and signed public material (the service contract; the relay's parsing code).
- **Convergence.** Devices converge on recorded adversarial schedules with no lost acknowledged edits (convergence tests, 8 schedules per change and 48 weekly).
- **Review.** "Independently reviewed by <reviewer> at <revision>", once that is true.

Not to be claimed: blanket "quantum-safe" or "unbreakable"; "audited" or "certified" beyond the actual review; zero data loss after a relay restored from an older backup; availability guarantees.
