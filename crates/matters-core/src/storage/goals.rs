//! Goal references (#212, [ADR 0004]): a project, or the newest occurrence
//! of a repeating series, can name the goal in the Goals app it serves.
//! Matters stores only the goal's ID; the goal itself lives in the Goals
//! app. A series' goal passes to its next occurrence with the rule
//! (`super::repeats`), and earlier occurrences keep the goal they served.
//!
//! [ADR 0004]: https://github.com/thestephenromano/matters/blob/main/docs/decisions/0004-goal-references.md

use rusqlite::{params, OptionalExtension};

use super::Database;
use crate::crdt::Command;
use crate::error::{MattersError, Result};

/// Refuse a goal ID that is not a UUID in canonical text form.
pub(crate) fn check_goal(goal: Option<&str>) -> Result<()> {
    match goal {
        Some(goal) if !crate::crdt::is_goal_id(goal) => Err(MattersError::Validation(format!(
            "'{goal}' is not a goal ID"
        ))),
        _ => Ok(()),
    }
}

impl Database {
    /// Make project `id` serve `goal`, or no goal (`None`). Undo sets the
    /// previous value back.
    pub fn set_project_goal(&self, id: &str, goal: Option<&str>) -> Result<()> {
        check_goal(goal)?;
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let changed = tx.execute(
                    "UPDATE projects SET goal_id = ?1, updated_at = ?2 WHERE id = ?3",
                    params![goal, now, id],
                )?;
                if changed == 0 {
                    return Err(MattersError::NotFound {
                        entity: "project",
                        id: id.to_string(),
                    });
                }
                Ok(())
            },
            |_, ()| {
                Ok(vec![Command::SetGoal {
                    id: id.into(),
                    goal: goal.map(str::to_owned),
                }])
            },
        )
    }

    /// Make the repeating series whose newest occurrence is task `id` serve
    /// `goal`, or no goal (`None`). Only that occurrence, the one carrying
    /// the rule, can change the series' goal; earlier occurrences keep the
    /// goal they served, and a task that doesn't repeat serves its
    /// project's goal. Undo sets the previous value back.
    pub fn set_series_goal(&self, id: &str, goal: Option<&str>) -> Result<()> {
        check_goal(goal)?;
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let changed = tx.execute(
                    "UPDATE tasks SET goal_id = ?1, updated_at = ?2
                     WHERE id = ?3 AND repeat_rule IS NOT NULL",
                    params![goal, now, id],
                )?;
                if changed == 0 {
                    let exists = tx
                        .query_row("SELECT 1 FROM tasks WHERE id = ?1", params![id], |_| Ok(()))
                        .optional()?
                        .is_some();
                    return Err(if exists {
                        MattersError::Validation(
                            "only the newest occurrence of a repeating task serves a goal".into(),
                        )
                    } else {
                        MattersError::NotFound {
                            entity: "task",
                            id: id.to_string(),
                        }
                    });
                }
                Ok(())
            },
            |_, ()| {
                Ok(vec![Command::SetGoal {
                    id: id.into(),
                    goal: goal.map(str::to_owned),
                }])
            },
        )
    }
}
