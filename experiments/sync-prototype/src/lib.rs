//! Bounded Loro/SQLite adoption experiment. Not used by the application.

mod model;
mod replica;
#[cfg(feature = "record-contract")]
pub mod wire;

pub use model::{Command, Entity, Kind, Projection, Schedule};
pub use replica::{Admission, Fault, InboxEntry, Record, Replica, Request, RestoreStats};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// The experiment's single untyped-error constructor, shared by test support.
pub fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}

/// Whether a database holds any user schema object: table, index, view or
/// trigger. Only SQLite's own reserved `sqlite_` objects are ignored; GLOB is
/// case-sensitive and has no single-character wildcard for `_`. A store that
/// finds any object must then require its own format marker before writing.
pub fn has_user_schema(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*')",
        [],
        |r| r.get(0),
    )?)
}

/// Rebuild the bounded experiment's task projection from staged engine state.
/// This is not a complete hostile-document resource or schema validator.
pub fn project_document(doc: &loro::LoroDoc) -> Result<Projection> {
    model::project(doc)
}
