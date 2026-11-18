use std::collections::BTreeMap;
use chrono::NaiveDate;
use rusqlite::params;

use crate::domain::{Area, Project, ProjectProgress, Task, View};
use crate::error::Result;
use crate::storage::db::{Database, TASK_COLUMNS};

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewCounts {
    pub inbox: usize,
    pub today: usize,
    pub upcoming: usize,
    pub anytime: usize,
    pub someday: usize,
    pub logbook: usize,
}

// Membership rule for each view, written once and shared by the list query and
// the count query so the sidebar badge always matches what the view shows.
// `?1` is the reference date (YYYY-MM-DD) wherever a rule depends on it.
// These rules apply to live tasks only: `select_live` and `counts` exclude the
// Trash, whose tasks belong to no other view.

/// Live tasks: everything not in the Trash.
const LIVE: &str = "trashed_at IS NULL";

/// TRASH: trashed tasks, whatever their status.
const TRASH_WHERE: &str = "trashed_at IS NOT NULL";

/// INBOX: unassigned tasks with no project, area, date, or Someday flag.
const INBOX_WHERE: &str = "status = 'open'
    AND project_id IS NULL AND area_id IS NULL
    AND start_date IS NULL AND is_someday = 0";

/// TODAY: scheduled for today or earlier (overdue), or undated with a deadline
/// today or earlier.
const TODAY_WHERE: &str = "status = 'open' AND is_someday = 0
    AND (start_date <= ?1 OR (start_date IS NULL AND deadline <= ?1))";

/// UPCOMING: start date strictly after the reference date.
const UPCOMING_WHERE: &str = "status = 'open' AND is_someday = 0 AND start_date > ?1";

/// ANYTIME: actionable now and organised under an area or project.
const ANYTIME_WHERE: &str = "status = 'open' AND is_someday = 0
    AND (start_date IS NULL OR start_date <= ?1)
    AND (project_id IS NOT NULL OR area_id IS NOT NULL)";

/// SOMEDAY: explicitly deferred.
const SOMEDAY_WHERE: &str = "status = 'open' AND is_someday = 1";

/// LOGBOOK: finished tasks.
const LOGBOOK_WHERE: &str = "status IN ('completed', 'canceled')";

/// PROJECT: a project's live tasks, done or not (the same set its progress
/// ring counts). `?1` is the project id.
const PROJECT_WHERE: &str = "project_id = ?1 AND status IN ('open', 'completed')";

/// Stable order for manually sorted lists; `id` breaks exact ties.
const MANUAL_ORDER: &str = "sort_order ASC, created_at ASC, id ASC";

/// Logbook order: most recently finished first.
const LOGBOOK_ORDER: &str = "completed_at DESC, updated_at DESC, id ASC";

/// Trash order: most recently trashed first.
const TRASH_ORDER: &str = "trashed_at DESC, id ASC";

pub struct QueryEngine<'a> {
    db: &'a Database,
}

