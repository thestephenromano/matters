use serde::{Deserialize, Serialize};

use super::task::Task;
use crate::error::{MattersError, Result};

/// A task's home list: the Inbox, an area, or a project, optionally under
/// one of its headings (SPEC §2). This is also where the Move popover sends
/// a task (SPEC §4, [`crate::Database::move_task`]).
///
/// [`Task`] stores its home as three optional ids; [`Home::of`] is the one
/// reading of them, and refuses the combinations that name no home.
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
    /// for the database, which checks it on every write.
    pub fn of(task: &Task) -> Result<Self> {
        match (&task.area_id, &task.project_id, &task.heading_id) {
            (None, None, None) => Ok(Home::Inbox),
            (Some(area), None, None) => Ok(Home::Area(area.clone())),
            (None, Some(project), heading) => Ok(Home::Project {
                project: project.clone(),
                heading: heading.clone(),
            }),
            (Some(_), Some(_), _) => Err(MattersError::Validation(format!(
                "task '{}' can't be in both an area and a project",
                task.id
            ))),
            (_, None, Some(_)) => Err(MattersError::Validation(format!(
                "task '{}' has a heading but no project",
                task.id
            ))),
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
