//! Bounded Loro/SQLite adoption experiment. Not used by the application.

mod model;
mod replica;

pub use model::{Command, Entity, Kind, Projection, Schedule};
pub use replica::{Admission, Fault, InboxEntry, Record, Replica, Request, RestoreStats};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}
