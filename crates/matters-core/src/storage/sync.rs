//! Recording typed writes in the sync engine's document (#35, ADR 0001).
//!
//! While sync is off nothing here runs: every write commits exactly as
//! before. Once it is on, a write's SQL change, its engine commands and the
//! journalled engine update commit in one SQLite transaction. If anything
//! in that transaction fails, the engine is rebuilt from the journal, so it
//! never holds a change SQLite rolled back.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use super::db::Database;
use crate::crdt::{Anchor, Command, Document, Kind};
use crate::domain::{Area, ChecklistItem, Heading, Project, Status, Task};
use crate::error::{MattersError, Result};

/// A fresh engine actor for each open, so a copied database never reuses
/// another copy's operation IDs (ADR 0001, C15).
fn fresh_peer() -> u64 {
    uuid::Uuid::new_v4().as_u64_pair().0
}

/// The engine as the journal leaves it, or `None` while sync is off.
pub(crate) fn load_engine(conn: &Connection) -> Result<Option<Document>> {
    let enabled: Option<i64> = conn
        .query_row("SELECT enabled_at FROM sync_state", [], |r| r.get(0))
        .optional()?;
    if enabled.is_none() {
        return Ok(None);
    }
    let mut stmt = conn.prepare("SELECT bytes FROM sync_journal ORDER BY seq")?;
    let updates = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(Document::from_updates(
        updates.iter().map(Vec::as_slice),
        fresh_peer(),
    )?))
}

