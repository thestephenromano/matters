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
| `genesis` | `[root_key1952, recovery_kem_key1249, generation, roster, admin48, epoch, checkpoint48_or_null, key_commitment48]` | Root signature, independently supplied expected route and pinned root public key; generation 0, epoch 0, null checkpoint only. Commits to the epoch-0 content key. |
| `membership` | `[generation, sequence, previous_manifest48, roster, admin48, epoch, recovery_kem_key1249, [], key_commitment48]` | Previous manifest's admin; generation 0; sequence and epoch each advance by exactly one; same admin/recovery keys; unchanged or reduced roster only. Commits to the new epoch's content key. |
| `writer_start` | `[manifest48, document16, device48, incarnation32, actor_u64, predecessor_incarnation32_or_null]` | Current roster device signs as itself; document is independently expected; actor and incarnation admission described below. |
| `writer_handoff` | `[manifest48, document16, device48, incarnation32, actor_u64, predecessor_incarnation32, closing_head48_or_null]` | Current device signs a fresh successor and an exact closed content prefix. See [writer lifecycle](SYNC_WRITER_LIFECYCLE.md) for durable admission and historical eligibility. |
| `key_envelope` | `[manifest48, epoch, recipient48, recipient_key_fingerprint48, "content-key", encapsulation1153, ciphertext, authority48]` | Current admin signs and is the authority; recipient must be in the current verified roster. |

Self-signing a genesis does not establish trust: the root public key and app/tenant/collection binding must arrive through a trusted independent path. Tests supply these out of band. This does not implement the user ceremony or authenticate an account login as enrollment.

## Membership and writer admission

A transition is verified using its named predecessor's admin, including when checking a competing successor of an older known manifest. Missing predecessors return `MissingPrevious`. Invalid signatures, routing, roster binding or unsupported changes leave accepted state intact. Replaying an already accepted record's **exact signed bytes** returns a duplicate result without rolling state backward.

Two independently valid successors of one predecessor freeze authority: retain both signed byte strings as fork evidence and reject further key distribution, writer admission and content opening. Neither timestamps nor relay arrival order resolves the fork. Signatures use fresh randomness; signing the same membership body again produces a distinct signed record and can constitute a fork. Retry must retain exact existing bytes. This experiment has no durable receipt/outbox implementation.

Supported transitions are removal and epoch rotation with an empty closing frontier. Enrollment additions, admin transfer, nonempty historical frontiers, root recovery and recovery-recipient envelopes are explicitly unsupported. The admin chooses the next epoch's CEK before signing the transition, because the transition commits to it (see below). Removed devices cannot be envelope recipients under the known current manifest, and old envelopes and old epoch-key handles fail after a transition. Rotation cannot erase old keys; a withheld transition is not detectable from a root pin alone. There is no globally latest membership or immediate offline-revocation guarantee.

## Epoch content-key commitment

Each epoch is defined by exactly one signed genesis or membership message, and that message commits to the epoch's content key ([#112](https://github.com/thestephenromano/matters/issues/112)):

```text
key_commitment = SHA-384(CBOR(["matters-sync/v0/epoch-key-commitment", suite,
                               app16, tenant16, collection16, generation, epoch, CEK32]))
```

`wrap_key` refuses a key that does not match the current epoch's commitment, and `unwrap_key` refuses an admin-signed envelope whose key does not match (`KeyCommitment`). An `EpochKey` handle therefore exists only for the committed key, so an admin cannot give different devices different keys for one epoch, and a receiver that cannot open a signed record knows the record is at fault. Envelopes stay per-recipient; the recipient-key fingerprint in an envelope binds the recipient, not the content key.

