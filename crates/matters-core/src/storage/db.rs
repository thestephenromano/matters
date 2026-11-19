use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::schema::{
    MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5, MIGRATION_V6, SCHEMA_V1,
};
use super::trash;
use crate::domain::{
    Area, ChecklistItem, ChecklistProgress, DeletedHeading, Heading, Project, Status, Task,
};
use crate::error::{MattersError, Result};

/// Columns read by [`Database::map_task_row`], in its expected order.
pub(crate) const TASK_COLUMNS: &str =
    "id, area_id, project_id, heading_id, title, notes, start_date,
     time_of_day, is_someday, deadline, status, completed_at, sort_order,
     created_at, updated_at, trashed_at, today_order";

/// Columns read by [`Database::map_area_row`], in its expected order.
pub(crate) const AREA_COLUMNS: &str = "id, title, sort_order, created_at, updated_at, trashed_at";

/// Columns read by [`Database::map_project_row`], in its expected order.
pub(crate) const PROJECT_COLUMNS: &str =
    "id, area_id, title, notes, start_date, time_of_day, is_someday,
     deadline, status, completed_at, sort_order, created_at, updated_at, trashed_at";

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
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
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
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
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
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
        for (target, migration) in [
            (2, MIGRATION_V2),
            (3, MIGRATION_V3),
            (4, MIGRATION_V4),
            (5, MIGRATION_V5),
            (6, MIGRATION_V6),
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

    pub(crate) fn with_conn_mut<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T>,
    {
        let mut conn = self.conn.lock().unwrap();
        f(&mut conn)
    }

    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().unwrap();
        f(&conn)
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
        let conn = self.conn.lock().unwrap();
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

    /// Rename an area. Its order and trash state change only through the
    /// ordering and trash operations, so a stale copy can't undo them.
    pub fn update_area(&self, area: &Area) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE areas SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![area.title, now, area.id],
        )?;
        Ok(())
    }

    /// Rename an area. The name is cleaned like a task title
    /// ([`Task::clean_title`]); a blank name is refused.
    pub fn rename_area(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("an area needs a name".into()));
        };
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE areas SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        if changed == 0 {
            return Err(MattersError::NotFound {
                entity: "area",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Rename a project. The name is cleaned like a task title
    /// ([`Task::clean_title`]); a blank name is refused.
    pub fn rename_project(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("a project needs a name".into()));
        };
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE projects SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        if changed == 0 {
            return Err(MattersError::NotFound {
                entity: "project",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// File a project in an area, or take it out of one (`None`). The area
    /// must exist and not be in the Trash.
    pub fn set_project_area(&self, project_id: &str, area_id: Option<&str>) -> Result<()> {
        if let Some(area_id) = area_id {
            match self.get_area(area_id)? {
                Some(area) if !area.is_trashed() => {}
                _ => {
                    return Err(MattersError::NotFound {
                        entity: "area",
                        id: area_id.to_string(),
                    })
                }
            }
        }
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE projects SET area_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![area_id, now, project_id],
        )?;
        if changed == 0 {
            return Err(MattersError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            });
        }
        Ok(())
    }

    /// Move an area to the Trash, taking its projects and tasks with it: they
    /// leave every view, and the Trash lists the area in their place. Its
    /// contents keep their own state, so Put Back restores them as they were.
    pub fn trash_area(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE areas SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Put a trashed area back with its projects and tasks. Anything trashed
    /// on its own before the area stays in the Trash.
    pub fn restore_area(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE areas SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            params![now, id],
        )?;
        Ok(())
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
        let conn = self.conn.lock().unwrap();
        let start_date = project.start_date.map(|d| d.to_string());
        let deadline = project.deadline.map(|d| d.to_string());
        let is_someday = if project.is_someday { 1 } else { 0 };

        conn.execute(
            "INSERT INTO projects (
                id, area_id, title, notes, start_date, time_of_day, is_someday,
                deadline, status, completed_at, sort_order, created_at, updated_at, trashed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            ],
        )?;
        Ok(())
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

    /// Projects not in the Trash (themselves or through their area), in
    /// sidebar order.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        self.select_projects(&format!(
            "{} ORDER BY sort_order ASC, created_at ASC, id ASC",
            trash::LIVE_PROJECT
        ))
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

    /// Write the project's editable fields; like [`Self::update_task`], never
    /// its order or trash state.
    pub fn update_project(&self, project: &Project) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let start_date = project.start_date.map(|d| d.to_string());
        let deadline = project.deadline.map(|d| d.to_string());
        let is_someday = if project.is_someday { 1 } else { 0 };

        conn.execute(
            "UPDATE projects SET
                area_id = ?1, title = ?2, notes = ?3, start_date = ?4, time_of_day = ?5,
                is_someday = ?6, deadline = ?7, status = ?8, completed_at = ?9,
                updated_at = ?10
             WHERE id = ?11",
            params![
                project.area_id,
                project.title,
                project.notes,
                start_date,
                project.time_of_day,
                is_someday,
                deadline,
                project.status.as_str(),
                project.completed_at,
                now,
                project.id,
            ],
        )?;
        Ok(())
    }

    /// Move a project to the Trash, taking its tasks with it: they leave
    /// every view, and the Trash lists the project in their place. Tasks keep
    /// their own state, so Put Back restores the project as it was.
    pub fn trash_project(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE projects SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Put a trashed project back with its tasks. A task that was trashed on
    /// its own before the project stays in the Trash.
    pub fn restore_project(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE projects SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            params![now, id],
        )?;
        Ok(())
    }

    // =========================================================================
    // Heading Operations
    // =========================================================================

    pub fn create_heading(&self, heading: &Heading) -> Result<()> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE headings SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        if changed == 0 {
            return Err(MattersError::NotFound {
                entity: "heading",
                id: id.to_string(),
            });
        }
        Ok(())
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
        self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;
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
            let task_ids = {
                let mut stmt =
                    tx.prepare("SELECT id FROM tasks WHERE heading_id = ?1 ORDER BY id")?;
                let ids = stmt
                    .query_map(params![id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                ids
            };
            // Tasks lose the heading through `ON DELETE SET NULL`; clearing
            // it here too keeps that true without the pragma.
            tx.execute(
                "UPDATE tasks SET heading_id = NULL WHERE heading_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM headings WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(DeletedHeading { heading, task_ids })
        })
    }

    /// Undo [`Self::delete_heading`]: the heading comes back, with the tasks
    /// that were under it and are still in its project without a heading.
    pub fn restore_heading(&self, deleted: &DeletedHeading) -> Result<()> {
        self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;
            let h = &deleted.heading;
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
            {
                let mut stmt = tx.prepare(
                    "UPDATE tasks SET heading_id = ?1
                     WHERE id = ?2 AND project_id = ?3 AND heading_id IS NULL",
                )?;
                for task in &deleted.task_ids {
                    stmt.execute(params![h.id, task, h.project_id])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
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

    pub fn create_task(&self, task: &Task) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let start_date = task.start_date.map(|d| d.to_string());
        let deadline = task.deadline.map(|d| d.to_string());
        let is_someday = if task.is_someday { 1 } else { 0 };

        conn.execute(
            "INSERT INTO tasks (
                id, area_id, project_id, heading_id, title, notes, start_date,
                time_of_day, is_someday, deadline, status, completed_at, sort_order,
                created_at, updated_at, trashed_at, today_order
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
            ],
        )?;
        Ok(())
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"))?;
        let task = stmt.query_row(params![id], Self::map_task_row).optional()?;
        Ok(task)
    }

    /// Write the task's editable fields. Order and trash state are not among
    /// them: they change only through the ordering operations
    /// (`crate::ordering`) and [`Self::trash_task`] / [`Self::restore_task`],
    /// so saving a stale copy can't reorder, trash or resurrect a task.
    pub fn update_task(&self, task: &Task) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let start_date = task.start_date.map(|d| d.to_string());
        let deadline = task.deadline.map(|d| d.to_string());
        let is_someday = if task.is_someday { 1 } else { 0 };

        conn.execute(
            "UPDATE tasks SET
                area_id = ?1, project_id = ?2, heading_id = ?3, title = ?4, notes = ?5,
                start_date = ?6, time_of_day = ?7, is_someday = ?8, deadline = ?9,
                status = ?10, completed_at = ?11, updated_at = ?12
             WHERE id = ?13",
            params![
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
                now,
                task.id,
            ],
        )?;
        Ok(())
    }

    /// Rename a task. The title is cleaned ([`Task::clean_title`]); a blank
    /// title is refused and the task keeps its name.
    pub fn rename_task(&self, id: &str, title: &str) -> Result<()> {
        let Some(title) = Task::clean_title(title) else {
            return Err(MattersError::Validation("a task needs a title".into()));
        };
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        if changed == 0 {
            return Err(MattersError::NotFound {
                entity: "task",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    pub fn complete_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE tasks SET status = 'completed', completed_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn uncomplete_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE tasks SET status = 'open', completed_at = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    /// Move a task to the Trash. It keeps its status and place, and leaves
    /// every view except the Trash until it is put back. Trashing a task that
    /// is already in the Trash leaves its original trash time.
    pub fn trash_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE tasks SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Put a trashed task back where it was.
    pub fn restore_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        conn.execute(
            "UPDATE tasks SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Permanently delete everything in the Trash: trashed tasks, trashed
    /// projects and trashed areas, with everything inside them (headings,
    /// tasks, and the tasks' checklist items and tags). This is the only way
    /// a task, project or area is destroyed. Returns how many tasks were
    /// deleted.
    pub fn empty_trash(&self) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // Innermost first: each pass reads the trash state of the containers
        // that later passes delete.
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
        tx.commit()?;
        Ok(tasks)
    }

    // =========================================================================
    // Checklist Operations
    // =========================================================================

    pub fn create_checklist_item(&self, item: &ChecklistItem) -> Result<()> {
        self.with_conn(|conn| insert_checklist_item(conn, item))
    }

    /// Add `item` at the end of its task's checklist. Its title is cleaned
    /// like a task's ([`Task::clean_title`]); a blank one is refused.
    pub fn add_checklist_item(&self, item: &mut ChecklistItem) -> Result<()> {
        item.title = checklist_title(&item.title)?;
        self.with_conn_mut(|conn| {
            let tx = conn.transaction()?;
            item.sort_order = checklist_end_key(&tx, &item.task_id)?;
            insert_checklist_item(&tx, item)?;
            tx.commit()?;
            Ok(())
        })
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
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE checklist_items SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        checklist_item_found(changed, id)
    }

    /// Tick or untick a checklist item. Setting the state, rather than
    /// flipping it, means a click on a stale row can't undo what it shows.
    pub fn set_checklist_item_done(&self, id: &str, done: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = crate::clock::now_millis();
        let changed = conn.execute(
            "UPDATE checklist_items SET is_completed = ?1, updated_at = ?2 WHERE id = ?3",
            params![done, now, id],
        )?;
        checklist_item_found(changed, id)
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

    pub fn delete_checklist_item(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM checklist_items WHERE id = ?1", params![id])?;
        Ok(())
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
        })
    }

    fn map_heading_row(row: &Row) -> rusqlite::Result<Heading> {
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
        })
    }

    fn map_checklist_row(row: &Row) -> rusqlite::Result<ChecklistItem> {
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
fn checklist_item_found(changed: usize, id: &str) -> Result<()> {
    if changed == 0 {
        return Err(MattersError::NotFound {
            entity: "checklist item",
            id: id.to_string(),
        });
    }
    Ok(())
}

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
