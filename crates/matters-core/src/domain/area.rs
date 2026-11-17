use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub id: String,
    pub title: String,
    pub sort_order: f64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Area {
    pub fn new(id: impl Into<String>, title: impl Into<String>, sort_order: f64) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: id.into(),
            title: title.into(),
            sort_order,
            created_at: now,
            updated_at: now,
        }
    }
}