Binding rests on SHA-384 collision resistance (192-bit classical). The CEK is 256 uniformly random bits, so the published commitment reveals nothing usable about it, including to removed devices and the relay. This is a plain hash commitment rather than an [MLS](https://www.rfc-editor.org/rfc/rfc9420.html)-style confirmation MAC, because the admin signs the whole epoch message, and rather than a key-committing AEAD, which would commit each device's records to that device's key without making devices agree. Once every receiver holds the same committed CEK, every receiver derives the same per-record key, so record encryption needs no further commitment. Independent fixtures cover the commitment bytes for two epochs.

Writer admission retains bindings by `(document, incarnation)` and maintains one active chain per `(document, device)`. A null predecessor starts the chain once. A replacement names an existing incarnation of the same device/document, uses an unused engine actor, and becomes the active head; the replaced incarnation immediately becomes inadmissible for this first-record helper. Historical actor IDs stay reserved. Retries of the same signed claim, including a fresh randomized signature over it, are idempotent while their manifest is current and never reactivate retired heads. An old-manifest writer-start replay is stale after rotation; the active retained binding remains usable by a current member.

Two distinct valid writer starts naming the same predecessor, including two null roots or a delayed competitor of an older parent, retain both signed byte strings and freeze that device/document chain. Neither delivery order nor the first accepted branch resolves it. Subsequent starts and content opening for that chain return `Fork`; other devices/documents and key distribution remain usable. Forged competitors do not freeze anything. Same-device incompatible incarnation definitions or competing successor claims freeze the lineage even when an actor is reused. Other identity/actor collisions return `Actor`; another device cannot freeze an identity owner merely by claiming its ID. Membership forks still freeze the whole authority. No durable quarantine, historical-record eligibility or reconciliation is implemented; retired historical data needs the later frontier/receive-chain work.

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

[Independent fixtures](../experiments/sync-prototype/tests/vectors/key-distribution/README.md) pin six complete structural messages, signing bytes, SHA-384 hashes, key fingerprints, device IDs, HPKE info/AAD and inner plaintext. Python cbor2/hashlib supplies the oracle; cddl-cat checks the wire shapes with its separate decoder. Ten malformed fixtures and every truncation of all six messages reject. The maximum-roster genesis compares both byte count and SHA-384, not length alone.

The experiment chooses 1..16 roster members, 64 KiB maximum control input, at most 16 membership transitions and 16 admitted writer incarnations per authority instance. These budgets bound test support and preserve inspectable histories; they are not production capacity promises. The independent 16-member genesis is 58,899 bytes. One additional member, transition or writer is explicitly rejected. Input is capped before decoding; fixed key lengths and roster count are checked before copying. The existing content-record cap remains separate. These controls do not establish a decompression, engine CPU, memory or total cryptographic-work bound for hostile signed Loro payloads.

The [active execution evidence](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) binds source, schemas, documents and fixture generators to a source commit. CI runs on every pull request and main push, checks its hashes, independently regenerates both fixture sets, and executes formatting, Clippy and the full prototype suite. This avoids a separate path allowlist drifting from the evidence manifest. Generator guard tests exercise drift and version-pin rejection with and without Python optimization. The key-distribution tests cover forged signers, changed recipients/keys, stale epochs, malformed authenticated key plaintext, invalid roster material, actor collisions, unsupported ceremonies and competing valid membership successors.

The control oracle uses patterned public bytes and invalid zero signatures. It proves CBOR/hash agreement, not valid cryptographic objects. Actual envelope tests use the selected provider on both sides: they do not establish independent interoperability of the exact hybrid suite. No timing claim, aarch64 run, fuzz campaign, side-channel claim, production usage-bound proof or independent security review follows from this experiment.

## Remaining implementation gates

Before application integration: durable authenticated membership/fork evidence and writer state; secure local key storage; root/enrollment possession ceremonies; recovery envelopes and all-devices-lost recovery; historical frontier admission; receive-chain/idempotency/quarantine semantics; crash-safe encrypted outbox and immutable retry bytes; restore/clone safety; task-domain import validation; aggregate crypto and hostile-import resource budgets; cross-target execution and independent review. The generic service API, acknowledged failure domain, transport/storage adoption and relay conformance remain with #34. The existing SQLite-backed task app remains unchanged.

The [review record](../experiments/sync-prototype/REVIEW_KEY_DISTRIBUTION.md) maps the review findings to their owning invariants and regression evidence.

## Durable receiver extension

[Durable authenticated receive](SYNC_DURABLE_RECEIVE.md) reuses the current-membership and writer-signature gate for multi-record chains. It pins the signed authority bundle to a dedicated SQLite database, retains encrypted pending/quarantined records and fork evidence, and atomically commits accepted replay order and task projections. The original first-record helper remains as a narrow regression fixture. Changing membership, historical admission, key storage and production integration remain outside this extension.

## Signed writer handoff

The [writer lifecycle extension](SYNC_WRITER_LIFECYCLE.md) adds a distinct `writer_handoff` kind and signature context, preserving existing `writer_start` bytes. Its independent populated/null closing-head vectors extend the structural oracle. The durable inbox admits roots and handoffs, replays control verdicts and gates retired history by exact signed prefixes. The strict in-memory first-record helper remains active-writer-only. Membership, epoch rotation, enrollment and key storage remain separate gates.

## Production code

[`matters-sync`'s authority module](../crates/matters-sync/src/authority.rs) carries this contract into production for [#37](https://github.com/thestephenromano/matters/issues/37): the same message codec, authority rules, envelope transcripts, key commitment and budgets, on the content-record keys and codec of the same crate. [Its tests](../crates/matters-sync/tests/authority_tests.rs) check the independent golden fixtures above in place. The first-record Loro import gate stays prototype test support; chain position and engine admission belong to the durable receiver.

## Enrollment

Production code extends the experiment for [#37](https://github.com/thestephenromano/matters/issues/37), following the maintainer decision of 2026-11-21 recorded there. A new device joins through a one-time invitation from the admin device. The construction uses only HKDF-SHA384 and HMAC-SHA384 (libcrux 0.0.9, the provider family already pinned) and ML-DSA-65, with no classical key agreement and no PAKE. [The enrollment module](../crates/matters-sync/src/authority/enrollment.rs) implements it.

```text
invitation = [0, suite, "invitation", app16, tenant16, collection16, root48, manifest48, secret16]
offer      = [0, suite, "invitation_offer", app16, tenant16, collection16, root48, manifest48, tag48]
code       = Crockford base32 of secret16: 26 characters, the first 0-7
key(label) = HKDF-SHA384(salt = suite, ikm = secret16, info = CBOR([label, app16, tenant16, collection16]))
lookup16   = key("matters-sync/v0/invitation-lookup")
tag        = HMAC-SHA384(key("matters-sync/v0/invitation-offer"), offer's first eight items as an array)
enrollment_request body = [manifest48, signing_key1952, kem_key1249, binding48], signer = device48
binding    = HMAC-SHA384(key("matters-sync/v0/enrollment"),
             CBOR(["matters-sync/v0/enrollment", suite, app16, tenant16, collection16,
                   root48, manifest48, device48, signing_key1952, kem_key1249]))
```

- **Invitation.** The admin draws 16 bytes of OS entropy and shows the invitation as a QR code. Alternatively it shows the 26-character code and publishes the offer under the lookup, so a device that has only the code can fetch the route, root ID and manifest. The code-holding device checks the offer's tag before using anything in it. The secret is one-time, and the admin's application forgets it after one accepted request.
- **Request.** The new device pins the root by the invitation's root ID (`Authority::bootstrap_from_invitation`) and signs an `enrollment_request` with its own signing key, as the device ID of its two keys.
- **Acceptance.** `Authority::accept_enrollment` checks, before the admin signs anything:
  - the invitation's route, root and current manifest;
  - the request's route, kind and manifest;
  - that the signer is the device ID of the named keys;
  - the signature, under the named key, which proves possession;
  - the binding, compared in constant time, which proves the requester saw the secret;
  - that the device is not already a member, and that the roster has room.

  It returns the roster entry, and the admin then signs an ordinary `membership` transition that adds it.
- **Membership.** A membership transition may now add members as well as remove them. Admin transfer and recovery-key changes are still refused.
- **Retained history.** `wrap_history_key` and `unwrap_history_key` carry the content key of any known earlier manifest, as an ordinary `key_envelope` naming that manifest and epoch. The envelope is signed by the *current* admin, its recipient must be a *current* member, and the key must match the commitment of that epoch. The resulting epoch-key handle is bound to the old epoch, so it can never seal. `unwrap_key` still refuses any envelope that doesn't name the current manifest.

The server never knows the secret. An account reset therefore can't produce a valid binding, and a substituted key directory can't either: swapping in the server's keys fails the binding, and swapping only one key breaks the device ID.

The secret's 128 bits face no offline attack before the admin uses it once; quantum search would leave 64 bits of work against a secret that lives for minutes.

**Evidence:** `authority_tests.rs` covers:
- QR and typed-code enrollment end to end, including retained keys for two earlier epochs;
- server-substituted keys, requests without the secret, and requests made for other invitations, collections and roots;
- stale invitations and requests;
- a full roster, and offers that are forged, non-canonical or from other versions;
- history envelopes for removed devices, wrong keys and mismatched epochs;
- redaction of the secret.

`tests/vectors/invitation_kat.py` independently computes the code, the lookup, the invitation bytes and the offer tag with the Python standard library, and the test checks all four.

**Not covered yet:**
- admitting earlier-epoch records into the engine;
- persistence and secure storage of device keys and authority state.

These remain open in #37.

## Recovery

When every device is lost, a 256-bit recovery secret restores the collection. This follows the maintainer decisions of 2026-11-21 on [#37](https://github.com/thestephenromano/matters/issues/37). The user keeps the secret as 24 English BIP39 words; nothing else holds the private keys it derives. [The recovery module](../crates/matters-sync/src/authority/recovery.rs) implements it.

```text
seed(label)  = HKDF-SHA384(salt = suite, ikm = secret32,
                           info = CBOR([label, app16, tenant16, collection16])), 32 bytes
root key     = ML-DSA-65 KeyGen(seed("matters-sync/v0/recovery-root"))
recovery KEM = MLKEM768-P256 DeriveKeyPair(seed("matters-sync/v0/recovery-kem"))
root_recovery body = membership body, signed by the root under "matters-sync/v0/root_recovery"
```

- **Words.** The bip39 crate (3.0.0, CC0-1.0) converts the 256-bit entropy to 24 words with an 8-bit checksum and back. BIP39's PBKDF2 seed step is not used: the words carry the secret itself. Parsing accepts any case and whitespace. It requires exactly 24 words from the English list with a matching checksum, so a typo fails with `Format` rather than deriving other keys.
- **Bootstrap.** The first device derives both keys and signs the genesis with the root key (`genesis`). The genesis commits to the recovery KEM key. The device then shows the words once and keeps neither private key. The root ID the collection pins is therefore a function of the words and the route.
- **Recovery envelopes.** The admin keeps one recovery envelope per epoch (`wrap_recovery_key`). It is an ordinary `key_envelope` whose recipient and recipient-key fingerprint are both the recovery KEM key's fingerprint. `unwrap_recovery_key` requires:
  - a known manifest and its epoch;
  - the signature of that epoch's admin;
  - the collection's recovery key;
  - the epoch's key commitment.

  It works while the authority is frozen, because reading history back is what recovery is for.
- **A new generation.** A recovering device bootstraps from the genesis with the root key derived from the words, so other words fail with `Identity`. It replays the control records the relay holds, opens the recovery envelopes, then signs a `root_recovery` (`Authority::recover`). The transition starts generation N+1 from the newest manifest it knows of generation N:
  - the next sequence and epoch;
  - one member, who is the admin;
  - the same recovery key;
  - a commitment to a fresh content key.
- **Admitting a recovery** (`apply_recovery`). Every device checks the root signature against its pin. The recovery then becomes current even if generation N had later transitions the recoverer did not see, or was frozen by a membership fork, and it clears that freeze. From then on, any membership transition of an earlier generation is `Stale`, whoever signed it. Exact replays of accepted records still return a duplicate result. Two different valid recoveries to one generation retain both and freeze; a recovery to the next generation clears the freeze.
- **After recovery.** Earlier devices are no longer members: they re-enroll through an invitation from the new admin. Until then they keep their unsent work for explicit recovery.

**Material that fails safely.** Wrong words derive another root and recovery key, which bootstrap, `recover` and `unwrap_recovery_key` all refuse with `Identity`. An epoch whose recovery envelope was never written cannot be read from the words alone, and that loss is explicit rather than silent. If both the words and every device are gone, the content cannot be recovered; there is no server-held bypass.

**Evidence:** `authority_tests.rs` covers:
- the four published 256-bit English BIP39 vectors, loose typing, and bad checksums, counts and words;
- known-answer root and KEM seeds from `tests/vectors/invitation_kat.py`, and their binding to the route;
- the full flow: grow and rotate, lose everything, recover from the words, unwrap every epoch, start generation 1, and have a retained device follow it;
- wrong words, missing envelopes, and forged or misdated recovery envelopes;
- recoveries signed by the admin, with two members, another recovery key, a skipped generation or epoch, a repeated sequence or an unknown previous;
- recovery from a stale view, competing recoveries and the way out, and a recovery clearing a membership fork;
- redaction of the secret and keys.

**Not covered yet:**
- writing the words and recovery envelopes into the app;
- storing envelopes on the relay (#38);
- restore-testing the bundle as epochs change.
