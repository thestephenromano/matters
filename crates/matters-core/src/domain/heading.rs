use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heading {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Heading {
    pub fn new(
        id: impl Into<String>,
        project_id: impl Into<String>,
        title: impl Into<String>,
        sort_order: f64,
    ) -> Self {
        let now = crate::clock::now_millis();
        Self {
            id: id.into(),
            project_id: project_id.into(),
            title: title.into(),
            sort_order,
            created_at: now,
            updated_at: now,
        }
    }
}

/// A deleted heading and the tasks that were under it, so the deletion can
/// be undone ([`crate::Database::restore_heading`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedHeading {
    pub heading: Heading,
    pub task_ids: Vec<String>,
}
