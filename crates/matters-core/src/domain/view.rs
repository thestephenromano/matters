use chrono::{Days, NaiveDate};

use super::status::Status;
use super::task::Task;
use super::when::Schedule;

/// A list the user can navigate to. This is the single definition of the GTD
/// views shared by every frontend: their keys, titles, and where a task created
/// while looking at the view is placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Inbox,
    Today,
    Upcoming,
    Anytime,
    Someday,
    Logbook,
    Project(String),
}

impl View {
    /// The fixed sidebar views, in display order.
    pub fn sidebar() -> [View; 6] {
        [
            View::Inbox,
            View::Today,
            View::Upcoming,
            View::Anytime,
            View::Someday,
            View::Logbook,
        ]
    }

    /// Stable identifier used across the UI boundary.
    pub fn key(&self) -> &'static str {
        match self {
            View::Inbox => "inbox",
            View::Today => "today",
            View::Upcoming => "upcoming",
            View::Anytime => "anytime",
            View::Someday => "someday",
            View::Logbook => "logbook",
            View::Project(_) => "project",
        }
    }

    /// Inverse of [`View::key`]. A project view needs a non-empty project id.
    pub fn from_key(key: &str, project_id: Option<&str>) -> Option<View> {
        Some(match key {
            "inbox" => View::Inbox,
            "today" => View::Today,
            "upcoming" => View::Upcoming,
            "anytime" => View::Anytime,
            "someday" => View::Someday,
            "logbook" => View::Logbook,
            "project" => match project_id {
                Some(id) if !id.is_empty() => View::Project(id.to_string()),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// Display title. A project's title lives in the database, so this returns
    /// a generic label for [`View::Project`].
    pub fn title(&self) -> &'static str {
        match self {
            View::Inbox => "Inbox",
            View::Today => "Today",
            View::Upcoming => "Upcoming",
            View::Anytime => "Anytime",
            View::Someday => "Someday",
            View::Logbook => "Logbook",
            View::Project(_) => "Project",
        }
    }

    pub fn project_id(&self) -> Option<&str> {
        match self {
            View::Project(id) => Some(id),
            _ => None,
        }
    }

    /// The view to open to show `task`, as a search result for example.
    ///
    /// A project's tasks live in the project; finished tasks in the Logbook;
    /// otherwise the task's schedule decides (Someday, Upcoming, Today), then
    /// whether it is filed under an area (Anytime) or not (Inbox). An undated
    /// task with a due deadline is shown in Today even when it is also listed
    /// in the Inbox.
    pub fn containing(task: &Task, today: NaiveDate) -> View {
        if let Some(project_id) = &task.project_id {
            return View::Project(project_id.clone());
        }
        if matches!(task.status, Status::Completed | Status::Canceled) {
            return View::Logbook;
        }
        if task.is_someday {
            return View::Someday;
        }
        match task.start_date {
            Some(date) if date > today => View::Upcoming,
            Some(_) => View::Today,
            None if task.deadline.is_some_and(|d| d <= today) => View::Today,
            None if task.area_id.is_some() => View::Anytime,
            None => View::Inbox,
        }
    }

    /// Build a new open task as if it was captured while looking at this view
    /// (SPEC §3, "Capturing into a view").
    ///
    /// The task lands in the view it was typed into wherever the model allows:
    /// Today and Someday schedule it accordingly, Upcoming schedules it for
    /// tomorrow, a project view files it in that project. Anytime only lists
    /// tasks that belong to an area or project, and the Logbook only lists
    /// finished tasks, so captures there go to the Inbox (SPEC §3.1: a task
    /// created without an explicit project or date defaults to the Inbox).
    pub fn new_task(&self, id: impl Into<String>, title: impl Into<String>, today: NaiveDate) -> Task {
        let mut task = Task::new(id, title, 0.0);
        let schedule = match self {
            View::Today => Schedule::Today { evening: false },
            View::Upcoming => Schedule::Date(today.checked_add_days(Days::new(1)).unwrap_or(today)),
            View::Someday => Schedule::Someday,
            View::Inbox | View::Anytime | View::Logbook | View::Project(_) => Schedule::None,
        };
        task.apply_schedule(&schedule, today);
        task.project_id = self.project_id().map(str::to_string);
        task
    }
}
