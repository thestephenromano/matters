//! QML-facing objects.
//!
//! * `AppController` owns navigation state, sidebar data and every command.
//!   After any change it bumps `revision`, which tells views to reload.
//! * `TaskListModel` is the task list for one view. It reconciles rows by
//!   task id so delegates (and their expansion, focus and edit state) survive
//!   reloads; see [`crate::list_diff`].
//!
//! View membership, "When" parsing and task placement all live in
//! `matters_core`; this layer only translates between QML and those types.

use core::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QVariant};
use matters_core::{local_today, Database, DayTracker, Project, QueryEngine, Schedule, Status, Task, View};
use uuid::Uuid;

use crate::list_diff::{reconcile, ListSink};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;
    }

    // `auto_cxx_name` exposes every Rust snake_case name to C++ and QML in
    // camelCase, the QML naming convention (`current_view` -> `currentView`).
    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, current_view)]
        #[qproperty(QString, current_title)]
        #[qproperty(QString, current_project_id)]
        #[qproperty(f64, current_project_progress)]
        #[qproperty(i32, inbox_count)]
        #[qproperty(i32, today_count)]
        #[qproperty(i32, upcoming_count)]
        #[qproperty(i32, anytime_count)]
        #[qproperty(i32, someday_count)]
        #[qproperty(i32, logbook_count)]
        #[qproperty(QString, projects_json)]
        #[qproperty(QString, search_results_json)]
        #[qproperty(QString, error_message)]
        #[qproperty(i32, revision)]
        type AppController = super::AppControllerRust;

        #[qinvokable]
        fn select_view(self: Pin<&mut AppController>, view: &QString);
        #[qinvokable]
        fn select_project(self: Pin<&mut AppController>, project_id: &QString);
        #[qinvokable]
        fn add_task(self: Pin<&mut AppController>, title: &QString);
        #[qinvokable]
        fn toggle_task(self: Pin<&mut AppController>, task_id: &QString);
        #[qinvokable]
        fn delete_task(self: Pin<&mut AppController>, task_id: &QString);
        #[qinvokable]
        fn schedule_task(self: Pin<&mut AppController>, task_id: &QString, when: &QString);
        #[qinvokable]
        fn update_task_notes(self: Pin<&mut AppController>, task_id: &QString, notes: &QString);
        #[qinvokable]
        fn create_project(self: Pin<&mut AppController>, title: &QString);
        /// Navigate to the view that contains the task (`View::containing`).
        #[qinvokable]
        fn reveal_task(self: Pin<&mut AppController>, task_id: &QString);
        #[qinvokable]
        fn search(self: Pin<&mut AppController>, query: &QString);
        #[qinvokable]
        fn clear_error(self: Pin<&mut AppController>);
        /// Refresh if the local date changed since the views were built.
        /// Called periodically by the UI.
        #[qinvokable]
        fn check_date(self: Pin<&mut AppController>);
    }

    impl cxx_qt::Initialize for AppController {}

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(QString, error_message)]
        type TaskListModel = super::TaskListModelRust;

        /// Show the tasks of `view` (a `View` key; `project_id` for projects).
        #[qinvokable]
        fn load(self: Pin<&mut TaskListModel>, view: &QString, project_id: &QString);
        /// Row of the task with this id, or -1.
        #[qinvokable]
        fn row_of(self: &TaskListModel, task_id: &QString) -> i32;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[inherit]
        fn begin_insert_rows(self: Pin<&mut TaskListModel>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        fn end_insert_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn begin_remove_rows(self: Pin<&mut TaskListModel>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        fn end_remove_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn begin_move_rows(
            self: Pin<&mut TaskListModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            destination_parent: &QModelIndex,
            destination_child: i32,
        ) -> bool;
        #[inherit]
        fn end_move_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn index(self: &TaskListModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;

        #[qsignal]
        #[inherit]
        fn data_changed(
            self: Pin<&mut TaskListModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[cxx_override]
        fn data(self: &TaskListModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn role_names(self: &TaskListModel) -> QHash_i32_QByteArray;
        #[cxx_override]
        fn row_count(self: &TaskListModel, parent: &QModelIndex) -> i32;
    }
}

// =============================================================================
// Shared database
// =============================================================================

/// QML instantiates both objects through `Default`, which can neither fail nor
/// take arguments, so `main` opens the database first and installs it here.
static DATABASE: OnceLock<Database> = OnceLock::new();

pub fn install_database(db: Database) {
    if DATABASE.set(db).is_err() {
        panic!("matters-ui: database installed twice");
    }
}

fn database() -> Database {
    DATABASE
        .get()
        .cloned()
        .expect("matters-ui: QML object created before install_database()")
}

fn to_json<T: serde::Serialize>(value: &T) -> QString {
    QString::from(serde_json::to_string(value).expect("domain types serialize").as_str())
}

// =============================================================================
// AppController
// =============================================================================

pub struct AppControllerRust {
    current_view: QString,
    current_title: QString,
    current_project_id: QString,
    current_project_progress: f64,
    inbox_count: i32,
    today_count: i32,
    upcoming_count: i32,
    anytime_count: i32,
    someday_count: i32,
    logbook_count: i32,
    projects_json: QString,
    search_results_json: QString,
    error_message: QString,
    revision: i32,

    view: View,
    /// The date the sidebar and header were last built for.
    day: DayTracker,
    search_query: String,
    db: Database,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        let view = View::Today;
        Self {
            current_view: QString::from(view.key()),
            current_title: QString::from(view.title()),
            current_project_id: QString::default(),
            current_project_progress: 0.0,
            inbox_count: 0,
            today_count: 0,
            upcoming_count: 0,
            anytime_count: 0,
            someday_count: 0,
            logbook_count: 0,
            projects_json: QString::from("[]"),
            search_results_json: QString::from("[]"),
            error_message: QString::default(),
            revision: 0,
            view,
            day: DayTracker::new(local_today()),
            search_query: String::new(),
            db: database(),
        }
    }
}

impl cxx_qt::Initialize for qobject::AppController {
    fn initialize(self: Pin<&mut Self>) {
        self.changed();
    }
}

impl qobject::AppController {
    pub fn select_view(self: Pin<&mut Self>, view: &QString) {
        match View::from_key(&String::from(view), None) {
            Some(view) => self.set_view(view),
            None => self.report(format!("unknown view '{view}'")),
        }
    }

    pub fn select_project(self: Pin<&mut Self>, project_id: &QString) {
        match View::from_key("project", Some(&String::from(project_id))) {
            Some(view) => self.set_view(view),
            None => self.report("no project selected".to_string()),
        }
    }

    /// Capture a task in the current view (placement rules: `View::new_task`).
    pub fn add_task(self: Pin<&mut Self>, title: &QString) {
        let title = String::from(title).trim().to_string();
        if title.is_empty() {
            return;
        }
        let task = self.view.new_task(Uuid::new_v4().to_string(), title, local_today());
        let result = self.db.create_task(&task);
        self.finish(result);
    }

    pub fn toggle_task(self: Pin<&mut Self>, task_id: &QString) {
        let id = String::from(task_id);
        let db = &self.db;
        let result = db.get_task(&id).and_then(|task| match task {
            Some(task) if task.status == Status::Completed => db.uncomplete_task(&id),
            Some(_) => db.complete_task(&id),
            None => Ok(()),
        });
        self.finish(result);
    }

    pub fn delete_task(self: Pin<&mut Self>, task_id: &QString) {
        let result = self.db.delete_task(&String::from(task_id));
        self.finish(result);
    }

    /// `when` is a `Schedule::parse` string: today, evening, someday, none or
    /// YYYY-MM-DD.
    pub fn schedule_task(self: Pin<&mut Self>, task_id: &QString, when: &QString) {
        let Some(schedule) = Schedule::parse(&String::from(when)) else {
            return self.report(format!("'{when}' is not a schedule"));
        };
        let result = self.edit_task(&String::from(task_id), |task| {
            task.apply_schedule(&schedule, local_today())
        });
        self.finish(result);
    }

    pub fn update_task_notes(self: Pin<&mut Self>, task_id: &QString, notes: &QString) {
        let notes = String::from(notes);
        let result = self.edit_task(&String::from(task_id), |task| task.notes = notes);
        self.finish(result);
    }

    pub fn create_project(self: Pin<&mut Self>, title: &QString) {
        let title = String::from(title).trim().to_string();
        if title.is_empty() {
            return;
        }
        let project = Project::new(Uuid::new_v4().to_string(), title, 0.0);
        let result = self.db.create_project(&project);
        self.finish(result);
    }

    pub fn reveal_task(self: Pin<&mut Self>, task_id: &QString) {
        match self.db.get_task(&String::from(task_id)) {
            Ok(Some(task)) => self.set_view(View::containing(&task, local_today())),
            Ok(None) => self.report(format!("task '{task_id}' no longer exists")),
            Err(err) => self.report(err.to_string()),
        }
    }

    pub fn search(mut self: Pin<&mut Self>, query: &QString) {
        self.as_mut().rust_mut().search_query = String::from(query).trim().to_string();
        self.run_search();
    }

    pub fn clear_error(self: Pin<&mut Self>) {
        self.set_error_message(QString::default());
    }

    pub fn check_date(self: Pin<&mut Self>) {
        if self.day.day() != local_today() {
            self.changed();
        }
    }

    // ---- internals ----------------------------------------------------------

    fn edit_task(&self, id: &str, edit: impl FnOnce(&mut Task)) -> matters_core::Result<()> {
        if let Some(mut task) = self.db.get_task(id)? {
            edit(&mut task);
            self.db.update_task(&task)?;
        }
        Ok(())
    }

    fn set_view(mut self: Pin<&mut Self>, view: View) {
        self.as_mut().set_current_view(QString::from(view.key()));
        self.as_mut()
            .set_current_project_id(QString::from(view.project_id().unwrap_or_default()));
        self.as_mut().rust_mut().view = view;
        self.changed();
    }

    /// Report a command's outcome and publish the new state.
    fn finish(mut self: Pin<&mut Self>, result: matters_core::Result<()>) {
        if let Err(err) = result {
            self.as_mut().report(err.to_string());
        }
        self.changed();
    }

    fn report(self: Pin<&mut Self>, message: String) {
        eprintln!("matters: {message}");
        self.set_error_message(QString::from(message.as_str()));
    }

    /// Re-read what the sidebar and header show, then bump `revision` so the
    /// task list reloads. Only aggregate counts are queried for the sidebar;
    /// the rows of the current view are loaded by `TaskListModel`.
    fn changed(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().day.advance(local_today());
        if let Err(err) = self.as_mut().refresh_sidebar_and_header() {
            self.as_mut().report(err.to_string());
        }
        if !self.search_query.is_empty() {
            self.as_mut().run_search();
        }
        let next = self.revision.wrapping_add(1);
        self.set_revision(next);
    }

    fn refresh_sidebar_and_header(mut self: Pin<&mut Self>) -> matters_core::Result<()> {
        let (counts, projects, title, progress) = {
            let engine = QueryEngine::new(&self.db);
            let counts = engine.counts(self.day.day())?;
            let projects = self.db.list_projects()?;
            let (title, progress) = match &self.view {
                View::Project(id) => (
                    self.db.get_project(id)?.map(|p| p.title).unwrap_or_else(|| "Project".into()),
                    engine.project_progress(id)?.ratio(),
                ),
                other => (other.title().to_string(), 0.0),
            };
            (counts, projects, title, progress)
        };

        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        self.as_mut().set_inbox_count(count(counts.inbox));
        self.as_mut().set_today_count(count(counts.today));
        self.as_mut().set_upcoming_count(count(counts.upcoming));
        self.as_mut().set_anytime_count(count(counts.anytime));
        self.as_mut().set_someday_count(count(counts.someday));
        self.as_mut().set_logbook_count(count(counts.logbook));
        self.as_mut().set_projects_json(to_json(&projects));
        self.as_mut().set_current_title(QString::from(title.as_str()));
        self.as_mut().set_current_project_progress(progress);
        Ok(())
    }

    fn run_search(self: Pin<&mut Self>) {
        let result = if self.search_query.is_empty() {
            Ok(QString::from("[]"))
        } else {
            QueryEngine::new(&self.db)
                .quick_find(&self.search_query)
                .map(|found| to_json(&found.tasks))
        };
        match result {
            Ok(json) => self.set_search_results_json(json),
            Err(err) => self.report(err.to_string()),
        }
    }
}

// =============================================================================
// TaskListModel
// =============================================================================

#[repr(i32)]
#[derive(Clone, Copy)]
enum Role {
    TaskId = 0x0100, // Qt::UserRole
    Title,
    Notes,
    Done,
    StartDate,
    Evening,
}

impl Role {
    const ALL: [(Role, &'static str); 6] = [
        (Role::TaskId, "taskId"),
        (Role::Title, "title"),
        (Role::Notes, "notes"),
        (Role::Done, "done"),
        (Role::StartDate, "startDate"),
        (Role::Evening, "evening"),
    ];

    fn from_i32(role: i32) -> Option<Role> {
        Role::ALL.iter().map(|(r, _)| *r).find(|r| *r as i32 == role)
    }
}

pub struct TaskListModelRust {
    error_message: QString,
    rows: Vec<Task>,
    db: Database,
    /// Set while `load` is notifying views. A `load` requested from inside one
    /// of those notifications is queued here and run afterwards, so row
    /// mutations never interleave.
    loading: bool,
    queued: Option<(String, String)>,
}

impl Default for TaskListModelRust {
    fn default() -> Self {
        Self {
            error_message: QString::default(),
            rows: Vec::new(),
            db: database(),
            loading: false,
            queued: None,
        }
    }
}

impl qobject::TaskListModel {
    pub fn load(mut self: Pin<&mut Self>, view: &QString, project_id: &QString) {
        let mut request = (String::from(view), String::from(project_id));
        if self.loading {
            self.as_mut().rust_mut().queued = Some(request);
            return;
        }
        self.as_mut().rust_mut().loading = true;
        loop {
            self.as_mut().load_now(&request.0, &request.1);
            match self.as_mut().rust_mut().queued.take() {
                Some(next) => request = next,
                None => break,
            }
        }
        self.as_mut().rust_mut().loading = false;
    }

    fn load_now(mut self: Pin<&mut Self>, view_key: &str, project_id: &str) {
        let Some(view) = View::from_key(view_key, Some(project_id)) else {
            let message = format!("unknown view '{view_key}'");
            return self.set_error_message(QString::from(message.as_str()));
        };
        match QueryEngine::new(&self.db).tasks_in(&view, local_today()) {
            Ok(tasks) => {
                reconcile(&mut QtRows(self.as_mut()), tasks, |t| t.id.clone());
                self.set_error_message(QString::default());
            }
            Err(err) => {
                eprintln!("matters: {err}");
                self.set_error_message(QString::from(err.to_string().as_str()));
            }
        }
    }

    pub fn row_of(&self, task_id: &QString) -> i32 {
        let id = String::from(task_id);
        self.rows
            .iter()
            .position(|t| t.id == id)
            .map_or(-1, qt_row)
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = usize::try_from(index.row()).ok();
        let (Some(task), Some(role)) = (row.and_then(|r| self.rows.get(r)), Role::from_i32(role)) else {
            return QVariant::default();
        };
        match role {
            Role::TaskId => QVariant::from(&QString::from(task.id.as_str())),
            Role::Title => QVariant::from(&QString::from(task.title.as_str())),
            Role::Notes => QVariant::from(&QString::from(task.notes.as_str())),
            Role::Done => QVariant::from(&(task.status == Status::Completed)),
            Role::StartDate => QVariant::from(&QString::from(
                task.start_date.map(|d| d.to_string()).unwrap_or_default().as_str(),
            )),
            Role::Evening => QVariant::from(&task.is_evening()),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (role, name) in Role::ALL {
            roles.insert(role as i32, QByteArray::from(name));
        }
        roles
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0 // flat list: only the root has children
        } else {
            i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
        }
    }
}

/// Adapts the Qt model to [`ListSink`] so [`reconcile`] drives the
/// `begin*`/`end*` notifications around each mutation.
struct QtRows<'a>(Pin<&'a mut qobject::TaskListModel>);

fn qt_row(row: usize) -> i32 {
    i32::try_from(row).expect("row index fits in i32")
}

impl ListSink<Task> for QtRows<'_> {
    fn rows(&mut self) -> &mut Vec<Task> {
        &mut self.0.as_mut().rust_mut().get_mut().rows
    }
    fn begin_remove(&mut self, row: usize) {
        let row = qt_row(row);
        self.0.as_mut().begin_remove_rows(&QModelIndex::default(), row, row);
    }
    fn end_remove(&mut self) {
        self.0.as_mut().end_remove_rows();
    }
    fn begin_insert(&mut self, row: usize) {
        let row = qt_row(row);
        self.0.as_mut().begin_insert_rows(&QModelIndex::default(), row, row);
    }
    fn end_insert(&mut self) {
        self.0.as_mut().end_insert_rows();
    }
    fn begin_move(&mut self, from: usize, to: usize) {
        let root = QModelIndex::default();
        let from = qt_row(from);
        let moved = self.0.as_mut().begin_move_rows(&root, from, from, &root, qt_row(to));
        assert!(moved, "beginMoveRows rejected a move from {from} to {to}");
    }
    fn end_move(&mut self) {
        self.0.as_mut().end_move_rows();
    }
    fn changed(&mut self, row: usize) {
        let index = self.0.index(qt_row(row), 0, &QModelIndex::default());
        self.0.as_mut().data_changed(&index, &index, &QList::<i32>::default());
    }
}
