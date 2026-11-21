//! Repeating tasks as rows (#61; decided there): the newest occurrence of a
//! series carries its rule, and passes it on to the next occurrence when
//! that is created.
//!
//! * On a fixed schedule, the next occurrence is created when its date
//!   comes ([`Database::advance_repeats`]), whether or not the current one
//!   is done. After days away, only the latest date due is created, not one
//!   per missed date.
//! * After completion, completing the newest occurrence creates the next
//!   one in the same transaction ([`Database::complete_task_on`]).
//!
//! A new occurrence is a copy of the one before: title, notes, home, This
//! Evening, reminder, tags, and the checklist unticked, with its deadline
//! as far from its date as before. Its id is derived from its series and
//! date (`repeat::occurrence_id`), and it is inserted only if missing, so a
//! crash, a second run or a second device never makes two.
//!
//! Every occurrence is an ordinary task: the views, counts, Trash and
//! Logbook treat it like any other. A series pauses while its newest
//! occurrence is in the Trash or its project is finished, and ends when
//! that occurrence is deleted or its rule cleared.

use jiff::civil::Date;
use jiff::ToSpan;
use rusqlite::{params, OptionalExtension, Transaction};

use super::db::{Database, TASK_COLUMNS};
use super::task_actions::{checklist, key_after, live_task};
use super::trash::LIVE_TASK;
use crate::crdt::Command;
use crate::domain::Task;
use crate::error::{MattersError, Result};
use crate::repeat::{occurrence_id, stored_rule, RepeatRule};

impl Database {
    /// Make task `id` repeat by `rule`, or stop it repeating (`None`). The
    /// task becomes the newest occurrence of its series (a new series, named
    /// by its id, if it had none); any other task of the series carrying a
    /// rule gives it up. The rule starts on the task's date; a task with no
    /// date, or deferred to Someday, is scheduled for `today`.
    pub fn set_task_repeat(&self, id: &str, rule: Option<RepeatRule>, today: Date) -> Result<()> {
        if let Some(rule) = &rule {
            rule.check().map_err(MattersError::Validation)?;
        }
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let task = live_task(tx, id)?;
                let Some(mut rule) = rule else {
                    tx.execute(
                        "UPDATE tasks SET repeat_rule = NULL, updated_at = ?2 WHERE id = ?1",
                        params![id, now],
                    )?;
                    return Ok((false, Vec::new()));
                };
                let dated = task.start_date.filter(|_| !task.is_someday);
                let date = dated.unwrap_or(today);
                rule.start = date;
                let series = task.series.clone().unwrap_or_else(|| id.to_string());
                let others = ids(
                    tx,
                    "SELECT id FROM tasks
                     WHERE repeat_series = ?1 AND id != ?2 AND repeat_rule IS NOT NULL",
                    params![series, id],
                )?;
                tx.execute(
                    "UPDATE tasks SET repeat_rule = NULL WHERE repeat_series = ?1 AND id != ?2",
                    params![series, id],
                )?;
                tx.execute(
                    "UPDATE tasks SET repeat_rule = ?2, repeat_series = ?3,
                         repeat_date = COALESCE(repeat_date, ?4),
                         start_date = ?4, is_someday = 0,
                         time_of_day = CASE WHEN ?5 THEN time_of_day END, updated_at = ?6
                     WHERE id = ?1",
                    params![
                        id,
                        stored_rule(Some(&rule)),
                        series,
                        date.to_string(),
                        dated.is_some(),
                        now
                    ],
                )?;
                Ok((dated.is_none(), others))
            },
            |tx, (rescheduled, others)| {
                let task = live_task(tx, id)?;
                let mut commands = Vec::new();
                for other in others {
                    commands.push(Command::SetRepeat {
                        id: other.clone(),
                        rule: None,
                    });
                }
                if *rescheduled {
                    commands.extend(super::sync::rescheduled(tx, id)?);
                }
                if let (Some(series), Some(date)) = (&task.series, task.occurrence) {
                    commands.push(Command::SetOccurrence {
                        id: id.into(),
                        series: series.clone(),
                        date,
                    });
                }
                commands.push(Command::SetRepeat {
                    id: id.into(),
                    rule: task.repeat,
                });
                Ok(commands)
            },
        )
        .map(|_| ())
    }

    /// Create the occurrences of fixed-schedule series due by `today`: for
    /// each series, the latest scheduled date after its newest occurrence's
    /// and no later than `today`. Returns the new occurrences' ids.
    pub fn advance_repeats(&self, today: Date) -> Result<Vec<String>> {
        let now = crate::clock::now_millis();
        let (created, _) = self.write(
            now,
            |tx| {
                let heads: Vec<Task> = {
                    let mut stmt = tx.prepare(&format!(
                        "SELECT {TASK_COLUMNS} FROM tasks
                         WHERE repeat_rule IS NOT NULL AND {LIVE_TASK}
                           AND (project_id IS NULL OR project_id IN
                                (SELECT id FROM projects WHERE status = 'open'))
                         ORDER BY id"
                    ))?;
                    let rows = stmt.query_map([], Database::map_task_row)?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                let mut created = Vec::new();
                let mut commands = Vec::new();
                for head in heads {
                    let Some(rule) = &head.repeat else { continue };
                    if rule.after_completion || rule.paused {
                        continue;
                    }
                    let Some(last) = head.occurrence.or(head.start_date) else {
                        continue;
                    };
                    if let Some(date) = rule.latest_due(last, today) {
                        if let Some(id) = create_occurrence(tx, &head, date, now, &mut commands)? {
                            created.push(id);
                        }
                    }
                }
                Ok((created, commands))
            },
            |_, (_, commands)| Ok(commands.clone()),
        )?;
        Ok(created)
    }

    /// The repeating series' future occurrences from `from` up to `until`
    /// (exclusive), for Upcoming to preview (#61): of each live series
    /// whose project, if any, is open.
    pub fn repeat_previews(&self, from: Date, until: Date) -> Result<Vec<crate::repeat::Preview>> {
        let heads: Vec<Task> = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks
                 WHERE repeat_rule IS NOT NULL AND {LIVE_TASK}
                   AND (project_id IS NULL OR project_id IN
                        (SELECT id FROM projects WHERE status = 'open'))"
            ))?;
            let rows = stmt.query_map([], Database::map_task_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        Ok(heads
            .iter()
            .flat_map(|head| crate::repeat::Preview::of(head, from, until))
            .collect())
    }

    /// Complete task `id` on local day `today`. If it is the newest
    /// occurrence of a series that repeats after completion, the next
    /// occurrence is created in the same change.
    pub fn complete_task_on(&self, id: &str, today: Date) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let changed = tx.execute(
                    "UPDATE tasks SET status = 'completed', completed_at = ?1, updated_at = ?1,
                        closed_by_project = 0 WHERE id = ?2",
                    params![now, id],
                )? > 0;
                let mut commands = Vec::new();
                if !changed {
                    return Ok(commands);
                }
                commands.push(Command::SetStatus {
                    id: id.into(),
                    status: crate::domain::Status::Completed,
                    completed_at: Some(now),
                    closed_by_project: false,
                });
                let head = tx
                    .query_row(
                        &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1 AND {LIVE_TASK}"),
                        params![id],
                        Database::map_task_row,
                    )
                    .optional()?;
                if let Some(head) = head {
                    if let Some(rule) = head
                        .repeat
                        .as_ref()
                        .filter(|r| r.after_completion && !r.paused)
                    {
                        if let Some(date) = rule.after_completed(today) {
                            create_occurrence(tx, &head, date, now, &mut commands)?;
                        }
                    }
                }
                Ok(commands)
            },
            |_, commands| Ok(commands.clone()),
        )
        .map(|_| ())
    }
}

