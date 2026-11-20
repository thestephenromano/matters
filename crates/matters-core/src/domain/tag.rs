use serde::{Deserialize, Serialize};

/// A tag (#57): a name, unique ignoring case, optionally nested under
/// another tag. Tags are assigned to tasks, projects and areas; a task
/// matches the tags of its project and area too, and matching a tag
/// matches its descendants (`crate::QueryEngine`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub name: String,
    /// The tag this one is nested under, if any.
    pub parent_id: Option<String>,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Tag {
    pub fn new(id: impl Into<String>, name: impl Into<String>, parent_id: Option<String>) -> Self {
        let now = crate::clock::now_millis();
        Self {
            id: id.into(),
            name: name.into(),
            parent_id,
            sort_order: 0.0,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Something a tag can be assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tagged<'a> {
    Task(&'a str),
    Project(&'a str),
    Area(&'a str),
}

impl Tagged<'_> {
    /// The assignment table and its item column.
    pub(crate) fn table(&self) -> (&'static str, &'static str) {
        match self {
            Tagged::Task(_) => ("task_tags", "task_id"),
            Tagged::Project(_) => ("project_tags", "project_id"),
            Tagged::Area(_) => ("area_tags", "area_id"),
        }
    }

    /// The item's table, entity name and id.
    pub(crate) fn item(&self) -> (&'static str, &'static str, &str) {
        match self {
            Tagged::Task(id) => ("tasks", "task", id),
            Tagged::Project(id) => ("projects", "project", id),
            Tagged::Area(id) => ("areas", "area", id),
        }
    }
}
