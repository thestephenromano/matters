use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::status::Status;
use super::when::Schedule;

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
    /// When the task was moved to the Trash; `None` while it is not trashed.
    /// Trash is kept apart from `status` so a trashed task keeps its
    /// completion state and Put Back returns it exactly where it was. Only
    /// `Database::trash_task` and `Database::restore_task` change it.
    #[serde(default)]
    pub trashed_at: Option<i64>,
    /// Position in the task's home list (Inbox, area or project).
    pub sort_order: f64,
    /// Position in Today, which keeps its own order (`crate::ordering`).
    #[serde(default)]
    pub today_order: f64,
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
            trashed_at: None,
            sort_order,
            today_order: 0.0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_trashed(&self) -> bool {
        self.trashed_at.is_some()
    }

    pub fn is_open(&self) -> bool {
        self.status == Status::Open
    }

    /// Set the task's "When". All three stored fields (start date, time of day,
    /// someday flag) are written together so a task can never be, for example,
    /// both Someday and scheduled for Today.
    pub fn apply_schedule(&mut self, schedule: &Schedule, today: NaiveDate) {
        let (start_date, time_of_day, is_someday) = schedule.to_db_fields(today);
        self.start_date = start_date;
        self.time_of_day = time_of_day.map(str::to_string);
        self.is_someday = is_someday;
    }

    pub fn is_evening(&self) -> bool {
        self.time_of_day.as_deref() == Some("evening")
    }
}