/// The ids `sql` selects, with `args`.
fn ids(tx: &Transaction<'_>, sql: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map(args, |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Create the occurrence of `head`'s series on `date`, a copy of `head`
/// that takes over its rule, and add its engine commands to `commands`.
/// Returns its id, or `None` when nothing was created: the occurrence
/// exists already (`head` then gives up its rule, which has passed on), or
/// the series has no occurrences left.
fn create_occurrence(
    tx: &Transaction<'_>,
    head: &Task,
    date: Date,
    now: i64,
    commands: &mut Vec<Command>,
) -> Result<Option<String>> {
    let Some(rule) = &head.repeat else {
        return Ok(None);
    };
    let series = head.series.clone().unwrap_or_else(|| head.id.clone());
    let id = occurrence_id(&series, date)
        .ok_or_else(|| MattersError::Validation(format!("series '{series}' is not an id")))?;
    let exists = tx
        .query_row("SELECT 1 FROM tasks WHERE id = ?1", params![id], |_| Ok(()))
        .optional()?
        .is_some();
    let next_rule = rule.passed_on();
    if exists || next_rule.is_none() {
        if exists {
            give_up_rule(tx, head, commands)?;
        }
        return Ok(None);
    }
    // The deadline keeps its distance from the date.
    let deadline = match (head.deadline, head.start_date) {
        (Some(deadline), Some(start)) => start
            .until(deadline)
            .ok()
            .and_then(|span| date.checked_add(i64::from(span.get_days()).days()).ok()),
        (deadline, _) => deadline,
    };
    let sort_order = key_after(tx, "sort_order", head.sort_order)?;
    let today_order = key_after(tx, "today_order", head.today_order)?;
    tx.execute(
        "INSERT INTO tasks (
            id, area_id, project_id, heading_id, title, notes, start_date, time_of_day,
            is_someday, deadline, status, completed_at, sort_order, created_at, updated_at,
            trashed_at, today_order, closed_by_project, reminder_time,
            repeat_rule, repeat_series, repeat_date
         )
         SELECT ?2, area_id, project_id, heading_id, title, notes, ?3, time_of_day,
            0, ?4, 'open', NULL, ?5, ?6, ?6,
            NULL, ?7, 0, reminder_time,
            ?8, ?9, ?3
         FROM tasks WHERE id = ?1",
        params![
            head.id,
            id,
            date.to_string(),
            deadline.map(|d| d.to_string()),
            sort_order,
            now,
            today_order,
            stored_rule(next_rule.as_ref()),
            series
        ],
    )?;
    for (title, _, sort_order) in checklist(tx, &head.id)? {
        tx.execute(
            "INSERT INTO checklist_items
                (id, task_id, title, is_completed, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?5)",
            params![uuid::Uuid::new_v4().to_string(), id, title, sort_order, now],
        )?;
    }
    tx.execute(
        "INSERT INTO task_tags (task_id, tag_id) SELECT ?2, tag_id FROM task_tags WHERE task_id = ?1",
        params![head.id, id],
    )?;
    give_up_rule(tx, head, commands)?;
    commands.extend(super::sync::created_task_with_children(tx, &id)?);
    Ok(Some(id))
}

/// `head` no longer carries its series' rule: it has passed it on.
fn give_up_rule(tx: &Transaction<'_>, head: &Task, commands: &mut Vec<Command>) -> Result<()> {
    tx.execute(
        "UPDATE tasks SET repeat_rule = NULL WHERE id = ?1",
        params![head.id],
    )?;
    commands.push(Command::SetRepeat {
        id: head.id.clone(),
        rule: None,
    });
    Ok(())
}
