use jiff::civil::Date;

use super::home::Home;
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
    Trash,
    Project(String),
    /// The tasks filed directly in an area (not in its projects).
    Area(String),
}

impl View {
    /// The fixed sidebar views, in display order.
    pub fn sidebar() -> [View; 7] {
        [
            View::Inbox,
            View::Today,
            View::Upcoming,
            View::Anytime,
            View::Someday,
            View::Logbook,
            View::Trash,
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
            View::Trash => "trash",
            View::Project(_) => "project",
            View::Area(_) => "area",
        }
    }

    /// Inverse of [`View::key`]. A project or area view needs the non-empty
    /// id of its project or area in `container_id`.
    pub fn from_key(key: &str, container_id: Option<&str>) -> Option<View> {
        Some(match key {
            "inbox" => View::Inbox,
            "today" => View::Today,
            "upcoming" => View::Upcoming,
            "anytime" => View::Anytime,
            "someday" => View::Someday,
            "logbook" => View::Logbook,
            "trash" => View::Trash,
            "project" => match container_id {
                Some(id) if !id.is_empty() => View::Project(id.to_string()),
                _ => return None,
            },
            "area" => match container_id {
                Some(id) if !id.is_empty() => View::Area(id.to_string()),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// Display title. Project and area titles live in the database, so this
    /// returns a generic label for [`View::Project`] and [`View::Area`].
    pub fn title(&self) -> &'static str {
        match self {
            View::Inbox => "Inbox",
            View::Today => "Today",
            View::Upcoming => "Upcoming",
            View::Anytime => "Anytime",
            View::Someday => "Someday",
            View::Logbook => "Logbook",
            View::Trash => "Trash",
            View::Project(_) => "Project",
            View::Area(_) => "Area",
        }
    }

    /// Whether the user arranges this list by hand (`crate::ordering`).
    /// Upcoming is ordered by date, the Logbook and Trash by when items were
    /// finished or trashed. Anytime and Someday mix tasks from several areas
    /// and projects, so they wait for grouped sections (#25).
    pub fn is_reorderable(&self) -> bool {
        matches!(
            self,
            View::Inbox | View::Today | View::Project(_) | View::Area(_)
        )
    }

    pub fn project_id(&self) -> Option<&str> {
        match self {
            View::Project(id) => Some(id),
            _ => None,
        }
    }

    pub fn area_id(&self) -> Option<&str> {
        match self {
            View::Area(id) => Some(id),
            _ => None,
        }
    }

    /// The id of the project or area this view shows, if any.
    pub fn container_id(&self) -> Option<&str> {
        self.project_id().or(self.area_id())
    }

    /// The view to open to show `task`, as a search result for example.
    ///
    /// Trashed tasks are only in the Trash. Otherwise a project's tasks live
    /// in the project; finished tasks in the Logbook;
    /// otherwise the task's schedule decides (Someday, Upcoming, Today), then
    /// whether it is filed in an area (that area) or not (Inbox). An undated
    /// task with a due deadline is shown in Today even when it is also listed
    /// in the Inbox.
    pub fn containing(task: &Task, today: Date) -> View {
        if task.is_trashed() {
            return View::Trash;
        }
        let home = Home::listed(task);
        if let Home::Project { project, .. } = home {
            return View::Project(project);
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
            None => match home {
                Home::Area(area) => View::Area(area),
                _ => View::Inbox,
            },
        }
    }

    /// Whether a task can be captured while looking at this view. The
    /// Logbook lists only finished tasks and the Trash only trashed ones, so
    /// neither holds new work; frontends hide their quick-add field there.
    pub fn accepts_new_tasks(&self) -> bool {
        !matches!(self, View::Logbook | View::Trash)
    }

    /// Build a new open task as if it was captured while looking at this view
    /// (SPEC §3, "Capturing into a view"), or `None` where tasks can't be
    /// captured ([`View::accepts_new_tasks`]) or the title is blank
    /// ([`Task::clean_title`]).
    ///
    /// The task lands in the view it was typed into wherever the model allows:
    /// Today and Someday schedule it accordingly, Upcoming schedules it for
    /// tomorrow, a project or area view files it there. Anytime only lists
    /// tasks that belong to an area or project, so captures there go to the
    /// Inbox (SPEC §3.1: a task created without an explicit project or date
    /// defaults to the Inbox).
    pub fn new_task(&self, id: impl Into<String>, title: &str, today: Date) -> Option<Task> {
        let title = Task::clean_title(title)?;
        let schedule = match self {
            View::Logbook | View::Trash => return None,
            View::Today => Schedule::Today { evening: false },
            View::Upcoming => Schedule::Date(today.tomorrow().unwrap_or(today)),
            View::Someday => Schedule::Someday,
            View::Inbox | View::Anytime | View::Project(_) | View::Area(_) => Schedule::None,
        };
        let mut task = Task::new(id, title, 0.0);
        task.apply_schedule(&schedule, today);
        task.project_id = self.project_id().map(str::to_string);
        task.area_id = self.area_id().map(str::to_string);
        Some(task)
    }
}
