use std::collections::{BTreeMap, HashMap};
use chrono::{NaiveDate, TimeZone};
use rusqlite::params;

use crate::domain::{Area, Project, ProjectProgress, Task, View};
use crate::error::Result;
use crate::storage::db::{Database, AREA_COLUMNS, PROJECT_COLUMNS, TASK_COLUMNS};
use crate::storage::trash::{self, LISTED_TRASHED_TASK as TRASH_WHERE, LIVE_TASK as LIVE};

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

// `LIVE` and `TRASH_WHERE` (the tasks the Trash lists one by one) come from
// `storage::trash`, which defines trash membership for every query.

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

/// Today's own manual order (`crate::ordering`), then the home order.
const TODAY_ORDER: &str = "today_order ASC, sort_order ASC, created_at ASC, id ASC";

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
            .select_live(TODAY_WHERE, TODAY_ORDER, params![ref_str])?
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

    /// Anytime, grouped (#25): each area in sidebar order with its own tasks
    /// first and then its projects' tasks, project by project; then projects
    /// with no area. Within a group, the manual order is kept.
    pub fn anytime(&self, reference_date: NaiveDate) -> Result<Vec<Task>> {
        let ref_str = reference_date.format("%Y-%m-%d").to_string();
        let mut tasks = self.select_live(ANYTIME_WHERE, MANUAL_ORDER, params![ref_str])?;
        let areas: HashMap<String, Area> = self.db.list_areas()?.into_iter().map(|a| (a.id.clone(), a)).collect();
        let projects: HashMap<String, Project> =
            self.db.list_projects()?.into_iter().map(|p| (p.id.clone(), p)).collect();
        // (no area?, area order, area id, in a project?, project order, project id)
        let group = |task: &Task| {
            let project = task.project_id.as_ref().and_then(|id| projects.get(id));
            let area_id = project.and_then(|p| p.area_id.clone()).or_else(|| task.area_id.clone());
            let area = area_id.as_ref().and_then(|id| areas.get(id));
            (
                area.is_none(),
                area.map_or(0.0, |a| a.sort_order),
                area.map_or(String::new(), |a| a.id.clone()),
                project.is_some(),
                project.map_or(0.0, |p| p.sort_order),
                project.map_or(String::new(), |p| p.id.clone()),
            )
        };
        // Stable: the manual order survives within each group.
        tasks.sort_by(|a, b| {
            let (ga, gb) = (group(a), group(b));
            ga.0.cmp(&gb.0)
                .then(ga.1.total_cmp(&gb.1))
                .then(ga.2.cmp(&gb.2))
                .then(ga.3.cmp(&gb.3))
                .then(ga.4.total_cmp(&gb.4))
                .then(ga.5.cmp(&gb.5))
        });
        Ok(tasks)
    }

    /// The section each listed task belongs to in `view` (#25), as a key the
    /// frontends turn into a heading; consecutive rows with the same key form
    /// one section, and an empty key has no heading.
    ///
    /// * Today: `evening` for This Evening.
    /// * Upcoming: `date:YYYY-MM-DD`, the start date.
    /// * Anytime: `project:<id>:<title>` or `area:<id>:<title>`.
    /// * Logbook: `done:YYYY-MM-DD`, the local day it was finished
    ///   (`done:` when unknown).
    ///
    /// Dates are left for the frontend to format in the user's locale.
    pub fn section_keys(&self, view: &View, tasks: &[Task]) -> Result<Vec<String>> {
        Ok(match view {
            View::Today => tasks.iter().map(|t| if t.is_evening() { "evening".into() } else { String::new() }).collect(),
            View::Upcoming => tasks
                .iter()
                .map(|t| t.start_date.map_or(String::new(), |d| format!("date:{d}")))
                .collect(),
            View::Anytime => {
                let areas: HashMap<String, String> =
                    self.db.list_areas()?.into_iter().map(|a| (a.id, a.title)).collect();
                let projects: HashMap<String, String> =
                    self.db.list_projects()?.into_iter().map(|p| (p.id, p.title)).collect();
                tasks
                    .iter()
                    .map(|t| match (&t.project_id, &t.area_id) {
                        (Some(id), _) => projects.get(id).map_or(String::new(), |title| format!("project:{id}:{title}")),
                        (None, Some(id)) => areas.get(id).map_or(String::new(), |title| format!("area:{id}:{title}")),
                        (None, None) => String::new(),
                    })
                    .collect()
            }
            View::Logbook => tasks
                .iter()
                .map(|t| {
                    let day = t
                        .completed_at
                        .and_then(|ms| chrono::Local.timestamp_millis_opt(ms).single())
                        .map(|at| at.date_naive().to_string());
                    format!("done:{}", day.unwrap_or_default())
                })
                .collect(),
            View::Inbox | View::Someday | View::Trash | View::Project(_) => vec![String::new(); tasks.len()],
        })
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
            let mut proj_stmt = conn.prepare(&format!(
                "SELECT {PROJECT_COLUMNS} FROM projects
                 WHERE {} AND status = 'open' AND (title LIKE ?1 OR notes LIKE ?1)
                 ORDER BY sort_order ASC LIMIT 10",
                trash::LIVE_PROJECT
            ))?;
            let projects = proj_stmt
                .query_map(params![pattern], Database::map_project_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            // 3. Search Areas
            let mut area_stmt = conn.prepare(&format!(
                "SELECT {AREA_COLUMNS} FROM areas
                 WHERE {} AND title LIKE ?1
                 ORDER BY sort_order ASC LIMIT 10",
                trash::LIVE_AREA
            ))?;
            let areas = area_stmt
                .query_map(params![pattern], Database::map_area_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            Ok(QuickFindResult {
                tasks,
                projects,
                areas,
            })
        })
    }
}
