use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::status::Status;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub area_id: Option<String>,
    pub project_id: Option<String>,
    pub heading_id: Option<String>,
    pub title: String,
    pub notes: String,
    pub start_date: Option<NaiveDate>,
    pub time_of_day: Option<String>, // Some("evening") or None
    pub is_someday: bool,
    pub deadline: Option<NaiveDate>,
    pub status: Status,
    pub completed_at: Option<i64>,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Task {
    pub fn new(id: impl Into<String>, title: impl Into<String>, sort_order: f64) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: id.into(),
            area_id: None,
            project_id: None,
            heading_id: None,
            title: title.into(),
            notes: String::new(),
            start_date: None,
            time_of_day: None,
            is_someday: false,
            deadline: None,
            status: Status::Open,
            completed_at: None,
            sort_order,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_open(&self) -> bool {
        self.status == Status::Open
    }

    pub fn is_evening(&self) -> bool {
        self.time_of_day.as_deref() == Some("evening")
    }
}
