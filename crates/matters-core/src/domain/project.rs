use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use super::status::Status;
use super::task::DeadlineStatus;
use super::when::Schedule;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Project {
    pub id: String,
    pub area_id: Option<String>,
    pub title: String,
    pub notes: String,
    pub start_date: Option<Date>,
    pub time_of_day: Option<String>,
    pub is_someday: bool,
    pub deadline: Option<Date>,
    pub status: Status,
    pub completed_at: Option<i64>,
    /// When the project was moved to the Trash, taking its tasks with it;
    /// `None` while it is not trashed. Only `Database::trash_project` and
    /// `Database::restore_project` change it.
    #[serde(default)]
    pub trashed_at: Option<i64>,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Project {
    pub fn is_trashed(&self) -> bool {
        self.trashed_at.is_some()
    }

    /// Set the project's When (#56), all three stored fields together, as
    /// for a task ([`crate::Task::apply_schedule`]). A project that is
    /// Someday or starts after today holds back its tasks
    /// ([`crate::QueryEngine`]).
    pub fn apply_schedule(&mut self, schedule: &Schedule, today: Date) {
        let (start_date, time_of_day, is_someday) = schedule.to_db_fields(today);
        self.start_date = start_date;
        self.time_of_day = time_of_day.map(str::to_string);
        self.is_someday = is_someday;
    }

    /// Where the project stands against its deadline on `today`.
    pub fn deadline_status(&self, today: Date) -> DeadlineStatus {
        DeadlineStatus::of(self.deadline, self.status, today)
    }

    pub fn new(id: impl Into<String>, title: impl Into<String>, sort_order: f64) -> Self {
        let now = crate::clock::now_millis();
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
            trashed_at: None,
            sort_order,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Completed vs. total task count, for rendering the project's circular progress ring.
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
