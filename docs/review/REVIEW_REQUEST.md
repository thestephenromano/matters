# Request for an independent review: Matters encrypted sync

Status: a draft for the maintainer to adapt and send ([#45](https://github.com/thestephenromano/matters/issues/45)). Nothing here has been sent, and no reviewer is engaged. Fill in the bracketed parts before sending.

---

**Subject:** Independent security review of an end-to-end encrypted, post-quantum sync protocol and its implementation (Rust, about 13,000 lines)

Dear [reviewer],

I maintain [Matters](https://github.com/thestephenromano/matters), a personal task manager for the Linux desktop (Qt). Its optional sync keeps a person's devices in step through a relay that stores only ciphertext. I am looking for an independent review of the protocol and its implementation before any real user's data goes through it. I would like your assessment, and if you take this on, your report.

## What it is

- **Tasks.** A user's tasks live in a local SQLite database. Each change is a CRDT update (Loro), sealed on the device as a signed, encrypted record.
- **Cryptography.** ML-DSA-65 signs every record and control message. AES-256-GCM, under a per-record HKDF key, encrypts content. Content keys travel in HPKE envelopes with the MLKEM768-P256 hybrid KEM, which follows a pinned IETF draft, draft-ietf-hpke-pq-05 (through hpke-rs), with that draft's code point. The libcrux implementations are pinned. The full HPKE suite has not yet been checked against an independent implementation (ADR 0002).
- **Membership.** An admin device admits others through one-time invitations (an HMAC binding under the invitation's secret), removes them with a new epoch, and rotates keys. If every device is lost, 24 recovery words restore the collection as a new generation, signed by a root key that the words and the collection's route derive.
- **The relay.** PostgreSQL and an HTTP API store and serve records, authority objects and envelopes. It authenticates device sessions against the roster it verifies, enforces quotas, and never holds content keys or plaintext.
- **Checkpoints.** Full-history snapshots carry a signed statement of what they cover. They let a new device start without replaying everything, and receivers judge them against their own history.

## What I ask you to review

The exact source revision is [revision, to be tagged at the start, e.g. `sync-review-1` at `e0c9b7f`]. The scope is:

| Part | Where | Size |
| --- | --- | --- |
| Codecs, cryptography, authority, checkpoints | `crates/matters-sync` | about 4,300 lines |
| Sealing, receive, membership, enrollment, checkpoints and migration on the device | `crates/matters-core/src/storage` (the sync modules) | about 4,700 lines |
| The client | `crates/matters-sync-client` | about 1,100 lines |
| The relay | `crates/matters-relay` | about 2,500 lines |
| The app's sync session | `crates/matters-sync-app` | about 700 lines |
| The protocol as specified | `docs/SYNC_*.md`, `docs/protocol/*.cddl` | about 13,000 words |

The review packet, `docs/review/SYNC_REVIEW_PACKET.md`, lists every document and artifact, and the questions I most need answered. In short:
- Does the record transcript bind everything a receiver relies on, and can a key or nonce ever be reused?
- Can any transition, writer claim, envelope, enrollment or root recovery be admitted without the authority the contract states?
- Can a revoked, stale or frozen writer's records, or a malicious member's checkpoint, become applied state on an honest device?
- What does the relay observe, and is the metadata exposure stated completely?
- What is unbounded in the parsers, and what must change in deployment, secrets or update trust?

Out of scope, unless you think otherwise: the task-management features, the desktop UIs beyond how they show sync's state, and the CRDT engine's internals (Loro).

## What there is to work from

- `ci/check.sh` reproduces the per-change gates. The relay's tests need the PostgreSQL setup in `docs/SYNC_VERIFICATION.md`; without it they stop the script. The weekly suite (longer fuzzing and the full set of convergence schedules) runs separately, as `docs/SYNC_VERIFICATION.md` describes.
- `docs/SYNC_VERIFICATION.md` maps each claimed property to the test that checks it, and lists what is not claimed.
- Independent Python encoders check the wire formats byte for byte. The invitation and checkpoint oracles use only the standard library; the record-contract and key-distribution generators use `cbor2`, and the record contract's also uses pyca/cryptography.
- Fuzz targets cover every parser of outside input.
- Recorded convergence schedules run three devices through the real relay, with restarts, lost acknowledgements and restores.
- Measurements are recorded under `docs/evidence/`.

## What I would like back

- A report scoped to the revision above. Each finding should have a severity, its location, and what would show it fixed.
- Your view of which findings block a release, and of the residual risks as the packet states them.
- A check of the fixes for the release-blocking findings, once they are in.

## Reporting

The repository is public. Please send findings privately [to the address, or through GitHub's private vulnerability reporting once enabled], not as public issues. I will turn each one into a tracked fix, and publish only with your agreement.

## Practicalities

[Your timeline, the effort you would estimate, and terms.] I am glad to walk you through the design first, and to answer questions throughout.

Kind regards,
[name]
