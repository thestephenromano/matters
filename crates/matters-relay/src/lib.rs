//! The durable encrypted sync relay (#38): the store behind
//! docs/SYNC_SERVICE_CONTRACT.md, on PostgreSQL (ADR 0003).
//!
//! The relay stores and serves opaque signed bytes. It reads only routing
//! fields and the public parts of authority objects it needs to authorize
//! uploads; it never holds content keys and knows nothing about tasks.
//!
//! - A **route** is provisioned by an operator with its root key and
//!   signed genesis, which is verified before the route exists.
//! - A **record** append is durable when it returns: the bytes, the record
//!   ID's uniqueness, the digest and the route's next gapless sequence
//!   commit in one transaction (`synchronous_commit = on`). A retry of the
//!   same bytes returns the original receipt; other bytes under the same ID
//!   are a permanent conflict. Appends to one route serialize on its row.
//! - **Authority objects** are addressed by their SHA-384. Membership
//!   transitions and root recoveries are verified against the route's
//!   authority before they are stored, because uploads are authorized by
//!   the newest accepted roster; other kinds are checked structurally and
//!   left for clients to verify.
//! - Pages are read up to the high-water mark captured at their start,
//!   under the route's generation; a cursor from another generation is
//!   refused, so a restore can never be mistaken for continuity.

pub mod api;
mod cbor;
mod export;
mod fault;
pub mod hints;
mod sessions;
mod store;

pub use sessions::{Session, CHALLENGE_MS, SESSION_MS};
pub use store::{Appended, Cursor, Page, Receipt, Relay, RelayError, MAX_PAGE};
