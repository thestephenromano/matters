use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeOfDay {
    Anytime,
    Evening,
}

impl TimeOfDay {
    pub fn as_db_str(&self) -> Option<&'static str> {
        match self {
            TimeOfDay::Anytime => None,
            TimeOfDay::Evening => Some("evening"),
        }
    }

    pub fn from_db_str(s: Option<&str>) -> Self {
        match s {
            Some("evening") => TimeOfDay::Evening,
            _ => TimeOfDay::Anytime,
        }
    }
}

/// The "When" representation in Things 3 semantics:
/// A task can be scheduled for Today (daytime or evening), on a specific calendar date,
/// marked for Someday, or unscheduled (Inbox / Anytime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Schedule {
    Today { evening: bool },
    Date(NaiveDate),
    Someday,
    None,
}

impl Schedule {
    pub fn is_someday(&self) -> bool {
        matches!(self, Schedule::Someday)
    }

    pub fn to_db_fields(&self, today: NaiveDate) -> (Option<NaiveDate>, Option<&'static str>, bool) {
        match self {
            Schedule::Today { evening } => (
                Some(today),
                if *evening { Some("evening") } else { None },
                false,
            ),
            Schedule::Date(d) => (Some(*d), None, false),
            Schedule::Someday => (None, None, true),
            Schedule::None => (None, None, false),
        }
    }
}