impl Database {
    /// Turn sync on for an empty database. Existing data needs the
    /// migration of #41, so a database with any rows is refused. Until
    /// every write is recorded (#35), only tests turn it on.
    #[cfg(feature = "test-support")]
    pub fn enable_sync(&self) -> Result<()> {
        self.with_engine(|conn, engine| {
            if engine.is_some() {
                return Err(MattersError::Validation("sync is already on".into()));
            }
            let rows: i64 = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM areas) + (SELECT COUNT(*) FROM projects)
                      + (SELECT COUNT(*) FROM tasks) + (SELECT COUNT(*) FROM tags)",
                [],
                |r| r.get(0),
            )?;
            if rows > 0 {
                return Err(MattersError::Validation(
                    "existing data must be migrated before sync (#41)".into(),
                ));
            }
            conn.execute(
                "INSERT INTO sync_state (singleton, enabled_at) VALUES (1, ?1)",
                params![crate::clock::now_millis()],
            )?;
            *engine = Some(Document::new(fresh_peer())?);
            Ok(())
        })
    }

    /// Whether writes are being recorded for sync.
    pub fn sync_enabled(&self) -> bool {
        self.with_engine(|_, engine| Ok(engine.is_some()))
            .unwrap_or(false)
    }

    /// The engine's view of the data, to compare with the rows.
    #[cfg(feature = "test-support")]
    pub fn engine_snapshot(&self) -> Result<Option<crate::crdt::Snapshot>> {
        self.with_engine(|_, engine| engine.as_ref().map(Document::project).transpose())
    }

    /// The engine actor this open edits as.
    #[cfg(feature = "test-support")]
    pub fn engine_peer(&self) -> Option<u64> {
        self.with_engine(|_, engine| Ok(engine.as_ref().map(Document::peer)))
            .unwrap_or(None)
    }

    /// Run `apply` in one write transaction. With sync on, also apply the
    /// commands `commands` derives from its result to the engine at time
    /// `at`, and journal the update, before the same commit.
    pub(crate) fn write<T>(
        &self,
        at: i64,
        apply: impl FnOnce(&Transaction<'_>) -> Result<T>,
        commands: impl FnOnce(&Transaction<'_>, &T) -> Result<Vec<Command>>,
    ) -> Result<T> {
        self.with_engine(|conn, engine| {
            let tx = conn.transaction()?;
            let value = apply(&tx)?;
            let Some(doc) = engine.as_mut() else {
                tx.commit()?;
                return Ok(value);
            };
            let committed = (|| {
                let commands = commands(&tx, &value)?;
                if !commands.is_empty() {
                    let before = doc.version();
                    doc.apply(&commands, at)?;
                    tx.execute(
                        "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 1)",
                        params![doc.updates_since(&before)?],
                    )?;
                }
                Ok::<_, MattersError>(tx.commit()?)
            })();
            if let Err(error) = committed {
                *engine = load_engine(conn)?;
                return Err(error);
            }
            Ok(value)
        })
    }
}

/// Where an entity goes among its siblings: right after the one before it
/// in its list's order, as `previous` finds it, or first.
fn after(previous: Option<String>) -> Anchor {
    previous.map_or(Anchor::First, Anchor::After)
}

fn previous(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Option<String>> {
    Ok(tx.query_row(sql, args, |r| r.get(0)).optional()?)
}

pub(crate) fn create_area(tx: &Transaction<'_>, area: &Area) -> Result<Vec<Command>> {
    let prev = previous(
        tx,
        "SELECT id FROM areas WHERE id != ?1 AND sort_order < ?2 ORDER BY sort_order DESC LIMIT 1",
        &[&area.id, &area.sort_order],
    )?;
    let mut commands = vec![Command::Create {
        id: area.id.clone(),
        kind: Kind::Area,
        parent: None,
        at: after(prev),
        title: area.title.clone(),
        created_at: area.created_at,
    }];
    if area.trashed_at.is_some() {
        commands.push(Command::SetTrashed {
            id: area.id.clone(),
            trashed_at: area.trashed_at,
        });
    }
    Ok(commands)
}

/// The fields of a project or task beyond its title, as commands.
fn fields(
    id: &str,
    notes: &str,
    when: (Option<jiff::civil::Date>, bool, bool),
    deadline: Option<jiff::civil::Date>,
    status: Status,
    completed_at: Option<i64>,
    trashed_at: Option<i64>,
) -> Vec<Command> {
    let id = id.to_string();
    let mut commands = Vec::new();
    if !notes.is_empty() {
        commands.push(Command::SetNotes {
            id: id.clone(),
            notes: notes.into(),
        });
    }
    if when != (None, false, false) {
        commands.push(Command::SetSchedule {
            id: id.clone(),
            start_date: when.0,
            evening: when.1,
            someday: when.2,
        });
    }
    if deadline.is_some() {
        commands.push(Command::SetDeadline {
            id: id.clone(),
            deadline,
        });
    }
    if status != Status::Open {
        commands.push(Command::SetStatus {
            id: id.clone(),
            status,
            completed_at,
            closed_by_project: false,
        });
    }
    if trashed_at.is_some() {
        commands.push(Command::SetTrashed { id, trashed_at });
    }
    commands
}

pub(crate) fn create_project(tx: &Transaction<'_>, project: &Project) -> Result<Vec<Command>> {
    let prev = previous(
        tx,
        "SELECT id FROM projects WHERE id != ?1 AND area_id IS ?2 AND sort_order < ?3
         ORDER BY sort_order DESC LIMIT 1",
        &[&project.id, &project.area_id, &project.sort_order],
    )?;
    let mut commands = vec![Command::Create {
        id: project.id.clone(),
        kind: Kind::Project,
        parent: project.area_id.clone(),
        at: after(prev),
        title: project.title.clone(),
        created_at: project.created_at,
    }];
    commands.extend(fields(
        &project.id,
        &project.notes,
        (
            project.start_date,
            project.time_of_day.as_deref() == Some("evening"),
            project.is_someday,
        ),
        project.deadline,
        project.status,
        project.completed_at,
        project.trashed_at,
    ));
    Ok(commands)
}

pub(crate) fn create_heading(tx: &Transaction<'_>, heading: &Heading) -> Result<Vec<Command>> {
    let prev = previous(
        tx,
        "SELECT id FROM headings WHERE id != ?1 AND project_id = ?2 AND sort_order < ?3
         ORDER BY sort_order DESC LIMIT 1",
        &[&heading.id, &heading.project_id, &heading.sort_order],
    )?;
    Ok(vec![Command::Create {
        id: heading.id.clone(),
        kind: Kind::Heading,
        parent: Some(heading.project_id.clone()),
        at: after(prev),
        title: heading.title.clone(),
        created_at: heading.created_at,
    }])
}

/// The tree parent of a task's home: its heading, project or area.
pub(crate) fn task_parent(task: &Task) -> Option<String> {
    task.heading_id
        .clone()
        .or_else(|| task.project_id.clone())
        .or_else(|| task.area_id.clone())
}

/// The task before `task` in its home list, by `sort_order`.
pub(crate) fn previous_in_home(tx: &Transaction<'_>, task: &Task) -> Result<Option<String>> {
    previous(
        tx,
        "SELECT id FROM tasks WHERE id != ?1 AND heading_id IS ?2
           AND (?2 IS NOT NULL OR (project_id IS ?3 AND (?3 IS NOT NULL OR area_id IS ?4)))
           AND sort_order < ?5
         ORDER BY sort_order DESC LIMIT 1",
        &[
            &task.id,
            &task.heading_id,
            &task.project_id,
            &task.area_id,
            &task.sort_order,
        ],
    )
}

pub(crate) fn create_task(tx: &Transaction<'_>, task: &Task) -> Result<Vec<Command>> {
    let mut commands = vec![Command::Create {
        id: task.id.clone(),
        kind: Kind::Task,
        parent: task_parent(task),
        at: after(previous_in_home(tx, task)?),
        title: task.title.clone(),
        created_at: task.created_at,
    }];
    let today = previous(
        tx,
        "SELECT id FROM tasks WHERE id != ?1 AND today_order < ?2
         ORDER BY today_order DESC LIMIT 1",
        &[&task.id, &task.today_order],
    )?;
    commands.push(Command::PlaceInToday {
        id: task.id.clone(),
        at: after(today),
    });
    commands.extend(fields(
        &task.id,
        &task.notes,
        (task.start_date, task.is_evening(), task.is_someday),
        task.deadline,
        task.status,
        task.completed_at,
        task.trashed_at,
    ));
    Ok(commands)
}

pub(crate) fn create_checklist_item(
    tx: &Transaction<'_>,
    item: &ChecklistItem,
) -> Result<Vec<Command>> {
    let prev = previous(
        tx,
        "SELECT id FROM checklist_items WHERE id != ?1 AND task_id = ?2 AND sort_order < ?3
         ORDER BY sort_order DESC LIMIT 1",
        &[&item.id, &item.task_id, &item.sort_order],
    )?;
    let mut commands = vec![Command::Create {
        id: item.id.clone(),
        kind: Kind::ChecklistItem,
        parent: Some(item.task_id.clone()),
        at: after(prev),
        title: item.title.clone(),
        created_at: item.created_at,
    }];
    if item.is_completed {
        commands.push(Command::SetDone {
            id: item.id.clone(),
            done: true,
        });
    }
    Ok(commands)
}

/// A task's When as stored, as one command.
pub(crate) fn task_schedule(tx: &Transaction<'_>, id: &str) -> Result<Vec<Command>> {
    let (start_date, time_of_day, someday): (Option<String>, Option<String>, bool) = tx.query_row(
        "SELECT start_date, time_of_day, is_someday FROM tasks WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(vec![Command::SetSchedule {
        id: id.into(),
        start_date: start_date.as_deref().and_then(crate::clock::parse_iso_date),
        evening: time_of_day.as_deref() == Some("evening"),
        someday,
    }])
}

/// A task's status, finish time and "closed by its project" mark as
/// stored, as one command.
pub(crate) fn task_status(tx: &Transaction<'_>, id: &str) -> Result<Vec<Command>> {
    let (status, completed_at, closed): (String, Option<i64>, bool) = tx.query_row(
        "SELECT status, completed_at, closed_by_project FROM tasks WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(vec![Command::SetStatus {
        id: id.into(),
        status: Status::from_str_loose(&status),
        completed_at,
        closed_by_project: closed,
    }])
}
