//! Which rows are in the Trash (SPEC §3.7), written once for every query.
//!
//! An item is in the Trash when it was trashed itself or when a container
//! holding it was: a trashed area takes its projects and tasks with it, and a
//! trashed project takes its tasks. The Trash lists only the outermost trashed
//! item, which stands for everything inside it, so Put Back restores the
//! contents as they were. An item trashed on its own before its container
//! stays in the Trash when the container is put back.
//!
//! Each constant is an SQL condition on the table it names, for use in a
//! `WHERE` clause.

macro_rules! trashed_areas {
    () => {
        "SELECT id FROM areas WHERE trashed_at IS NOT NULL"
    };
}

/// Projects that are trashed themselves or sit in a trashed area.
macro_rules! hidden_projects {
    () => {
        concat!(
            "SELECT id FROM projects WHERE trashed_at IS NOT NULL OR area_id IN (",
            trashed_areas!(),
            ")"
        )
    };
}

/// The row's area, if any, is not trashed.
macro_rules! area_present {
    () => {
        concat!(
            "(area_id IS NULL OR area_id NOT IN (",
            trashed_areas!(),
            "))"
        )
    };
}

/// A task's project and area, if any, are both present.
macro_rules! task_containers_present {
    () => {
        concat!(
            "(project_id IS NULL OR project_id NOT IN (",
            hidden_projects!(),
            ")) AND ",
            area_present!()
        )
    };
}

macro_rules! live_task {
    () => {
        concat!("trashed_at IS NULL AND ", task_containers_present!())
    };
}

/// Tasks shown in the app: not trashed, and not inside a trashed container.
pub(crate) const LIVE_TASK: &str = live_task!();

/// Tasks the Trash lists one by one: trashed themselves, in containers that
/// are not trashed.
pub(crate) const LISTED_TRASHED_TASK: &str =
    concat!("trashed_at IS NOT NULL AND ", task_containers_present!());

/// Every task in the Trash, directly or through a container: any task that
/// is not live.
pub(crate) const ANY_TRASHED_TASK: &str = concat!("NOT (", live_task!(), ")");

/// Projects shown in the app.
pub(crate) const LIVE_PROJECT: &str = concat!("trashed_at IS NULL AND ", area_present!());

/// Projects the Trash lists: trashed themselves, in an area that is not.
pub(crate) const LISTED_TRASHED_PROJECT: &str =
    concat!("trashed_at IS NOT NULL AND ", area_present!());

/// Every project in the Trash, directly or through its area.
pub(crate) const ANY_TRASHED_PROJECT: &str = concat!("id IN (", hidden_projects!(), ")");

/// Areas shown in the app.
pub(crate) const LIVE_AREA: &str = "trashed_at IS NULL";

/// Trashed areas, all of which the Trash lists.
pub(crate) const TRASHED_AREA: &str = "trashed_at IS NOT NULL";
