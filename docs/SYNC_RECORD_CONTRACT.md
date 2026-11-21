# Experimental encrypted content-record contract

Date: 2026-11-18. Tracks [#33](https://github.com/thestephenromano/matters/issues/33), [#34](https://github.com/thestephenromano/matters/issues/34), [#36](https://github.com/thestephenromano/matters/issues/36). Scope: exact content-record bytes and admission obligations for the approved bounded experiment. This is not a frozen production protocol or a completed membership, key-envelope, recovery or relay implementation.

The governing construction is [ADR 0002](decisions/0002-post-quantum-sync.md). This document resolves the content-record slice of its wire-schema gate. The [CDDL](protocol/sync-record-v0.cddl), [experimental Rust adapter](../experiments/sync-prototype/src/wire.rs) and [fixtures](../experiments/sync-prototype/tests/vectors/record-contract/README.md) travel together. The adapter only parses/encodes; a parsed record is never proof of authorization. Production persistence and app integration remain with #35/#36/#39.

## Adoption and compatibility

Adopt [minicbor 2.3.1](https://docs.rs/minicbor/2.3.1/minicbor/) with `default-features = false, features = ["std"]` for the experimental codec. Its typed decoder borrows byte strings from the input and permits fixed-shape parsing without constructing a recursive value tree. Add schema-specific bounds, shortest-form checks, full-input consumption and context equality. The library's ability to decode CBOR alone is not deterministic-profile validation. The maintainer explicitly approved this choice in the project conversation on 2026-11-18, before implementation.

[Ciborium 0.2.2](https://docs.rs/ciborium/0.2.2/ciborium/) is the established Serde alternative; its documented permissive decoding still needs a strict admission layer. Its general value model is useful as an interoperability comparison, but does not remove the fixed-shape/bounds work. RFC 8949 CBOR and RFC 8610 CDDL supply the format and schema language; no custom binary serialization or cryptographic primitive is introduced. Minicbor 2.3.1 declares the BlueOak-1.0.0 license; distribution review remains required. Python cbor2 is a fixture oracle only, not a shipped dependency; cddl-cat 0.7.1 (MIT) with Ciborium 0.2.2 is a test-only schema validator. The independent AES/HKDF fixture oracle uses Python cryptography/OpenSSL. The resolved Rust versions/checksums are in the experiment's Cargo.lock; source/license/validation identities are recorded with the execution evidence.

This slice makes `application_id` explicit in every signed base frame and content header. ADR 0002 previously left this context implicit in tenant identity. The application ID is a provisioned, immutable 16-byte namespace, distinct from tenant and collection. It is also required in every future key-envelope info/AAD and recovery context. Existing plaintext prototype records and synthetic provider-probe contexts are not this format and MUST NOT be accepted as encrypted records. There are no deployed encrypted v0 bytes to migrate. Any future change to these arrays, key derivation or signature input requires a new authenticated protocol version/suite; parsers do not guess formats or retry with weaker algorithms.

## Bytes and identities

Use RFC 8949 §4.2.1 core deterministic CBOR: definite-length arrays/strings, shortest unsigned integer and length encodings, exactly one complete top-level item. This schema permits only arrays, byte strings, the listed ASCII text labels, unsigned 64-bit epochs, and the single specified null. Maps, tags, floats, negative integers, booleans, indefinite lengths, unknown/trailing fields and nonminimal encodings are rejected before signature verification. UUIDs are their 16 network-order bytes, never hyphenated text. No Unicode normalization or case folding is involved. [RFC 8949](https://www.rfc-editor.org/rfc/rfc8949.html#section-4.2.1), [CDDL](https://www.rfc-editor.org/rfc/rfc8610.html)

The wire item is `[B, signature]`, with:

```text
B = [0, "matters-sync-pq-v0", kind,
     application_id, tenant_id, collection_id, signer_id,
     [H, ciphertext, author_id]]
H = [0, "matters-sync-pq-v0", application_id, tenant_id, collection_id,
     document_id, kind, epoch, manifest_hash, author_id,
     incarnation_id, record_id, logical_batch_id, previous_record_hash]
```

| Value | Exact type / meaning |
| --- | --- |
| `application_id`, `tenant_id`, `collection_id`, `document_id` | 16 bytes each. Route context comes from trusted local configuration; matching the fields only to one another is insufficient. |
| `kind` | Exact text `data` or `snapshot`, repeated identically in B and H. |
| `epoch` | Unsigned integer 0 through 2^64−1; an authorized transition establishes its meaning. No wrapping or timestamp interpretation. |
| `manifest_hash`, `previous_record_hash` | SHA-384 identifiers, 48 bytes. Previous is null only at the start of the document/incarnation chain. Hashes identify complete canonical signed objects, including their signatures. |
| `signer_id`, `author_id` | Identical 48-byte device IDs obtained from the verified membership roster. The roster binds both signing and recipient keys. The codec does not derive a device ID or accept an embedded verification key. Exact roster/key-fingerprint derivation remains a lifecycle-schema gate. |
| `incarnation_id`, `record_id`, `logical_batch_id` | 32 bytes each. Incarnation is authenticated by writer admission; record ID is fresh OS entropy per seal; batch ID is stable across permitted resealing. They are distinct namespaces. |
| `ciphertext` | AES-256-GCM ciphertext followed by its full 16-byte tag; 16 through 1,048,592 bytes. Zero-length plaintext is structurally possible; domain validation may reject an empty update. |
| `signature` | Exactly 3,309 bytes, the ML-DSA-65 signature encoding. Length alone establishes no validity. |

B and H must agree on version, suite, kind, application, tenant, collection and signer/author; the body's author must also agree. The expected local route includes document ID. A different document cannot be selected by an untrusted header. The adapter checks these equalities before returning a parsed value. It validates shortest encodings field by field, without a second payload-sized re-encoding allocation. It never exposes a fallible normalized replacement for the signed bytes: signature input is the exact original B slice, and AEAD AAD is the exact original H slice after canonical validation.

An incarnation's record chain is scoped to `(application, tenant, collection, document, incarnation)`. Its writer-admission record must bind that document and actor ID before content is accepted. A non-null predecessor must match that chain and authorized author, with one accepted successor; two signed successors constitute fork evidence. A record containing cross-document changes or engine actors not admitted to the author fails the downstream domain boundary. Writer-admission wire bytes and fork reconciliation remain #33/#37; the parser cannot establish these properties.

## Cryptographic transcript

Sign canonical B with the existing pure ML-DSA-65 API, context exactly ASCII `matters-sync/v0/data` or `matters-sync/v0/snapshot`. These are distinct domains. Use fresh signing randomness in actual sealing; fixed test seeds/randomness are public fixture material only. Verify using the author key selected by authenticated membership, not a key carried alongside the record. [FIPS 204](https://csrc.nist.gov/pubs/fips/204/final)

Content encryption keeps ADR 0002's construction:

```text
PRK = HKDF-Extract-SHA256(salt = record_id, IKM = CEK)
info = CBOR(["matters-sync/v0/record-key", H])
K = HKDF-Expand-SHA256(PRK, info, 32)
nonce = 000000000000000000000000   # 12 zero bytes
ciphertext = AES-256-GCM.Seal(K, nonce, plaintext, AAD = CBOR(H))
```

CEK is 32 OS-random bytes distributed under the approved post-quantum recipient-envelope scheme. One derived key is used for exactly one sealing invocation. All header fields, including the application, manifest, record kind and predecessor, influence both the KDF transcript and AAD. The receiver builds KDF info from the validated original H slice; the sender encodes a newly constructed Header. Both paths must match the same independent golden bytes. The signature additionally covers ciphertext and repeated clear context. The fixtures cross-check KDF and AES output with a different library stack; they do not establish independent interoperability of the selected hybrid HPKE suite or safety of the complete lifecycle. [HKDF](https://www.rfc-editor.org/rfc/rfc5869.html), [GCM](https://csrc.nist.gov/pubs/sp/800/38/d/final)

The selected HPKE suite remains Base mode, MLKEM768-P256 / HKDF-SHA256 / AES-256-GCM from [draft-ietf-hpke-pq-05](https://datatracker.ietf.org/doc/html/draft-ietf-hpke-pq-05). The [key-distribution experiment](SYNC_KEY_DISTRIBUTION.md) defines the bounded key-envelope slice; complete enrollment and recovery bytes remain separate gates. No classical-only fallback is permitted.

## Local sealing state machine and budgets

The [encrypted-outbox experiment](SYNC_ENCRYPTED_OUTBOX.md) exercises the local reservation, immutable retry and crash boundaries below in a separate SQLite database. It does not implement relay durability, epoch resealing or application integration.

| Durable state | Allowed next action | Crash/retry invariant |
| --- | --- | --- |
| Intent with retained batch ID | Reserve a new random record ID in SQLite; check local uniqueness | Failed/resumed attempts retain the original domain intent. |
| Reserved ID and immutable header | Seal once, sign once, then atomically store complete bytes and outbox entry | If outcome is uncertain and complete bytes are absent, abandon this reservation and use a new ID; never repeat sealing under the old header. |
| Sealed outbox bytes | Upload exactly those bytes | ACK loss and reconnect resend identical bytes, including randomized signature. |
| Relay durable receipt | Persist receipt/cursor; retain original history under the retention policy | Receipt is not proof of remote semantic admission or backup survival. |
| Authorized reseal after epoch change | New ID/key/header/signature; preserve logical batch and engine change IDs | Retain the old record/provenance; resealing is not transport retry. |

Reservations and exact signed bytes survive restart. Every open for new sealing establishes a fresh writer incarnation before new edits and uses fresh OS entropy; existing sealed bytes can still be retried. An unsealed intent retained from an old writer must not be signed after reopen: a restored head cannot prove the latest signed predecessor. Expose this work as recovery-required for later domain replay/authenticated-history reconciliation, preserving its original actor and batch. Never automatically rebind its engine payload to a new writer. Never persist/restore an RNG stream or recover availability through predictable randomness. Detect locally known reservation/record conflicts before sealing. Isolated clones cannot coordinate reservations; the random-ID calculation is a probability bound, not absolute uniqueness.

The 1 MiB plaintext cap is the explicit provisional workload budget inherited from ADR 0002. The parser borrows ciphertext and parses a fixed number of fields without recursive allocation. The maximum wire length is derived from the schema's largest fields, integer widths, snapshot label and non-null predecessor; tests compare both its independent SHA-256 and length, then check that boundary and one byte beyond. Const field-size expressions derive the Rust cap from H, B, outer framing and signature sizes. This bounds parser input, not end-to-end memory or signature/import CPU. No decompression is performed by this layer; uncompressed payload admission is bounded separately by the same plaintext cap. Snapshot assembly, total inbox storage and engine import memory still require independent budgets.

ADR 0002's provisional 2^32 records per CEK epoch gives an honest 256-bit random-ID birthday bound below 2^-193. It is an aggregate review budget, not something an offline device can enforce globally. It does not bound derived-key collisions, GCM forgery advantage or repeated verification attempts. Production enablement remains blocked until #33/#45 establish a reviewed multi-user security bound and #34/#36 define enforceable per-device allocation, failed-verification rate/concurrency limits, rotation and exhaustion behavior. There is deliberately no undocumented operational default pretending to satisfy those gates.

### Production sealing (#36, 2026-11-20)

Production code seals inside the edit's own transaction, as the maintainer decided on 2026-11-20. This replaces the separate reservation-then-seal step above. Each recorded local update draws a fresh random record ID, is sealed and signed, and is stored together with the journal row and the writer's new chain head, all before the edit's single commit. The table's invariants hold in a stronger form:
- **No uncertain reservation.** A crash or failure before the commit leaves no record and no ciphertext outside the process. The retried edit draws a new record ID, so a header is never sealed twice.
- **No stranded edit.** Nothing waits to be sealed after the commit, so no edit is left behind under a retired writer, and no re-issue path exists.
- **Fresh writer per open.** Each open that seals is a new writer incarnation with its own chain, so a copied or restored database never continues another copy's chain.
- **Retries resend the stored bytes.** Retries and lost acknowledgements return the exact stored bytes.

The cost is signing and encryption on each save (budget: ADR 0001), and the keys must be present whenever a sealing write happens. `matters_core::storage::sealing` implements this, and `tests/sealing_tests.rs` covers crashes at the commit, copies, reopens and concurrent handles.

## Receiver and relay obligations

Client admission proceeds in stages; no earlier stage implies a later one:

1. Enforce the body-byte cap while receiving, before buffering an unbounded stream. Parse the exact shape and canonical representation; reject unsupported version/suite/type. Compare with trusted route context.
2. Resolve the referenced manifest and writer admission from locally pinned, verified authority. Missing evidence means durable pending input, never temporary trust. Reject inconsistent signer/key/actor binding. Rate-limit verification before doing expensive cryptography.
3. Verify ML-DSA over original B. Check the manifest chain, epoch, revocation closing frontier and predecessor/fork conditions. A historical signature can remain valid while a new old-epoch submission is inadmissible.
4. Resolve the epoch key and open AEAD with original H; failure exposes no plaintext and changes no CRDT state. Missing authorized keys remain pending.
5. Enforce engine/actor/domain bounds, retain original bytes and commit admission, projection and receive progress atomically. Invalid data is quarantined with a non-secret reason and never advances applied progress. Durable received progress and applied progress are distinct.

`snapshot` has exact encoding/signature context, but **must not be imported as authoritative state** until #40's authenticated coverage and tail rules are implemented. A correctly signed snapshot is insufficient. No parser success can bypass this gate. Its plaintext is a checkpoint part, and a snapshot record stands outside its writer's data chain, with a null predecessor ([SYNC_CHECKPOINTS.md](SYNC_CHECKPOINTS.md), `sync-checkpoint-v0.cddl`).

The transport-independent service contract for #34 requires the following observable behavior. No HTTP library, storage engine, broker, deployment or availability SLA is selected here; each needs its adoption decision before implementation.

| Operation / case | Required result |
| --- | --- |
| Append `(application, tenant, collection, record_id, bytes)` | Bind request routing to authenticated service identity; reject cross-context uploads. Enforce current upload ACL independently of end-to-end client authorization. |
| Duplicate ID, identical complete bytes | Return the original durable receipt and sequence, with no second append or notification. Persist the uniqueness/receipt atomically with the record. |
| Duplicate ID, different bytes | Permanent conflict; never overwrite. Keep local evidence and unsent work. Scope record-ID uniqueness to application/tenant/collection, not document or epoch. |
| Durable ACK | Only after the chosen transactional store has committed bytes, identity and receipt within its stated failure domain. Notifications follow commit. Process-crash durability is distinct from disk/host/region loss and backup RPO; the deployment must declare these before a pilot. |
| Fetch / pagination | Use an opaque cursor scoped to route and storage generation. Paginate against a captured high-water mark; concurrent appends belong to the next pass. Stable immutable records support inclusive retries and client deduplication. Never advance a cursor past data not durably retained locally. |
| Missing predecessor | Retain pending bytes and fetch by authenticated hash/identity. An empty page or timeout is not proof that history never existed. |
| Restore / rollback | Changed storage generation invalidates old cursors with explicit resynchronization. Compare locally retained receipts/authority pins; preserve outbox/history. A malicious server can conceal a reset, so client evidence remains necessary. |
| Quota / oversized / unsupported | Distinct permanent rejection from retryable backpressure; preserve local intent and expose a recoverable user-visible condition. No silent delete or version downgrade. |
| Export / import | Preserve exact encrypted records, signed authority objects, envelopes and provenance. Transport indexes/cursors may change; authenticated bytes and identities do not. |

These are acceptance obligations, not a claim that a relay exists. Future service fixtures must exercise crashes before/after durable append, duplicate/conflicting uploads, route isolation, pagination with concurrent append, invalid cursors and restored backups against the selected real store. The content fixture suite exercises bytes/context/cryptography only.

## Compatibility and acceptance evidence

| Input / capability | Result |
| --- | --- |
| Canonical v0 content record, supported suite | Structurally parseable, still requires all authorization and cryptographic gates. |
| Plaintext prototype record, future version/suite, other message type | Reject; retain diagnostically where bounded policy allows. Never reinterpret as v0. |
| Extra/omitted fields, alternate encoding of the same value | Reject before verification; do not normalize and verify the normalized replacement. |
| Missing manifest/key/predecessor | Pending at the relevant admission boundary; no implicit authorization. |
| Valid snapshot signature without coverage | No authoritative import. |

Conformance coverage includes independent golden H/B bytes, KDF/AAD/AES values, exact parse/re-encode, integer-width boundaries, every truncation of a valid wire item, nonminimal/indefinite/tagged/trailing inputs, wrong field counts/lengths, repeated-field disagreement, expected-route mismatch, maximum ciphertext and over-limit input. Crypto probes verify signatures and reject mutated transcript fields, signature context and ciphertext using fixed public fixture keys. ML-DSA signatures generated with fixed public seeds/randomness by the selected implementation are same-provider regression evidence, not an independent signature oracle. The checked fixture README and execution record state what actually ran.

Remaining release gates include complete lifecycle/recipient-envelope bytes, independent exact-suite HPKE interoperability, nonce reservation crash/clone/restore implementation, authenticated actor admission, snapshot coverage, resource/security budget review, aarch64 execution, secret lifecycle and independent protocol review. #33/#34/#36 remain open; merging this bounded contract does not enable encrypted sync.

The [active execution record](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) records the tested source, commands, oracle identities and limitations. CI runs independent fixture regeneration and verifies the listed hashes against the actual checkout, including this final document. Finalize source/docs before capturing the record; the subsequent evidence commit changes only the record itself. [Review dispositions](../experiments/sync-prototype/REVIEW_RECORD_CONTRACT.md) distinguish confirmed gaps from historical-source and design qualifications.

## Production receiving

[`matters_core::storage::receive`](../crates/matters-core/src/storage/receive.rs) (#39, migration v16) is the receiver's half of this contract.

**Writer claims.** Each open that seals is a new writer. It signs a writer control for its incarnation and engine actor (`SYNC_WRITER_LIFECYCLE.md`), admits it locally, and queues it to publish with the device's other controls.
- A device's later writers hand off from its active admitted incarnation, naming the exact closing head of its record chain.
- A copied database therefore names the same predecessor as its original. Two competing handoffs freeze that device's writer lineage on every receiver.

**Checks.** `Database::receive_record` checks a record in this order:
1. route and document;
2. that it is not already applied (by record ID, the device's own records included);
3. that this device holds an envelope of the key of the epoch the record was sealed in;
4. signature, the author's membership in that epoch's roster, and the writer's admission (`Authority::authenticate_at`);
5. position in the writer's signed chain, and that it does not follow the writer's closing head;
6. that its engine update carries only the writer's admitted actor, and is an update, not a snapshot.

It then applies in one transaction: journalled as received, merged, the rows rebuilt, and the writer's chain head advanced.

**Records that wait.** A record waiting for its key, its writer's claim or its predecessor stays in `sync_pending`, across restarts. It is retried whenever a record, writer claim, transition or envelope arrives. A record after its writer's close, or of a frozen lineage, is kept there as well, never applied and never discarded. Forged or malformed bytes are refused and not kept.

**Limit.** A writer claim is admitted only under the manifest it was signed under. A claim signed just before a concurrent rotation therefore arrives stale everywhere, and its writer must claim again under the new epoch. That is the sync client's job (#39).

**Evidence:**
- `tests/receive_tests.rs` covers:
  - two devices converging, including records the newcomer received from before it joined;
  - duplicates and the device's own records;
  - out-of-order records across a restart;
  - records waiting for a writer claim or for an epoch key;
  - forged records, a foreign actor, a snapshot in place of an update;
  - a record after its writer's close;
  - a cloned device freezing its lineage.
- matters-sync's `authority_tests.rs` covers authentication at a record's own epoch after rotation and removal.
