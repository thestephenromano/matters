use thiserror::Error;

#[derive(Error, Debug)]
pub enum MattersError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Entity not found: {entity} with id '{id}'")]
    NotFound { entity: &'static str, id: String },

    #[error("Invalid data: {0}")]
    Validation(String),

    #[error("No per-user data directory could be determined (is HOME set?)")]
    NoDataDir,

    #[error("No per-user configuration directory could be determined (is HOME set?)")]
    NoConfigDir,

    /// The sync engine refused or failed an operation.
    #[error("Sync engine error: {0}")]
    Engine(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// The database was written by a newer Matters (#41): it is left
    /// untouched; use that version, or restore an older backup.
    #[error("This database needs a newer Matters (schema {found}; this version knows up to {supported})")]
    NewerSchema { found: i64, supported: i64 },

    /// Leaving sync would leave this many edits on this device only: the
    /// relay has not acknowledged them. Nothing changed (#42).
    #[error("{0} edits have not reached the relay yet")]
    UnsentWork(usize),

    /// A transition keeps this device a member, but its envelope of the
    /// new epoch's key was not supplied: nothing changed (#39).
    #[error("The new epoch's key envelope is missing")]
    EnvelopeMissing,

    /// Another open of this database (another window, front end or the
    /// command line) has since become this device's writer: this handle
    /// can no longer seal, and nothing was written. Reload (#45).
    #[error("This database was opened again elsewhere; reload it to keep editing")]
    Superseded,

    /// A batch of changes would record more than one sync record holds:
    /// nothing changed. It is never split; send smaller batches (#203).
    #[error("These changes are too large to record at once ({bytes} bytes; the limit is {limit})")]
    BatchTooLarge { bytes: usize, limit: usize },

    /// A sync control record, envelope or key was refused (#37). Carries
    /// no key material.
    #[error("Sync authority error: {0}")]
    Authority(#[from] matters_sync::authority::Error),
}

pub type Result<T> = std::result::Result<T, MattersError>;
