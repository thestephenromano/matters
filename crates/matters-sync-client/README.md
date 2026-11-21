# matters-sync-client

The resumable encrypted sync client (#39) between any application's local store (`matters_sync::store::LocalStore`; Matters' `matters_core::Database` implements it) and a relay that speaks the [sync service contract](../../docs/SYNC_SERVICE_CONTRACT.md).

`Client::sync_once` runs one round:
1. Push the device's controls, then its records. Each is acknowledged in the store only on the relay's durable `200` or `201`, and a lost acknowledgement re-sends identical bytes.
2. Pull the authority log whole, pairing each transition with this device's envelope of the new epoch's key.
3. Pull the record log.

A page's cursor is saved only after the page is applied. Applying is idempotent, so an interrupted page is replayed whole. A changed relay generation (a restore) resets both cursors and re-queues everything, once per round.

`Client::run` repeats rounds:
- on relay hints over the WebSocket, local edits (`waker()`), and every minute;
- with exponential backoff and jitter after temporary failures;
- not at all after a condition that needs attention, until woken.

`status()` says which, without cryptographic detail. Store calls run on a blocking thread, never across a network wait.

**Transport:** reqwest on rustls over HTTP/2, with ring as the crypto provider and the operating system's trust store through rustls-platform-verifier, as decided for #39.

**Tests:** `tests/client_tests.rs` runs two Matters databases against the real relay and PostgreSQL. It covers:
- convergence;
- lost acknowledgements and a relay restore that duplicate nothing;
- a removed device that is told and keeps working locally;
- running clients following each other through hints.

Run it as the relay's tests are run; see [matters-relay](../matters-relay/README.md).
