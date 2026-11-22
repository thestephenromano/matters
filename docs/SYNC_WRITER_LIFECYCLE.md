# Durable writer handoff experiment

Status: bounded extension of [durable receive](SYNC_DURABLE_RECEIVE.md) and the [encrypted outbox](SYNC_ENCRYPTED_OUTBOX.md), tracking #35, #36 and #37. One pinned membership/key epoch and one document remain the scope. No production application, account enrollment, revocation or recovery ceremony is enabled.

## Decision and contract

Reuse authenticated hash-chain checkpoints, the transactional inbox/outbox pattern and the already approved SQLite/rusqlite, deterministic CBOR, ML-DSA and SHA-384 implementations. The [existing record contract](SYNC_RECORD_CONTRACT.md) defines the full-record hash and fork invariant; [SQLite transactions](https://www.sqlite.org/lang_transaction.html) supply the local atomic boundary. MLS remains the group-lifecycle alternative evaluated in ADR 0002; replacing that earlier protocol decision or introducing a broker is not needed for this fixed-membership writer handoff. This slice implements the approved restart/historical-record work with existing dependencies, not a new cryptographic primitive.

A fresh writer needs a new engine actor and incarnation. Retiring its predecessor cannot also discard that predecessor's already sealed ciphertext: a normal restart must still deliver those exact bytes. Conversely, a receiver must not accept unlimited later records under a retired writer's key. A signed closing head states the exact retained prefix.

The new message kind `writer_handoff` has its own authenticated kind/signature context. Existing `writer_start` bytes remain unchanged. The exact canonical body is:

```text
B = [0, "matters-sync-pq-v0", "writer_handoff", app16, tenant16, collection16,
     device48, [manifest48, document16, device48, new_incarnation32,
                new_actor_u64, previous_incarnation32, closing_head48_or_null]]
record = [B, signature3309]
signature context = ASCII("matters-sync/v0/writer_handoff")
```

The current member signs as itself. Its predecessor must already be admitted for that device/document, and the incarnation/actor must be unused. `closing_head` is SHA-384 of the complete last retained signed content record; null asserts an empty old chain. It is not a timestamp, server acknowledgement or claim of global freshness. Unsupported clients reject the new kind; there is no reinterpretation of legacy replacement starts. The existing six-field `writer_start` remains usable for roots; the durable receiver rejects its predecessor-bearing form because it lacks a closing boundary. The in-memory authority still admits that form for the [earlier first-record experiment](SYNC_KEY_DISTRIBUTION.md), and its writer history reports such a predecessor as retired without a signed boundary (`Boundary::Unbounded`), never as a root or an error.

Two authenticated successor claims for one incarnation, or incompatible same-device definitions of one incarnation, are competing writer histories. Reusing an actor or incarnation does not hide the signed contradiction. Actor-conflicting controls are re-evaluated when further controls arrive; identity collisions from another device cannot freeze the identity owner. Preserve the signed records and freeze that device/document lineage. Contradiction is judged on the signed claim, not its bytes: ML-DSA signing is randomized, so a re-signed identical claim is the same lineage step. It is retained, admitted as `same-writer-claim`, and freezes nothing. Senders should still retry with existing bytes. Root and handoff authority rules share the existing admission implementation. Membership authority, epoch commitment and author-key verification are unchanged.

## Sender transaction

`Outbox::open_announced` runs a single writer transaction that captures the old owner and its committed content head, allocates the new actor/incarnation, signs the root or handoff, stores the exact announcement, abandons uncertain reservations and replaces the owner. No new writer can seal before that commit. The signing callback is trusted test support, uses the approved signer in fixtures, and must not publish bytes; structural checks ensure it returns the requested canonical message but do not independently authenticate an arbitrary callback's signature.

The signer runs inside the transaction. Failure before commit leaves the old owner intact; failure/exit after commit leaves a deliverable announcement. A subsequent open creates another signed handoff, including an empty prefix when the previous writer sealed nothing. `Delivery::announcements` returns all stored signed bytes in creation order, recorded in an explicit sequence column rather than SQLite rowids, which a VACUUM or rewrite may renumber; it needs no keys, signing or writer allocation. Retries retain those bytes exactly. Records remain independently retrievable and ACK-able, including old-writer ciphertext.

The outbox pins announced versus standalone mode. Switching modes is refused: a standalone experiment cannot silently become a signed lineage, and a signed lineage cannot be reopened through an API that omits its announcement. Existing standalone test support remains available. Unsealed old-writer intents keep the existing recovery-required behavior; they are not rebound or signed under an old actor after reopen.

## Receiver transactions and eligibility

`receive_writer` durably retains bounded control input and an unprocessed classification. Exact-byte retries are idempotent. It does not invent a remote cursor or ACK contract. `process` derives writer authority from the pinned initial roots and committed admission order, attempts waiting controls to a fixed point, and commits admissions/fork verdicts with content classifications, snapshot and projection. Missing control predecessors wait. Malformed, unauthorized, legacy replacement and stale controls are quarantined. Signed control bytes and verdicts remain inspectable after restart. The configuration authority is verified once per open. Committed admission ordinals are immutable, so each pass replays only admissions committed since the cached authority was built; the cache advances only after the processing transaction commits, and a reopen replays the committed order once.

An unknown content incarnation waits for its writer admission only after its author's current-member signature verifies; forged or unsigned bytes are quarantined at once. A waiting record is re-authenticated only when its incarnation has been admitted. Content of an unknown incarnation whose author's lineage is frozen, such as content of a refused competing handoff, is classified `fork` (`frozen-writer-lineage`). Once established, immutable actor/signature facts remain cached. Retirement eligibility and writer-lineage forks are separate mutable policy and are checked on every processing pass. The strict first-record helper still refuses retired writers; the durable receiver uses a historical signature verifier and enforces the rules below before decryption/application.

- For a closed incarnation, walk the signed content hash chain backward from the declared closing head. Only authenticated records from that incarnation contribute. While the prefix is incomplete, the retired writer's own records keep applying in chain order: none can pass an absent closing head, and a record outside the prefix freezes the device once the prefix is complete. A missing closing record therefore delays only the successors, not the retained prefix.
- Null is a complete empty prefix. A known signed record outside a complete declared prefix, or a closing head pointing at another incarnation, contradicts the handoff. Freeze the device's lineage and retain all bytes and prior application for recovery.
- A replacement's content waits until every earlier closing head in its lineage has been applied. Empty intermediate writers do not bypass an earlier nonempty closure. This gate also applies when new content has no Loro causal dependency on the old records. A handoff chains through its closing prefix exactly as a same-writer successor would: if that complete prefix holds a quarantined or forked record, the successor's content is permanently `blocked-by-rejected-predecessor`, not waiting for history.
- Content-chain equivocation freezes the device's subsequent writers as well. Another device's chain can continue. Cross-writer record-ID reuse retains the existing evidence policy and does not by itself attribute fault or freeze either writer.
- No fork handling erases an already committed projection. Classifications expose the frozen state. The experiment does not reconcile competing histories or guarantee identical pre-fork views to receivers that saw different branches before the contradiction became observable.

Receiver reopen uses the same trusted route, root, genesis and initial roots; it replays the durable controls rather than asking the caller to mutate the configuration. Membership/key epoch changes remain unsupported. All public local actor IDs use an eight-byte little-endian blob in SQLite, preserving the full unsigned 64-bit range.

## Restore and cloning

An ordinary sequential restart has one signed successor and carries its old prefix forward. Restoring an older database may instead create a competing successor if another installation has progressed since that backup. There is no local evidence capable of proving that the unseen branch does not exist. The receiver retains both signed branches and freezes when they meet; the restored sender never seals old-writer intents. Local unsent work remains available for explicit recovery. This is detection and preservation, not transparent concurrent-clone merging or proof of freshness from an isolated relay.

Both stores increment their experimental format markers and refuse older markers; no live-database migration is implied. A consistent SQLite backup is the tested restore input, not an unsynchronized copy of a live WAL/database pair.

## Bounds and assurance

Retain at most 32 control deliveries, each at most 64 KiB, in addition to the existing 32-content-record inbox budget. Authority retains at most 16 admitted incarnations as in the earlier experiment. These are fixture capacities; exhaustion refuses new work while reopen, retained-byte delivery, exact retries and inspection remain possible. Outbox identity/announcement history has no lifetime open cap; production retention and disk-budget policies remain open.

Independent Python CBOR/SHA-384 fixtures include populated and null closing heads; existing vectors are preserved. Tests cover real outbox reopen and old/new content delivery, reversed control delivery, missing closing history, empty intermediate writers, competing replacements, forged/changed closing heads, retained pre-fork state, consistent-backup clones, transaction rejection and exact-byte retries. Six additional child-process exits cover before/after sender writer commit, control receipt and admission/application commit. Boundary tests cover full-width actor IDs, capacity, legacy admission rejection and mode changes. The active evidence records executed checks and targeted guard-removal witnesses.

In production ([`matters-sync`'s authority](../crates/matters-sync/src/authority.rs)), the authority has no lifetime cap on admitted incarnations. A device has at most one active writer per document, and a handed-off incarnation is closed, so reopening never exhausts admission. A membership transition or root recovery can also close a departing device's writers with the same exact-prefix closing head, in its closing frontier ([key distribution, "Revocation"](SYNC_KEY_DISTRIBUTION.md#revocation)). Receivers stop at a frontier's head exactly as at a handoff's.

No new performance, hostile-import resource, power/media/host-loss, cross-target or production-security claim follows. Secret storage, changing membership/epochs, revocation, administrative authority transfer, all-devices-lost recovery, collection-wide multi-document conflict indexing, a service API, domain/outbox atomic integration and independent security review remain separate gates.
