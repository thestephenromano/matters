# matters-sync-client

The resumable encrypted sync client (#39) between any application's local store (`matters_sync::store::LocalStore`; Matters' `matters_core::Database` implements it) and a relay that speaks the [sync service contract](../../docs/SYNC_SERVICE_CONTRACT.md).

`Client::sync_once` runs one round:
1. Push the device's controls, then its records. Each is acknowledged in the store only on the relay's durable `200` or `201`, and a lost acknowledgement re-sends identical bytes. A control the relay refuses for good is set aside, so the round still pulls.
2. Pull the authority log page by page, trying each of this device's envelopes of the new epoch's key for a transition until one opens. A transition none opens waits for a later round; it is never skipped.
3. Pull the record log.

A page's cursor is saved only after the page is applied. Applying is idempotent, so an interrupted page is replayed whole. A changed relay generation (a restore, or a page whose high-water mark is below the device's cursor) resets both cursors and re-queues everything, once per round, and every control the device knows is published again. A device the relay does not know publishes its membership transitions without a session and signs in once more; if the relay still refuses it, `Attention::RelayBehind` (its own authority still admits it) is told apart from `Attention::NotAMember`.

**Relay responses (#45):** the relay is trusted with availability only. A response body is read up to 32 MiB; a page must hold at most the 256 entries asked for, in ascending sequence, with a cursor that advances until it is complete; a round reads at most 16,384 pages of a log; a relay's `retry-after-ms` is followed for ten minutes at most; the hints WebSocket connects on its own task within 15 seconds. A relay that breaks these is retried later.

`Client::run` repeats rounds:
- on relay hints over the WebSocket, local edits (`waker()`), and every minute;
- with exponential backoff and jitter after temporary failures;
- not at all after a condition that needs attention, until woken.

`status()` says which, without cryptographic detail. Store calls run on a blocking thread, never across a network wait.

**Transport:** reqwest on rustls over HTTP/2, with ring as the crypto provider and the operating system's trust store through rustls-platform-verifier, as decided for #39. HTTPS and WSS only (`https_only`); plain `http://` only to this machine (`127.0.0.1`, `::1`, `localhost`), for tests and a relay on the same host (`check_endpoint`).

**Tests:** `tests/client_tests.rs` runs two Matters databases against the real relay and PostgreSQL. It covers:
- convergence;
- lost acknowledgements and a relay restore that duplicate nothing;
- a removed device that is told and keeps working locally;
- running clients following each other through hints.

Run it as the relay's tests are run; see [matters-relay](../matters-relay/README.md).

`tests/hostile_relay_tests.rs` needs no PostgreSQL: a scripted relay (`tests/support`) misbehaves on purpose. It covers a cursor that never advances, oversized bodies, a forged envelope ahead of the genuine one, a refused control, a relay restored behind a device's membership, a reset within a generation, an unbounded `retry-after-ms`, and a hints connection that never opens. `tests/fuzz_replay.rs` replays the `relay_page` and `relay_snapshots` fuzz corpora ([fuzz/](../../fuzz/README.md)).
