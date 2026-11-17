use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::status::Status;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Project {
    pub id: String,
    pub area_id: Option<String>,
    pub title: String,
    pub notes: String,
    pub start_date: Option<NaiveDate>,
    pub time_of_day: Option<String>,
    pub is_someday: bool,
    pub deadline: Option<NaiveDate>,
    pub status: Status,
    pub completed_at: Option<i64>,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Project {
    pub fn new(id: impl Into<String>, title: impl Into<String>, sort_order: f64) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: id.into(),
            area_id: None,
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
}

/// Represents the completed vs. total task count for rendering Things 3's circular progress ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectProgress {
    pub completed_tasks: usize,
    pub total_tasks: usize,
}

impl ProjectProgress {
    pub fn ratio(&self) -> f64 {
        if self.total_tasks == 0 {
            0.0
        } else {
            (self.completed_tasks as f64) / (self.total_tasks as f64)
        }
    }
}
