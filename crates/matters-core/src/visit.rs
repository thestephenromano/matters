//! What a task list shows during one visit to a view.
//!
//! Ticking a task done (or not done) normally changes which views it belongs
//! to, so a fresh query would drop it from the list at once. Instead, a task
//! whose completion status changes while the view is open stays in place,
//! with its new status, until the user leaves the view (SPEC §3, "Completing
//! a task"). Any other change that removes a task from the view, such as
//! rescheduling, trashing or putting it back, takes effect immediately.

use std::collections::HashSet;

use crate::domain::{Task, View};
use crate::error::Result;

#[derive(Debug, Default)]
pub struct Visit {
    view: Option<View>,
    /// Tasks kept in the list because their completion status changed during
    /// this visit.
    kept: HashSet<String>,
}

impl Visit {
    pub fn new() -> Self {
        Self::default()
    }

    /// The user navigated (to another view, or to the same one again): the
    /// next `rows` call starts a fresh visit.
    pub fn restart(&mut self) {
        self.view = None;
        self.kept.clear();
    }

    /// The rows to display for `view`.
    ///
    /// * `members` are the view's members now (`QueryEngine::tasks_in`).
    /// * `shown` is what the list currently displays.
    /// * `lookup` reads a task's current state by id.
    ///
    /// Showing a different view from last time starts a new visit, and the
    /// members are shown as they are.
    pub fn rows(
        &mut self,
        view: &View,
        members: Vec<Task>,
        shown: &[Task],
        lookup: impl Fn(&str) -> Result<Option<Task>>,
    ) -> Result<Vec<Task>> {
        if self.view.as_ref() != Some(view) {
            self.view = Some(view.clone());
            self.kept.clear();
            return Ok(members);
        }

        let mut rows = members;
        // Walk the displayed rows in order, so a kept task is re-inserted just
        // after the row that preceded it, keeping its place in the list.
        let mut anchor: Option<String> = None;
        for old in shown {
            if rows.iter().any(|t| t.id == old.id) {
                anchor = Some(old.id.clone());
                continue;
            }
            let Some(current) = lookup(&old.id)? else {
                continue; // deleted
            };
            if current.is_trashed() != old.is_trashed() {
                continue; // trashed or put back: always takes effect
            }
            if !(self.kept.contains(&old.id) || current.status != old.status) {
                continue; // left the view for another reason
            }
            self.kept.insert(old.id.clone());
            let at = anchor
                .as_ref()
                .and_then(|id| rows.iter().position(|t| &t.id == id))
                .map_or(0, |i| i + 1);
            anchor = Some(current.id.clone());
            rows.insert(at, current);
        }
        Ok(rows)
    }
}
