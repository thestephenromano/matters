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

    /// A transition keeps this device a member, but its envelope of the
    /// new epoch's key was not supplied: nothing changed (#39).
    #[error("The new epoch's key envelope is missing")]
    EnvelopeMissing,

    /// A sync control record, envelope or key was refused (#37). Carries
    /// no key material.
    #[error("Sync authority error: {0}")]
    Authority(#[from] matters_sync::authority::Error),
}

pub type Result<T> = std::result::Result<T, MattersError>;
