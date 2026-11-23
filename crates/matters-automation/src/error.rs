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
    /// A change in a batch is not one Matters can make; the message names
    /// the change by its index, from 0. Nothing in the batch was changed.
    InvalidChange(String),
    /// The view a batch was built from has changed since: read it again.
    /// Nothing was changed.
    StaleRevision(String),
    /// The batch is too large to record at once; send it as smaller
    /// batches. Nothing was changed.
    BatchTooLarge(String),
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
            e @ matters_core::MattersError::BatchTooLarge { .. } => {
                Error::BatchTooLarge(e.to_string())
            }
            other => Error::Failed(other.to_string()),
        }
    }
}

impl Error {
    /// This error, as change `index` of a batch caused it: an invalid
    /// argument there is an invalid change.
    pub(crate) fn in_change(self, index: usize) -> Error {
        let at = |m: String| format!("change {index}: {m}");
        match self {
            Error::InvalidArgument(m) | Error::InvalidChange(m) => Error::InvalidChange(at(m)),
            Error::NotFound(m) => Error::NotFound(at(m)),
            Error::PermissionDenied(m) => Error::PermissionDenied(at(m)),
            other => other,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
