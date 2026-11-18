use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::{Area, ChecklistItem, Heading, Project, Status, Task};
use crate::error::{MattersError, Result};
use super::schema::{MIGRATION_V2, SCHEMA_V1};

/// Columns read by [`Database::map_task_row`], in its expected order.
pub(crate) const TASK_COLUMNS: &str = "id, area_id, project_id, heading_id, title, notes, start_date,
     time_of_day, is_someday, deadline, status, completed_at, sort_order,
     created_at, updated_at, trashed_at";

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
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA_V1)?;
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
            params![now],
        )?;

        let version: i64 =
            conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0))?;
        if version < 2 {
            let tx = conn.transaction()?;
            tx.execute_batch(MIGRATION_V2)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?1)",
                params![now],
            )?;
            tx.commit()?;
        }
        Ok(())
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

    pub fn create_area(&self, area: &Area) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO areas (id, title, sort_order, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![area.id, area.title, area.sort_order, area.created_at, area.updated_at],
        )?;
        Ok(())
    }

    pub fn get_area(&self, id: &str) -> Result<Option<Area>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, title, sort_order, created_at, updated_at FROM areas WHERE id = ?1")?;
        let area = stmt.query_row(params![id], Self::map_area_row).optional()?;
        Ok(area)
    }

    pub fn list_areas(&self) -> Result<Vec<Area>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, title, sort_order, created_at, updated_at FROM areas ORDER BY sort_order ASC")?;
        let rows = stmt.query_map([], Self::map_area_row)?;
        let mut areas = Vec::new();
        for row in rows {
            areas.push(row?);
        }
        Ok(areas)
    }

    pub fn update_area(&self, area: &Area) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE areas SET title = ?1, sort_order = ?2, updated_at = ?3 WHERE id = ?4",
            params![area.title, area.sort_order, now, area.id],
        )?;
        Ok(())
    }

    pub fn delete_area(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM areas WHERE id = ?1", params![id])?;
        Ok(())
    }

    // =========================================================================
    // Project Operations
    // =========================================================================

    pub fn create_project(&self, project: &Project) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let start_date = project.start_date.map(|d| d.format("%Y-%m-%d").to_string());
        let deadline = project.deadline.map(|d| d.format("%Y-%m-%d").to_string());
        let is_someday = if project.is_someday { 1 } else { 0 };

        conn.execute(
            "INSERT INTO projects (
                id, area_id, title, notes, start_date, time_of_day, is_someday,
                deadline, status, completed_at, sort_order, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
            ],
        )?;
        Ok(())
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, area_id, title, notes, start_date, time_of_day, is_someday,
                    deadline, status, completed_at, sort_order, created_at, updated_at
             FROM projects WHERE id = ?1",
        )?;
        let project = stmt.query_row(params![id], Self::map_project_row).optional()?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, area_id, title, notes, start_date, time_of_day, is_someday,
                    deadline, status, completed_at, sort_order, created_at, updated_at
             FROM projects ORDER BY sort_order ASC",
        )?;
        let rows = stmt.query_map([], Self::map_project_row)?;
        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }
        Ok(projects)
    }

    pub fn update_project(&self, project: &Project) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let start_date = project.start_date.map(|d| d.format("%Y-%m-%d").to_string());
        let deadline = project.deadline.map(|d| d.format("%Y-%m-%d").to_string());
        let is_someday = if project.is_someday { 1 } else { 0 };

        conn.execute(
            "UPDATE projects SET
                area_id = ?1, title = ?2, notes = ?3, start_date = ?4, time_of_day = ?5,
                is_someday = ?6, deadline = ?7, status = ?8, completed_at = ?9,
                sort_order = ?10, updated_at = ?11
             WHERE id = ?12",
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
                project.sort_order,
                now,
                project.id,
            ],
        )?;
        Ok(())
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
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

    pub fn list_headings_for_project(&self, project_id: &str) -> Result<Vec<Heading>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, title, sort_order, created_at, updated_at
             FROM headings WHERE project_id = ?1 ORDER BY sort_order ASC",
        )?;
        let rows = stmt.query_map(params![project_id], Self::map_heading_row)?;
        let mut headings = Vec::new();
        for row in rows {
            headings.push(row?);
        }
        Ok(headings)
    }

    pub fn delete_heading(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM headings WHERE id = ?1", params![id])?;
        Ok(())
    }

    // =========================================================================
    // Task Operations
    // =========================================================================

    pub fn create_task(&self, task: &Task) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let start_date = task.start_date.map(|d| d.format("%Y-%m-%d").to_string());
        let deadline = task.deadline.map(|d| d.format("%Y-%m-%d").to_string());
        let is_someday = if task.is_someday { 1 } else { 0 };

        conn.execute(
            "INSERT INTO tasks (
                id, area_id, project_id, heading_id, title, notes, start_date,
                time_of_day, is_someday, deadline, status, completed_at, sort_order,
                created_at, updated_at, trashed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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

    /// Write the task's editable fields. Trash state is not one of them: it
    /// changes only through [`Self::trash_task`] and [`Self::restore_task`],
    /// so saving a stale copy can neither trash nor resurrect a task.
    pub fn update_task(&self, task: &Task) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let start_date = task.start_date.map(|d| d.format("%Y-%m-%d").to_string());
        let deadline = task.deadline.map(|d| d.format("%Y-%m-%d").to_string());
        let is_someday = if task.is_someday { 1 } else { 0 };

        conn.execute(
            "UPDATE tasks SET
                area_id = ?1, project_id = ?2, heading_id = ?3, title = ?4, notes = ?5,
                start_date = ?6, time_of_day = ?7, is_someday = ?8, deadline = ?9,
                status = ?10, completed_at = ?11, sort_order = ?12, updated_at = ?13
             WHERE id = ?14",
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
                task.sort_order,
                now,
                task.id,
            ],
        )?;
        Ok(())
    }

    pub fn complete_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE tasks SET status = 'completed', completed_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn uncomplete_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
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
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE tasks SET trashed_at = ?1, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Put a trashed task back where it was.
    pub fn restore_task(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE tasks SET trashed_at = NULL, updated_at = ?1 WHERE id = ?2 AND trashed_at IS NOT NULL",
            params![now, id],
        )?;
        Ok(())
    }

    /// Permanently delete every task in the Trash, with their checklist items
    /// and tags. This is the only way a task is destroyed. Returns how many
    /// tasks were deleted.
    pub fn empty_trash(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM tasks WHERE trashed_at IS NOT NULL", [])?)
    }

    // =========================================================================
    // Checklist Operations
    // =========================================================================

    pub fn create_checklist_item(&self, item: &ChecklistItem) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let is_completed = if item.is_completed { 1 } else { 0 };
        conn.execute(
            "INSERT INTO checklist_items (id, task_id, title, is_completed, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                item.id,
                item.task_id,
                item.title,
                is_completed,
                item.sort_order,
                item.created_at,
                item.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_checklist_items(&self, task_id: &str) -> Result<Vec<ChecklistItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, title, is_completed, sort_order, created_at, updated_at
             FROM checklist_items WHERE task_id = ?1 ORDER BY sort_order ASC",
        )?;
        let rows = stmt.query_map(params![task_id], Self::map_checklist_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn toggle_checklist_item(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE checklist_items SET is_completed = CASE WHEN is_completed = 1 THEN 0 ELSE 1 END, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn delete_checklist_item(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM checklist_items WHERE id = ?1", params![id])?;
        Ok(())
    }

    // =========================================================================
    // Row Mappers
    // =========================================================================

    fn map_area_row(row: &Row) -> rusqlite::Result<Area> {
        Ok(Area {
            id: row.get(0)?,
            title: row.get(1)?,
            sort_order: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    }

    fn map_project_row(row: &Row) -> rusqlite::Result<Project> {
        let start_date_str: Option<String> = row.get(4)?;
        let start_date = start_date_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());

        let is_someday_int: i64 = row.get(6)?;
        let is_someday = is_someday_int == 1;

        let deadline_str: Option<String> = row.get(7)?;
        let deadline = deadline_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());

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
        let start_date = start_date_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());

        let is_someday_int: i64 = row.get(8)?;
        let is_someday = is_someday_int == 1;

        let deadline_str: Option<String> = row.get(9)?;
        let deadline = deadline_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());

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
