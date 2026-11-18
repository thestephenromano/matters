# Key-distribution review remediation

Scope: review of PR #93 at `b15a891`; changes remain test-only. The [contract](../../docs/SYNC_KEY_DISTRIBUTION.md) states supported behavior; the [active evidence](evidence/record-contract-2026-11-18.json) binds final code, documents and checks to their source revision.

## Root causes and enforcement boundaries

The writer registry originally reserved actor/incarnation identities without representing an active lineage or an accepted successor. As a result, replacement never retired an old incarnation, and distinct roots/successors could all enter the registry. Two regression witnesses fail on the reviewed code: retired content returns `Ok` instead of `Stale`, and a competing signed successor returns `Ok(true)` instead of `Fork`.

Authority now owns one active writer head per device/document, immutable historical admission bytes, predecessor relations and bounded fork evidence. Successful replacement advances the head. Receive admission consults that head before opening content. A second valid successor freezes the affected lineage; it does not authorize either branch by relay arrival order. Exact historical retries never move the head. Capacity rejection occurs before inserting or advancing state. The existing 16-registration budget bounds retained history and lineages; each fork stores two control records, each under the existing 64 KiB cap. This is not durable quarantine or historical-data reconciliation.

The workflow maintained a path allowlist separate from its evidence manifest. Adding hashed documents did not update that allowlist. The workflow now runs on all pull requests and main pushes; every future manifest path participates without maintaining duplicate routing policy. The evidence guard also requires the design/ADR and new support/guard files explicitly.

## Findings and disposition

| Finding | Correction or deliberate boundary | Evidence |
| --- | --- | --- |
| Hashed design/ADR edits did not trigger CI | Remove workflow path filtering for both events. | Workflow configuration validation and CI execution; manifest guard still binds final files. |
| Replaced and branching writer incarnations remained admitted | Active lineage plus historical predecessor/signed-byte retention; retire old heads and freeze valid forks per device/document. | Replacement/replay test and competing roots/successors in both arrival orders, including a delayed competitor after a further replacement. |
| Schema imposed semantic restrictions absent from parse | CDDL describes u64 generation/epoch and optional checkpoint structurally, matching parse; authority retains generation-zero/bootstrap restrictions. | Independent CDDL decoder and parser accept the same four unsupported-authority cases; bootstrap/membership still reject them. |
| Membership-fork coverage did not exercise every admission path | Start with a valid admitted writer, key envelope and opened content, then fork membership. | Wrap, unwrap, writer admission and content opening all return `Fork` using otherwise-valid objects. |
| Python optimization removed oracle checks | Explicit failure branches in both generators, including the sibling content oracle's version, drift and HKDF consistency checks. | Committed subprocess tests run both modes against drifted fixtures and each wrong version pin. |
| Unreachable membership `Stale` branch | Remove it: every known non-head manifest already has a successor, and that case is handled as duplicate/fork. | Existing membership sequence, duplicate, missing-parent and fork tests. |
| Record KDF duplicated across suites | Shared test-only `record_crypto::content_key`; independent Python key vectors still check the helper, alongside explicit PRK/expand checks. | Record-contract oracle tests plus actual encrypted key-distribution flow. |
| Signature verification duplicated | One typed verification helper for control and content messages, preserving each domain context and original signed slice. | Existing wrong signer/context and signed-content rejection tests. |
| Two canonical-CBOR strategies | Retain deliberately: zero-copy field-width checks for large content; bounded whole-record comparison for small controls. No wire-format change. | Both independent fixture suites, negative encodings and truncation tests. |
| Device ID cloned full public keys | Hash borrowed public-key slices directly; no temporary roster entry. | Exact independent fingerprint/ID vectors and generated-device round trips. |

## Mutation evidence

Five targeted test-support mutations exercise retired-head admission, competing-successor admission, writer fork enforcement, replay reactivation and membership fork enforcement. The first run exposed a test-order gap: replaying the current head immediately after the retired head hid a mutation that reactivated writers on replay. The regression now checks the retired record immediately after each transition/retry. The final mutation outcomes and the initial survivor are retained in the execution evidence. These checks cover the named invariants, not exhaustive mutation coverage.

## Sibling search and limits

Searched authority bootstrap/membership/writer/envelope/content paths, both record-key derivations, both Python fixture generators and workflow/evidence routing. The same bare-assert issue existed in the content oracle and is corrected there too. Membership already froze conflicting successors; writer admission lacked the corresponding state. Content-record chain persistence is still outside this experiment and is not represented as fixed by these changes.

The authority, lineage evidence and secrets remain in memory. Historical updates from retired incarnations require future authenticated-frontier rules; this helper rejects them. Complete receive chains, recovery/enrollment, durable encrypted outbox, independent suite interoperability, cross-target execution and resource/security review remain open. No new performance or production-security claim is made.
