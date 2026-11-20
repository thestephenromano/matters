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

/// Test seam for crash tests: with the `test-support` feature, a process
/// whose `MATTERS_CRASH_AT` names this point exits on the spot, as a crash
/// would, without unwinding or committing.
fn crash_point(point: &str) {
    #[cfg(feature = "test-support")]
    if std::env::var("MATTERS_CRASH_AT").as_deref() == Ok(point) {
        std::process::exit(86);
    }
    #[cfg(not(feature = "test-support"))]
    let _ = point;
}

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
                crash_point("before-commit");
                Ok::<_, MattersError>(tx.commit()?)
            })();
            if let Err(error) = committed {
                *engine = load_engine(conn)?;
                return Err(error);
            }
            crash_point("after-commit");
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
    commands.extend(fields(
        &task.id,
        &task.notes,
        (task.start_date, task.is_evening(), task.is_someday),
        task.deadline,
        task.status,
        task.completed_at,
        task.trashed_at,
    ));
    if task.reminder.is_some() {
        commands.push(Command::SetReminder {
            id: task.id.clone(),
            time: task.reminder,
        });
    }
    // Placed in Today's order within its section, once its When is known.
    commands.extend(reorder(
        tx,
        "tasks",
        "today_order",
        std::slice::from_ref(&task.id),
    )?);
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
/// A task's new When, and its place in the Today section it now belongs to.
pub(crate) fn rescheduled(tx: &Transaction<'_>, id: &str) -> Result<Vec<Command>> {
    let mut commands = task_schedule(tx, id)?;
    commands.extend(reorder(tx, "tasks", "today_order", &[id.to_string()])?);
    Ok(commands)
}

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

