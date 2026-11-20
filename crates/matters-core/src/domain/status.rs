use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    Completed,
    Canceled,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Completed => "completed",
            Status::Canceled => "canceled",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "completed" => Status::Completed,
            "canceled" | "cancelled" => Status::Canceled,
            _ => Status::Open,
        }
    }
}

/// How something finished: the closed [`Status`]es. Finishing a project
/// takes one for the project and one for its open tasks
/// ([`crate::Database::finish_project`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Completed,
    Canceled,
}

impl From<Outcome> for Status {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Completed => Status::Completed,
            Outcome::Canceled => Status::Canceled,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn statuses_read_back_as_written_in_either_spelling() {
        for status in [Status::Open, Status::Completed, Status::Canceled] {
            assert_eq!(Status::from_str_loose(&status.to_string()), status);
        }
        assert_eq!(Status::Canceled.to_string(), "canceled");
        assert_eq!(Status::from_str_loose("Cancelled"), Status::Canceled);
        assert_eq!(Status::from_str_loose("COMPLETED"), Status::Completed);
        assert_eq!(Status::from_str_loose("someday"), Status::Open);
    }
}
