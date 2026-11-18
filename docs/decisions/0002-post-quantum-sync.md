# ADR 0002: post-quantum recipient envelopes and signed sync records

Date: 2026-11-18. Status: **proposed protocol for evaluation; not approved or production-ready**. Tracks [#33](https://github.com/thestephenromano/matters/issues/33). Implements no cryptography. The application composition below needs interoperability evidence and independent review, even though its cryptographic constructions come from existing specifications.

## Recommendation

Use **HPKE Base mode with MLKEM768-P256, HKDF-SHA256 and AES-256-GCM** for collection-key envelopes, **ML-DSA-65** for independently verifiable authorization/authorship, and **AES-256-GCM** for encrypted content records. Evaluate the pinned `hpke-rs` libcrux provider. Do not implement a hybrid KEM combiner, a ratchet or cryptographic primitives.

The KEM is defined by **draft-ietf-hpke-pq-05**, not a final RFC. Bind that exact revision to a private application suite ID, `matters-sync-pq-v0`, and treat this as an experimental wire format. Its category-3 ML-KEM/ML-DSA parameter choices are a proposed security/performance budget; they do not imply that every component of the hybrid or the whole application has category-3 strength. [PQ HPKE draft](https://datatracker.ietf.org/doc/html/draft-ietf-hpke-pq-05), [ML-KEM standard](https://csrc.nist.gov/pubs/fips/203/final), [ML-DSA standard](https://csrc.nist.gov/pubs/fips/204/final)

The original [HPKE RFC](https://www.rfc-editor.org/rfc/rfc9180.html) supplies the established envelope framework. The selected PQ hybrid does not provide HPKE authenticated-KEM mode; use Base mode and verify a separate authorized ML-DSA signature over the envelope. HPKE encryption alone does not establish sender identity.

## Alternatives and exact candidates

Checksums, full source revisions, source-file hashes and licenses are in the [dependency evidence](../evidence/sync-dependencies-2026-11-18.json). These are direct candidate pins, not a resolved dependency graph or a successful build.

| Component | Candidate and configuration | Reason / limit |
| --- | --- | --- |
| Recipient envelopes | `hpke-rs =0.8.0`, default features disabled; `std`, `libcrux`, `draft-ietf-hpke-pq` | Existing HPKE implementation and hybrid combiner. MPL-2.0. Do not use `--all-features`: the older Connolly draft feature is mutually exclusive. |
| Crypto provider | `hpke-rs-libcrux =0.8.0`, matching PQ feature and system RNG | MPL-2.0. Its inspected source explicitly implements the pinned PQ draft's P-curve hybrids. Use its established KDF/AEAD interfaces for record protection; resolve/pin `hpke-rs-crypto` and the full transitive graph before code. |
| Hybrid primitives | `libcrux-kem =0.0.10` as inspected implementation evidence | Apache-2.0. No application-level combiner. Verify the actually resolved provider graph matches the inspected implementation. |
| Signatures | `libcrux-ml-dsa =0.0.11`, defaults disabled, `std`, `mldsa65` | Apache-2.0. Use the public pure-ML-DSA API with explicit context and fresh signing randomness; no hazmat/internal-signing API. |
| Local secret service | `secret-service =5.2.0`, `rt-tokio-crypto-rust` candidate | MIT OR Apache-2.0; Rust 1.87 minimum. Runtime choice remains subject to the client integration decision. |
| Alternative primitives | `aws-lc-rs =1.19.0` | ISC AND (Apache-2.0 OR ISC). Packaged KEM/signature APIs include ML-KEM and ML-DSA, but this inspection did not establish a compatible complete hybrid HPKE adapter. Keep as independent primitive/vector comparison; building an adapter needs a separate applicability decision. |
| Group alternative | `openmls =0.9.2` / MIT; Rust 1.91 minimum | MLS supplies group membership/epochs and forward-secrecy machinery. The inspected package README lists classical suites. It does not meet this PQ requirement as inspected. Retained document history also needs its own policy. Defer rather than implement custom TreeKEM. |

The provider also exposes `XWingDraft06`; that identifier must not be described as compatibility with a later X-Wing draft. Prefer the explicitly inspected P-curve hybrid for this experiment. The immutable [provider source](https://github.com/celabshq/libcrux/blob/42d68bd49b244b09bd02626c6ffa95e54dc64f91/crates/protocols/hpke/libcrux_provider/src/lib.rs) and checksummed archive are the evidence for these feature/API findings. [MLS architecture](https://www.rfc-editor.org/rfc/rfc9420.html), [OpenMLS package](https://docs.rs/crate/openmls/0.9.2/source/README.md)

Required pilot targets are native Linux x86_64 and aarch64. Source inspection is not target validation. Build/test both with their actual CPU backends, including portable fallback, and record exact compiler/flags. Review each selected module's verification coverage, generated-code provenance, constant-time assumptions, zeroization and secret copies. libcrux's documented pre-release/assurance limits do not establish compiled whole-program side-channel resistance. Nothing here claims FIPS validation of Matters or of this configuration. MPL obligations and the complete dependency license graph need review before distribution.

## Security boundary

Trust an uncompromised client, its OS, OS entropy and approved cryptographic implementation. The relay, transport path and service operator may record, reorder, replay, substitute, fork or withhold data. An authorized writer may submit hostile bytes. Keep the parser/importer resource-bounded and reject invalid authorship before CRDT import.

Protect content, history, snapshots, keys and recovery material from the relay, including harvest-now-decrypt-later exposure. Independently authenticate device enrollment, administrative transitions and record authors. Login is service access control only; a password reset, account administrator or substituted server key cannot authorize decryption. TLS remains an additional transport boundary, never the only protection for content keys.

Visible metadata includes account/tenant and collection IDs, public device keys/roles, document routing IDs if included, sizes, timing, IP addresses and access patterns. This proposal does not hide that metadata. It does not prevent an authorized writer from damaging content, a compromised unlocked device from reading it, or the relay from destroying its only copy.

Clients pin authenticated membership/checkpoint heads and reject rollback relative to those pins. A new or isolated client cannot prove global freshness from an adversarial relay alone; it needs an independent trusted-device/recovery checkpoint. Fork detection is conditional on contradictory evidence becoming available. Rotation and revocation do **not** establish forward secrecy or post-compromise security while historical keys remain recoverable.

## Encoding and suite contract

Proposed wire encoding is [RFC 8949 core deterministic CBOR](https://www.rfc-editor.org/rfc/rfc8949.html#section-4.2.1), using fixed-position arrays, byte strings and unsigned integers. Text fields use UTF-8; security identifiers are bytes. No floats, indefinite lengths, duplicate/unknown fields or alternative integer encodings. Reject noncanonical input before cryptographic interpretation. A codec implementation is selected in [#34](https://github.com/thestephenromano/matters/issues/34), before writing a codec. This ADR specifies the semantic arrays; golden wire bytes and codec conformance are still required.

All signed messages have `B = [protocol_version, suite_id, message_type, tenant_id, collection_id, signer_id, body]`, with version `0` and the exact suite string above. Sign canonical `B` using pure ML-DSA-65 with context `matters-sync/v0/<message_type>`. The outer object is `[B, signature]`; verify `signer_id` against the authority required by the message table, not merely against a key supplied with the message. Domain labels are ASCII. Hash referenced signed objects with SHA-384 over their complete canonical bytes. IDs are fixed-length opaque byte strings, with lengths fixed in the codec schema; application UUIDs are 16 bytes, random record/challenge/incarnation IDs are 32 bytes and hashes are 48 bytes.

Suite internals: HPKE KEM `MLKEM768-P256` (`0x0050` in the pinned draft), KDF HKDF-SHA256 (`0x0001`), AEAD AES-256-GCM (`0x0002`), Base mode (`0`). Content AEAD uses a 256-bit key, 96-bit nonce and full 128-bit tag. Signature keys/signatures use the standard ML-DSA-65 encodings. Fingerprints hash the canonical `[algorithm, public_key]`, never an untyped key string. Algorithm/format changes require a new suite and authenticated migration; reject unknown suites with a recoverable upgrade-required error. There is no fallback to classical-only key exchange or signatures.

## Trust chain and message bodies

Use one offline recovery root and one active administrative device per collection for v0. Normal device writes are concurrent; administrative transitions are deliberately serialized. The service can store transitions but cannot choose their authority. The root is an ML-DSA-65 public key pinned at bootstrap, with its private key encrypted only in the recovery bundle after bootstrap. Every device has separate ML-DSA signing and hybrid-KEM recipient key pairs. Device IDs bind their public-key fingerprints, and replacing either key requires an authorized transition.

| Type | Body fields in order | Authorization / invariant |
| --- | --- | --- |
| `genesis` | `root_public_key, recovery_kem_public_key, generation, roster, admin_id, epoch, initial_checkpoint_hash` | Root-signed, generation/epoch zero; establishes sequence zero. Roster entries are `[device_id, signature_public_key, kem_public_key, role]`, sorted by device ID; roles are writer or writer+admin. Exactly one admin. Trust requires local creation or verified out-of-band root binding. |
| `membership` | `generation, sequence, previous_manifest_hash, roster, admin_id, epoch, recovery_kem_public_key, closed_epoch_frontier` | Signed by the previous manifest's admin; sequence increments exactly once, previous hash matches, epoch advances for removal/key changes. The frontier is a sorted list of `[writer_incarnation, signed_record_head_hash]` closing admissible old history. Same sequence with different valid bytes is a fork, not last-write-wins. |
| `admin_accept` | `generation, proposed_manifest_hash, new_admin_id` | Countersigned by new admin before a transfer manifest becomes active. Old admin's signature remains required. A pending proposal is not authority. |
| `recovery_transition` | `new_generation, previous_known_manifest_hash, recovery_nonce, new_roster, new_admin_id, new_epoch, new_recovery_kem_public_key, closed_epoch_frontier` | Root-signed, increments generation relative to the anchored history; new admin countersigns. Retained devices require extension of their known chain or explicit recovery reconciliation. Conflicting root recoveries are quarantined, never resolved by server order or timestamps. |
| `enroll_request` | `request_id, root_hash, current_manifest_hash, device_id, signature_public_key, kem_public_key, device_challenge` | New device self-signs as proof of signing-key possession; this grants no authority. Admin verifies the complete binding through an authenticated QR exchange. |
| `enroll_challenge` | `request_hash, challenge_commitment, manifest_hash, hpke_encapsulation, ciphertext` | Admin-signed; fresh secret challenge encrypted to the candidate KEM key with request/root binding in HPKE info/AAD. Only its SHA-384 commitment is clear; publishing the challenge itself would defeat KEM possession proof. Request ID is single-use. |
| `enroll_finish` | `request_hash, challenge_hash, recovered_challenge` | New device signs. Admin verifies both challenges/request, explicit local enrollment confirmation and current manifest before adding the device. |
| `key_envelope` | `manifest_hash, epoch, recipient_id, recipient_key_fingerprint, purpose, hpke_encapsulation, ciphertext, authority_id` | Current admin signs; recipient must be in roster or the configured recovery recipient. Plaintext is `[epoch, content_epoch_key, key_policy]`. All clear fields except ciphertext/encapsulation form HPKE AAD; info is canonical `[suite_id, tenant_id, collection_id, purpose, recipient_key_fingerprint]`. One fresh Base-mode context per envelope. |
| `writer_start` | `manifest_hash, device_id, incarnation_id, engine_actor_id, predecessor_incarnation_or_null` | Authorized device signs; pins an actor namespace to a device. Reject ambiguous reuse/overlapping counter ranges. A restore creates a fresh incarnation. |
| `data` / `snapshot` | `header, ciphertext, author_device_id` | Authorized author signs the entire body. Header fields are specified below. Verify signature, membership, context, bounds and AEAD before importing. Snapshots additionally require coverage evidence in #40; a signature alone is insufficient. |

The local codec schema must make the table's field types, exact lengths, enum values and limits executable, with rejected-format fixtures. Until that work and byte vectors land, this is a reviewable protocol proposal, not a frozen interoperable implementation or grounds to close #33's complete-message acceptance item.

Administrative requests arriving concurrently queue at the single admin. If the admin is unavailable, ordinary local edits continue, but enrollment/revocation waits for admin recovery or root recovery. Recovery is an explicit user operation; it is not automatically triggered by a network timeout. A cloned admin producing two valid successors causes a visible fork requiring reconciliation. This availability cost is intentional for a first personal-device implementation; multi-admin/quorum management needs an existing protocol evaluation later.

## Record protection, retries and restore

The data header is the canonical array `[version, suite_id, tenant_id, collection_id, document_id, record_type, epoch, manifest_hash, author_device_id, incarnation_id, record_id, logical_batch_id, previous_record_hash]`. Reject disagreement between the header and outer signed message's suite, context, type or author. `record_id` is a fresh 32-byte OS-random value on **every new sealing operation**, independent of restored database counters. `logical_batch_id` identifies the same application batch across permitted epoch re-encryption; engine change IDs provide final idempotency. Null previous hash denotes the first record for an incarnation. Do not infer a global ordering from these per-writer chains.

For a content epoch key `CEK` generated from 32 OS-random bytes:

```text
PRK = HKDF-Extract-SHA256(salt = record_id, IKM = CEK)
K   = HKDF-Expand-SHA256(PRK,
        info = CBOR(["matters-sync/v0/record-key", header]), L = 32)
nonce = 12 zero bytes
ciphertext = AES-256-GCM.Seal(K, nonce, plaintext, AAD = CBOR(header))
```

This proposes one AEAD invocation per derived key; the fixed nonce is safe only under that single-use invariant and the KDF assumptions. Never edit plaintext under an existing sealed header/key. Reserve locally generated record IDs durably before sealing and reject known reuse; retain abandoned reservations. Write the exact signed ciphertext to the SQLite outbox before network I/O. Retries send those bytes unchanged. Changing payload, author, epoch or format always creates a fresh record ID/key. If a crash interrupts sealing before persistence, reseal with fresh OS entropy and a new reservation. Do not restore a serialized RNG state. Reservations cannot coordinate isolated clones; their remaining collision risk is bounded below. [HKDF](https://www.rfc-editor.org/rfc/rfc5869.html), [GCM](https://csrc.nist.gov/pubs/sp/800/38/d/final)

Random IDs remove reliance on a copied monotonic nonce counter; they do not make collisions impossible. For `N` honest random IDs, the birthday bound is at most `N(N−1)/2^257`. A provisional cap of `2^32` records per epoch leaves that ID-collision probability below `2^-193`. Derived-key collision probability, GCM block/authentication bounds and adversarial verification attempts need their own analysis; this calculation is not a complete security proof. Proposed record plaintext cap is 1 MiB, with larger snapshots chunked under separately sealed records. It bounds single-record memory and cryptographic work; measurements must justify or revise it. Quota, failed-verification and epoch-rotation limits remain release-blocking review items, not hidden defaults.

Use a fresh OS-seeded one-shot HPKE context for each envelope; do not reuse/restore its sequence state. Abort key generation, encryption or enrollment if entropy acquisition fails. Do not substitute predictable randomness for availability. Repeated enrollment/key delivery can resend exact stored envelopes or generate fully fresh ones.

On database clone/restore, retain previously sealed outbox bytes and original change IDs. Create a fresh writer incarnation/engine actor before new edits; cryptographic keys can remain the same authorized device identity, but a separate physical device should complete enrollment with new keys. Detect known actor collisions and quarantine ambiguity. No claim is made that every copied running process or cloned OS RNG can be detected; process/VM snapshots of live secret/RNG state are outside the trusted-OS assumption.

## Membership, stale writes and historical access

Genesis and each membership transition bind all recipient keys. Enrollment verifies the root fingerprint and full request/challenge transcript over a bidirectional QR exchange; displaying a server-supplied key without that binding is insufficient. Send content keys only after the authorized manifest is accepted. New personal devices receive all retained history keys under the explicit whole-collection policy.

Revocation advances the key epoch, generates a fresh CEK, and issues envelopes only to retained devices plus the recovery recipient. Retained writers refresh authenticated membership before upload; a malicious server can still hide a newer transition from an isolated writer. Clients that know the transition reject newly presented old-epoch records outside the signed closing frontier. A device timestamp does not prove a record predates revocation.

Records included in the closing frontier remain valid historical input even if their author is later revoked. Still-authorized devices with offline changes outside that frontier preserve them and reauthorize/reseal under the current epoch with the same logical change identities. A revoked device's new backlog is not automatically accepted; an authorized user may inspect/export and explicitly import it under a new provenance record. The frontier must be computed from authenticated record chains, not an opaque server assertion. Full frontier/coverage verification is shared with #34/#40 and is a required prototype gate.

The relay independently enforces current upload permissions for operational abuse control. Client authorization remains authoritative against a malicious relay. Detect duplicate record IDs with different bytes as conflicts. Preserve missing dependencies and original records while fetching ancestors. A snapshot may import changes by many authors only with the authenticated original history/coverage required by #40; a snapshot signer cannot simply claim every contained engine actor as their own.

## Recovery and local key storage

Generate a separate 32-byte random recovery secret, offline root signing key pair and recovery hybrid-KEM key pair at bootstrap. Do not derive this secret from an account password. Encrypt a recovery bundle containing the root private key, recovery KEM private key, genesis/root binding and checkpoint anchor. Use the same reviewed HKDF/AEAD pattern with an independent `matters-sync/v0/recovery-key` domain label, random bundle ID and the recovery secret as IKM; the canonical recovery header binds suite, collection, root hash and bundle version. No collection content key is used to protect its own recovery secret.

The user stores the high-entropy secret separately from the encrypted bundle, with an integrity-checked export and restore rehearsal. After verifying recovery availability, remove the bootstrap root private key from routine online device storage. Its ciphertext can be stored by the service; the service must never obtain the recovery secret or unencrypted root/private KEM keys. Ordinary device private keys and retained CEKs live in the desktop Secret Service or in memory while unlocked, not plaintext application config.

Every new epoch supplies an admin-signed HPKE key envelope to the **recovery public key**, allowing the existing recovery private key to recover subsequent history without rewriting the offline secret after every edit. Verify the complete root/admin chain and recipient/epoch binding when using those envelopes. Recovery still depends on availability of the encrypted bundle, envelopes and history. Test their backup/restore as one unit. If the recovery public key/root changes, produce and verify a new recovery export; older recovery material retains whatever past access it already had.

With all ordinary devices lost, decrypt the bundle locally, verify its anchor against available history and any independent checkpoint, then root-authorize a new generation/admin and rotate CEKs. A stale recovery bundle plus an adversarial server cannot prove the latest global state; report this limit before reconciling conflicting history. Losing all authorized devices and usable recovery material means permanent data loss. There is no operator bypass.

Use the Linux [Secret Service](https://specifications.freedesktop.org/secret-service/latest/) via the candidate crate for persistent secrets. If unavailable or locked, fail closed for persistence: offer an explicitly unlocked memory-only session or keep sync locked; never silently write plaintext keys. The local D-Bus/keyring boundary relies on the trusted OS and is not claimed to be post-quantum secure against a hostile local observer. The existing SQLite database, WAL, exports and backups remain plaintext. Full-device encryption or encrypted SQLite is a separate product decision; E2EE is not a device-at-rest claim. Prevent keys/plaintext from entering logs, crash reports and diagnostic uploads; assess memory copies and best-effort zeroization separately.

## Verification and remaining decisions

Independent protocol review must cover the composition, authority/recovery state machine, record-key derivation, canonical encoding, KEM key validation, authentication order, bounds, side channels, secret lifecycle, rollback/forks and all classical-only bypass paths. Recheck [FIPS 203](https://csrc.nist.gov/pubs/fips/203/final) and [FIPS 204](https://csrc.nist.gov/pubs/fips/204/final) errata at implementation time; both publication pages currently carry errata notices. Use [NIST SP 800-227](https://csrc.nist.gov/pubs/sp/800/227/final) for KEM usage review. No errata conformance or audit completion is asserted here.

| Gate | Required evidence / owning issue |
| --- | --- |
| Adoption decision | Maintainer approval of this bounded experiment, draft-wire-format exposure and local secret-storage boundary; #33 stays open pending complete decisions. |
| Build/provenance | Resolved lockfile, exact features/licenses, x86_64/aarch64 builds, portable and optimized paths, security-update owner; #36. |
| Primitive/envelope correctness | NIST vectors and published HPKE vectors, plus independent cross-implementation fixtures for the **exact** MLKEM768-P256/HKDF-SHA256/AES-256-GCM combination. Available vectors for other combinations are insufficient; #36. |
| Wire schema | Exact CBOR bytes, IDs/enums/limits, malformed/noncanonical rejection, length bounds before allocation; #34/#36. |
| Auth/context attacks | Change each header/envelope field, switch tenant/collection/device/epoch, replace keys, forge author, downgrade suite, replay enrollments, inject invalid public keys; reject before state mutation; #36/#37. |
| Nonce/restore | Crash before/after sealing/SQLite commit/send/ACK; retry; cloned DBs with concurrent writers; forced random-ID collision; entropy failure. A conflicting ID must never permit altered ciphertext overwrite; #36/#39. |
| Lifecycle | Simultaneous admin proposals, cloned-admin fork, transfer failure, revoked offline backlog, valid historical frontier, lost-admin recovery, conflicting recoveries, stale recovery export, missing epoch envelope and total key loss; #37/#40. |
| Secret boundary | Keyring locked/absent, log/crash artifact scans, service storage inspection and restore rehearsal; #37/#43. |
| Production release | Quantified key/AEAD/verification budgets; authenticated snapshot coverage; full independent protocol/implementation review with resolved release blockers; #43/#45. |

The implementation may proceed only after the component adoption decision. Production still requires the separate gates above. Replacing the hybrid, codec, KDF construction or authority model is a protocol change needing a new reviewed decision, not a dependency bump disguised as maintenance.
