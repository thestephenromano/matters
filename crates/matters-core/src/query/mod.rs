use jiff::civil::Date;
use rusqlite::params;
use std::collections::{BTreeMap, HashMap};

use crate::domain::{Area, Heading, Project, ProjectProgress, Task, View};
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
    pub date: Date,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectView {
    pub project: Project,
    pub progress: ProjectProgress,
    pub headings: Vec<(String, Vec<Task>)>, // (heading_title, tasks)
    pub unsectioned_tasks: Vec<Task>,
}

/// A row of a task list as frontends show it ([`QueryEngine::list_rows`]):
/// a task; a project's heading with no tasks listed under it (#22), as a
/// heading with tasks needs no row of its own, being their section
/// ([`QueryEngine::section_keys`]); or a finished project in the Logbook,
/// which stands for its tasks (#55).
#[derive(Debug, Clone, PartialEq)]
pub enum ListRow {
    Task(Task),
    EmptyHeading(Heading),
    Project(Project),
}

/// The section key of tasks under heading `id` titled `title`.
pub fn heading_section_key(id: &str, title: &str) -> String {
    format!("heading:{id}:{title}")
}

/// The Logbook's section key for something finished at `completed_at`
/// (milliseconds): `done:YYYY-MM-DD`, its local day, or `done:` when that
/// is unknown.
pub fn logbook_section_key(completed_at: Option<i64>) -> String {
    let day = completed_at
        .and_then(crate::clock::local_day_of_millis)
        .map(|day| day.to_string());
    format!("done:{}", day.unwrap_or_default())
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

/// The task is in no project, or in an open one. A finished project's tasks
/// are listed only in the project itself; the Logbook lists the project as
/// one row (#55).
macro_rules! outside_finished_projects {
    () => {
        "(project_id IS NULL OR project_id IN (SELECT id FROM projects WHERE status = 'open'))"
    };
}

/// TODAY: scheduled for today or earlier (overdue), or undated with a deadline
/// today or earlier.
const TODAY_WHERE: &str = concat!(
    "status = 'open' AND is_someday = 0
    AND (start_date <= ?1 OR (start_date IS NULL AND deadline <= ?1)) AND ",
    outside_finished_projects!()
);

/// UPCOMING: start date strictly after the reference date.
const UPCOMING_WHERE: &str = concat!(
    "status = 'open' AND is_someday = 0 AND start_date > ?1 AND ",
    outside_finished_projects!()
);

/// ANYTIME: actionable now and organised under an area or project.
const ANYTIME_WHERE: &str = concat!(
    "status = 'open' AND is_someday = 0
    AND (start_date IS NULL OR start_date <= ?1)
    AND (project_id IS NOT NULL OR area_id IS NOT NULL) AND ",
    outside_finished_projects!()
);

/// SOMEDAY: explicitly deferred.
const SOMEDAY_WHERE: &str = concat!(
    "status = 'open' AND is_someday = 1 AND ",
    outside_finished_projects!()
);

/// LOGBOOK: finished tasks, apart from those of finished projects, which
/// the Logbook lists as the project ([`QueryEngine::list_rows`]).
const LOGBOOK_WHERE: &str = concat!(
    "status IN ('completed', 'canceled') AND ",
    outside_finished_projects!()
);

/// PROJECT: a project's live tasks, done or not (the same set its progress
/// ring counts). `?1` is the project id.
const PROJECT_WHERE: &str = "project_id = ?1 AND status IN ('open', 'completed')";

/// AREA: the tasks filed directly in an area, done or not; its projects'
/// tasks belong to the projects. `?1` is the area id.
const AREA_WHERE: &str = "area_id = ?1 AND project_id IS NULL AND status IN ('open', 'completed')";

/// Stable order for manually sorted lists; `id` breaks exact ties.
const MANUAL_ORDER: &str = "sort_order ASC, created_at ASC, id ASC";

/// A project's order: the tasks without a heading, then each heading's
/// tasks in the headings' order (#22); within each, the manual order.
const PROJECT_ORDER: &str =
    "(SELECT h.sort_order FROM headings h WHERE h.id = tasks.heading_id) ASC, \
     (SELECT h.created_at FROM headings h WHERE h.id = tasks.heading_id) ASC, \
     heading_id ASC, sort_order ASC, created_at ASC, id ASC";

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
    fn select_live(
        &self,
        rule: &str,
        order: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Task>> {
        self.select_tasks(&format!("{LIVE} AND ({rule})"), order, params)
    }

    fn select_tasks(
        &self,
        where_clause: &str,
        order: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE {where_clause} ORDER BY {order}");
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params, Database::map_task_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// The tasks a view lists, in display order. Today lists daytime tasks
    /// before evening ones; Upcoming is ordered by date.
    pub fn tasks_in(&self, view: &View, reference_date: Date) -> Result<Vec<Task>> {
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
            View::Area(id) => self.select_live(AREA_WHERE, MANUAL_ORDER, params![id])?,
        })
    }

    /// Badge counts for every sidebar view in one aggregate query, without
    /// loading any rows.
    pub fn counts(&self, reference_date: Date) -> Result<ViewCounts> {
        let ref_str = reference_date.to_string();
        let sql = format!(
            "SELECT
                COUNT(CASE WHEN {INBOX_WHERE} THEN 1 END),
                COUNT(CASE WHEN {TODAY_WHERE} THEN 1 END),
                COUNT(CASE WHEN {UPCOMING_WHERE} THEN 1 END),
                COUNT(CASE WHEN {ANYTIME_WHERE} THEN 1 END),
                COUNT(CASE WHEN {SOMEDAY_WHERE} THEN 1 END),
                COUNT(CASE WHEN {LOGBOOK_WHERE} THEN 1 END),
                (SELECT COUNT(*) FROM projects
                  WHERE {} AND status IN ('completed', 'canceled'))
             FROM tasks WHERE {LIVE}",
            trash::LIVE_PROJECT
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
                    logbook: n(5)? + n(6)?,
                })
            })?)
        })
    }

    pub fn inbox(&self) -> Result<Vec<Task>> {
        self.select_live(INBOX_WHERE, MANUAL_ORDER, [])
    }

    /// Today's tasks, split into daytime and This Evening.
    pub fn today(&self, reference_date: Date) -> Result<TodayView> {
        let ref_str = reference_date.to_string();
        let (evening, daytime) = self
            .select_live(TODAY_WHERE, TODAY_ORDER, params![ref_str])?
            .into_iter()
            .partition(Task::is_evening);
        Ok(TodayView { daytime, evening })
    }

    /// Future tasks grouped by calendar date.
    pub fn upcoming(&self, reference_date: Date) -> Result<Vec<UpcomingGroup>> {
        let ref_str = reference_date.to_string();
        let tasks = self.select_live(
            UPCOMING_WHERE,
            &format!("start_date ASC, {MANUAL_ORDER}"),
            params![ref_str],
        )?;

        let mut date_map: BTreeMap<Date, Vec<Task>> = BTreeMap::new();
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
    pub fn anytime(&self, reference_date: Date) -> Result<Vec<Task>> {
        let ref_str = reference_date.to_string();
        let mut tasks = self.select_live(ANYTIME_WHERE, MANUAL_ORDER, params![ref_str])?;
        let areas: HashMap<String, Area> = self
            .db
            .list_areas()?
            .into_iter()
            .map(|a| (a.id.clone(), a))
            .collect();
        let projects: HashMap<String, Project> = self
            .db
            .list_projects()?
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
        // (no area?, area order, area id, in a project?, project order, project id)
        let group = |task: &Task| {
            let project = task.project_id.as_ref().and_then(|id| projects.get(id));
            let area_id = project
                .and_then(|p| p.area_id.clone())
                .or_else(|| task.area_id.clone());
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
    /// * A project: `heading:<id>:<title>` under a heading (#22).
    ///
    /// Dates are left for the frontend to format in the user's locale.
    pub fn section_keys(&self, view: &View, tasks: &[Task]) -> Result<Vec<String>> {
        Ok(match view {
            View::Today => tasks
                .iter()
                .map(|t| {
                    if t.is_evening() {
                        "evening".into()
                    } else {
                        String::new()
                    }
                })
                .collect(),
            View::Upcoming => tasks
                .iter()
                .map(|t| t.start_date.map_or(String::new(), |d| format!("date:{d}")))
                .collect(),
            View::Anytime => {
                let areas: HashMap<String, String> = self
                    .db
                    .list_areas()?
                    .into_iter()
                    .map(|a| (a.id, a.title))
                    .collect();
                let projects: HashMap<String, String> = self
                    .db
                    .list_projects()?
                    .into_iter()
                    .map(|p| (p.id, p.title))
                    .collect();
                tasks
                    .iter()
                    .map(|t| match (&t.project_id, &t.area_id) {
                        (Some(id), _) => projects
                            .get(id)
                            .map_or(String::new(), |title| format!("project:{id}:{title}")),
                        (None, Some(id)) => areas
                            .get(id)
                            .map_or(String::new(), |title| format!("area:{id}:{title}")),
                        (None, None) => String::new(),
                    })
                    .collect()
            }
            View::Logbook => tasks
                .iter()
                .map(|t| logbook_section_key(t.completed_at))
                .collect(),
            View::Project(id) => {
                let headings: HashMap<String, String> = self
                    .db
                    .list_headings_for_project(id)?
                    .into_iter()
                    .map(|h| (h.id, h.title))
                    .collect();
                tasks
                    .iter()
                    .map(|t| {
                        t.heading_id
                            .as_ref()
                            .and_then(|h| {
                                headings.get(h).map(|title| heading_section_key(h, title))
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            }
            View::Inbox | View::Someday | View::Trash | View::Area(_) => {
                vec![String::new(); tasks.len()]
            }
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
        self.select_live(PROJECT_WHERE, PROJECT_ORDER, params![project_id])
    }

    /// Every trashed task, most recently trashed first.
    pub fn trash(&self) -> Result<Vec<Task>> {
        self.select_tasks(TRASH_WHERE, TRASH_ORDER, [])
    }

    /// The rows of `view` showing `tasks` (as [`Self::tasks_in`] lists
    /// them):
    ///
    /// * In a project, each heading with no task among `tasks` gets a row of
    ///   its own where its tasks would be, so it can be seen and moved into.
    /// * In the Logbook, each finished project is a row, placed among the
    ///   tasks by when it was finished (#55).
    ///
    /// Other views list only their tasks.
    pub fn list_rows(&self, view: &View, tasks: Vec<Task>) -> Result<Vec<ListRow>> {
        if *view == View::Logbook {
            return self.logbook_rows(tasks);
        }
        let View::Project(project_id) = view else {
            return Ok(tasks.into_iter().map(ListRow::Task).collect());
        };
        let headings = self.db.list_headings_for_project(project_id)?;
        // Group 0 is above the headings; group i + 1 is heading i. A task
        // under a heading that no longer exists is above them.
        let mut groups: Vec<Vec<Task>> = vec![Vec::new(); headings.len() + 1];
        for task in tasks {
            let group = task
                .heading_id
                .as_ref()
                .and_then(|h| headings.iter().position(|x| &x.id == h))
                .map_or(0, |i| i + 1);
            groups[group].push(task);
        }
        let mut groups = groups.into_iter();
        let mut rows: Vec<ListRow> = groups
            .next()
            .unwrap_or_default()
            .into_iter()
            .map(ListRow::Task)
            .collect();
        for (heading, group) in headings.into_iter().zip(groups) {
            if group.is_empty() {
                rows.push(ListRow::EmptyHeading(heading));
            } else {
                rows.extend(group.into_iter().map(ListRow::Task));
            }
        }
        Ok(rows)
    }

    /// The Logbook's tasks with the finished projects merged in, most
    /// recently finished first; a task finished at the same moment as a
    /// project comes first. A task without a finish time (reopened during
    /// this visit, `crate::Visit`) keeps its place after the projects
    /// finished before the task above it.
    fn logbook_rows(&self, tasks: Vec<Task>) -> Result<Vec<ListRow>> {
        let mut projects = self.db.list_finished_projects()?.into_iter().peekable();
        let mut rows = Vec::new();
        for task in tasks {
            while let Some(project) = projects.next_if(|p| p.completed_at > task.completed_at) {
                rows.push(ListRow::Project(project));
            }
            rows.push(ListRow::Task(task));
        }
        rows.extend(projects.map(ListRow::Project));
        Ok(rows)
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
    /// Instant search matching titles or notes across open tasks, projects,
    /// and areas. The query is matched as typed, anywhere in the text, with
    /// ASCII letters matched in either case; `%` and `_` are plain characters.
    pub fn quick_find(&self, query: &str) -> Result<QuickFindResult> {
        let pattern = contains_pattern(query);

        self.db.with_conn(|conn| {
            // 1. Search Tasks
            let mut task_stmt = conn.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks
                 WHERE {LIVE} AND status = 'open' AND (title LIKE ?1 ESCAPE '\\' OR notes LIKE ?1 ESCAPE '\\')
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
                 WHERE {} AND status = 'open' AND (title LIKE ?1 ESCAPE '\\' OR notes LIKE ?1 ESCAPE '\\')
                 ORDER BY sort_order ASC LIMIT 10",
                trash::LIVE_PROJECT
            ))?;
            let projects = proj_stmt
                .query_map(params![pattern], Database::map_project_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            // 3. Search Areas
            let mut area_stmt = conn.prepare(&format!(
                "SELECT {AREA_COLUMNS} FROM areas
                 WHERE {} AND title LIKE ?1 ESCAPE '\\'
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

/// A `LIKE` pattern matching `query` literally anywhere in the text: `%`,
/// `_` and the escape character `\` are escaped, for `ESCAPE '\'`.
fn contains_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for c in query.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}
