# Authenticated key-distribution experiment

Status: bounded, test-only extension of [ADR 0002](decisions/0002-post-quantum-sync.md) and the [content-record contract](SYNC_RECORD_CONTRACT.md). Tracks [#33](https://github.com/thestephenromano/matters/issues/33), [#34](https://github.com/thestephenromano/matters/issues/34), [#36](https://github.com/thestephenromano/matters/issues/36) and [#37](https://github.com/thestephenromano/matters/issues/37); it closes none of them.

## Decision and implementation boundary

Reuse the already approved minicbor 2.3.1, HPKE/libcrux and ML-DSA stack, with the existing lockfile unchanged. The established constructions are [HPKE Base mode](https://www.rfc-editor.org/rfc/rfc9180.html), the [MLKEM768-P256 draft-05 construction](https://datatracker.ietf.org/doc/html/draft-ietf-hpke-pq-05), [ML-DSA-65](https://csrc.nist.gov/pubs/fips/204/final), and [core deterministic CBOR](https://www.rfc-editor.org/rfc/rfc8949.html#section-4.2.1). The suite remains `matters-sync-pq-v0`: MLKEM768-P256, HKDF-SHA256 and AES-256-GCM, with pure ML-DSA-65 signatures. No new cryptographic construction, transport, queue, storage provider or dependency is selected here. [MLS](https://www.rfc-editor.org/rfc/rfc9420.html) remains the established group-lifecycle alternative evaluated in the ADR; this experiment does not claim its group security properties.

The implementation lives exclusively in [integration-test support](../experiments/sync-prototype/tests/support/key_distribution.rs). Nothing is exported to the application or attached to the prototype's SQLite journal. Authority, writer bindings and keys exist only in test memory. Keys are generated using the selected provider's OS-seeded RNG; entropy errors propagate. Secret-bearing wrappers have no Debug, Clone or serialization implementation, but this does **not** establish memory erasure, secure storage or crash safety.

## Exact bytes and identities

[CDDL](protocol/sync-key-distribution-v0.cddl) fixes the structural message shapes. The authority rules below define the admitted experiment subset. The signature input is the original canonical input slice:

```text
B = [0, "matters-sync-pq-v0", kind, app16, tenant16, collection16, signer48, body]
record = [B, signature3309]
signature context = ASCII("matters-sync/v0/" + kind)
manifest hash = SHA-384(complete canonical signed record)
```

All arrays have definite, exact lengths. Integer and length encodings are shortest; identifiers are byte strings. No tags, indefinite lengths, floats, trailing data or unknown message kinds are accepted. Parsing uses typed fixed fields and checks the roster count before allocating it. After parsing, re-encoding the whole bounded control record must reproduce the input. This intentionally differs from the content parser's field-width checks: content parsing borrows payloads up to its roughly 1 MiB wire cap without copying them, while whole-record comparison is confined to the 64 KiB control budget. Both strategies enforce the same deterministic-CBOR rules and have independent negative fixtures. The borrowed original `B` is verified, not a replacement transcript. Structural parsing is separate from authorization: for example, nonzero recovery generations parse but are unsupported by this experiment's authority state machine.

Key fingerprints are `SHA-384(CBOR([algorithm_label, public_key_bytes]))`. Labels are exactly `ML-DSA-65` and `MLKEM768-P256/draft-ietf-hpke-pq-05`. A device ID is:

```text
SHA-384(CBOR(["matters-sync/v0/device", signing_key_fingerprint48, kem_key_fingerprint48]))
```

The root signer ID is its signing-key fingerprint. Device IDs bind both keys; roles are separately authenticated by the roster. Roster entries are `[device48, signing_key1952, kem_key1249, role]`, sorted strictly by device ID, with no duplicates. Role 0 is writer; role 1 is writer+admin. Exactly one entry is admin and matches the body's admin ID. Public-key sizes and identity hashes do not certify mathematical key validity or prove possession; actual HPKE operations reject unusable recipient material. Enrollment possession proofs remain open.

| Kind | Exact body | Authority and supported subset |
| --- | --- | --- |
| `genesis` | `[root_key1952, recovery_kem_key1249, generation, roster, admin48, epoch, checkpoint48_or_null]` | Root signature, independently supplied expected route and pinned root public key; generation 0, epoch 0, null checkpoint only. |
| `membership` | `[generation, sequence, previous_manifest48, roster, admin48, epoch, recovery_kem_key1249, []]` | Previous manifest's admin; generation 0; sequence and epoch each advance by exactly one; same admin/recovery keys; unchanged or reduced roster only. |
| `writer_start` | `[manifest48, document16, device48, incarnation32, actor_u64, predecessor_incarnation32_or_null]` | Current roster device signs as itself; document is independently expected; actor and incarnation admission described below. |
| `key_envelope` | `[manifest48, epoch, recipient48, recipient_key_fingerprint48, "content-key", encapsulation1153, ciphertext, authority48]` | Current admin signs and is the authority; recipient must be in the current verified roster. |

Self-signing a genesis does not establish trust: the root public key and app/tenant/collection binding must arrive through a trusted independent path. Tests supply these out of band. This does not implement the user ceremony or authenticate an account login as enrollment.

## Membership and writer admission

A transition is verified using its named predecessor's admin, including when checking a competing successor of an older known manifest. Missing predecessors return `MissingPrevious`. Invalid signatures, routing, roster binding or unsupported changes leave accepted state intact. Replaying an already accepted record's **exact signed bytes** returns a duplicate result without rolling state backward.

Two independently valid successors of one predecessor freeze authority: retain both signed byte strings as fork evidence and reject further key distribution, writer admission and content opening. Neither timestamps nor relay arrival order resolves the fork. Signatures use fresh randomness; signing the same membership body again produces a distinct signed record and can constitute a fork. Retry must retain exact existing bytes. This experiment has no durable receipt/outbox implementation.

Supported transitions are removal and epoch rotation with an empty closing frontier. Enrollment additions, admin transfer, nonempty historical frontiers, root recovery and recovery-recipient envelopes are explicitly unsupported. A fresh CEK is supplied by the test after rotation. Removed devices cannot be envelope recipients under the known current manifest, and old envelopes and old epoch-key handles fail after a transition. Rotation cannot erase old keys; a withheld transition is not detectable from a root pin alone. There is no globally latest membership or immediate offline-revocation guarantee.

Writer admission retains bindings by `(document, incarnation)` and maintains one active chain per `(document, device)`. A null predecessor starts the chain once. A replacement names an existing incarnation of the same device/document, uses an unused engine actor, and becomes the active head; the replaced incarnation immediately becomes inadmissible for this first-record helper. Historical actor IDs stay reserved. Exact signed-byte retries are idempotent while their manifest is current and never reactivate retired heads. An old-manifest writer-start replay is stale after rotation; the active retained binding remains usable by a current member.

Two distinct valid writer starts naming the same predecessor, including two null roots or a delayed competitor of an older parent, retain both signed byte strings and freeze that device/document chain. Neither delivery order nor the first accepted branch resolves it. Subsequent starts and content opening for that chain return `Fork`; other devices/documents and key distribution remain usable. Forged competitors do not freeze anything. Conflicting incarnation IDs or reused actor IDs return `Actor` before chain admission. Membership forks still freeze the whole authority. No durable quarantine, historical-record eligibility or reconciliation is implemented; retired historical data needs the later frontier/receive-chain work.

## Key envelope transcripts

The sender looks up the recipient key in authenticated authority state. It cannot accept a replacement key from an untrusted directory call. Each envelope uses one fresh HPKE Base context. Encode the following independently specified arrays canonically:

```text
info = [suite, app16, tenant16, collection16, "content-key", recipient_key_fingerprint48]
AAD  = [0, suite, "key_envelope", app16, tenant16, collection16, admin48,
        [manifest48, epoch, recipient48, recipient_key_fingerprint48, "content-key", admin48]]
plaintext = [epoch, content_epoch_key32, "whole-collection"]
```

AAD omits only the encapsulation and ciphertext from the full clear envelope frame. The admin signature covers the complete envelope including both. The recipient verifies expected route, current manifest/epoch, admin authority and signature, then checks that the recipient ID, public key and fingerprint match its own device and the verified roster. Only then does HPKE open run. Inner epoch, key size, policy, complete consumption and canonical plaintext must all match before returning an epoch-key handle bound to route/manifest/epoch.

The ciphertext upper bound is 77 bytes: array head 1 + maximum u64 9 + 32-byte key with head 34 + 16-byte policy with head 17 + GCM tag 16. Parsing admits 16..77 bytes so malformed short plaintext is rejected after authentication rather than interpreted as a key. Encapsulation size is 1088-byte ML-KEM ciphertext + 65-byte uncompressed P-256 point; the public key is 1184 + 65 bytes.

## Demonstrated two-device flow

The [integration tests](../experiments/sync-prototype/tests/key_distribution.rs) generate a root and two device key pairs. Both devices authenticate the same pinned-root roster. Admin A creates a CEK and a signed HPKE envelope for B; B unwraps it. A signs a writer-start binding a document/incarnation to a Loro actor, then signs and encrypts that actor's first Loro update. B verifies admission and the ML-DSA signature, derives the record key using the original validated header bytes, decrypts, and imports the update into a fresh Loro document. The resulting text is checked exactly.

The import gate accepts only a first `data` record with null predecessor, current manifest/epoch and an admitted author/incarnation. Loro metadata must describe an Updates blob beginning at actor counter zero with no causal frontiers and one admitted actor. A staged fresh import must produce exactly the advertised version vector with no pending updates. Snapshot blobs, causal tails, unknown actors, invalid blobs and signed records from the wrong device fail. The helper neither advances a receive head nor validates task-domain projections, and repeated calls are not an exactly-once receive protocol. Its fixture sealer requires a fresh OS-random record ID per call and must not be used as a production sealing API.

## Evidence and chosen experiment budgets

[Independent fixtures](../experiments/sync-prototype/tests/vectors/key-distribution/README.md) pin four complete structural messages, signing bytes, SHA-384 hashes, key fingerprints, device IDs, HPKE info/AAD and inner plaintext. Python cbor2/hashlib supplies the oracle; cddl-cat checks the wire shapes with its separate decoder. Ten malformed fixtures and every truncation of all four messages reject. The maximum-roster genesis compares both byte count and SHA-384, not length alone.

The experiment chooses 1..16 roster members, 64 KiB maximum control input, at most 16 membership transitions and 16 admitted writer incarnations per authority instance. These budgets bound test support and preserve inspectable histories; they are not production capacity promises. The independent 16-member genesis is 58,849 bytes. One additional member, transition or writer is explicitly rejected. Input is capped before decoding; fixed key lengths and roster count are checked before copying. The existing content-record cap remains separate. These controls do not establish a decompression, engine CPU, memory or total cryptographic-work bound for hostile signed Loro payloads.

The [active execution evidence](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) binds source, schemas, documents and fixture generators to a source commit. CI runs on every pull request and main push, checks its hashes, independently regenerates both fixture sets, and executes formatting, Clippy and the full prototype suite. This avoids a separate path allowlist drifting from the evidence manifest. Generator guard tests exercise drift and version-pin rejection with and without Python optimization. The key-distribution tests cover forged signers, changed recipients/keys, stale epochs, malformed authenticated key plaintext, invalid roster material, actor collisions, unsupported ceremonies and competing valid membership successors.

The control oracle uses patterned public bytes and invalid zero signatures. It proves CBOR/hash agreement, not valid cryptographic objects. Actual envelope tests use the selected provider on both sides: they do not establish independent interoperability of the exact hybrid suite. No timing claim, aarch64 run, fuzz campaign, side-channel claim, production usage-bound proof or independent security review follows from this experiment.

## Remaining implementation gates

Before application integration: durable authenticated membership/fork evidence and writer state; secure local key storage; root/enrollment possession ceremonies; recovery envelopes and all-devices-lost recovery; historical frontier admission; receive-chain/idempotency/quarantine semantics; crash-safe encrypted outbox and immutable retry bytes; restore/clone safety; task-domain import validation; aggregate crypto and hostile-import resource budgets; cross-target execution and independent review. The generic service API, acknowledged failure domain, transport/storage adoption and relay conformance remain with #34. The existing SQLite-backed task app remains unchanged.

The [review record](../experiments/sync-prototype/REVIEW_KEY_DISTRIBUTION.md) maps the review findings to their owning invariants and regression evidence.