impl<'a> QueryEngine<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Live tasks matching a view rule.
    fn select_live(&self, rule: &str, order: &str, params: impl rusqlite::Params) -> Result<Vec<Task>> {
        self.select_tasks(&format!("{LIVE} AND ({rule})"), order, params)
    }

    fn select_tasks(&self, where_clause: &str, order: &str, params: impl rusqlite::Params) -> Result<Vec<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE {where_clause} ORDER BY {order}");
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params, Database::map_task_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// The tasks a view lists, in display order. Today lists daytime tasks
    /// before evening ones; Upcoming is ordered by date.
    pub fn tasks_in(&self, view: &View, reference_date: NaiveDate) -> Result<Vec<Task>> {
        Ok(match view {
            View::Inbox => self.inbox()?,
            View::Today => {
                let today = self.today(reference_date)?;
                let mut tasks = today.daytime;
                tasks.extend(today.evening);
                tasks
            }
            View::Upcoming => self
                .upcoming(reference_date)?
                .into_iter()
                .flat_map(|group| group.tasks)
                .collect(),
            View::Anytime => self.anytime(reference_date)?,
            View::Someday => self.someday_tasks()?,
            // Every finished task, so the list matches its badge count.
            View::Logbook => self.select_live(LOGBOOK_WHERE, LOGBOOK_ORDER, [])?,
            View::Trash => self.trash()?,
            View::Project(id) => self.project_tasks(id)?,
        })
    }

    /// Badge counts for every sidebar view in one aggregate query, without
    /// loading any rows.
    pub fn counts(&self, reference_date: NaiveDate) -> Result<ViewCounts> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();
        let sql = format!(
            "SELECT
                COUNT(CASE WHEN {INBOX_WHERE} THEN 1 END),
                COUNT(CASE WHEN {TODAY_WHERE} THEN 1 END),
                COUNT(CASE WHEN {UPCOMING_WHERE} THEN 1 END),
                COUNT(CASE WHEN {ANYTIME_WHERE} THEN 1 END),
                COUNT(CASE WHEN {SOMEDAY_WHERE} THEN 1 END),
                COUNT(CASE WHEN {LOGBOOK_WHERE} THEN 1 END)
             FROM tasks WHERE {LIVE}"
        );
        self.db.with_conn(|conn| {
            Ok(conn.query_row(&sql, params![ref_str], |r| {
                let n = |i| r.get::<_, i64>(i).map(|v| v as usize);
                Ok(ViewCounts {
                    inbox: n(0)?,
                    today: n(1)?,
                    upcoming: n(2)?,
                    anytime: n(3)?,
                    someday: n(4)?,
                    logbook: n(5)?,
                })
            })?)
        })
    }

    pub fn inbox(&self) -> Result<Vec<Task>> {
        self.select_live(INBOX_WHERE, MANUAL_ORDER, [])
    }

    /// Today's tasks, split into daytime and This Evening.
    pub fn today(&self, reference_date: NaiveDate) -> Result<TodayView> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();
        let (evening, daytime) = self
            .select_live(TODAY_WHERE, MANUAL_ORDER, params![ref_str])?
            .into_iter()
            .partition(Task::is_evening);
        Ok(TodayView { daytime, evening })
    }

    /// Future tasks grouped by calendar date.
    pub fn upcoming(&self, reference_date: NaiveDate) -> Result<Vec<UpcomingGroup>> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();
        let tasks = self.select_live(
            UPCOMING_WHERE,
            &format!("start_date ASC, {MANUAL_ORDER}"),
            params![ref_str],
        )?;

        let mut date_map: BTreeMap<NaiveDate, Vec<Task>> = BTreeMap::new();
        for task in tasks {
            if let Some(date) = task.start_date {
                date_map.entry(date).or_default().push(task);
            }
        }
        Ok(date_map
            .into_iter()
            .map(|(date, tasks)| UpcomingGroup { date, tasks })
            .collect())
    }

    pub fn anytime(&self, reference_date: NaiveDate) -> Result<Vec<Task>> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();
        self.select_live(ANYTIME_WHERE, MANUAL_ORDER, params![ref_str])
    }

    pub fn someday_tasks(&self) -> Result<Vec<Task>> {
        self.select_live(SOMEDAY_WHERE, MANUAL_ORDER, [])
    }

    /// The `limit` most recently finished tasks.
    pub fn logbook(&self, limit: usize) -> Result<Vec<Task>> {
        self.select_live(
            LOGBOOK_WHERE,
            &format!("{LOGBOOK_ORDER} LIMIT ?1"),
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
        )
    }

    pub fn project_tasks(&self, project_id: &str) -> Result<Vec<Task>> {
        self.select_live(PROJECT_WHERE, MANUAL_ORDER, params![project_id])
    }

    /// Every trashed task, most recently trashed first.
    pub fn trash(&self) -> Result<Vec<Task>> {
        self.select_tasks(TRASH_WHERE, TRASH_ORDER, [])
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
                 WHERE project_id = ?1 AND trashed_at IS NULL",
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
            let mut task_stmt = conn.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks
                 WHERE {LIVE} AND status = 'open' AND (title LIKE ?1 OR notes LIKE ?1)
                 ORDER BY sort_order ASC LIMIT 20"
            ))?;
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
