use std::collections::BTreeMap;
use chrono::NaiveDate;
use rusqlite::params;

use crate::domain::{Area, Project, ProjectProgress, Task};
use crate::error::Result;
use crate::storage::db::Database;

#[derive(Debug, Clone, Default)]
pub struct TodayView {
    pub daytime: Vec<Task>,
    pub evening: Vec<Task>,
}

#[derive(Debug, Clone, Default)]
pub struct UpcomingGroup {
    pub date: NaiveDate,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectView {
    pub project: Project,
    pub progress: ProjectProgress,
    pub headings: Vec<(String, Vec<Task>)>, // (heading_title, tasks)
    pub unsectioned_tasks: Vec<Task>,
}

#[derive(Debug, Clone, Default)]
pub struct QuickFindResult {
    pub tasks: Vec<Task>,
    pub projects: Vec<Project>,
    pub areas: Vec<Area>,
}

pub struct QueryEngine<'a> {
    db: &'a Database,
}

impl<'a> QueryEngine<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// INBOX:
    /// Unassigned tasks that have no project, no area, no scheduled date, and are not Someday.
    pub fn inbox(&self) -> Result<Vec<Task>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status = 'open'
                   AND project_id IS NULL
                   AND area_id IS NULL
                   AND start_date IS NULL
                   AND is_someday = 0
                 ORDER BY sort_order ASC, created_at ASC",
            )?;
            let rows = stmt.query_map([], Database::map_task_row)?;
            let mut tasks = Vec::new();
            for r in rows {
                tasks.push(r?);
            }
            Ok(tasks)
        })
    }

    /// TODAY:
    /// Tasks scheduled for today or earlier (overdue), or tasks with no start date whose deadline is today or earlier.
    /// Split cleanly into Daytime and This Evening.
    pub fn today(&self, reference_date: NaiveDate) -> Result<TodayView> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();

        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status = 'open'
                   AND is_someday = 0
                   AND (
                       start_date <= ?1
                       OR (start_date IS NULL AND deadline <= ?1)
                   )
                 ORDER BY sort_order ASC, created_at ASC",
            )?;

            let rows = stmt.query_map(params![ref_str], Database::map_task_row)?;
            let mut view = TodayView::default();

            for r in rows {
                let task = r?;
                if task.is_evening() {
                    view.evening.push(task);
                } else {
                    view.daytime.push(task);
                }
            }

            Ok(view)
        })
    }

    /// UPCOMING:
    /// Tasks with start_date strictly after the reference date, grouped by calendar date.
    pub fn upcoming(&self, reference_date: NaiveDate) -> Result<Vec<UpcomingGroup>> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();

        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status = 'open'
                   AND is_someday = 0
                   AND start_date > ?1
                 ORDER BY start_date ASC, sort_order ASC",
            )?;

            let rows = stmt.query_map(params![ref_str], Database::map_task_row)?;
            let mut date_map: BTreeMap<NaiveDate, Vec<Task>> = BTreeMap::new();

            for r in rows {
                let task = r?;
                if let Some(date) = task.start_date {
                    date_map.entry(date).or_default().push(task);
                }
            }

            let groups = date_map
                .into_iter()
                .map(|(date, tasks)| UpcomingGroup { date, tasks })
                .collect();

            Ok(groups)
        })
    }

    /// ANYTIME:
    /// Actionable tasks that are ready to work on right now:
    /// Open, not in Someday, start_date is NULL or <= reference_date, and organized under an Area or Project.
    pub fn anytime(&self, reference_date: NaiveDate) -> Result<Vec<Task>> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();

        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status = 'open'
                   AND is_someday = 0
                   AND (start_date IS NULL OR start_date <= ?1)
                   AND (project_id IS NOT NULL OR area_id IS NOT NULL)
                 ORDER BY sort_order ASC, created_at ASC",
            )?;

            let rows = stmt.query_map(params![ref_str], Database::map_task_row)?;
            let mut tasks = Vec::new();
            for r in rows {
                tasks.push(r?);
            }
            Ok(tasks)
        })
    }

    /// SOMEDAY:
    /// Open tasks and projects explicitly deferred to Someday.
    pub fn someday_tasks(&self) -> Result<Vec<Task>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status = 'open'
                   AND is_someday = 1
                 ORDER BY sort_order ASC, created_at ASC",
            )?;

            let rows = stmt.query_map([], Database::map_task_row)?;
            let mut tasks = Vec::new();
            for r in rows {
                tasks.push(r?);
            }
            Ok(tasks)
        })
    }

    /// LOGBOOK:
    /// Completed or canceled tasks, ordered by most recently completed.
    pub fn logbook(&self, limit: usize) -> Result<Vec<Task>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status IN ('completed', 'canceled')
                 ORDER BY completed_at DESC, updated_at DESC
                 LIMIT ?1",
            )?;

            let rows = stmt.query_map(params![limit as i64], Database::map_task_row)?;
            let mut tasks = Vec::new();
            for r in rows {
                tasks.push(r?);
            }
            Ok(tasks)
        })
    }

    /// PROJECT PROGRESS:
    /// Calculates the completion ratio for the project's circular progress ring.
    pub fn project_progress(&self, project_id: &str) -> Result<ProjectProgress> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT
                    COUNT(CASE WHEN status = 'completed' THEN 1 END) as completed,
                    COUNT(CASE WHEN status IN ('open', 'completed') THEN 1 END) as total
                 FROM tasks
                 WHERE project_id = ?1",
            )?;

            let row = stmt.query_row(params![project_id], |r| {
                let completed: i64 = r.get(0)?;
                let total: i64 = r.get(1)?;
                Ok(ProjectProgress {
                    completed_tasks: completed as usize,
                    total_tasks: total as usize,
                })
            })?;

            Ok(row)
        })
    }

    /// QUICK FIND:
    /// Instant fuzzy search matching titles or notes across open tasks, projects, and areas.
    pub fn quick_find(&self, query: &str) -> Result<QuickFindResult> {
        let pattern = format!("%{}%", query);

        self.db.with_conn(|conn| {
            // 1. Search Tasks
            let mut task_stmt = conn.prepare(
                "SELECT id, area_id, project_id, heading_id, title, notes, start_date,
                        time_of_day, is_someday, deadline, status, completed_at, sort_order,
                        created_at, updated_at
                 FROM tasks
                 WHERE status = 'open' AND (title LIKE ?1 OR notes LIKE ?1)
                 ORDER BY sort_order ASC LIMIT 20",
            )?;
            let task_rows = task_stmt.query_map(params![pattern], Database::map_task_row)?;
            let mut tasks = Vec::new();
            for r in task_rows {
                tasks.push(r?);
            }

            // 2. Search Projects
            let mut proj_stmt = conn.prepare(
                "SELECT id, area_id, title, notes, start_date, time_of_day, is_someday,
                        deadline, status, completed_at, sort_order, created_at, updated_at
                 FROM projects
                 WHERE status = 'open' AND (title LIKE ?1 OR notes LIKE ?1)
                 ORDER BY sort_order ASC LIMIT 10",
            )?;
            let mut projects = Vec::new();
            let mut rows = proj_stmt.query(params![pattern])?;
            while let Some(row) = rows.next()? {
                let start_date_str: Option<String> = row.get(4)?;
                let start_date = start_date_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
                let is_someday_int: i64 = row.get(6)?;
                let deadline_str: Option<String> = row.get(7)?;
                let deadline = deadline_str.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
                let status_str: String = row.get(8)?;

                projects.push(Project {
                    id: row.get(0)?,
                    area_id: row.get(1)?,
                    title: row.get(2)?,
                    notes: row.get(3)?,
                    start_date,
                    time_of_day: row.get(5)?,
                    is_someday: is_someday_int == 1,
                    deadline,
                    status: crate::domain::Status::from_str_loose(&status_str),
                    completed_at: row.get(9)?,
                    sort_order: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                });
            }

            // 3. Search Areas
            let mut area_stmt = conn.prepare(
                "SELECT id, title, sort_order, created_at, updated_at
                 FROM areas
                 WHERE title LIKE ?1
                 ORDER BY sort_order ASC LIMIT 10",
            )?;
            let mut areas = Vec::new();
            let mut rows = area_stmt.query(params![pattern])?;
            while let Some(row) = rows.next()? {
                areas.push(Area {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    sort_order: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                });
            }

            Ok(QuickFindResult {
                tasks,
                projects,
                areas,
            })
        })
    }
}
