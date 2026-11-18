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

/// Judge one record by the record and what it depends on, never by records
/// applied concurrently. `staged` holds the record's operations with no pending
/// engine updates; `start`/`end` are its partial version range from the blob
/// metadata. Returns the rejection reason, or `None` when admissible:
/// - `domain-projection`: the record's causal past does not project;
/// - `domain-repair`: it needs a merge repair (#113) that the same past
///   without the record did not, so only concurrency produces conflict copies;
/// - `deletion-evidence`: it removes a tombstone.
///
/// The past without the record is causally closed: nothing else can depend on
/// operations that were unpublished until this record.
pub fn causal_past_verdict(
    staged: &loro::LoroDoc,
    start: &loro::VersionVector,
    end: &loro::VersionVector,
) -> Result<Option<&'static str>> {
    let frontier: loro::Frontiers = end
        .iter()
        .filter(|(_, counter)| **counter > 0)
        .map(|(peer, counter)| loro::ID::new(*peer, *counter - 1))
        .collect();
    let past = staged.fork_at(&frontier)?;
    let Ok(after) = model::project(&past) else {
        return Ok(Some("domain-projection"));
    };
    let mut before = past.oplog_vv();
    for peer in end.keys() {
        let counter = start.get(peer).copied().unwrap_or(0);
        before.set_end(loro::ID::new(*peer, counter));
    }
    let prior = past.fork_at(&past.vv_to_frontiers(&before))?;
    let prior = model::project(&prior)
        .map_err(|_| invalid("causal past without the record does not project"))?;
    if !after.repairs.is_subset(&prior.repairs) {
        return Ok(Some("domain-repair"));
    }
    if !prior.tombstones.is_subset(&after.tombstones) {
        return Ok(Some("deletion-evidence"));
    }
    Ok(None)
}

/// Rebuild the bounded experiment's task projection from staged engine state.
/// This is not a complete hostile-document resource or schema validator.
pub fn project_document(doc: &loro::LoroDoc) -> Result<Projection> {
    model::project(doc)
}
