//! Bounded Loro/SQLite adoption experiment. Not used by the application.

mod model;
mod replica;
#[cfg(feature = "record-contract")]
pub mod wire;

pub use model::{Command, Entity, Kind, Projection, Schedule};
pub use replica::{Admission, Fault, InboxEntry, Record, Replica, Request, RestoreStats};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}

/// Rebuild the bounded experiment's task projection from staged engine state.
/// This is not a complete hostile-document resource or schema validator.
pub fn project_document(doc: &loro::LoroDoc) -> Result<Projection> {
    model::project(doc)
}
