//! The interface's documented errors (docs/AUTOMATION.md).

use zbus::DBusError;

/// Every error a method returns, named
/// `io.github.thestephenromano.Matters.Automation1.Error.<Variant>`.
#[derive(Debug, DBusError)]
#[zbus(prefix = "io.github.thestephenromano.Matters.Automation1.Error")]
pub enum Error {
    /// No token was given, or it is not one of this device's clients'.
    UnknownToken(String),
    /// The client was revoked; it needs setting up again.
    RevokedToken(String),
    /// The client lacks the permission the method needs; the message names
    /// it.
    PermissionDenied(String),
    /// An argument is not one the method accepts (an unknown view name).
    InvalidArgument(String),
    /// No item has that ID.
    NotFound(String),
    /// Matters could not complete the call (its database failed).
    Failed(String),
    #[zbus(error)]
    ZBus(zbus::Error),
}

impl From<matters_core::MattersError> for Error {
    fn from(e: matters_core::MattersError) -> Self {
        match e {
            e @ matters_core::MattersError::NotFound { .. } => Error::NotFound(e.to_string()),
            matters_core::MattersError::Validation(what) => Error::InvalidArgument(what),
            other => Error::Failed(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
