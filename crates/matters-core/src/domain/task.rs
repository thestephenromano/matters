use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use super::status::Status;
use super::when::Schedule;

/// Where a task stands against its deadline on a given day (#20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineStatus {
    /// No deadline.
    None,
    /// A deadline still to come, or a finished task's deadline.
    Set,
    /// An open task due today.
    DueToday,
    /// An open task past its deadline.
    Overdue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub area_id: Option<String>,
    pub project_id: Option<String>,
    pub heading_id: Option<String>,
    pub title: String,
    pub notes: String,
    pub start_date: Option<Date>,
    pub time_of_day: Option<String>, // Some("evening") or None
    pub is_someday: bool,
    pub deadline: Option<Date>,
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
        let now = crate::clock::now_millis();
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

    /// A title as stored: line breaks, tabs and runs of spaces become single
    /// spaces, and the ends are trimmed. `None` when nothing is left: a task
    /// always has a visible title.
    pub fn clean_title(raw: &str) -> Option<String> {
        let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        (!title.is_empty()).then_some(title)
    }

    /// Where this task stands against its deadline on `today`. Only open
    /// tasks are due or overdue: a finished task's deadline is just shown.
    pub fn deadline_status(&self, today: Date) -> DeadlineStatus {
        match self.deadline {
            None => DeadlineStatus::None,
            Some(_) if self.status != Status::Open => DeadlineStatus::Set,
            Some(d) if d < today => DeadlineStatus::Overdue,
            Some(d) if d == today => DeadlineStatus::DueToday,
            Some(_) => DeadlineStatus::Set,
        }
    }

    pub fn is_trashed(&self) -> bool {
        self.trashed_at.is_some()
    }

    /// Set the task's "When". All three stored fields (start date, time of day,
    /// someday flag) are written together so a task can never be, for example,
    /// both Someday and scheduled for Today.
    pub fn apply_schedule(&mut self, schedule: &Schedule, today: Date) {
        let (start_date, time_of_day, is_someday) = schedule.to_db_fields(today);
        self.start_date = start_date;
        self.time_of_day = time_of_day.map(str::to_string);
        self.is_someday = is_someday;
    }

    pub fn is_evening(&self) -> bool {
        self.time_of_day.as_deref() == Some("evening")
    }
}
