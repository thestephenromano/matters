use serde::{Deserialize, Serialize};

use super::task::Task;
use crate::error::{MattersError, Result};

/// A task's home list: the Inbox, an area, or a project, optionally under
/// one of its headings (SPEC §2). This is also where the Move popover sends
/// a task (SPEC §4, [`crate::Database::set_task_home`]).
///
/// [`Task`] stores its home as three optional ids. [`Home::listed`] is the
/// one reading of them, and [`Home::of`] the same reading that refuses the
/// combinations that name no home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Home {
    Inbox,
    Area(String),
    Project {
        project: String,
        heading: Option<String>,
    },
}

impl Home {
    /// The home `task`'s ids describe. A task is in at most one of an area
    /// or a project, and a heading only in a project; anything else is
    /// refused. Whether the heading belongs to that project is a question
    /// for the database, which checks it when a task is stored or moved.
    pub fn of(task: &Task) -> Result<Self> {
        if task.area_id.is_some() && task.project_id.is_some() {
            return Err(MattersError::Validation(format!(
                "task '{}' can't be in both an area and a project",
                task.id
            )));
        }
        if task.heading_id.is_some() && task.project_id.is_none() {
            return Err(MattersError::Validation(format!(
                "task '{}' has a heading but no project",
                task.id
            )));
        }
        Ok(Self::listed(task))
    }

    /// The home the lists show `task` in: its project when it names one,
    /// otherwise its area, otherwise the Inbox. For a valid task this is
    /// [`Home::of`]. A row stored before that rule held, with both an area
    /// and a project say, is listed in its project, and this says so.
    pub fn listed(task: &Task) -> Self {
        match (&task.project_id, &task.area_id) {
            (Some(project), _) => Home::Project {
                project: project.clone(),
                heading: task.heading_id.clone(),
            },
            (None, Some(area)) => Home::Area(area.clone()),
            (None, None) => Home::Inbox,
        }
    }

    /// Point `task`'s ids at this home.
    pub fn apply(&self, task: &mut Task) {
        let (area, project, heading) = match self {
            Home::Inbox => (None, None, None),
            Home::Area(area) => (Some(area), None, None),
            Home::Project { project, heading } => (None, Some(project), heading.as_ref()),
        };
        task.area_id = area.cloned();
        task.project_id = project.cloned();
        task.heading_id = heading.cloned();
    }
}
