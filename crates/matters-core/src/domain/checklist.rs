use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub is_completed: bool,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ChecklistItem {
    pub fn new(
        id: impl Into<String>,
        task_id: impl Into<String>,
        title: impl Into<String>,
        sort_order: f64,
    ) -> Self {
        let now = crate::clock::now_millis();
        Self {
            id: id.into(),
            task_id: task_id.into(),
            title: title.into(),
            is_completed: false,
            sort_order,
            created_at: now,
            updated_at: now,
        }
    }
}

/// How much of a task's checklist is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecklistProgress {
    pub done: usize,
    pub total: usize,
}
