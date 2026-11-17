use thiserror::Error;

#[derive(Error, Debug)]
pub enum MattersError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Entity not found: {entity} with id '{id}'")]
    NotFound { entity: &'static str, id: String },

    #[error("Invalid data: {0}")]
    Validation(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, MattersError>;
