//! Undoing an action on several tasks at once (#64).
//!
//! Before a batch action (complete, cancel, trash, schedule, set a
//! deadline, move or tag the selected tasks) a frontend takes a
//! [`TaskSnapshot`] of them; Undo restores it. Each part of a task goes back
//! through the same typed operation that changes it, and only where it
//! differs, so restoring never touches what the action left alone. The
//! batch actions themselves are the single-task operations, applied to each
//! selected task.

use jiff::civil::Date;

use crate::domain::{Home, Schedule, Status, Tagged, Task};
use crate::error::{MattersError, Result};
use crate::storage::Database;

/// Tasks as they were, with their own tags.
#[derive(Debug, Clone, Default)]
pub struct TaskSnapshot {
    saved: Vec<(Task, Vec<String>)>,
}

impl TaskSnapshot {
    /// Remember tasks `ids` as they are now. Ids of missing tasks are
    /// skipped.
    pub fn take(db: &Database, ids: &[String]) -> Result<Self> {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut tags = db.tags_of_tasks(&refs)?;
        let mut saved = Vec::new();
        for id in ids {
            if let Some(task) = db.get_task(id)? {
                let own = tags
                    .remove(id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| t.id)
                    .collect();
                saved.push((task, own));
            }
        }
        Ok(Self { saved })
    }

    /// How many tasks are remembered.
    pub fn len(&self) -> usize {
        self.saved.len()
    }

    pub fn is_empty(&self) -> bool {
        self.saved.is_empty()
    }

    /// Put every remembered task back as it was, on local day `today`:
    /// in or out of the Trash, its home and place in its lists, When,
    /// deadline, status and tags. A task deleted since (the Trash emptied)
    /// stays deleted, and a tag deleted since isn't given back. A task that
    /// can't go back (its old project is in the Trash, say) doesn't stop the
    /// others; the first such error is returned once they are done.
    pub fn restore(&self, db: &Database, today: Date) -> Result<()> {
        let mut first_error = None;
        for (old, tags) in &self.saved {
            if let Err(err) = restore_task(db, old, tags, today) {
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Put task `old.id` back as `old`, with own tags `tags`.
fn restore_task(db: &Database, old: &Task, tags: &[String], today: Date) -> Result<()> {
    let Some(now) = db.get_task(&old.id)? else {
        return Ok(());
    };
    let id = old.id.as_str();
    if old.trashed_at.is_none() && now.trashed_at.is_some() {
        db.restore_task(id)?;
    }
    let home = Home::of(old)?;
    if Home::of(&now)? != home {
        db.set_task_home(id, &home)?;
    }
    if (old.start_date, &old.time_of_day, old.is_someday)
        != (now.start_date, &now.time_of_day, now.is_someday)
    {
        restore_when(db, old, today)?;
    }
    if old.deadline != now.deadline {
        db.set_task_deadline(id, old.deadline)?;
    }
    if old.reminder != now.reminder {
        db.set_task_reminder(id, old.reminder)?;
    }
    if old.status != now.status {
        match old.status {
            Status::Open => db.uncomplete_task(id)?,
            Status::Completed => db.complete_task(id)?,
            Status::Canceled => db.cancel_task(id)?,
        }
    }
    restore_tags(db, id, tags)?;
    let now = db.get_task(id)?.unwrap_or(now);
    if (old.sort_order, old.today_order) != (now.sort_order, now.today_order) {
        crate::ordering::restore_task_order(db, id, old.sort_order, old.today_order)?;
    }
    if old.trashed_at.is_some() && now.trashed_at.is_none() {
        db.trash_task(id)?;
    }
    Ok(())
}

/// Give task `old.id` the When `old` had: its date (or none, or Someday),
/// and This Evening.
fn restore_when(db: &Database, old: &Task, today: Date) -> Result<()> {
    let schedule = match (old.is_someday, old.start_date) {
        (true, _) => Schedule::Someday,
        (false, Some(date)) => Schedule::Date(date),
        (false, None) => Schedule::None,
    };
    db.set_task_schedule(&old.id, &schedule, today)?;
    if old.is_evening() {
        db.set_task_evening(&old.id, true, today)?;
    }
    Ok(())
}

/// Give task `id` exactly the tags `tags` again; a tag deleted since is
/// left out.
fn restore_tags(db: &Database, id: &str, tags: &[String]) -> Result<()> {
    let now: Vec<String> = db
        .tags_of_tasks(&[id])?
        .remove(id)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.id)
        .collect();
    for tag in now.iter().filter(|t| !tags.contains(t)) {
        db.remove_tag_from(Tagged::Task(id), tag)?;
    }
    for tag in tags.iter().filter(|t| !now.contains(t)) {
        match db.add_tag_to(Tagged::Task(id), tag) {
            Err(MattersError::NotFound { entity: "tag", .. }) => {}
            other => other?,
        }
    }
    Ok(())
}