/// The IDs `sql` selects for `id`, its only parameter.
pub(crate) fn task_ids(tx: &Transaction<'_>, sql: &str, id: &str) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(sql)?;
    let ids = stmt
        .query_map(params![id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// A project's status and those of `tasks`, as stored, as commands.
pub(crate) fn project_and_tasks_status(
    tx: &Transaction<'_>,
    project: &str,
    tasks: &[String],
) -> Result<Vec<Command>> {
    let (status, completed_at): (String, Option<i64>) = tx.query_row(
        "SELECT status, completed_at FROM projects WHERE id = ?1",
        params![project],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut commands = vec![Command::SetStatus {
        id: project.into(),
        status: Status::from_str_loose(&status),
        completed_at,
        closed_by_project: false,
    }];
    for task in tasks {
        commands.extend(task_status(tx, task)?);
    }
    Ok(commands)
}

/// Move each of `tasks` to where its row now says it lives, in its list's
/// order, so the engine's lists match the rows.
pub(crate) fn refile_in_order(tx: &Transaction<'_>, tasks: &[String]) -> Result<Vec<Command>> {
    let mut rows = Vec::new();
    for id in tasks {
        rows.push(tx.query_row(
            &format!(
                "SELECT {} FROM tasks WHERE id = ?1",
                super::db::TASK_COLUMNS
            ),
            params![id],
            Database::map_task_row,
        )?);
    }
    rows.sort_by(|x, y| x.sort_order.total_cmp(&y.sort_order));
    let mut commands = Vec::new();
    for task in &rows {
        commands.push(Command::Move {
            id: task.id.clone(),
            parent: task_parent(task),
            at: after(previous_in_home(tx, task)?),
        });
    }
    Ok(commands)
}

/// Re-place each row of `table` whose `column` key changed (`ids`), right
/// after the row before it by that key among its siblings, in ascending key
/// order. Rows whose keys stayed keep their places, so the engine's order
/// ends up as the keys say, even after a renumbering.
pub(crate) fn reorder(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    ids: &[String],
) -> Result<Vec<Command>> {
    // The siblings of a row, and its key.
    let siblings = match (table, column) {
        // Today's keys order each section (daytime, This Evening) apart.
        ("tasks", "today_order") => {
            "(time_of_day IS 'evening') = ((SELECT time_of_day FROM tasks WHERE id = ?1) IS 'evening')"
        }
        ("tasks", _) => {
            "heading_id IS (SELECT heading_id FROM tasks WHERE id = ?1)
             AND project_id IS (SELECT project_id FROM tasks WHERE id = ?1)
             AND area_id IS (SELECT area_id FROM tasks WHERE id = ?1)"
        }
        ("headings", _) => "project_id = (SELECT project_id FROM headings WHERE id = ?1)",
        ("projects", _) => "area_id IS (SELECT area_id FROM projects WHERE id = ?1)",
        ("checklist_items", _) => "task_id = (SELECT task_id FROM checklist_items WHERE id = ?1)",
        _ => "1",
    };
    let key_sql = format!("SELECT {column} FROM {table} WHERE id = ?1");
    let mut keyed = Vec::new();
    for id in ids {
        let key: f64 = tx.query_row(&key_sql, params![id], |r| r.get(0))?;
        keyed.push((key, id));
    }
    keyed.sort_by(|x, y| x.0.total_cmp(&y.0));
    let prev_sql = format!(
        "SELECT id FROM {table} WHERE id != ?1 AND {siblings} AND {column} < ?2
         ORDER BY {column} DESC LIMIT 1"
    );
    let mut commands = Vec::new();
    for (key, id) in keyed {
        let at = after(previous(tx, &prev_sql, &[id, &key])?);
        commands.push(if column == "today_order" {
            Command::PlaceInToday { id: id.clone(), at }
        } else {
            Command::Place { id: id.clone(), at }
        });
    }
    Ok(commands)
}

/// A heading filed in another project, with its tasks: each is moved, so
/// the engine stamps them as the rows are.
pub(crate) fn moved_heading(tx: &Transaction<'_>, id: &str) -> Result<Vec<Command>> {
    let heading = tx.query_row(
        "SELECT id, project_id, title, sort_order, created_at, updated_at FROM headings WHERE id = ?1",
        params![id],
        |r| {
            Ok(Heading {
                id: r.get(0)?,
                project_id: r.get(1)?,
                title: r.get(2)?,
                sort_order: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        },
    )?;
    let prev = previous(
        tx,
        "SELECT id FROM headings WHERE id != ?1 AND project_id = ?2 AND sort_order < ?3
         ORDER BY sort_order DESC LIMIT 1",
        &[&heading.id, &heading.project_id, &heading.sort_order],
    )?;
    let mut commands = vec![Command::Move {
        id: id.into(),
        parent: Some(heading.project_id.clone()),
        at: after(prev),
    }];
    let tasks = task_ids(tx, "SELECT id FROM tasks WHERE heading_id = ?1", id)?;
    commands.extend(refile_in_order(tx, &tasks)?);
    Ok(commands)
}

/// Where tag `id` goes among its siblings: after the one before it by key.
pub(crate) fn tag_anchor(tx: &Transaction<'_>, id: &str) -> Result<Anchor> {
    Ok(after(previous(
        tx,
        "SELECT t.id FROM tags t, tags me WHERE me.id = ?1 AND t.id != me.id
           AND t.parent_id IS me.parent_id AND t.sort_order < me.sort_order
         ORDER BY t.sort_order DESC LIMIT 1",
        &[&id],
    )?))
}

/// Tags whose parent changed, each nested where its row says, in key
/// order among its new siblings.
pub(crate) fn tags_nested(tx: &Transaction<'_>, ids: &[String]) -> Result<Vec<Command>> {
    let mut rows = Vec::new();
    for id in ids {
        let (parent, key): (Option<String>, f64) = tx.query_row(
            "SELECT parent_id, sort_order FROM tags WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        rows.push((key, id, parent));
    }
    rows.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut commands = Vec::new();
    for (_, id, parent) in rows {
        commands.push(Command::MoveTag {
            id: id.clone(),
            parent,
            at: tag_anchor(tx, id)?,
        });
    }
    Ok(commands)
}

/// A tag given to or taken off an item, when that changed anything.
pub(crate) fn link(changed: bool, item: &str, tag: &str, present: bool) -> Vec<Command> {
    if !changed {
        return Vec::new();
    }
    vec![Command::Link {
        item: item.into(),
        tag: tag.into(),
        present,
    }]
}

/// Every entity Empty Trash deletes, its cascades included: trashed tasks
/// and those inside trashed projects or areas, with their checklist items;
/// trashed projects and those in trashed areas, with their headings; and
/// trashed areas.
pub(crate) fn trashed_entities(tx: &Transaction<'_>) -> Result<Vec<String>> {
    use super::trash::{ANY_TRASHED_PROJECT, ANY_TRASHED_TASK, TRASHED_AREA};
    let sql = format!(
        "SELECT id FROM tasks WHERE {ANY_TRASHED_TASK}
         UNION SELECT id FROM checklist_items WHERE task_id IN (SELECT id FROM tasks WHERE {ANY_TRASHED_TASK})
         UNION SELECT id FROM projects WHERE {ANY_TRASHED_PROJECT}
         UNION SELECT id FROM headings WHERE project_id IN (SELECT id FROM projects WHERE {ANY_TRASHED_PROJECT})
         UNION SELECT id FROM areas WHERE {TRASHED_AREA}"
    );
    let mut stmt = tx.prepare(&sql)?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// A new task, its checklist and its tags, as stored, as commands.
pub(crate) fn created_task_with_children(tx: &Transaction<'_>, id: &str) -> Result<Vec<Command>> {
    let task = tx.query_row(
        &format!(
            "SELECT {} FROM tasks WHERE id = ?1",
            super::db::TASK_COLUMNS
        ),
        params![id],
        Database::map_task_row,
    )?;
    let mut commands = create_task(tx, &task)?;
    let items = task_ids(
        tx,
        "SELECT id FROM checklist_items WHERE task_id = ?1 ORDER BY sort_order",
        id,
    )?;
    for item in items {
        let item = tx.query_row(
            "SELECT id, task_id, title, is_completed, sort_order, created_at, updated_at
             FROM checklist_items WHERE id = ?1",
            params![item],
            Database::map_checklist_row,
        )?;
        commands.extend(create_checklist_item(tx, &item)?);
    }
    for tag in task_ids(tx, "SELECT tag_id FROM task_tags WHERE task_id = ?1", id)? {
        commands.extend(link(true, id, &tag, true));
    }
    Ok(commands)
}

/// A task turned into a project: the project with its tags, its new tasks,
/// and the task and its checklist gone for good (`gone`). A concurrent edit
/// to the old task elsewhere is dropped, as for Empty Trash.
pub(crate) fn converted(
    tx: &Transaction<'_>,
    project: &str,
    new_tasks: &[String],
    gone: &[String],
) -> Result<Vec<Command>> {
    let row = tx.query_row(
        &format!(
            "SELECT {} FROM projects WHERE id = ?1",
            super::db::PROJECT_COLUMNS
        ),
        params![project],
        Database::map_project_row,
    )?;
    let mut commands = create_project(tx, &row)?;
    for tag in task_ids(
        tx,
        "SELECT tag_id FROM project_tags WHERE project_id = ?1",
        project,
    )? {
        commands.extend(link(true, project, &tag, true));
    }
    for task in new_tasks {
        commands.extend(created_task_with_children(tx, task)?);
    }
    commands.push(Command::Purge { ids: gone.to_vec() });
    Ok(commands)
}

impl Database {
    /// Apply an engine update from another replica: it is journalled as
    /// received (never sent back out), merged, and the rows are rebuilt
    /// from the engine, in one transaction. An update whose dependencies
    /// have not arrived stays in the journal and applies once they do.
    pub fn receive_update(&self, bytes: &[u8]) -> Result<()> {
        self.with_engine(|conn, engine| {
            let Some(doc) = engine.as_mut() else {
                return Err(MattersError::Validation("sync is off".into()));
            };
            let tx = conn.transaction()?;
            let applied = (|| {
                tx.execute(
                    "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 0)",
                    params![bytes],
                )?;
                doc.merge(bytes)?;
                rebuild_rows(&tx, &doc.project()?)?;
                crash_point("before-commit");
                Ok::<_, MattersError>(tx.commit()?)
            })();
            if let Err(error) = applied {
                *engine = load_engine(conn)?;
                return Err(error);
            }
            crash_point("after-commit");
            Ok(())
        })
    }

    /// This replica's own engine updates journalled after `after`, in
    /// order, with their journal sequence numbers: what a sync client
    /// sends.
    pub fn outgoing_updates(&self, after: i64) -> Result<Vec<(i64, Vec<u8>)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, bytes FROM sync_journal WHERE outgoing = 1 AND seq > ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![after], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Throw the rows away and rebuild them from the engine, as receiving
    /// does: the views must come out the same.
    #[cfg(feature = "test-support")]
    pub fn rebuild_from_engine(&self) -> Result<()> {
        self.with_engine(|conn, engine| {
            let doc = engine
                .as_ref()
                .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
            let tx = conn.transaction()?;
            rebuild_rows(&tx, &doc.project()?)?;
            Ok(tx.commit()?)
        })
    }
}

/// Replace every domain row with the engine's. Order keys become ranks.
fn rebuild_rows(tx: &Transaction<'_>, s: &crate::crdt::Snapshot) -> Result<()> {
    tx.execute_batch(
        "DELETE FROM task_tags; DELETE FROM project_tags; DELETE FROM area_tags;
         DELETE FROM checklist_items; DELETE FROM tasks; DELETE FROM headings;
         DELETE FROM projects; DELETE FROM areas; DELETE FROM tags;",
    )?;
    let date = |d: Option<jiff::civil::Date>| d.map(|d| d.to_string());
    for a in &s.areas {
        tx.execute(
            "INSERT INTO areas (id, title, sort_order, created_at, updated_at, trashed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                a.id,
                a.title,
                a.sort_order,
                a.created_at,
                a.updated_at,
                a.trashed_at
            ],
        )?;
    }
    for p in &s.projects {
        tx.execute(
            "INSERT INTO projects (id, area_id, title, notes, start_date, time_of_day, is_someday,
                deadline, status, completed_at, sort_order, created_at, updated_at, trashed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                p.id,
                p.area_id,
                p.title,
                p.notes,
                date(p.start_date),
                p.time_of_day,
                p.is_someday,
                date(p.deadline),
                p.status.as_str(),
                p.completed_at,
                p.sort_order,
                p.created_at,
                p.updated_at,
                p.trashed_at
            ],
        )?;
    }
    for h in &s.headings {
        tx.execute(
            "INSERT INTO headings (id, project_id, title, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                h.id,
                h.project_id,
                h.title,
                h.sort_order,
                h.created_at,
                h.updated_at
            ],
        )?;
    }
    for t in &s.tasks {
        tx.execute(
            "INSERT INTO tasks (id, area_id, project_id, heading_id, title, notes, start_date,
                time_of_day, is_someday, deadline, status, completed_at, sort_order, created_at,
                updated_at, trashed_at, today_order, closed_by_project, reminder_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19)",
            params![
                t.id,
                t.area_id,
                t.project_id,
                t.heading_id,
                t.title,
                t.notes,
                date(t.start_date),
                t.time_of_day,
                t.is_someday,
                date(t.deadline),
                t.status.as_str(),
                t.completed_at,
                t.sort_order,
                t.created_at,
                t.updated_at,
                t.trashed_at,
                t.today_order,
                s.closed_by_project.contains(&t.id),
                t.reminder.map(crate::reminders::stored_time)
            ],
        )?;
    }
    for i in &s.checklist_items {
        tx.execute(
            "INSERT INTO checklist_items (id, task_id, title, is_completed, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![i.id, i.task_id, i.title, i.is_completed, i.sort_order, i.created_at, i.updated_at],
        )?;
    }
    for t in &s.tags {
        tx.execute(
            "INSERT INTO tags (id, name, parent_id, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                t.id,
                t.name,
                t.parent_id,
                t.sort_order,
                t.created_at,
                t.updated_at
            ],
        )?;
    }
    for (kind, item, tag) in &s.links {
        let table = match kind {
            crate::crdt::LinkKind::Task => "task_tags (task_id, tag_id)",
            crate::crdt::LinkKind::Project => "project_tags (project_id, tag_id)",
            crate::crdt::LinkKind::Area => "area_tags (area_id, tag_id)",
        };
        tx.execute(
            &format!("INSERT INTO {table} VALUES (?1, ?2)"),
            params![item, tag],
        )?;
    }
    Ok(())
}
