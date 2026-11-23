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
| `membership` | `[generation, sequence, previous_manifest48, roster, admin48, epoch, recovery_kem_key1249, frontier, key_commitment48]` | Previous manifest's admin; generation 0; sequence and epoch each advance by exactly one; same admin/recovery keys; unchanged or reduced roster only. Commits to the new epoch's content key. The experiment admits only an empty `frontier`; production populates it (see [Revocation](#revocation)). |
| `writer_start` | `[manifest48, document16, device48, incarnation32, actor_u64, predecessor_incarnation32_or_null]` | Current roster device signs as itself; document is independently expected; actor and incarnation admission described below. |
| `writer_handoff` | `[manifest48, document16, device48, incarnation32, actor_u64, predecessor_incarnation32, closing_head48_or_null]` | Current device signs a fresh successor and an exact closed content prefix. See [writer lifecycle](SYNC_WRITER_LIFECYCLE.md) for durable admission and historical eligibility. |
| `key_envelope` | `[manifest48, epoch, recipient48, recipient_key_fingerprint48, "content-key", encapsulation1153, ciphertext, authority48]` | Current admin signs and is the authority; recipient must be in the current verified roster. |

Self-signing a genesis does not establish trust: the root public key and app/tenant/collection binding must arrive through a trusted independent path. Tests supply these out of band. This does not implement the user ceremony or authenticate an account login as enrollment.

## Membership and writer admission

A transition is verified using its named predecessor's admin, including when checking a competing successor of an older known manifest. Missing predecessors return `MissingPrevious`. Invalid signatures, routing, roster binding or unsupported changes leave accepted state intact. Replaying an already accepted record's **exact signed bytes** returns a duplicate result without rolling state backward.

Two independently valid successors of one predecessor freeze authority: retain both signed byte strings as fork evidence and reject further key distribution, writer admission and content opening. Neither timestamps nor relay arrival order resolves the fork. Signatures use fresh randomness; signing the same membership body again produces a distinct signed record and can constitute a fork. Retry must retain exact existing bytes. This experiment has no durable receipt/outbox implementation.

The experiment supports removal and epoch rotation with an empty closing frontier. In the experiment, enrollment additions, admin transfer, nonempty historical frontiers, root recovery and recovery-recipient envelopes are explicitly unsupported. Production supports all of these except admin transfer; see the sections from [Production code](#production-code) on. The admin chooses the next epoch's CEK before signing the transition, because the transition commits to it (see below). Removed devices cannot be envelope recipients under the known current manifest, and old envelopes and old epoch-key handles fail after a transition. Rotation cannot erase old keys; a withheld transition is not detectable from a root pin alone. There is no globally latest membership or immediate offline-revocation guarantee.

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

The experiment chooses 1..16 roster members, 64 KiB maximum control input, at most 16 membership transitions and 16 admitted writer incarnations per authority instance. These budgets bound test support and preserve inspectable histories. The independent 16-member genesis is 58,899 bytes. In the experiment, one additional member, transition or writer is explicitly rejected.

Production keeps the per-object budgets: 1..16 roster members, at most 64 closing-frontier entries, and 64 KiB per control. It has no lifetime cap on transitions, recoveries or writer incarnations. Each of those is a signed control the device stores anyway. A device has at most one active writer per document, because a second root or a second successor of one incarnation freezes its lineage. A handed-off incarnation is closed and no longer counts as active. So the active writers of a document are bounded by the roster, however often devices reopen. `authority_tests.rs` applies 20 transitions and 40 successive writers. `revocation_tests.rs` (`matters-core`) runs 40 reopen-and-resume cycles across two devices that keep sealing and receiving, and applies a removal and a recovery after 20 transitions. Input is capped before decoding; fixed key lengths and roster count are checked before copying. The existing content-record cap remains separate. These controls do not establish a decompression, engine CPU, memory or total cryptographic-work bound for hostile signed Loro payloads.

The [active execution evidence](../experiments/sync-prototype/evidence/record-contract-2026-11-18.json) binds source, schemas, documents and fixture generators to a source commit. CI runs on every pull request and main push, checks its hashes, independently regenerates both fixture sets, and executes formatting, Clippy and the full prototype suite. This avoids a separate path allowlist drifting from the evidence manifest. Generator guard tests exercise drift and version-pin rejection with and without Python optimization. The key-distribution tests cover forged signers, changed recipients/keys, stale epochs, malformed authenticated key plaintext, invalid roster material, actor collisions, unsupported ceremonies and competing valid membership successors.

The control oracle uses patterned public bytes and invalid zero signatures. It proves CBOR/hash agreement, not valid cryptographic objects. Actual envelope tests use the selected provider on both sides: they do not establish independent interoperability of the exact hybrid suite. No timing claim, aarch64 run, fuzz campaign, side-channel claim, production usage-bound proof or independent security review follows from this experiment.

## Remaining implementation gates

Before application integration: durable authenticated membership/fork evidence and writer state; secure local key storage; root/enrollment possession ceremonies; recovery envelopes and all-devices-lost recovery; historical frontier admission (in production, see [Revocation](#revocation)); receive-chain/idempotency/quarantine semantics; crash-safe encrypted outbox and immutable retry bytes; restore/clone safety; task-domain import validation; aggregate crypto and hostile-import resource budgets; cross-target execution and independent review. The generic service API, acknowledged failure domain, transport/storage adoption and relay conformance remain with #34. The existing SQLite-backed task app remains unchanged.

The [review record](../experiments/sync-prototype/REVIEW_KEY_DISTRIBUTION.md) maps the review findings to their owning invariants and regression evidence.

## Durable receiver extension

[Durable authenticated receive](SYNC_DURABLE_RECEIVE.md) reuses the current-membership and writer-signature gate for multi-record chains. It pins the signed authority bundle to a dedicated SQLite database, retains encrypted pending/quarantined records and fork evidence, and atomically commits accepted replay order and task projections. The original first-record helper remains as a narrow regression fixture. Changing membership, historical admission, key storage and production integration remain outside this extension.

## Signed writer handoff

The [writer lifecycle extension](SYNC_WRITER_LIFECYCLE.md) adds a distinct `writer_handoff` kind and signature context, preserving existing `writer_start` bytes. Its independent populated/null closing-head vectors extend the structural oracle. The durable inbox admits roots and handoffs, replays control verdicts and gates retired history by exact signed prefixes. The strict in-memory first-record helper remains active-writer-only. Membership, epoch rotation, enrollment and key storage remain separate gates.

## Production code

[`matters-sync`'s authority module](../crates/matters-sync/src/authority.rs) carries this contract into production for [#37](https://github.com/thestephenromano/matters/issues/37): the same message codec, authority rules, envelope transcripts and key commitment, on the content-record keys and codec of the same crate. The production budgets are given above, and the closing frontier under [Revocation](#revocation). [Its tests](../crates/matters-sync/tests/authority_tests.rs) check the independent golden fixtures above in place. The first-record Loro import gate stays prototype test support; chain position and engine admission belong to the durable receiver.

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

- **Invitation.** The admin draws 16 bytes (128 bits) of OS entropy, read directly through `getrandom`, and shows the invitation as a QR code. Alternatively it shows the 26-character code and publishes the offer under the lookup, so a device that has only the code can fetch the route, root ID and manifest. The code-holding device checks the offer's tag before using anything in it. The secret is one-time, and the admin's application forgets it after one accepted request.
- **Request.** The new device pins the root by the invitation's root ID (`Authority::bootstrap_from_invitation`) and signs an `enrollment_request` with its own signing key, as the device ID of its two keys. It leaves the request in the relay's enrollment mailbox under the invitation's lookup ID, and the admin's device collects it there (SYNC_SERVICE_CONTRACT.md, #42).
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

**Not covered yet:** admitting earlier-epoch records into the engine. It remains open in #37.

## Recovery

When every device is lost, a 256-bit recovery secret restores the collection. All 256 bits are drawn from the OS through `getrandom`. This follows the maintainer decisions of 2026-11-21 on [#37](https://github.com/thestephenromano/matters/issues/37). The user keeps the secret as 24 English BIP39 words; nothing else holds the private keys it derives. [The recovery module](../crates/matters-sync/src/authority/recovery.rs) implements it.

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
- **Admitting a recovery** (`apply_recovery`). Every device checks the root signature against its pin. The recovery then becomes current even if generation N had later transitions the recoverer did not see, or was frozen by a membership fork, and it clears that freeze. From then on, any membership transition of an earlier generation is `Stale`, whoever signed it. Exact replays of accepted records still return a duplicate result. Two different valid recoveries to one generation retain both and freeze. Once frozen, a further different recovery to that generation keeps the first evidence pair, and the device stores nothing more of it. A recovery to the next generation clears the freeze.
- **After recovery.** Earlier devices are no longer members: they re-enroll through an invitation from the new admin. Until then they keep their unsent work for explicit recovery.
- **Through the relay (#42).** The recovering device signs in as the root (SYNC_SERVICE_CONTRACT.md, "The root's session"), reads the authority log, recovers locally, and publishes the root recovery and its recovery envelope. Its own writer claim then goes through its ordinary session.
- **The recovery kit.** The keys derive from the words *and* the route, so recovering needs the route too. Applications show the relay address, the collection's ID (its tenant and collection) and the 24 words together, and the user keeps all three.

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

## Persistence

A device's membership survives restarts, and is never split between stores ([`matters_core::storage::membership`](../crates/matters-core/src/storage/membership.rs), migration v13).

- **Keyring.** It holds only the two 32-byte seeds the device's ML-DSA and MLKEM keys are made from, under a random entry name. Behind the `KeyStore` trait this is oo7 in the app, per the #37 decision ([`matters-keyring`](../crates/matters-keyring/src/lib.rs): the Secret Service on the host, the secret portal's encrypted file keyring under Flatpak), and a map in tests. `matters-keyring`'s tests run against oo7's file backend; the Secret Service backend shares the adapter code above it but needs a keyring daemon that CI does not run.
- **SQLite** holds only public, signed or encrypted material:
  - `sync_device`: the route, the document, the pinned root ID, the device ID, the keyring entry's name and the device's standing;
  - `sync_controls`: the signed control records the device accepted, in order, including the competing record of any fork;
  - `sync_envelopes`: the key envelopes addressed to the device, one per epoch.
- **Replay on open.** Each open rebuilds the authority by replaying `sync_controls`, pinned by the stored root ID. A stored control that this version refuses is skipped rather than failing the open. A control refused as `Fork` that adds no new evidence (its authority or lineage was already frozen) is not stored.
- **One commit per change.** `apply_control` commits a transition, the new epoch's envelope and the device's standing in one transaction. If the device stays a member, the envelope is required, and without it nothing is stored.
- **Sealing follows.** It continues under the new epoch's key, or stops when the device is removed or the authority freezes on a fork.

**Writes always seal or wait.** A joined, active device seals every recorded write, keeping #36's rule that nothing is left to seal later. After an open, writes are refused until `resume_sync` loads the keys. Missing keys, or a keyring entry holding another device's keys, are explicit errors and change nothing.

**Unsent work is kept.** A removed or frozen device keeps editing locally. Its writes are journalled, unsealed and unsent, for explicit recovery.

**One writer per database.** Each open that seals is a new writer incarnation. Its claim is signed by the transaction of its first seal, with that seal, and hands off the device's last claimed writer at its exact head; an open that never seals claims nothing. Two handles can be open on one database at once, such as the Qt and GTK front ends, or the app and the command line. The newer open's writer is then the active one: the newest row of `sync_writers`, claimed or not. Every write checks this inside its transaction, before anything is written. A write from an older handle is refused with `MattersError::Superseded`, and nothing is journalled or sealed. The caller reloads: `Database::reopen` starts the handle over from the database as it stands, and `resume_sync` then makes it the writer, as a new open would. The app offers this as Reload, which takes over from the other open in turn. `tests/revocation_tests.rs`, `tests/sealing_tests.rs` and `tests/writer_claim_tests.rs` cover this with two handles on one file.

**Entropy.** Record IDs, content keys, device seeds, the recovery secret and signing randomness take 32 bytes from the OS, read directly through `getrandom`, so each carries 256 bits. The invitation secret takes 16 bytes, so 128 bits.

`create_collection` makes the first device's collection in one transaction: the device's keys, the recovery words, the genesis signed by the words' root, and the device's own envelope of the first key. Its seeds reach the keyring before that commit. A crash in between leaves an unused keyring entry and no collection, and a retry starts clean.

**Evidence:** `tests/membership_tests.rs` covers:
- creating a collection and sealing under it;
- refusing writes after a reopen, then resuming as a new writer under the same key;
- refusing missing or foreign keys;
- transitions without an envelope, with a damaged one, as replays, and across a restart;
- a recovery that revokes the device, keeping writes unsent, across a restart;
- a fork that freezes sealing, keeping both successors, across a restart;
- a crash before the collection commits.

## Joining and administering

[`matters_core::storage::enrollment`](../crates/matters-core/src/storage/enrollment.rs) (migration v14) runs the enrollment of [Enrollment](#enrollment) between two databases.

**The admin** (`invite`, `admit`, `remove_member`, `rotate_epoch`) signs the next transition with a fresh content key. It wraps that key for every member and for the recovery key. When admitting, it also wraps every earlier epoch's key for the new member, taken from its own stored envelopes. The transition, its own envelope and everything for others commit in one transaction. Sealing then continues under the new epoch.

**Publishing.** Everything signed for others waits in `sync_publish`, and `outgoing_controls` returns exactly the stored bytes until `acknowledge_control`. Signing a transition again would fork the collection, so a retry never re-signs. A crash before the commit publishes nothing, and the admission can simply be made again.

**A new device** (`start_enrollment`) generates its keys and signs its request under the invitation. The request is kept, so asking again under the same invitation returns the same bytes. A new invitation into the same collection (another secret or manifest, as when the first request never reached the admin) replaces the request. The new request is signed with the same device keys and bound to the new invitation. An invitation into another collection is refused until `cancel_enrollment` gives the pending one up. `finish_enrollment` then takes:
- the collection's control records, ending with the admin's transition adding the device;
- the envelopes the admin wrapped for it.

It verifies each object and skips any that fails, keeping only the controls it accepted. A forged writer claim, a stray enrollment request or a forged envelope therefore cannot block joining. It tries every candidate envelope, requires one for the current epoch, and turns sync on, stores what it accepted and starts sealing in one transaction. Until then, nothing is half-joined.

**One document.** Every device seals for the same document of the collection, so a joining device needs nothing more to find it.

**Evidence:** `tests/enrollment_tests.rs` covers:
- a second device joining through a scanned invitation, after which both seal under the new epoch with one key, and the new device reads what the admin wrote before it joined;
- outgoing bytes kept until acknowledged;
- a restart of the new device;
- rotation and removal reaching the member, and only the admin making them;
- finishing before the admission, or without a current envelope;
- another collection's invitation while joining;
- a crash before an admission commits.

`tests/revocation_tests.rs` covers:
- joining and recovering with an unsigned claim, an enrollment request and forged envelopes injected;
- a replaced invitation, and a cancelled enrollment.

## Recovering, pausing and leaving

**Recovering onto a new device.** `recover_collection` takes the 24 words, the collection's control records as the relay holds them, and its recovery envelopes. `recover_collection_holding` also takes the content records the relay holds. Then:
1. It bootstraps from the root the words derive; other words fail with `Identity` before anything is stored. Controls that fail verification are skipped.
2. It opens every epoch key it has an envelope for, trying every candidate envelope and skipping those that do not open.
3. It signs the root recovery that makes the new device the sole admin of the next generation. Given records, the recovery closes every writer of the superseded generation at the head of the chain those records hold, at null where they hold nothing (see [Revocation](#revocation)).
4. It keeps its own envelopes of the new key and the recovered ones, so later admissions can hand on history.
5. It queues the recovery and its recovery envelope for publishing, all in one transaction.

It reports the manifests of epochs that had no recovery envelope. Their records cannot be read again, and the report makes that loss explicit. Every epoch has one from the start: creating a collection now publishes epoch 0's recovery envelope too.

**Pausing.** Pausing sync is a transport matter (#39). A joined device keeps sealing every write, so the records wait in the outbox and go out when sending resumes. Nothing is ever journalled unsealed on a device that is still a member.

**Leaving.** A device that leaves its collection never loses unsent work:
- One the admin removed, or that a recovery superseded, stops sealing. Its later writes are journalled and kept. Its sealed but unacknowledged records stay in the outbox.
- Its rows remain the user's local data.
- Rejoining is a fresh enrollment, under new device keys.

**Evidence:** `tests/enrollment_tests.rs` covers:
- recovering a collection whose only device was lost, reading its epoch-0 records and sealing for generation 1;
- refusing wrong words and a database that already syncs;
- a surviving device following the recovery;
- an epoch without its envelope being reported unrecoverable.

## Writer admission in production

Production admits a writer claim signed under any known manifest of the current membership generation, provided the claiming device was in that manifest's roster *and is in the current roster*. The claim's signature is checked with the key that the claim manifest's roster lists. The experiment admitted only claims signed under the current manifest.

The first condition follows from how claims are read. Replicas read the relay's authority log in order. A claim a device signs just before a concurrent transition therefore reaches every replica after it, and under the experiment's rule every replica refused it, so that writer's records waited forever. #39's recorded-schedule convergence test found exactly this case. The second condition is revocation: a device that the current roster no longer lists claims no new writer.

- **What admission grants:** nothing by itself. Each record is authorized against the roster of its own epoch (`Authority::authenticate_at`), so admitting a claim from an earlier manifest gives no device rights it lacked when it signed. A removed device's records are further bounded by [Revocation](#revocation).
- **Replays:** an admitted claim replayed later is the same step and changes nothing.
- **Competing definitions:** these still freeze the lineage.
- **Refused claims:** a claim under an unknown manifest, or one from a recovery's earlier generation, is stale. A device outside the claim manifest's roster is refused (`Unauthorized`). A device outside the current roster is refused (`Revoked`), unless a signed closing frontier names that exact writer.

## Revocation

A device leaves the roster in one of two ways: the admin removes it, or a root recovery supersedes its generation. A signed record it made earlier stays authentic, and its epoch's key cannot be taken back. So revocation bounds what is *admitted*: the leaving device's records are admitted only up to a prefix that the remaining authority signed. This is the historical closing frontier, the ninth field of the `membership` and `root_recovery` body ([CDDL](protocol/sync-key-distribution-v0.cddl)):

```text
frontier = [0*64 [device48, document16, incarnation32, closing_head48_or_null]]
```

Each entry names one writer incarnation of a device that is not in the new roster. Its closing head is SHA-384 of the last retained signed content record of that writer's chain, or null for an empty prefix. This is the exact-prefix meaning of a `writer_handoff`'s closing head ([writer lifecycle](SYNC_WRITER_LIFECYCLE.md)). Entries are strictly sorted by device, document and incarnation. A frontier never names a member of the roster it starts, and it names only devices that were members earlier in its history. An empty frontier is the no-removal case, and the existing vectors keep it byte for byte.

**What the admin signs.** `remove_member` closes each of the removed device's writers that no signed prefix closes yet, at the head of the chain the admin holds:
- the head it applied, or that the checkpoint it started from covered;
- followed through the signed records it keeps waiting, while each step has exactly one successor;
- null if it holds nothing of that writer.

A writer already handed off at a signed head needs no entry of its own. Its prefix stays bounded by that handoff, provided the chain of handoffs after it reaches a writer the frontier names.

**What a recovery signs.** `recover_collection_holding` closes every writer of the superseded generation at what the records it is given hold. `recover_collection` holds no records, so its recovery closes nothing yet, and the old generation's records are not admitted. Every later transition by the new admin (an admission, a rotation, a removal) closes such a writer at the head it then holds, once it holds something of it. A device recovering through the relay therefore passes the relay's records to `recover_collection_holding`. A receiver keeps a `Revoked` record waiting rather than discarding it, because it is signed and a later frontier may name its writer.

**What receivers admit.** For a device not in the current roster:
- A writer named by a frontier admits records up to its closing head. `writer_history` reports that head as the writer's `closed` boundary, so the receiver stops there exactly as for a handoff, and a record after it waits `AfterClose`. A record within the prefix still applies on a device that had not received it yet.
- A writer handed off at a signed head admits records up to that head, if the handoffs after it lead to a named writer.
- Any other writer admits nothing: `authenticate_at` refuses its records with `Revoked`, after their signature verifies.
- A new `writer_start` or `writer_handoff` is refused with `Revoked`.
- A lineage frozen on fork evidence (a second signed successor, or competing handoffs) is recovered this way. Its writers admitted within such a prefix are admitted again, and the receiver applies only the records on the chain that ends at the closing head, leading on from what it applied. The rest wait `AfterClose` ([durable receive](SYNC_DURABLE_RECEIVE.md#recovering-a-frozen-lineage)). Removing the device is how an admin recovers its fork; the device can be invited again as a new member.

The first signed entry for an incarnation binds, and a later one changes nothing. A removed device ID is never added back: rejoining is a fresh enrollment under new keys. These rules depend only on which controls were accepted, not on the order replicas read claims and transitions in.

**The contract's scope.** An honest admin's frontier closes what the admin has seen. Records the removed device made but the admin never received are not admitted by anyone, even if the device made them before it was removed. A device that applied a removed device's records beyond the frontier before it learned of the removal keeps them, as for a handoff whose prefix it had passed. From then on it admits nothing more of that writer: a receiver refuses a writer's next record once its chain holds the closing head (a null head is held from the start). The frontier therefore bounds every record admitted after the removal is known. As before, a withheld transition is not detectable from a root pin alone.

**Known limit: the authority log is not compacted.** The log only grows. It gains one writer claim of about 3.7 KB per writing open, the first seal of an open that writes or makes a checkpoint, and a transition plus about 4.9 KB of envelope per member per membership change. An open that writes nothing, such as a `matters-syncd` run or an app launch with no edit, adds nothing (`writer_claim_tests.rs`). The relay bounds a route by its byte quota and at most 2,048 transitions ([service contract](SYNC_SERVICE_CONTRACT.md)). A device that reads the whole log holds it in memory, under the client's caps:
- **Joining or recovering:** the authority log up to 128 MiB. At 4 writing opens a day on each of 3 devices, this is 134,217,728 / (12 × 3,695 × 365) ≈ 8.3 years of writing opens. A single device at 4 writing opens a day has about 25 years. Membership changes add little at this scale: a change with 3 members is about 33 KB.
- **Recovering:** the relay's content records up to 512 MiB as well. At about 4.8 KB per edit (the #40 workloads), that is about 110,000 edits, or about 6 years at 50 edits a day.

Past either cap, joining or recovery fails with a retry. Nothing is lost, but a device cannot join until the log is compacted. Compaction would replace the claims of retired writers and old epochs' envelopes with a signed summary. It is not implemented.

**Evidence:**
- `authority_tests.rs`:
  - records within, at and beyond a removal frontier;
  - the same with the handoff and the removal read in either order;
  - an empty frontier;
  - a handed-off writer bounded through a named successor;
  - malformed, unsorted and member-naming frontiers;
  - a removed ID re-added;
  - a recovery closing at what it holds, and one holding nothing.
- `tests/vectors/key-distribution/generate.py` computes a removal frontier (one populated head, one null) and a root-recovery frontier independently. Both are checked byte for byte here, and with cddl-cat in the prototype.
- `revocation_tests.rs` (`matters-core`):
  - After A removes B, a record B signs under the old epoch, chained on its head, waits `AfterClose` on A and on a third device C. A record within the frontier still applies on C. B's claim when it seals after reopening is refused by both.
  - The same holds for a generation-0 device after a root recovery that holds its records.
- `receive_limits_tests.rs` (`matters-core`): a lineage frozen on a second signed successor, and one frozen on competing handoffs, recovered by the admin's removal up to its closing head, with nothing off the closed prefix applied.
