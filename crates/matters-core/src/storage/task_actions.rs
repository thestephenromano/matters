//! Making new things from a task (#65): a copy of it, or a project made of
//! it. Each is one transaction: it happens whole or not at all.
//! (Cancelling a task is `Database::cancel_task`; `uncomplete_task` reopens
//! it.)

use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;

use super::db::TASK_COLUMNS;
use super::trash::LIVE_TASK;
use super::Database;
use crate::domain::{Status, Task};
use crate::error::{MattersError, Result};

impl Database {
    /// Copy task `id` and return the copy's id. The copy is open and listed
    /// just below the original, in its home and in Today; it has the same
    /// title, notes, home, When, deadline, reminder and tags, and the same checklist,
    /// with every item unticked. The task must be live (not in the Trash).
    pub fn duplicate_task(&self, id: &str) -> Result<String> {
        let copy = Uuid::new_v4().to_string();
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let task = live_task(tx, id)?;
                let sort_order = key_after(tx, "sort_order", task.sort_order)?;
                let today_order = key_after(tx, "today_order", task.today_order)?;
                tx.execute(
                    "INSERT INTO tasks (
                    id, area_id, project_id, heading_id, title, notes, start_date,
                    time_of_day, is_someday, deadline, status, completed_at, sort_order,
                    created_at, updated_at, trashed_at, today_order, closed_by_project,
                    reminder_time
                 )
                 SELECT ?2, area_id, project_id, heading_id, title, notes, start_date,
                    time_of_day, is_someday, deadline, 'open', NULL, ?3,
                    ?5, ?5, NULL, ?4, 0, reminder_time
                 FROM tasks WHERE id = ?1",
                    params![id, copy, sort_order, today_order, now],
                )?;
                for (title, _, sort_order) in checklist(tx, id)? {
                    tx.execute(
                        "INSERT INTO checklist_items
                        (id, task_id, title, is_completed, sort_order, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 0, ?4, ?5, ?5)",
                        params![Uuid::new_v4().to_string(), copy, title, sort_order, now],
                    )?;
                }
                tx.execute(
                    "INSERT INTO task_tags (task_id, tag_id)
                 SELECT ?2, tag_id FROM task_tags WHERE task_id = ?1",
                    params![id, copy],
                )?;
                Ok(copy.clone())
            },
            |tx, copy| super::sync::created_task_with_children(tx, copy),
        )
    }

    /// Turn open task `id` into a project and return the project's id. The
    /// project has the task's title, notes, When, deadline and tags, and is
    /// filed in the task's area, or in the area of the project the task was
    /// in; it goes last in the sidebar. Each checklist item becomes one of
    /// its tasks, in order, done if the item was ticked. The task itself is
    /// gone. A finished task, or one in the Trash, can't be converted.
    pub fn convert_task_to_project(&self, id: &str) -> Result<String> {
        let project = Uuid::new_v4().to_string();
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let task = live_task(tx, id)?;
                if task.status != Status::Open {
                    return Err(MattersError::Validation(
                        "only an open task can become a project".into(),
                    ));
                }
                let area: Option<String> = match &task.project_id {
                    Some(project) => tx.query_row(
                        "SELECT area_id FROM projects WHERE id = ?1",
                        params![project],
                        |r| r.get(0),
                    )?,
                    None => task.area_id.clone(),
                };
                let sort_order: f64 = tx.query_row(
                    "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM projects",
                    [],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO projects (
                    id, area_id, title, notes, start_date, time_of_day, is_someday,
                    deadline, status, completed_at, sort_order, created_at, updated_at,
                    trashed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', NULL, ?9, ?10, ?10, NULL)",
                    params![
                        project,
                        area,
                        task.title,
                        task.notes,
                        task.start_date.map(|d| d.to_string()),
                        task.time_of_day,
                        task.is_someday,
                        task.deadline.map(|d| d.to_string()),
                        sort_order,
                        now,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO project_tags (project_id, tag_id)
                 SELECT ?2, tag_id FROM task_tags WHERE task_id = ?1",
                    params![id, project],
                )?;
                let (home, today): (f64, f64) = tx.query_row(
                    "SELECT COALESCE(MAX(sort_order), 0) + 1, COALESCE(MAX(today_order), 0) + 1
                 FROM tasks",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let mut new_tasks = Vec::new();
                let old_items = super::sync::task_ids(
                    tx,
                    "SELECT id FROM checklist_items WHERE task_id = ?1",
                    id,
                )?;
                for (i, (title, done, _)) in checklist(tx, id)?.into_iter().enumerate() {
                    let (status, completed_at) = if done {
                        (Status::Completed, Some(now))
                    } else {
                        (Status::Open, None)
                    };
                    let new_task = Uuid::new_v4().to_string();
                    new_tasks.push(new_task.clone());
                    tx.execute(
                        "INSERT INTO tasks (
                        id, project_id, title, notes, status, completed_at, sort_order,
                        today_order, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?8)",
                        params![
                            new_task,
                            project,
                            title,
                            status.as_str(),
                            completed_at,
                            home + i as f64,
                            today + i as f64,
                            now,
                        ],
                    )?;
                }
                tx.execute(
                    "DELETE FROM checklist_items WHERE task_id = ?1",
                    params![id],
                )?;
                tx.execute("DELETE FROM task_tags WHERE task_id = ?1", params![id])?;
                tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
                let mut gone = old_items;
                gone.push(id.to_string());
                Ok((project.clone(), new_tasks, gone))
            },
            |tx, (project, new_tasks, gone)| super::sync::converted(tx, project, new_tasks, gone),
        )
        .map(|(project, _, _)| project)
    }
}

/// Task `id`, if it is live; `NotFound` otherwise.
pub(super) fn live_task(tx: &Transaction, id: &str) -> Result<Task> {
    tx.query_row(
        &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1 AND {LIVE_TASK}"),
        params![id],
        Database::map_task_row,
    )
    .optional()?
    .ok_or_else(|| MattersError::NotFound {
        entity: "task",
        id: id.to_string(),
    })
}

/// A key in task order `column` just after `key`: halfway to the next one,
/// or one past it when it is the last.
pub(super) fn key_after(tx: &Transaction, column: &str, key: f64) -> Result<f64> {
    let next: Option<f64> = tx.query_row(
        &format!("SELECT MIN({column}) FROM tasks WHERE {column} > ?1"),
        params![key],
        |r| r.get(0),
    )?;
    Ok(next.map_or(key + 1.0, |next| (key + next) / 2.0))
}

/// Task `id`'s checklist, in order: each item's title, whether it is
/// ticked, and its order key.
pub(super) fn checklist(tx: &Transaction, id: &str) -> Result<Vec<(String, bool, f64)>> {
    let mut stmt = tx.prepare(
        "SELECT title, is_completed, sort_order FROM checklist_items
         WHERE task_id = ?1 ORDER BY sort_order ASC, created_at ASC, id ASC",
    )?;
    let items = stmt
        .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(items)
}
