use jiff::civil::Date;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::schema::{
    MIGRATION_V10, MIGRATION_V11, MIGRATION_V12, MIGRATION_V13, MIGRATION_V14, MIGRATION_V15,
    MIGRATION_V16, MIGRATION_V17, MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5,
    MIGRATION_V6, MIGRATION_V7, MIGRATION_V8, MIGRATION_V9, SCHEMA_V1,
};
use super::trash;
use crate::domain::{
    Area, ChecklistItem, ChecklistProgress, DeletedHeading, Heading, Home, Outcome, Project,
    Schedule, Status, Task,
};
use crate::error::{MattersError, Result};

/// Columns read by [`Database::map_task_row`], in its expected order.
pub(crate) const TASK_COLUMNS: &str =
    "id, area_id, project_id, heading_id, title, notes, start_date,
     time_of_day, is_someday, deadline, status, completed_at, sort_order,
     created_at, updated_at, trashed_at, today_order, reminder_time,
     repeat_rule, repeat_series, repeat_date, goal_id";

/// Columns read by [`Database::map_area_row`], in its expected order.
pub(crate) const AREA_COLUMNS: &str = "id, title, sort_order, created_at, updated_at, trashed_at";

/// Columns read by [`Database::map_project_row`], in its expected order.
/// The newest schema version this build migrates to.
pub const LATEST_SCHEMA: i64 = 17;

pub(crate) const PROJECT_COLUMNS: &str =
    "id, area_id, title, notes, start_date, time_of_day, is_someday,
     deadline, status, completed_at, sort_order, created_at, updated_at, trashed_at, goal_id";

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    /// The sync engine's document while sync is on (`super::sync`). Locked
    /// after `conn`, never before it.
    engine: Arc<Mutex<Option<crate::crdt::Document>>>,
    /// This open's sealing writer and collection membership, if any
    /// (`super::sealing`, `super::membership`). Locked after `engine`.
    sealing: Arc<Mutex<super::sealing::SyncKeys>>,
}

impl Database {
    /// The per-user database location: `<data dir>/matters/matters.db`.
    ///
    /// The data dir follows the platform convention (`dirs::data_dir`); on
    /// Linux that is `$XDG_DATA_HOME` when it is an absolute path, otherwise
    /// `$HOME/.local/share`, as the XDG Base Directory spec requires.
    pub fn default_path() -> Result<PathBuf> {
        dirs::data_dir()
            .map(|dir| dir.join("matters").join("matters.db"))
            .ok_or(MattersError::NoDataDir)
    }

    /// Open (creating if needed) the per-user database at [`Self::default_path`].
    ///
    /// Any failure is returned to the caller; there is deliberately no silent
    /// fallback to an in-memory store, which would discard the user's work.
    pub fn open_default() -> Result<Self> {
        Self::open(Self::default_path()?)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::with_connection(conn)
    }

    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if mode != "wal" {
            return Err(MattersError::Validation(format!(
                "file-backed storage requires WAL journal mode; SQLite selected {mode}"
            )));
        }
        Self::with_connection(conn)
    }

    fn with_connection(conn: Connection) -> Result<Self> {
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            engine: Arc::new(Mutex::new(None)),
            sealing: Arc::new(Mutex::new(Default::default())),
        };
        db.init_schema()?;
        let engine = super::sync::load_engine(&db.conn.lock().unwrap())?;
        *db.engine.lock().unwrap() = engine;
        db.sealing.lock().unwrap().required =
            super::membership::joined_and_active(&db.conn.lock().unwrap())?;
        Ok(db)
    }

    /// The connection and the engine, locked in that order.
    pub(crate) fn with_engine<T>(
        &self,
        f: impl FnOnce(&mut Connection, &mut Option<crate::crdt::Document>) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn.lock().unwrap();
        let mut engine = self.engine.lock().unwrap();
        f(&mut conn, &mut engine)
    }

    /// The connection, the engine and the sealing and membership state,
    /// locked in that order.
    pub(crate) fn with_sealing<T>(
        &self,
        f: impl FnOnce(
            &mut Connection,
            &mut Option<crate::crdt::Document>,
            &mut super::sealing::SyncKeys,
        ) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self.conn.lock().unwrap();
        let mut engine = self.engine.lock().unwrap();
        let mut sealing = self.sealing.lock().unwrap();
        f(&mut conn, &mut engine, &mut sealing)
    }

    fn init_schema(&self) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_V1)?;
        let now = crate::clock::now_millis();
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
            params![now],
        )?;

        let version: i64 =
            conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })?;
        // A newer Matters wrote this database: refuse it, rather than write
        // around tables (sync's among them) this version does not know.
        if version > LATEST_SCHEMA {
            return Err(MattersError::NewerSchema {
                found: version,
                supported: LATEST_SCHEMA,
            });
        }
        for (target, migration) in [
            (2, MIGRATION_V2),
            (3, MIGRATION_V3),
            (4, MIGRATION_V4),
            (5, MIGRATION_V5),
            (6, MIGRATION_V6),
            (7, MIGRATION_V7),
            (8, MIGRATION_V8),
            (9, MIGRATION_V9),
            (10, MIGRATION_V10),
            (11, MIGRATION_V11),
            (12, MIGRATION_V12),
            (13, MIGRATION_V13),
            (14, MIGRATION_V14),
            (15, MIGRATION_V15),
            (16, MIGRATION_V16),
            (17, MIGRATION_V17),
        ] {
            if version < target {
                let tx = conn.transaction()?;
                tx.execute_batch(migration)?;
                tx.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![target, now],
                )?;
                tx.commit()?;
            }
        }
        Ok(())
    }

    /// Read through the connection. SQLite refuses any write for the
    /// duration (`PRAGMA query_only`), so this can't bypass the typed
    /// operations.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().unwrap();
        conn.pragma_update(None, "query_only", true)?;
        let result = f(&conn);
        conn.pragma_update(None, "query_only", false)?;
        result
    }

    /// Raw read-write access, for tests that stage states the typed
    /// operations never produce. Only with the `test-support` feature.
    #[cfg(feature = "test-support")]
    pub fn with_raw_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T>,
    {
        let mut conn = self.conn.lock().unwrap();
        f(&mut conn)
    }

    /// Run one `UPDATE … WHERE id = ?` that sets `assignments` (with
    /// `updated_at`), or `NotFound` when there is no such row. `values`
    /// fill the assignments' placeholders, `?1` onwards; the id and the
    /// time take the next two. With sync on, `commands` gives the engine
    /// commands for the change, from the updated row and the time.
    fn update_fields(
        &self,
        table: &str,
        entity: &'static str,
        id: &str,
        assignments: &str,
        values: &[&dyn rusqlite::ToSql],
        commands: impl FnOnce(&rusqlite::Transaction<'_>, i64) -> Result<Vec<crate::crdt::Command>>,
    ) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |conn| {
                let n = values.len();
                let sql = format!(
                    "UPDATE {table} SET {assignments}, updated_at = ?{} WHERE id = ?{}",
                    n + 2,
                    n + 1
                );
                let mut all: Vec<&dyn rusqlite::ToSql> = values.to_vec();
                all.push(&id);
                all.push(&now);
                if conn.execute(&sql, all.as_slice())? == 0 {
                    return Err(MattersError::NotFound {
                        entity,
                        id: id.to_string(),
                    });
                }
                Ok(())
            },
            |tx, ()| commands(tx, now),
        )
    }

    /// Run one `UPDATE` that may match no row (an already trashed item,
    /// say). With sync on, `commands` describes the change only when a row
    /// changed.
    fn update_if(
        &self,
        sql: &str,
        args: &[&dyn rusqlite::ToSql],
        at: i64,
        commands: impl FnOnce() -> Vec<crate::crdt::Command>,
    ) -> Result<()> {
        self.write(
            at,
            |conn| Ok(conn.execute(sql, args)? > 0),
            |_, changed| Ok(if *changed { commands() } else { Vec::new() }),
        )
        .map(|_| ())
    }

    // =========================================================================
    // Area Operations
    // =========================================================================

    /// Create an area at the end of the sidebar list.
    pub fn add_area(&self, area: &mut Area) -> Result<()> {
        area.sort_order = self.next_order("areas")?;
        self.create_area(area)
    }

    /// One past the largest `sort_order` in `table` (a fixed table name).
    fn next_order(&self, table: &str) -> Result<f64> {
        self.with_conn(|conn| {
            Ok(conn.query_row(
                &format!("SELECT COALESCE(MAX(sort_order), 0) + 1 FROM {table}"),
                [],
                |r| r.get(0),
            )?)
        })
    }

    pub fn create_area(&self, area: &Area) -> Result<()> {
        self.write(
            area.updated_at,
            |conn| {
                conn.execute(
                    "INSERT INTO areas (id, title, sort_order, created_at, updated_at, trashed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        area.id,
                        area.title,
                        area.sort_order,
                        area.created_at,
                        area.updated_at,
                        area.trashed_at
                    ],
                )?;
                Ok(())
            },
            |tx, ()| super::sync::create_area(tx, area),
        )
    }

    pub fn get_area(&self, id: &str) -> Result<Option<Area>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {AREA_COLUMNS} FROM areas WHERE id = ?1"))?;
        let area = stmt.query_row(params![id], Self::map_area_row).optional()?;
        Ok(area)
    }

    /// Areas not in the Trash, in sidebar order.
    pub fn list_areas(&self) -> Result<Vec<Area>> {
        self.select_areas(&format!(
            "{} ORDER BY sort_order ASC, created_at ASC, id ASC",
            trash::LIVE_AREA
        ))
    }

    /// Trashed areas, most recently trashed first.
    pub fn list_trashed_areas(&self) -> Result<Vec<Area>> {
        self.select_areas(&format!(
            "{} ORDER BY trashed_at DESC, id ASC",
            trash::TRASHED_AREA
        ))
    }

    fn select_areas(&self, filter_and_order: &str) -> Result<Vec<Area>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {AREA_COLUMNS} FROM areas WHERE {filter_and_order}"
        ))?;
        let rows = stmt.query_map([], Self::map_area_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Rename an area. The name is cleaned like a task title
    /// ([`Task::clean_title`]); a blank name is refused.
    pub fn rename_area(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("an area needs a name".into()));
        };
        self.update_fields("areas", "area", id, "title = ?1", &[&title], |_, _| {
            Ok(vec![crate::crdt::Command::Rename {
                id: id.into(),
                title: title.clone(),
            }])
        })
    }

    /// Rename a project. The name is cleaned like a task title
    /// ([`Task::clean_title`]); a blank name is refused.
    pub fn rename_project(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("a project needs a name".into()));
        };
        self.update_fields(
            "projects",
            "project",
            id,
            "title = ?1",
            &[&title],
            |_, _| {
                Ok(vec![crate::crdt::Command::Rename {
                    id: id.into(),
                    title: title.clone(),
                }])
            },
        )
    }

    /// File a project in an area, or take it out of one (`None`), at the
    /// end of the area's projects. Neither the project nor the area may be
    /// in the Trash, the project itself or through its area, so a project
    /// in the Trash is put back only with Put Back. This is the only way a
    /// project changes area; filing it where it already is changes nothing.
    pub fn set_project_area(&self, project_id: &str, area_id: Option<&str>) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                if let Some(area_id) = area_id {
                    Self::check_live(tx, "areas", trash::LIVE_AREA, "area", area_id)?;
                }
                let current: Option<Option<String>> = tx
                    .query_row(
                        &format!(
                            "SELECT area_id FROM projects WHERE id = ?1 AND {}",
                            trash::LIVE_PROJECT
                        ),
                        params![project_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(current) = current else {
                    return Err(MattersError::NotFound {
                        entity: "project",
                        id: project_id.to_string(),
                    });
                };
                if current.as_deref() == area_id {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE projects SET area_id = ?1, updated_at = ?2,
                        sort_order = (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM projects)
                     WHERE id = ?3",
                    params![area_id, now, project_id],
                )?;
                Ok(true)
            },
            |_, moved| {
                Ok(if *moved {
                    vec![crate::crdt::Command::Move {
                        id: project_id.into(),
                        parent: area_id.map(str::to_string),
                        at: crate::crdt::Anchor::Last,
                    }]
                } else {
                    Vec::new()
                })
            },
        )
        .map(|_| ())
    }

    /// Move an area to the Trash, taking its projects and tasks with it: they
    /// leave every view, and the Trash lists the area in their place. Its
    /// contents keep their own state, so Put Back restores them as they were.
    pub fn trash_area(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE areas SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetTrashed {
                    id: id.into(),
                    trashed_at: Some(now),
                }]
            },
        )
    }

    /// Put a trashed area back with its projects and tasks. Anything trashed
    /// on its own before the area stays in the Trash.
    pub fn restore_area(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE areas SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetTrashed {
                    id: id.into(),
                    trashed_at: None,
                }]
            },
        )
    }

    // =========================================================================
    // Project Operations
    // =========================================================================

    /// Create a project at the end of the sidebar list.
    pub fn add_project(&self, project: &mut Project) -> Result<()> {
        project.sort_order = self.next_order("projects")?;
        self.create_project(project)
    }

    pub fn create_project(&self, project: &Project) -> Result<()> {
        super::goals::check_goal(project.goal_id.as_deref())?;
        self.write(
            project.updated_at,
            |conn| {
                let start_date = project.start_date.map(|d| d.to_string());
                let deadline = project.deadline.map(|d| d.to_string());
                let is_someday = if project.is_someday { 1 } else { 0 };

                conn.execute(
                    "INSERT INTO projects (
                id, area_id, title, notes, start_date, time_of_day, is_someday,
                deadline, status, completed_at, sort_order, created_at, updated_at, trashed_at,
                goal_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        project.id,
                        project.area_id,
                        project.title,
                        project.notes,
                        start_date,
                        project.time_of_day,
                        is_someday,
                        deadline,
                        project.status.as_str(),
                        project.completed_at,
                        project.sort_order,
                        project.created_at,
                        project.updated_at,
                        project.trashed_at,
                        project.goal_id,
                    ],
                )?;
                Ok(())
            },
            |tx, ()| super::sync::create_project(tx, project),
        )
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"
        ))?;
        let project = stmt
            .query_row(params![id], Self::map_project_row)
            .optional()?;
        Ok(project)
    }

    /// The open projects not in the Trash (themselves or through their
    /// area), in sidebar order. Finished projects are listed by
    /// [`Self::list_finished_projects`].
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        self.select_projects(&format!(
            "{} AND status = 'open' ORDER BY sort_order ASC, created_at ASC, id ASC",
            trash::LIVE_PROJECT
        ))
    }

    /// Completed and canceled projects not in the Trash, most recently
    /// finished first, as the Logbook lists them.
    pub fn list_finished_projects(&self) -> Result<Vec<Project>> {
        self.select_projects(&format!(
            "{} AND status IN ('completed', 'canceled')
             ORDER BY completed_at DESC, updated_at DESC, id ASC",
            trash::LIVE_PROJECT
        ))
    }

    /// Complete or cancel a project (`outcome`), closing its open tasks as
    /// `open_tasks` says: the caller must choose, so no task is closed
    /// without being asked about (#55). The tasks this closes are marked,
    /// and [`Self::reopen_project`] reopens exactly those; tasks finished
    /// before keep their state. Tasks in the Trash are left as they are.
    ///
    /// The project must be open and not in the Trash, itself or through its
    /// area.
    pub fn finish_project(&self, id: &str, outcome: Outcome, open_tasks: Outcome) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                if Self::live_project_status(tx, id)? != Status::Open {
                    return Err(MattersError::Validation(format!(
                        "project '{id}' is already finished"
                    )));
                }
                let closing = super::sync::task_ids(
                    tx,
                    "SELECT id FROM tasks WHERE project_id = ?1 AND status = 'open' AND trashed_at IS NULL",
                    id,
                )?;
                tx.execute(
                    "UPDATE projects SET status = ?1, completed_at = ?2, updated_at = ?2 WHERE id = ?3",
                    params![Status::from(outcome).as_str(), now, id],
                )?;
                tx.execute(
                    "UPDATE tasks SET status = ?1, completed_at = ?2, updated_at = ?2, closed_by_project = 1
                     WHERE project_id = ?3 AND status = 'open' AND trashed_at IS NULL",
                    params![Status::from(open_tasks).as_str(), now, id],
                )?;
                Ok(closing)
            },
            |tx, closing| super::sync::project_and_tasks_status(tx, id, closing),
        )
        .map(|_| ())
    }

    /// Undo [`Self::finish_project`]: the project is open again, and so are
    /// the tasks finishing it closed, unless they were reopened or finished
    /// by hand since. The project must be finished and not in the Trash.
    pub fn reopen_project(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                if Self::live_project_status(tx, id)? == Status::Open {
                    return Err(MattersError::Validation(format!(
                        "project '{id}' is not finished"
                    )));
                }
                let reopening = super::sync::task_ids(
                    tx,
                    "SELECT id FROM tasks WHERE project_id = ?1 AND closed_by_project = 1",
                    id,
                )?;
                tx.execute(
                    "UPDATE projects SET status = 'open', completed_at = NULL, updated_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
                tx.execute(
                    "UPDATE tasks SET status = 'open', completed_at = NULL, updated_at = ?1, closed_by_project = 0
                     WHERE project_id = ?2 AND closed_by_project = 1",
                    params![now, id],
                )?;
                Ok(reopening)
            },
            |tx, reopening| super::sync::project_and_tasks_status(tx, id, reopening),
        )
        .map(|_| ())
    }

    /// The status of project `id`; `NotFound` when it doesn't exist or is in
    /// the Trash, itself or through its area.
    fn live_project_status(conn: &Connection, id: &str) -> Result<Status> {
        let status: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT status FROM projects WHERE id = ?1 AND {}",
                    trash::LIVE_PROJECT
                ),
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        status
            .map(|s| Status::from_str_loose(&s))
            .ok_or_else(|| MattersError::NotFound {
                entity: "project",
                id: id.to_string(),
            })
    }

    /// Projects the Trash lists, most recently trashed first. A project in a
    /// trashed area is represented by the area (`list_trashed_areas`).
    pub fn list_trashed_projects(&self) -> Result<Vec<Project>> {
        self.select_projects(&format!(
            "{} ORDER BY trashed_at DESC, id ASC",
            trash::LISTED_TRASHED_PROJECT
        ))
    }

    fn select_projects(&self, filter_and_order: &str) -> Result<Vec<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects WHERE {filter_and_order}"
        ))?;
        let rows = stmt.query_map([], Self::map_project_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace a project's notes.
    pub fn set_project_notes(&self, id: &str, notes: &str) -> Result<()> {
        self.update_fields(
            "projects",
            "project",
            id,
            "notes = ?1",
            &[&notes],
            |_, _| {
                Ok(vec![crate::crdt::Command::SetNotes {
                    id: id.into(),
                    notes: notes.into(),
                }])
            },
        )
    }

    /// Set a project's When ([`Project::apply_schedule`]): start date,
    /// evening and Someday change together.
    pub fn set_project_schedule(&self, id: &str, schedule: &Schedule, today: Date) -> Result<()> {
        let (start_date, time_of_day, is_someday) = schedule.to_db_fields(today);
        self.update_fields(
            "projects",
            "project",
            id,
            "start_date = ?1, time_of_day = ?2, is_someday = ?3",
            &[
                &start_date.map(|d| d.to_string()),
                &time_of_day,
                &is_someday,
            ],
            |_, _| {
                Ok(vec![crate::crdt::Command::SetSchedule {
                    id: id.into(),
                    start_date,
                    evening: time_of_day.is_some(),
                    someday: is_someday,
                }])
            },
        )
    }

    /// Set or clear a project's deadline.
    pub fn set_project_deadline(&self, id: &str, deadline: Option<Date>) -> Result<()> {
        self.update_fields(
            "projects",
            "project",
            id,
            "deadline = ?1",
            &[&deadline.map(|d| d.to_string())],
            |_, _| {
                Ok(vec![crate::crdt::Command::SetDeadline {
                    id: id.into(),
                    deadline,
                }])
            },
        )
    }

    /// Move a project to the Trash, taking its tasks with it: they leave
    /// every view, and the Trash lists the project in their place. Tasks keep
    /// their own state, so Put Back restores the project as it was.
    pub fn trash_project(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE projects SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetTrashed {
                    id: id.into(),
                    trashed_at: Some(now),
                }]
            },
        )
    }

    /// Put a trashed project back with its tasks. A task that was trashed on
    /// its own before the project stays in the Trash.
    pub fn restore_project(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE projects SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetTrashed {
                    id: id.into(),
                    trashed_at: None,
                }]
            },
        )
    }

    // =========================================================================
    // Heading Operations
    // =========================================================================

    pub fn create_heading(&self, heading: &Heading) -> Result<()> {
        self.write(
            heading.updated_at,
            |conn| {
                conn.execute(
            "INSERT INTO headings (id, project_id, title, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                heading.id,
                heading.project_id,
                heading.title,
                heading.sort_order,
                heading.created_at,
                heading.updated_at,
            ],
        )?;
                Ok(())
            },
            |tx, ()| super::sync::create_heading(tx, heading),
        )
    }

    /// Add `heading` at the end of its project's headings. Its title is
    /// cleaned like a task's ([`Task::clean_title`]); a blank one, or a
    /// project that doesn't exist, is refused and nothing is added.
    pub fn add_heading(&self, heading: &mut Heading) -> Result<()> {
        let Some(title) = Task::clean_title(&heading.title) else {
            return Err(MattersError::Validation("a heading needs a title".into()));
        };
        heading.title = title;
        heading.sort_order = self.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM headings WHERE project_id = ?1",
                params![heading.project_id],
                |r| r.get(0),
            )?)
        })?;
        self.create_heading(heading)
    }

    pub fn get_heading(&self, id: &str) -> Result<Option<Heading>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id, project_id, title, sort_order, created_at, updated_at
                 FROM headings WHERE id = ?1",
                params![id],
                Self::map_heading_row,
            )
            .optional()?)
    }

    /// Rename a heading; the title is cleaned and a blank one refused, as
    /// for [`Self::add_heading`].
    pub fn rename_heading(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("a heading needs a title".into()));
        };
        self.update_fields(
            "headings",
            "heading",
            id,
            "title = ?1",
            &[&title],
            |_, _| {
                Ok(vec![crate::crdt::Command::Rename {
                    id: id.into(),
                    title: title.clone(),
                }])
            },
        )
    }

    /// A project's headings in their order. Tied keys (headings never
    /// arranged) fall back to the order they were added.
    pub fn list_headings_for_project(&self, project_id: &str) -> Result<Vec<Heading>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, sort_order, created_at, updated_at
             FROM headings WHERE project_id = ?1
             ORDER BY sort_order ASC, created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![project_id], Self::map_heading_row)?;
        let mut headings = Vec::new();
        for row in rows {
            headings.push(row?);
        }
        Ok(headings)
    }

    /// Delete a heading. Its tasks stay in the project, in the part above
    /// the headings (decided on #22). Returns what [`Self::restore_heading`]
    /// needs to undo it.
    pub fn delete_heading(&self, id: &str) -> Result<DeletedHeading> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let heading = tx
                    .query_row(
                        "SELECT id, project_id, title, sort_order, created_at, updated_at
                         FROM headings WHERE id = ?1",
                        params![id],
                        Self::map_heading_row,
                    )
                    .optional()?
                    .ok_or_else(|| MattersError::NotFound {
                        entity: "heading",
                        id: id.to_string(),
                    })?;
                let task_ids = super::sync::task_ids(
                    tx,
                    "SELECT id FROM tasks WHERE heading_id = ?1 ORDER BY id",
                    id,
                )?;
                // Tasks lose the heading through `ON DELETE SET NULL`;
                // clearing it here too keeps that true without the pragma.
                tx.execute(
                    "UPDATE tasks SET heading_id = NULL, updated_at = ?2 WHERE heading_id = ?1",
                    params![id, now],
                )?;
                tx.execute("DELETE FROM headings WHERE id = ?1", params![id])?;
                Ok(DeletedHeading { heading, task_ids })
            },
            |tx, deleted| {
                let mut commands = vec![crate::crdt::Command::SetRemoved {
                    id: id.into(),
                    removed: true,
                }];
                commands.extend(super::sync::refile_in_order(tx, &deleted.task_ids)?);
                Ok(commands)
            },
        )
    }

    /// Undo [`Self::delete_heading`]: the heading comes back, with the tasks
    /// that were under it and are still in its project without a heading.
    pub fn restore_heading(&self, deleted: &DeletedHeading) -> Result<()> {
        let now = crate::clock::now_millis();
        let h = &Heading {
            updated_at: now,
            ..deleted.heading.clone()
        };
        self.write(
            now,
            |tx| {
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
                let mut stmt = tx.prepare(
                    "UPDATE tasks SET heading_id = ?1, updated_at = ?4
                     WHERE id = ?2 AND project_id = ?3 AND heading_id IS NULL",
                )?;
                let mut back = Vec::new();
                for task in &deleted.task_ids {
                    if stmt.execute(params![h.id, task, h.project_id, now])? > 0 {
                        back.push(task.clone());
                    }
                }
                Ok(back)
            },
            |tx, back| {
                let mut commands = super::sync::create_heading(tx, h)?;
                commands.extend(super::sync::refile_in_order(tx, back)?);
                Ok(commands)
            },
        )
        .map(|_| ())
    }

    // =========================================================================
    // Task Operations
    // =========================================================================

    /// Create a task at the end of its lists: its home list (Inbox, area or
    /// project) and Today. Frontends capture tasks with this; `create_task`
    /// keeps the order it is given.
    pub fn add_task(&self, task: &mut Task) -> Result<()> {
        let (home, today) = self.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COALESCE(MAX(sort_order), 0) + 1, COALESCE(MAX(today_order), 0) + 1 FROM tasks",
                [],
                |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)),
            )?)
        })?;
        task.sort_order = home;
        task.today_order = today;
        self.create_task(task)
    }

    /// Store a new task as given, order keys included. Its home must be
    /// valid: at most one of an area or a project, and a heading only of
    /// its own project ([`Home::of`]); nothing is stored otherwise.
    pub fn create_task(&self, task: &Task) -> Result<()> {
        super::goals::check_goal(task.goal_id.as_deref())?;
        self.write(
            task.updated_at,
            |conn| {
                Self::check_home(conn, task)?;
                let start_date = task.start_date.map(|d| d.to_string());
                let deadline = task.deadline.map(|d| d.to_string());
                let is_someday = if task.is_someday { 1 } else { 0 };

                conn.execute(
                    "INSERT INTO tasks (
                id, area_id, project_id, heading_id, title, notes, start_date,
                time_of_day, is_someday, deadline, status, completed_at, sort_order,
                created_at, updated_at, trashed_at, today_order, reminder_time,
                repeat_rule, repeat_series, repeat_date, goal_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21, ?22)",
                    params![
                        task.id,
                        task.area_id,
                        task.project_id,
                        task.heading_id,
                        task.title,
                        task.notes,
                        start_date,
                        task.time_of_day,
                        is_someday,
                        deadline,
                        task.status.as_str(),
                        task.completed_at,
                        task.sort_order,
                        task.created_at,
                        task.updated_at,
                        task.trashed_at,
                        task.today_order,
                        task.reminder.map(crate::reminders::stored_time),
                        crate::repeat::stored_rule(task.repeat.as_ref()),
                        task.series,
                        task.occurrence.map(|d| d.to_string()),
                        task.goal_id,
                    ],
                )?;
                Ok(())
            },
            |tx, ()| super::sync::create_task(tx, task),
        )
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"))?;
        let task = stmt.query_row(params![id], Self::map_task_row).optional()?;
        Ok(task)
    }

    /// Replace a task's notes.
    ///
    /// Tasks change field by field, never by saving a whole copy: a stale
    /// copy can't undo another edit, and each change is one intent a sync
    /// layer can record. Order, trash state and home change only through
    /// the ordering operations (`crate::ordering`), [`Self::trash_task`] /
    /// [`Self::restore_task`] and [`Self::set_task_home`].
    pub fn set_task_notes(&self, id: &str, notes: &str) -> Result<()> {
        self.update_fields("tasks", "task", id, "notes = ?1", &[&notes], |_, _| {
            Ok(vec![crate::crdt::Command::SetNotes {
                id: id.into(),
                notes: notes.into(),
            }])
        })
    }

    /// Set a task's When ([`Task::apply_schedule`]): start date, evening
    /// and Someday change together.
    pub fn set_task_schedule(&self, id: &str, schedule: &Schedule, today: Date) -> Result<()> {
        let (start_date, time_of_day, is_someday) = schedule.to_db_fields(today);
        self.update_fields(
            "tasks",
            "task",
            id,
            "start_date = ?1, time_of_day = ?2, is_someday = ?3",
            &[
                &start_date.map(|d| d.to_string()),
                &time_of_day,
                &is_someday,
            ],
            |tx, _| super::sync::rescheduled(tx, id),
        )
    }

    /// Move a Today task between the daytime section and This Evening,
    /// keeping its date (an overdue task stays overdue). A task with no
    /// date that goes to This Evening is scheduled for `today`.
    pub(crate) fn set_task_evening(&self, id: &str, evening: bool, today: Date) -> Result<()> {
        self.update_fields(
            "tasks",
            "task",
            id,
            "time_of_day = CASE WHEN ?1 THEN 'evening' END,
             start_date = CASE WHEN ?1 THEN COALESCE(start_date, ?2) ELSE start_date END",
            &[&evening, &today.to_string()],
            |tx, _| super::sync::rescheduled(tx, id),
        )
    }

    /// Set or clear a task's reminder (#60): the time of day it is due on
    /// the task's start date. Seconds are dropped.
    pub fn set_task_reminder(&self, id: &str, time: Option<jiff::civil::Time>) -> Result<()> {
        let time = time.map(crate::reminders::to_minute);
        self.update_fields(
            "tasks",
            "task",
            id,
            "reminder_time = ?1",
            &[&time.map(crate::reminders::stored_time)],
            |_, _| {
                Ok(vec![crate::crdt::Command::SetReminder {
                    id: id.into(),
                    time,
                }])
            },
        )
    }

    /// Remind about task `id` at `time` (to the minute) on its start date.
    /// A reminder needs a date: a task with none, or deferred to Someday, is
    /// scheduled for `today` in the same change.
    pub fn remind_task_at(&self, id: &str, time: jiff::civil::Time, today: Date) -> Result<()> {
        let time = crate::reminders::to_minute(time);
        self.update_fields(
            "tasks",
            "task",
            id,
            "reminder_time = ?1,
             time_of_day = CASE WHEN start_date IS NULL OR is_someday = 1 THEN NULL
                                ELSE time_of_day END,
             start_date = CASE WHEN start_date IS NULL OR is_someday = 1 THEN ?2
                               ELSE start_date END,
             is_someday = 0",
            &[&crate::reminders::stored_time(time), &today.to_string()],
            |tx, _| {
                let mut commands = super::sync::rescheduled(tx, id)?;
                commands.push(crate::crdt::Command::SetReminder {
                    id: id.into(),
                    time: Some(time),
                });
                Ok(commands)
            },
        )
    }

    /// Set or clear a task's deadline.
    pub fn set_task_deadline(&self, id: &str, deadline: Option<Date>) -> Result<()> {
        self.update_fields(
            "tasks",
            "task",
            id,
            "deadline = ?1",
            &[&deadline.map(|d| d.to_string())],
            |_, _| {
                Ok(vec![crate::crdt::Command::SetDeadline {
                    id: id.into(),
                    deadline,
                }])
            },
        )
    }

    /// Rename a task. The title is cleaned ([`Task::clean_title`]); a blank
    /// title is refused and the task keeps its name.
    pub fn rename_task(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("a task needs a title".into()));
        };
        self.update_fields("tasks", "task", id, "title = ?1", &[&title], |_, _| {
            Ok(vec![crate::crdt::Command::Rename {
                id: id.into(),
                title: title.clone(),
            }])
        })
    }

    /// File a task in `home` (the Move popover, SPEC §4), at the end of
    /// that list: under a heading, at the end of the heading's tasks. The
    /// task keeps everything else: its schedule, deadline, notes and
    /// checklist. So a scheduled task sent to the Inbox shows where its
    /// schedule puts it, as the Inbox lists only unscheduled tasks.
    ///
    /// The task must not be in the Trash, and neither may the destination,
    /// itself or through its area; a heading must belong to the project
    /// named with it. Moving a task to where it already is changes nothing,
    /// not even its place in the list.
    ///
    /// This is the only way a task changes home. (`crate::ordering::move_task`
    /// moves a task up or down within a list.)
    pub fn set_task_home(&self, id: &str, home: &Home) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let mut task = tx
                    .query_row(
                        &format!(
                            "SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1 AND {}",
                            trash::LIVE_TASK
                        ),
                        params![id],
                        Self::map_task_row,
                    )
                    .optional()?
                    .ok_or_else(|| MattersError::NotFound {
                        entity: "task",
                        id: id.to_string(),
                    })?;
                match home {
                    Home::Inbox => {}
                    Home::Area(area) => {
                        Self::check_live(tx, "areas", trash::LIVE_AREA, "area", area)?;
                    }
                    Home::Project { project, heading } => {
                        Self::check_live(tx, "projects", trash::LIVE_PROJECT, "project", project)?;
                        if let Some(heading) = heading {
                            Self::check_heading(tx, project, heading)?;
                        }
                    }
                }
                if Home::of(&task).ok().as_ref() == Some(home) {
                    return Ok(None);
                }
                home.apply(&mut task);
                tx.execute(
                    "UPDATE tasks SET area_id = ?1, project_id = ?2, heading_id = ?3, updated_at = ?4,
                        sort_order = (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM tasks)
                     WHERE id = ?5",
                    params![task.area_id, task.project_id, task.heading_id, now, id],
                )?;
                Ok(Some(task))
            },
            |_, moved| {
                Ok(moved
                    .iter()
                    .map(|task| crate::crdt::Command::Move {
                        id: task.id.clone(),
                        parent: super::sync::task_parent(task),
                        at: crate::crdt::Anchor::Last,
                    })
                    .collect())
            },
        )
        .map(|_| ())
    }

    /// Refuse a new task whose ids name no valid home: both an area and a
    /// project, a heading without a project ([`Home::of`]), or a heading of
    /// another project. Only [`Self::create_task`] and
    /// [`Self::set_task_home`] write a task's home, and both check it, so no
    /// path can store a broken hierarchy.
    fn check_home(conn: &Connection, task: &Task) -> Result<()> {
        if let Home::Project {
            project,
            heading: Some(heading),
        } = Home::of(task)?
        {
            Self::check_heading(conn, &project, &heading)?;
        }
        Ok(())
    }

    /// `NotFound` unless heading `heading` exists, `Validation` unless it
    /// belongs to `project`.
    fn check_heading(conn: &Connection, project: &str, heading: &str) -> Result<()> {
        let owner: Option<String> = conn
            .query_row(
                "SELECT project_id FROM headings WHERE id = ?1",
                params![heading],
                |r| r.get(0),
            )
            .optional()?;
        match owner {
            None => Err(MattersError::NotFound {
                entity: "heading",
                id: heading.to_string(),
            }),
            Some(owner) if owner != project => Err(MattersError::Validation(format!(
                "heading '{heading}' is not in project '{project}'"
            ))),
            Some(_) => Ok(()),
        }
    }

    /// `NotFound` unless `table` has a row `id` meeting `live`, one of the
    /// [`trash`] conditions.
    fn check_live(
        conn: &Connection,
        table: &str,
        live: &str,
        entity: &'static str,
        id: &str,
    ) -> Result<()> {
        let found = conn
            .query_row(
                &format!("SELECT 1 FROM {table} WHERE id = ?1 AND {live}"),
                params![id],
                |_| Ok(()),
            )
            .optional()?;
        found.ok_or_else(|| MattersError::NotFound {
            entity,
            id: id.to_string(),
        })
    }

    /// Complete task `id` today; a task repeating after completion gets
    /// its next occurrence (`Database::complete_task_on`, #61).
    pub fn complete_task(&self, id: &str) -> Result<()> {
        self.complete_task_on(id, crate::clock::local_today())
    }

    /// Cancel a task: it is finished without being done, and goes to the
    /// Logbook like a completed one.
    pub fn cancel_task(&self, id: &str) -> Result<()> {
        self.update_fields(
            "tasks",
            "task",
            id,
            "status = 'canceled', completed_at = ?1, closed_by_project = 0",
            &[&crate::clock::now_millis()],
            |tx, _| super::sync::task_status(tx, id),
        )
    }

    pub fn uncomplete_task(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE tasks SET status = 'open', completed_at = NULL, updated_at = ?1,
                closed_by_project = 0 WHERE id = ?2",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetStatus {
                    id: id.into(),
                    status: Status::Open,
                    completed_at: None,
                    closed_by_project: false,
                }]
            },
        )
    }

    /// Move a task to the Trash. It keeps its status and place, and leaves
    /// every view except the Trash until it is put back. Trashing a task that
    /// is already in the Trash leaves its original trash time.
    pub fn trash_task(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE tasks SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetTrashed {
                    id: id.into(),
                    trashed_at: Some(now),
                }]
            },
        )
    }

    /// Put a trashed task back where it was.
    pub fn restore_task(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.update_if(
            "UPDATE tasks SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            &[&now, &id],
            now,
            || {
                vec![crate::crdt::Command::SetTrashed {
                    id: id.into(),
                    trashed_at: None,
                }]
            },
        )
    }

    /// Permanently delete everything in the Trash: trashed tasks, trashed
    /// projects and trashed areas, with everything inside them (headings,
    /// tasks, and the tasks' checklist items and tags). This is the only way
    /// a task, project or area is destroyed. Returns how many tasks were
    /// deleted.
    pub fn empty_trash(&self) -> Result<usize> {
        self.write(
            crate::clock::now_millis(),
            |tx| {
                // Everything this deletes, cascades included, for sync: the
                // exact set this device saw (ADR 0001, Empty Trash).
                let doomed = super::sync::trashed_entities(tx)?;
                // Innermost first: each pass reads the trash state of the
                // containers that later passes delete.
                let tasks = tx.execute(
                    &format!("DELETE FROM tasks WHERE {}", trash::ANY_TRASHED_TASK),
                    [],
                )?;
                tx.execute(
                    &format!("DELETE FROM projects WHERE {}", trash::ANY_TRASHED_PROJECT),
                    [],
                )?;
                tx.execute(
                    &format!("DELETE FROM areas WHERE {}", trash::TRASHED_AREA),
                    [],
                )?;
                Ok((tasks, doomed))
            },
            |_, (_, doomed)| {
                Ok(if doomed.is_empty() {
                    Vec::new()
                } else {
                    vec![crate::crdt::Command::Purge {
                        ids: doomed.clone(),
                    }]
                })
            },
        )
        .map(|(tasks, _)| tasks)
    }

    // =========================================================================
    // Checklist Operations
    // =========================================================================

    pub fn create_checklist_item(&self, item: &ChecklistItem) -> Result<()> {
        self.write(
            item.updated_at,
            |conn| insert_checklist_item(conn, item),
            |tx, ()| super::sync::create_checklist_item(tx, item),
        )
    }

    /// Add `item` at the end of its task's checklist. Its title is cleaned
    /// like a task's ([`Task::clean_title`]); a blank one is refused.
    pub fn add_checklist_item(&self, item: &mut ChecklistItem) -> Result<()> {
        item.title = checklist_title(&item.title)?;
        self.write(
            item.updated_at,
            |tx| {
                item.sort_order = checklist_end_key(tx, &item.task_id)?;
                insert_checklist_item(tx, item)?;
                Ok(item.clone())
            },
            super::sync::create_checklist_item,
        )
        .map(|_| ())
    }

    pub fn get_checklist_item(&self, id: &str) -> Result<Option<ChecklistItem>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id, task_id, title, is_completed, sort_order, created_at, updated_at
                 FROM checklist_items WHERE id = ?1",
                params![id],
                Self::map_checklist_row,
            )
            .optional()?)
    }

    /// A task's checklist in its order. Tied keys (items never arranged)
    /// fall back to the order they were added.
    pub fn list_checklist_items(&self, task_id: &str) -> Result<Vec<ChecklistItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, title, is_completed, sort_order, created_at, updated_at
             FROM checklist_items WHERE task_id = ?1
             ORDER BY sort_order ASC, created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![task_id], Self::map_checklist_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Rename a checklist item; the title is cleaned and a blank one refused,
    /// as for [`Self::add_checklist_item`].
    pub fn rename_checklist_item(&self, id: &str, title: &str) -> Result<()> {
        let title = checklist_title(title)?;
        self.update_fields(
            "checklist_items",
            "checklist item",
            id,
            "title = ?1",
            &[&title],
            |_, _| {
                Ok(vec![crate::crdt::Command::Rename {
                    id: id.into(),
                    title: title.clone(),
                }])
            },
        )
    }

    /// Tick or untick a checklist item. Setting the state, rather than
    /// flipping it, means a click on a stale row can't undo what it shows.
    pub fn set_checklist_item_done(&self, id: &str, done: bool) -> Result<()> {
        self.update_fields(
            "checklist_items",
            "checklist item",
            id,
            "is_completed = ?1",
            &[&done],
            |_, _| {
                Ok(vec![crate::crdt::Command::SetDone {
                    id: id.into(),
                    done,
                }])
            },
        )
    }

    /// How many of each task's checklist items are done, for the tasks in
    /// `task_ids` that have a checklist. One indexed, grouped query per
    /// 500 tasks, not one per task.
    pub fn checklist_progress(
        &self,
        task_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, ChecklistProgress>> {
        let conn = self.conn.lock().unwrap();
        let mut progress = std::collections::HashMap::new();
        for chunk in task_ids.chunks(PROGRESS_CHUNK) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT task_id, COALESCE(SUM(is_completed), 0), COUNT(*) FROM checklist_items
                 WHERE task_id IN ({marks}) GROUP BY task_id"
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (id, done, total) = row?;
                progress.insert(
                    id,
                    ChecklistProgress {
                        done: done as usize,
                        total: total as usize,
                    },
                );
            }
        }
        Ok(progress)
    }

    /// Delete a checklist item. Creating it again with the same id
    /// ([`Self::create_checklist_item`]) is how Undo puts it back.
    pub fn delete_checklist_item(&self, id: &str) -> Result<()> {
        self.update_if(
            "DELETE FROM checklist_items WHERE id = ?1",
            &[&id],
            crate::clock::now_millis(),
            || {
                vec![crate::crdt::Command::SetRemoved {
                    id: id.into(),
                    removed: true,
                }]
            },
        )
    }

    // =========================================================================
    // Row Mappers
    // =========================================================================

    pub(crate) fn map_area_row(row: &Row) -> rusqlite::Result<Area> {
        Ok(Area {
            id: row.get(0)?,
            title: row.get(1)?,
            sort_order: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            trashed_at: row.get(5)?,
        })
    }

    pub(crate) fn map_project_row(row: &Row) -> rusqlite::Result<Project> {
        let start_date_str: Option<String> = row.get(4)?;
        let start_date = start_date_str.and_then(|s| crate::clock::parse_iso_date(&s));

        let is_someday_int: i64 = row.get(6)?;
        let is_someday = is_someday_int == 1;

        let deadline_str: Option<String> = row.get(7)?;
        let deadline = deadline_str.and_then(|s| crate::clock::parse_iso_date(&s));

        let status_str: String = row.get(8)?;
        let status = Status::from_str_loose(&status_str);

        Ok(Project {
            id: row.get(0)?,
            area_id: row.get(1)?,
            title: row.get(2)?,
            notes: row.get(3)?,
            start_date,
            time_of_day: row.get(5)?,
            is_someday,
            deadline,
            status,
            completed_at: row.get(9)?,
            sort_order: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            trashed_at: row.get(13)?,
            goal_id: row.get(14)?,
        })
    }

    pub(crate) fn map_heading_row(row: &Row) -> rusqlite::Result<Heading> {
        Ok(Heading {
            id: row.get(0)?,
            project_id: row.get(1)?,
            title: row.get(2)?,
            sort_order: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }

    pub(crate) fn map_task_row(row: &Row) -> rusqlite::Result<Task> {
        let start_date_str: Option<String> = row.get(6)?;
        let start_date = start_date_str.and_then(|s| crate::clock::parse_iso_date(&s));

        let is_someday_int: i64 = row.get(8)?;
        let is_someday = is_someday_int == 1;

        let deadline_str: Option<String> = row.get(9)?;
        let deadline = deadline_str.and_then(|s| crate::clock::parse_iso_date(&s));

        let status_str: String = row.get(10)?;
        let status = Status::from_str_loose(&status_str);

        Ok(Task {
            id: row.get(0)?,
            area_id: row.get(1)?,
            project_id: row.get(2)?,
            heading_id: row.get(3)?,
            title: row.get(4)?,
            notes: row.get(5)?,
            start_date,
            time_of_day: row.get(7)?,
            is_someday,
            deadline,
            status,
            completed_at: row.get(11)?,
            sort_order: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
            trashed_at: row.get(15)?,
            today_order: row.get(16)?,
            reminder: row
                .get::<_, Option<String>>(17)?
                .and_then(|s| crate::reminders::parse_stored_time(&s)),
            repeat: row
                .get::<_, Option<String>>(18)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            series: row.get(19)?,
            occurrence: row
                .get::<_, Option<String>>(20)?
                .and_then(|s| crate::clock::parse_iso_date(&s)),
            goal_id: row.get(21)?,
        })
    }

    pub(crate) fn map_checklist_row(row: &Row) -> rusqlite::Result<ChecklistItem> {
        let is_completed_int: i64 = row.get(3)?;
        Ok(ChecklistItem {
            id: row.get(0)?,
            task_id: row.get(1)?,
            title: row.get(2)?,
            is_completed: is_completed_int == 1,
            sort_order: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }
}

/// `Ok` when an update found checklist item `id` (`changed` rows).
/// Tasks per [`Database::checklist_progress`] query: well inside SQLite's
/// bound-parameter limit.
const PROGRESS_CHUNK: usize = 500;

/// A checklist item's title, cleaned like a task's; a blank one is refused.
pub(crate) fn checklist_title(title: &str) -> Result<String> {
    Task::clean_title(title)
        .ok_or_else(|| MattersError::Validation("a checklist item needs a title".into()))
}

/// The key that puts a new item at the end of `task_id`'s checklist.
pub(crate) fn checklist_end_key(conn: &Connection, task_id: &str) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM checklist_items WHERE task_id = ?1",
        params![task_id],
        |r| r.get(0),
    )?)
}

/// Each item of `task_id`'s checklist with its key, in displayed order (as
/// [`Database::list_checklist_items`]).
pub(crate) fn checklist_keys(conn: &Connection, task_id: &str) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, sort_order FROM checklist_items WHERE task_id = ?1
         ORDER BY sort_order ASC, created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn insert_checklist_item(conn: &Connection, item: &ChecklistItem) -> Result<()> {
    conn.execute(
        "INSERT INTO checklist_items (id, task_id, title, is_completed, sort_order, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            item.id,
            item.task_id,
            item.title,
            item.is_completed,
            item.sort_order,
            item.created_at,
            item.updated_at,
        ],
    )?;
    Ok(())
}
