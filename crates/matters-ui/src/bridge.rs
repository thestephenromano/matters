//! QML-facing objects.
//!
//! * `AppController` owns navigation state, sidebar data and every command.
//!   After any change it bumps `revision`, which tells views to reload.
//! * `TaskListModel` is the task list for one view. It reconciles rows by
//!   task id so delegates (and their expansion, focus and edit state) survive
//!   reloads; see [`matters_core::list_diff`].
//! * `ChecklistModel` is the checklist of one open task, reconciled the same
//!   way by item id.
//!
//! View membership, "When" parsing and task placement all live in
//! `matters_core`; this layer only translates between QML and those types.

use core::pin::Pin;
use std::collections::HashMap;
use std::sync::OnceLock;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QVariant,
};
use matters_core::{
    local_today, Area, ChecklistItem, ChecklistProgress, Database, DateOrder, DayTracker,
    DeadlineStatus, DeletedHeading, Heading, Home, ListRow, Outcome, Project, QueryEngine,
    Schedule, Status, Task, View, Visit,
};
use uuid::Uuid;

use matters_core::list_diff::{reconcile, ListSink};

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
        /// The open area, in an area view (#15).
        #[qproperty(QString, current_area_id)]
        #[qproperty(f64, current_project_progress)]
        /// The open project's status (`open`, `completed` or `canceled`),
        /// in a project view; empty elsewhere (#55).
        #[qproperty(QString, current_project_status)]
        /// How many of the open project's tasks are still open.
        #[qproperty(i32, current_project_open_tasks)]
        #[qproperty(i32, inbox_count)]
        #[qproperty(i32, today_count)]
        #[qproperty(i32, upcoming_count)]
        #[qproperty(i32, anytime_count)]
        #[qproperty(i32, someday_count)]
        #[qproperty(i32, logbook_count)]
        #[qproperty(QString, projects_json)]
        /// Areas in the sidebar, in order.
        #[qproperty(QString, areas_json)]
        /// Trashed areas, listed in the Trash view.
        #[qproperty(QString, trashed_areas_json)]
        /// Trashed projects, listed in the Trash view.
        #[qproperty(QString, trashed_projects_json)]
        #[qproperty(QString, search_results_json)]
        #[qproperty(QString, error_message)]
        #[qproperty(i32, revision)]
        /// Incremented on every navigation; a new value starts a new visit
        /// for the task list (`matters_core::Visit`).
        #[qproperty(i32, visit)]
        /// Whether the current view is arranged by hand
        /// (`View::is_reorderable`): reordering controls show only then.
        #[qproperty(bool, reorderable)]
        /// Whether tasks can be captured in the current view
        /// (`View::accepts_new_tasks`): not in the Logbook or the Trash.
        #[qproperty(bool, accepts_new_tasks)]
        /// Whether the desktop asks for reduced motion
        /// (`crate::motion::prefers_reduced_motion`); animations are skipped.
        #[qproperty(bool, reduce_motion)]
        type AppController = super::AppControllerRust;

        #[qinvokable]
        fn select_view(self: Pin<&mut AppController>, view: &QString);
        #[qinvokable]
        fn select_project(self: Pin<&mut AppController>, project_id: &QString);
        /// Open an area: the tasks filed directly in it.
        #[qinvokable]
        fn select_area(self: Pin<&mut AppController>, area_id: &QString);
        /// Create an area at the end of the sidebar and open it; returns its id,
        /// or an empty string for a blank name.
        #[qinvokable]
        fn create_area(self: Pin<&mut AppController>, title: &QString) -> QString;
        /// Rename an area; false, changing nothing, for a blank name.
        #[qinvokable]
        fn rename_area(self: Pin<&mut AppController>, area_id: &QString, title: &QString) -> bool;
        /// Move an area, with its projects and tasks, to the Trash. If it is
        /// the open view, the Inbox is shown instead.
        #[qinvokable]
        fn trash_area(self: Pin<&mut AppController>, area_id: &QString);
        /// Put a trashed area back with its projects and tasks.
        #[qinvokable]
        fn restore_area(self: Pin<&mut AppController>, area_id: &QString);
        /// Rename a project; false, changing nothing, for a blank name.
        #[qinvokable]
        fn rename_project(
            self: Pin<&mut AppController>,
            project_id: &QString,
            title: &QString,
        ) -> bool;
        /// File a project in an area, or take it out of one (empty `area_id`).
        #[qinvokable]
        fn set_project_area(self: Pin<&mut AppController>, project_id: &QString, area_id: &QString);
        #[qinvokable]
        /// Capture a task in the current view; returns its id, or an empty
        /// string when nothing was created.
        fn add_task(self: Pin<&mut AppController>, title: &QString) -> QString;
        /// Capture a task right below `below_id` in the current view (Enter on
        /// a selected task); returns its id, or an empty string when nothing
        /// was created (a blank title, or a list not arranged by hand).
        #[qinvokable]
        fn add_task_below(
            self: Pin<&mut AppController>,
            title: &QString,
            below_id: &QString,
        ) -> QString;
        #[qinvokable]
        fn toggle_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Move a task to the Trash (recoverable until the Trash is emptied).
        #[qinvokable]
        fn trash_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Put a trashed task back where it was.
        #[qinvokable]
        fn restore_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Move a task `step` places up (negative) or down in the current
        /// view, if the view is arranged by hand (`View::is_reorderable`).
        #[qinvokable]
        fn move_task(self: Pin<&mut AppController>, task_id: &QString, step: i32);
        /// Move a task in the current view so it follows `after_id`, or comes
        /// first when `after_id` is empty (drag and drop).
        #[qinvokable]
        fn place_task(self: Pin<&mut AppController>, task_id: &QString, after_id: &QString);
        /// Put a Today task in the daytime tasks or This Evening (`evening`),
        /// after `after_id` within that section, or first when it is empty.
        /// Changing section switches This Evening (drag and drop in Today).
        #[qinvokable]
        fn place_today_task(
            self: Pin<&mut AppController>,
            task_id: &QString,
            evening: bool,
            after_id: &QString,
        );
        /// Move a project in the sidebar so it follows `after_id`, or comes
        /// first when `after_id` is empty.
        #[qinvokable]
        fn place_project(self: Pin<&mut AppController>, project_id: &QString, after_id: &QString);
        /// Permanently delete everything in the Trash.
        #[qinvokable]
        fn empty_trash(self: Pin<&mut AppController>);
        /// Move a project and its tasks to the Trash. If it is the open view,
        /// the Inbox is shown instead.
        #[qinvokable]
        fn trash_project(self: Pin<&mut AppController>, project_id: &QString);
        /// Put a trashed project back with its tasks.
        #[qinvokable]
        fn restore_project(self: Pin<&mut AppController>, project_id: &QString);
        /// Complete or cancel a project (`outcome`: `completed` or
        /// `canceled`), closing its open tasks as `open_tasks` says, the same
        /// two choices (#55). Returns false, changing nothing, when refused.
        #[qinvokable]
        fn finish_project(
            self: Pin<&mut AppController>,
            project_id: &QString,
            outcome: &QString,
            open_tasks: &QString,
        ) -> bool;
        /// Reopen a finished project, with the tasks finishing it closed.
        #[qinvokable]
        fn reopen_project(self: Pin<&mut AppController>, project_id: &QString);
        #[qinvokable]
        fn schedule_task(self: Pin<&mut AppController>, task_id: &QString, when: &QString);
        #[qinvokable]
        fn update_task_notes(self: Pin<&mut AppController>, task_id: &QString, notes: &QString);
        /// What a typed When means (`Schedule::parse_typed`): `today`,
        /// `evening`, `someday`, an ISO date, or an empty string when it is
        /// refused. `month_first` follows the user's locale (`12/24`).
        #[qinvokable]
        fn preview_when(self: &AppController, text: &QString, month_first: bool) -> QString;
        /// Schedule a task from a typed When; false, changing nothing, when the
        /// text is refused.
        #[qinvokable]
        fn schedule_task_typed(
            self: Pin<&mut AppController>,
            task_id: &QString,
            text: &QString,
            month_first: bool,
        ) -> bool;
        /// What a typed deadline means (`Schedule::parse_typed_deadline`): an
        /// ISO date, or an empty string when it is refused.
        #[qinvokable]
        fn preview_deadline(self: &AppController, text: &QString, month_first: bool) -> QString;
        /// Set a task's deadline from typed text; false, changing nothing,
        /// when the text is refused.
        #[qinvokable]
        fn set_task_deadline_typed(
            self: Pin<&mut AppController>,
            task_id: &QString,
            text: &QString,
            month_first: bool,
        ) -> bool;
        /// Set a task's deadline to an ISO date, or clear it with an empty
        /// string; false when the date isn't valid.
        #[qinvokable]
        fn set_task_deadline(
            self: Pin<&mut AppController>,
            task_id: &QString,
            date: &QString,
        ) -> bool;
        /// Rename a task. Returns false, changing nothing, when the title is
        /// blank (`Database::rename_task`); the field then shows the old title.
        #[qinvokable]
        fn rename_task(self: Pin<&mut AppController>, task_id: &QString, title: &QString) -> bool;
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

        /// Add an item to a task's checklist: right below `after_id`, or at
        /// the end when it is empty. Returns the new item's id, or an empty
        /// string when nothing was added (a blank title).
        #[qinvokable]
        fn add_checklist_item(
            self: Pin<&mut AppController>,
            task_id: &QString,
            title: &QString,
            after_id: &QString,
        ) -> QString;
        /// Rename a checklist item. Returns false, changing nothing, when the
        /// title is blank; the field then shows the old title.
        #[qinvokable]
        fn rename_checklist_item(
            self: Pin<&mut AppController>,
            item_id: &QString,
            title: &QString,
        ) -> bool;
        #[qinvokable]
        fn set_checklist_item_done(self: Pin<&mut AppController>, item_id: &QString, done: bool);
        #[qinvokable]
        fn delete_checklist_item(self: Pin<&mut AppController>, item_id: &QString);
        /// Move a checklist item `step` places up (negative) or down.
        #[qinvokable]
        fn move_checklist_item(self: Pin<&mut AppController>, item_id: &QString, step: i32);
        /// Move a checklist item to follow `after_id` (first when empty).
        #[qinvokable]
        fn place_checklist_item(
            self: Pin<&mut AppController>,
            item_id: &QString,
            after_id: &QString,
        );

        /// Add a heading at the end of the current project. Returns its id,
        /// or an empty string when nothing was added (a blank title).
        #[qinvokable]
        fn add_heading(self: Pin<&mut AppController>, title: &QString) -> QString;
        /// Rename a heading. Returns false, changing nothing, when the title
        /// is blank.
        #[qinvokable]
        fn rename_heading(
            self: Pin<&mut AppController>,
            heading_id: &QString,
            title: &QString,
        ) -> bool;
        /// Delete a heading; its tasks stay in the project, above the
        /// headings. `restore_heading` undoes the last deletion.
        #[qinvokable]
        fn delete_heading(self: Pin<&mut AppController>, heading_id: &QString);
        #[qinvokable]
        fn restore_heading(self: Pin<&mut AppController>);
        /// Move a heading, with its tasks, `step` places up or down.
        #[qinvokable]
        fn move_heading(self: Pin<&mut AppController>, heading_id: &QString, step: i32);
        /// Put a task of the current project under `heading_id` (empty: above
        /// the headings), right after `after_id` there (first when empty).
        #[qinvokable]
        fn place_project_task(
            self: Pin<&mut AppController>,
            task_id: &QString,
            heading_id: &QString,
            after_id: &QString,
        );

        /// Where the Move popover can send `task_id` (SPEC §4), as JSON, in
        /// sidebar order: the Inbox; the projects without an area, each
        /// followed by its headings; then each area with its projects. Each
        /// entry has `kind` (`inbox`, `area`, `project` or `heading`), `id`,
        /// `title`, `context` (the project or area it sits in), `depth`
        /// (for indenting) and `current` (the task's home).
        #[qinvokable]
        fn move_destinations(self: &AppController, task_id: &QString) -> QString;
        /// File a task at the end of a destination from `move_destinations`
        /// (`kind` and `id`; the Inbox has no id). Returns false, changing
        /// nothing, when it is refused.
        #[qinvokable]
        fn move_task_to(
            self: Pin<&mut AppController>,
            task_id: &QString,
            kind: &QString,
            id: &QString,
        ) -> bool;
    }

    impl cxx_qt::Initialize for AppController {}

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(QString, error_message)]
        type TaskListModel = super::TaskListModelRust;

        /// Show the tasks of `view` (a `View` key; `project_id` for projects)
        /// during navigation `visit` (`AppController.visit`).
        #[qinvokable]
        fn load(self: Pin<&mut TaskListModel>, view: &QString, project_id: &QString, visit: i32);
        /// Row of the task with this id, or -1.
        #[qinvokable]
        fn row_of(self: &TaskListModel, task_id: &QString) -> i32;
        /// Id of the task at `row`, or an empty string when out of range.
        #[qinvokable]
        fn id_at(self: &TaskListModel, row: i32) -> QString;
        /// Title of the task at `row`, or an empty string when out of range.
        #[qinvokable]
        fn title_at(self: &TaskListModel, row: i32) -> QString;
        /// Whether the task at `row` is scheduled for This Evening.
        #[qinvokable]
        fn evening_at(self: &TaskListModel, row: i32) -> bool;
        /// The heading the row at `row` is under (a task) or is (an empty
        /// heading's row), or an empty string.
        #[qinvokable]
        fn heading_at(self: &TaskListModel, row: i32) -> QString;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[inherit]
        fn begin_insert_rows(
            self: Pin<&mut TaskListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_insert_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn begin_remove_rows(
            self: Pin<&mut TaskListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
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

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        type ChecklistModel = super::ChecklistModelRust;

        /// Show the checklist of task `task_id`. Rows are reconciled by item
        /// id, so a field being edited keeps its delegate across reloads.
        #[qinvokable]
        fn load(self: Pin<&mut ChecklistModel>, task_id: &QString);
        /// Id of the item at `row`, or an empty string when out of range.
        #[qinvokable]
        fn id_at(self: &ChecklistModel, row: i32) -> QString;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[inherit]
        fn begin_insert_rows(
            self: Pin<&mut ChecklistModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_insert_rows(self: Pin<&mut ChecklistModel>);
        #[inherit]
        fn begin_remove_rows(
            self: Pin<&mut ChecklistModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_remove_rows(self: Pin<&mut ChecklistModel>);
        #[inherit]
        fn begin_move_rows(
            self: Pin<&mut ChecklistModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            destination_parent: &QModelIndex,
            destination_child: i32,
        ) -> bool;
        #[inherit]
        fn end_move_rows(self: Pin<&mut ChecklistModel>);
        #[inherit]
        fn index(self: &ChecklistModel, row: i32, column: i32, parent: &QModelIndex)
            -> QModelIndex;

        #[qsignal]
        #[inherit]
        fn data_changed(
            self: Pin<&mut ChecklistModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );

        #[cxx_override]
        fn data(self: &ChecklistModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn role_names(self: &ChecklistModel) -> QHash_i32_QByteArray;
        #[cxx_override]
        fn row_count(self: &ChecklistModel, parent: &QModelIndex) -> i32;
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
    QString::from(
        serde_json::to_string(value)
            .expect("domain types serialize")
            .as_str(),
    )
}

// =============================================================================
// AppController
// =============================================================================

pub struct AppControllerRust {
    current_view: QString,
    current_title: QString,
    current_project_id: QString,
    current_project_progress: f64,
    current_project_status: QString,
    current_project_open_tasks: i32,
    inbox_count: i32,
    today_count: i32,
    upcoming_count: i32,
    anytime_count: i32,
    someday_count: i32,
    logbook_count: i32,
    projects_json: QString,
    areas_json: QString,
    trashed_areas_json: QString,
    current_area_id: QString,
    trashed_projects_json: QString,
    search_results_json: QString,
    error_message: QString,
    revision: i32,
    visit: i32,
    reorderable: bool,
    accepts_new_tasks: bool,
    reduce_motion: bool,

    view: View,
    /// The date the sidebar and header were last built for.
    day: DayTracker,
    search_query: String,
    db: Database,
    /// The last heading deleted, for Undo (`restore_heading`).
    deleted_heading: Option<DeletedHeading>,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        let view = View::Today;
        Self {
            current_view: QString::from(view.key()),
            current_title: QString::from(view.title()),
            current_project_id: QString::default(),
            current_project_progress: 0.0,
            current_project_status: QString::default(),
            current_project_open_tasks: 0,
            inbox_count: 0,
            today_count: 0,
            upcoming_count: 0,
            anytime_count: 0,
            someday_count: 0,
            logbook_count: 0,
            projects_json: QString::from("[]"),
            areas_json: QString::from("[]"),
            trashed_areas_json: QString::from("[]"),
            current_area_id: QString::default(),
            trashed_projects_json: QString::from("[]"),
            search_results_json: QString::from("[]"),
            error_message: QString::default(),
            revision: 0,
            visit: 0,
            reorderable: view.is_reorderable(),
            accepts_new_tasks: view.accepts_new_tasks(),
            reduce_motion: crate::motion::prefers_reduced_motion(),
            view,
            day: DayTracker::new(local_today()),
            search_query: String::new(),
            deleted_heading: None,
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

    pub fn select_area(self: Pin<&mut Self>, area_id: &QString) {
        match View::from_key("area", Some(&String::from(area_id))) {
            Some(view) => self.set_view(view),
            None => self.report("no area selected".to_string()),
        }
    }

    pub fn create_area(self: Pin<&mut Self>, title: &QString) -> QString {
        let Some(title) = Task::clean_title(&String::from(title)) else {
            return QString::default();
        };
        let mut area = Area::new(Uuid::new_v4().to_string(), title, 0.0);
        match self.db.add_area(&mut area) {
            Ok(()) => {
                self.set_view(View::Area(area.id.clone()));
                QString::from(area.id.as_str())
            }
            Err(err) => {
                self.finish(Err(err));
                QString::default()
            }
        }
    }

    pub fn rename_area(self: Pin<&mut Self>, area_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_area(&String::from(area_id), &title);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn trash_area(self: Pin<&mut Self>, area_id: &QString) {
        let id = String::from(area_id);
        let result = self.db.trash_area(&id);
        // The open area, or a project in it, is gone from the sidebar.
        let open_project_in_area = match self.view.project_id() {
            Some(project) => self
                .db
                .get_project(project)
                .ok()
                .flatten()
                .is_some_and(|p| p.area_id.as_deref() == Some(id.as_str())),
            None => false,
        };
        if result.is_ok() && (self.view.area_id() == Some(id.as_str()) || open_project_in_area) {
            return self.set_view(View::Inbox);
        }
        self.finish(result);
    }

    pub fn restore_area(self: Pin<&mut Self>, area_id: &QString) {
        let result = self.db.restore_area(&String::from(area_id));
        self.finish(result);
    }

    pub fn rename_project(self: Pin<&mut Self>, project_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_project(&String::from(project_id), &title);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_project_area(self: Pin<&mut Self>, project_id: &QString, area_id: &QString) {
        let area = String::from(area_id);
        let result = self.db.set_project_area(
            &String::from(project_id),
            (!area.is_empty()).then_some(area.as_str()),
        );
        self.finish(result);
    }

    pub fn select_project(self: Pin<&mut Self>, project_id: &QString) {
        match View::from_key("project", Some(&String::from(project_id))) {
            Some(view) => self.set_view(view),
            None => self.report("no project selected".to_string()),
        }
    }

    /// Capture a task in the current view (placement rules: `View::new_task`).
    pub fn add_task_below(self: Pin<&mut Self>, title: &QString, below_id: &QString) -> QString {
        let title = String::from(title);
        let Some(mut task) = self
            .view
            .new_task(Uuid::new_v4().to_string(), &title, local_today())
        else {
            return QString::default();
        };
        let result = matters_core::ordering::add_task_below(
            &self.db,
            &self.view,
            self.day.day(),
            &mut task,
            &String::from(below_id),
        );
        let id = if result.is_ok() {
            QString::from(task.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn add_task(self: Pin<&mut Self>, title: &QString) -> QString {
        let title = String::from(title);
        let Some(mut task) = self
            .view
            .new_task(Uuid::new_v4().to_string(), &title, local_today())
        else {
            // A blank title, or the Logbook or the Trash.
            return QString::default();
        };
        let result = self.db.add_task(&mut task);
        let id = if result.is_ok() {
            QString::from(task.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn move_task(self: Pin<&mut Self>, task_id: &QString, step: i32) {
        if !self.view.is_reorderable() {
            return;
        }
        let result = matters_core::ordering::move_task(
            &self.db,
            &self.view,
            self.day.day(),
            &String::from(task_id),
            step as isize,
        );
        self.finish(result);
    }

    pub fn place_task(self: Pin<&mut Self>, task_id: &QString, after_id: &QString) {
        if !self.view.is_reorderable() {
            return;
        }
        let after = String::from(after_id);
        let result = matters_core::ordering::place_task(
            &self.db,
            &self.view,
            self.day.day(),
            &String::from(task_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn place_today_task(
        self: Pin<&mut Self>,
        task_id: &QString,
        evening: bool,
        after_id: &QString,
    ) {
        if self.view != View::Today {
            return;
        }
        let after = String::from(after_id);
        let result = matters_core::ordering::place_in_today(
            &self.db,
            self.day.day(),
            &String::from(task_id),
            evening,
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn place_project(self: Pin<&mut Self>, project_id: &QString, after_id: &QString) {
        let after = String::from(after_id);
        let result = matters_core::ordering::place_project(
            &self.db,
            &String::from(project_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn move_destinations(&self, task_id: &QString) -> QString {
        let current = self
            .db
            .get_task(&String::from(task_id))
            .ok()
            .flatten()
            .and_then(|task| Home::of(&task).ok());
        match move_destinations(&self.db, current.as_ref()) {
            Ok(destinations) => to_json(&destinations),
            Err(err) => {
                eprintln!("matters: {err}");
                QString::from("[]")
            }
        }
    }

    pub fn move_task_to(
        self: Pin<&mut Self>,
        task_id: &QString,
        kind: &QString,
        id: &QString,
    ) -> bool {
        let id = String::from(id);
        let home = match String::from(kind).as_str() {
            "inbox" => Ok(Home::Inbox),
            "area" => Ok(Home::Area(id)),
            "project" => Ok(Home::Project {
                project: id,
                heading: None,
            }),
            "heading" => match self.db.get_heading(&id) {
                Ok(Some(heading)) => Ok(Home::Project {
                    project: heading.project_id,
                    heading: Some(heading.id),
                }),
                Ok(None) => Err(matters_core::MattersError::NotFound {
                    entity: "heading",
                    id,
                }),
                Err(err) => Err(err),
            },
            other => Err(matters_core::MattersError::Validation(format!(
                "'{other}' is not a destination"
            ))),
        };
        let result = home.and_then(|home| self.db.set_task_home(&String::from(task_id), &home));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn finish_project(
        self: Pin<&mut Self>,
        project_id: &QString,
        outcome: &QString,
        open_tasks: &QString,
    ) -> bool {
        let parse = |s: &QString| match String::from(s).as_str() {
            "completed" => Ok(Outcome::Completed),
            "canceled" => Ok(Outcome::Canceled),
            other => Err(matters_core::MattersError::Validation(format!(
                "'{other}' is not how a project finishes"
            ))),
        };
        let result = parse(outcome).and_then(|outcome| {
            let open_tasks = parse(open_tasks)?;
            self.db
                .finish_project(&String::from(project_id), outcome, open_tasks)
        });
        let ok = result.is_ok();
        self.finish_project_change(result);
        ok
    }

    pub fn reopen_project(self: Pin<&mut Self>, project_id: &QString) {
        let result = self.db.reopen_project(&String::from(project_id));
        self.finish_project_change(result);
    }

    /// Finishing or reopening a project changes its tasks all at once: the
    /// list starts a new visit, so they are shown as they now are rather
    /// than kept in place like a task ticked by hand (`Visit`).
    fn finish_project_change(mut self: Pin<&mut Self>, result: matters_core::Result<()>) {
        if result.is_ok() {
            let visit = self.visit.wrapping_add(1);
            self.as_mut().set_visit(visit);
        }
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

    pub fn trash_task(self: Pin<&mut Self>, task_id: &QString) {
        let result = self.db.trash_task(&String::from(task_id));
        self.finish(result);
    }

    pub fn restore_task(self: Pin<&mut Self>, task_id: &QString) {
        let result = self.db.restore_task(&String::from(task_id));
        self.finish(result);
    }

    pub fn empty_trash(self: Pin<&mut Self>) {
        let result = self.db.empty_trash().map(drop);
        self.finish(result);
    }

    pub fn trash_project(self: Pin<&mut Self>, project_id: &QString) {
        let id = String::from(project_id);
        let result = self.db.trash_project(&id);
        if result.is_ok() && self.view.project_id() == Some(id.as_str()) {
            return self.set_view(View::Inbox);
        }
        self.finish(result);
    }

    pub fn restore_project(self: Pin<&mut Self>, project_id: &QString) {
        let result = self.db.restore_project(&String::from(project_id));
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

    fn typed_when(&self, text: &QString, month_first: bool) -> Option<Schedule> {
        let order = if month_first {
            DateOrder::MonthFirst
        } else {
            DateOrder::DayFirst
        };
        Schedule::parse_typed(&String::from(text), self.day.day(), order)
    }

    pub fn preview_when(&self, text: &QString, month_first: bool) -> QString {
        QString::from(match self.typed_when(text, month_first) {
            None => String::new(),
            Some(Schedule::Today { evening: false }) => "today".into(),
            Some(Schedule::Today { evening: true }) => "evening".into(),
            Some(Schedule::Someday) => "someday".into(),
            Some(Schedule::None) => "none".into(),
            Some(Schedule::Date(date)) => date.to_string(),
        })
    }

    pub fn schedule_task_typed(
        self: Pin<&mut Self>,
        task_id: &QString,
        text: &QString,
        month_first: bool,
    ) -> bool {
        let Some(schedule) = self.typed_when(text, month_first) else {
            return false;
        };
        let today = self.day.day();
        let result = self.edit_task(&String::from(task_id), |task| {
            task.apply_schedule(&schedule, today)
        });
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    fn date_order(month_first: bool) -> DateOrder {
        if month_first {
            DateOrder::MonthFirst
        } else {
            DateOrder::DayFirst
        }
    }

    pub fn preview_deadline(&self, text: &QString, month_first: bool) -> QString {
        let date = Schedule::parse_typed_deadline(
            &String::from(text),
            self.day.day(),
            Self::date_order(month_first),
        );
        QString::from(date.map(|d| d.to_string()).unwrap_or_default().as_str())
    }

    pub fn set_task_deadline_typed(
        self: Pin<&mut Self>,
        task_id: &QString,
        text: &QString,
        month_first: bool,
    ) -> bool {
        let order = Self::date_order(month_first);
        let Some(date) = Schedule::parse_typed_deadline(&String::from(text), self.day.day(), order)
        else {
            return false;
        };
        let result = self.edit_task(&String::from(task_id), |task| task.deadline = Some(date));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_task_deadline(self: Pin<&mut Self>, task_id: &QString, date: &QString) -> bool {
        let date = String::from(date);
        let deadline = if date.is_empty() {
            None
        } else {
            // An ISO date, read the same way as a When date.
            match Schedule::parse(&date) {
                Some(Schedule::Date(d)) => Some(d),
                _ => return false,
            }
        };
        let result = self.edit_task(&String::from(task_id), |task| task.deadline = deadline);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn rename_task(self: Pin<&mut Self>, task_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if matters_core::Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_task(&String::from(task_id), &title);
        let renamed = result.is_ok();
        self.finish(result);
        renamed
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
        let mut project = Project::new(Uuid::new_v4().to_string(), title, 0.0);
        let result = self.db.add_project(&mut project);
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
        self.as_mut()
            .set_current_area_id(QString::from(view.area_id().unwrap_or_default()));
        self.as_mut().set_reorderable(view.is_reorderable());
        self.as_mut()
            .set_accepts_new_tasks(view.accepts_new_tasks());
        self.as_mut().rust_mut().view = view;
        let visit = self.visit.wrapping_add(1);
        self.as_mut().set_visit(visit);
        self.changed();
    }

    /// Add a checklist item below `after_id` ("" for the end). Returns the
    /// new item's ID, or "" when nothing was added.
    pub fn add_checklist_item(
        self: Pin<&mut Self>,
        task_id: &QString,
        title: &QString,
        after_id: &QString,
    ) -> QString {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return QString::default();
        }
        let mut item = ChecklistItem::new(
            Uuid::new_v4().to_string(),
            String::from(task_id),
            title,
            0.0,
        );
        let after = String::from(after_id);
        let result = if after.is_empty() {
            self.db.add_checklist_item(&mut item)
        } else {
            matters_core::ordering::add_checklist_item_below(&self.db, &mut item, &after)
        };
        let id = if result.is_ok() {
            QString::from(item.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn rename_checklist_item(self: Pin<&mut Self>, item_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self
            .db
            .rename_checklist_item(&String::from(item_id), &title);
        let renamed = result.is_ok();
        self.finish(result);
        renamed
    }

    pub fn set_checklist_item_done(self: Pin<&mut Self>, item_id: &QString, done: bool) {
        let result = self
            .db
            .set_checklist_item_done(&String::from(item_id), done);
        self.finish(result);
    }

    pub fn delete_checklist_item(self: Pin<&mut Self>, item_id: &QString) {
        let result = self.db.delete_checklist_item(&String::from(item_id));
        self.finish(result);
    }

    pub fn move_checklist_item(self: Pin<&mut Self>, item_id: &QString, step: i32) {
        let result = matters_core::ordering::move_checklist_item(
            &self.db,
            &String::from(item_id),
            step as isize,
        );
        self.finish(result);
    }

    pub fn place_checklist_item(self: Pin<&mut Self>, item_id: &QString, after_id: &QString) {
        let after = String::from(after_id);
        let result = matters_core::ordering::place_checklist_item(
            &self.db,
            &String::from(item_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn add_heading(self: Pin<&mut Self>, title: &QString) -> QString {
        let title = String::from(title);
        let Some(project_id) = self.view.project_id().map(str::to_string) else {
            return QString::default();
        };
        if Task::clean_title(&title).is_none() {
            return QString::default();
        }
        let mut heading = Heading::new(Uuid::new_v4().to_string(), project_id, title, 0.0);
        let result = self.db.add_heading(&mut heading);
        let id = if result.is_ok() {
            QString::from(heading.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn rename_heading(self: Pin<&mut Self>, heading_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_heading(&String::from(heading_id), &title);
        let renamed = result.is_ok();
        self.finish(result);
        renamed
    }

    pub fn delete_heading(mut self: Pin<&mut Self>, heading_id: &QString) {
        let result = self
            .db
            .delete_heading(&String::from(heading_id))
            .map(|deleted| {
                self.as_mut().rust_mut().deleted_heading = Some(deleted);
            });
        self.finish(result);
    }

    pub fn restore_heading(mut self: Pin<&mut Self>) {
        let Some(deleted) = self.as_mut().rust_mut().deleted_heading.take() else {
            return;
        };
        let result = self.db.restore_heading(&deleted);
        self.finish(result);
    }

    pub fn move_heading(self: Pin<&mut Self>, heading_id: &QString, step: i32) {
        let result = matters_core::ordering::move_heading(
            &self.db,
            &String::from(heading_id),
            step as isize,
        );
        self.finish(result);
    }

    pub fn place_project_task(
        self: Pin<&mut Self>,
        task_id: &QString,
        heading_id: &QString,
        after_id: &QString,
    ) {
        let Some(project_id) = self.view.project_id().map(str::to_string) else {
            return;
        };
        let heading = String::from(heading_id);
        let after = String::from(after_id);
        let result = matters_core::ordering::place_in_heading(
            &self.db,
            &project_id,
            self.day.day(),
            &String::from(task_id),
            (!heading.is_empty()).then_some(heading.as_str()),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
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
        let (
            counts,
            projects,
            trashed_projects,
            areas,
            trashed_areas,
            title,
            progress,
            status,
            open_tasks,
        ) = {
            let engine = QueryEngine::new(&self.db);
            let counts = engine.counts(self.day.day())?;
            let projects = self.db.list_projects()?;
            let trashed_projects = self.db.list_trashed_projects()?;
            let areas = self.db.list_areas()?;
            let trashed_areas = self.db.list_trashed_areas()?;
            let mut status = "";
            let mut open_tasks = 0;
            let (title, progress) = match &self.view {
                View::Project(id) => {
                    let project = self.db.get_project(id)?;
                    let progress = engine.project_progress(id)?;
                    status = project.as_ref().map_or("", |p| p.status.as_str());
                    open_tasks = progress.total_tasks - progress.completed_tasks;
                    (
                        project.map_or_else(|| "Project".into(), |p| p.title),
                        progress.ratio(),
                    )
                }
                View::Area(id) => (
                    self.db
                        .get_area(id)?
                        .map(|a| a.title)
                        .unwrap_or_else(|| "Area".into()),
                    0.0,
                ),
                other => (other.title().to_string(), 0.0),
            };
            (
                counts,
                projects,
                trashed_projects,
                areas,
                trashed_areas,
                title,
                progress,
                status,
                open_tasks,
            )
        };

        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        self.as_mut().set_inbox_count(count(counts.inbox));
        self.as_mut().set_today_count(count(counts.today));
        self.as_mut().set_upcoming_count(count(counts.upcoming));
        self.as_mut().set_anytime_count(count(counts.anytime));
        self.as_mut().set_someday_count(count(counts.someday));
        self.as_mut().set_logbook_count(count(counts.logbook));
        self.as_mut().set_projects_json(to_json(&projects));
        self.as_mut()
            .set_trashed_projects_json(to_json(&trashed_projects));
        self.as_mut().set_areas_json(to_json(&areas));
        self.as_mut()
            .set_trashed_areas_json(to_json(&trashed_areas));
        self.as_mut()
            .set_current_title(QString::from(title.as_str()));
        self.as_mut().set_current_project_progress(progress);
        self.as_mut()
            .set_current_project_status(QString::from(status));
        self.as_mut()
            .set_current_project_open_tasks(count(open_tasks));
        Ok(())
    }

    fn run_search(self: Pin<&mut Self>) {
        let result = if self.search_query.is_empty() {
            Ok(QString::from("[]"))
        } else {
            QueryEngine::new(&self.db)
                .quick_find(&self.search_query)
                .map(|found| to_json(&search_results(&found)))
        };
        match result {
            Ok(json) => self.set_search_results_json(json),
            Err(err) => self.report(err.to_string()),
        }
    }
}

/// One Quick Find result, as QML lists it: what it is (`area`, `project` or
/// `task`), its id and its title.
#[derive(serde::Serialize)]
struct SearchResult<'a> {
    kind: &'static str,
    id: &'a str,
    title: &'a str,
}

/// Quick Find's results in the order they are listed: areas, then projects,
/// then tasks (#26).
fn search_results(found: &matters_core::QuickFindResult) -> Vec<SearchResult<'_>> {
    let areas = found.areas.iter().map(|a| SearchResult {
        kind: "area",
        id: &a.id,
        title: &a.title,
    });
    let projects = found.projects.iter().map(|p| SearchResult {
        kind: "project",
        id: &p.id,
        title: &p.title,
    });
    let tasks = found.tasks.iter().map(|t| SearchResult {
        kind: "task",
        id: &t.id,
        title: &t.title,
    });
    areas.chain(projects).chain(tasks).collect()
}

/// One entry of the Move popover (`AppController::move_destinations`).
#[derive(serde::Serialize)]
struct MoveDestination {
    kind: &'static str,
    id: String,
    title: String,
    context: String,
    depth: u8,
    current: bool,
}

/// The Move popover's destinations, in sidebar order, with `current` (the
/// task's home) marked.
fn move_destinations(
    db: &Database,
    current: Option<&Home>,
) -> matters_core::Result<Vec<MoveDestination>> {
    let mut list = vec![MoveDestination {
        kind: "inbox",
        id: String::new(),
        title: "Inbox".into(),
        context: String::new(),
        depth: 0,
        current: current == Some(&Home::Inbox),
    }];
    let projects = db.list_projects()?;
    let add_project = |list: &mut Vec<MoveDestination>,
                       project: &Project,
                       context: &str,
                       depth: u8|
     -> matters_core::Result<()> {
        let here = |heading: Option<&str>| {
            current
                == Some(&Home::Project {
                    project: project.id.clone(),
                    heading: heading.map(str::to_string),
                })
        };
        list.push(MoveDestination {
            kind: "project",
            id: project.id.clone(),
            title: project.title.clone(),
            context: context.to_string(),
            depth,
            current: here(None),
        });
        for heading in db.list_headings_for_project(&project.id)? {
            list.push(MoveDestination {
                kind: "heading",
                current: here(Some(&heading.id)),
                id: heading.id,
                title: heading.title,
                context: project.title.clone(),
                depth: depth + 1,
            });
        }
        Ok(())
    };
    for project in projects.iter().filter(|p| p.area_id.is_none()) {
        add_project(&mut list, project, "", 0)?;
    }
    for area in db.list_areas()? {
        list.push(MoveDestination {
            kind: "area",
            current: current == Some(&Home::Area(area.id.clone())),
            id: area.id.clone(),
            title: area.title.clone(),
            context: String::new(),
            depth: 0,
        });
        for project in projects
            .iter()
            .filter(|p| p.area_id.as_deref() == Some(area.id.as_str()))
        {
            add_project(&mut list, project, &area.title, 1)?;
        }
    }
    Ok(list)
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
    /// ISO date, or empty.
    Deadline,
    /// `DeadlineStatus` on today: none, set, today or overdue.
    DeadlineStatus,
    /// The section the row sits under (`QueryEngine::section_keys`).
    Section,
    /// Whether the task is deferred to Someday.
    Someday,
    /// `task`, or `heading` for a heading with no tasks to list (#22).
    Kind,
    /// The heading a task is under, or an empty heading's own id.
    HeadingId,
    /// Checklist items done, and in all (0 without a checklist).
    ChecklistDone,
    ChecklistTotal,
}

impl Role {
    const ALL: [(Role, &'static str); 14] = [
        (Role::TaskId, "taskId"),
        (Role::Title, "title"),
        (Role::Notes, "notes"),
        (Role::Done, "done"),
        (Role::StartDate, "startDate"),
        (Role::Evening, "evening"),
        (Role::Deadline, "deadline"),
        (Role::DeadlineStatus, "deadlineStatus"),
        (Role::Section, "section"),
        (Role::Someday, "someday"),
        (Role::ChecklistDone, "checklistDone"),
        (Role::ChecklistTotal, "checklistTotal"),
        (Role::Kind, "kind"),
        (Role::HeadingId, "headingId"),
    ];

    fn from_i32(role: i32) -> Option<Role> {
        Role::ALL
            .iter()
            .map(|(r, _)| *r)
            .find(|r| *r as i32 == role)
    }
}

/// The key a row is reconciled by: a task's id, or `heading:<id>` for an
/// empty heading's row (task ids are UUIDs, so the two never meet).
fn row_key(row: &ListRow) -> String {
    match row {
        ListRow::Task(task) => task.id.clone(),
        ListRow::EmptyHeading(heading) => format!("heading:{}", heading.id),
        ListRow::Project(project) => format!("project:{}", project.id),
    }
}

fn row_task(row: &ListRow) -> Option<&Task> {
    match row {
        ListRow::Task(task) => Some(task),
        ListRow::EmptyHeading(_) | ListRow::Project(_) => None,
    }
}

pub struct TaskListModelRust {
    error_message: QString,
    rows: Vec<ListRow>,
    /// Section key per row (`row_key`), #25.
    sections: HashMap<String, String>,
    /// Checklist progress per listed task id that has a checklist (#21).
    progress: HashMap<String, ChecklistProgress>,
    db: Database,
    /// Keeps tasks ticked during this visit in place (see `matters_core::Visit`).
    visit: Visit,
    /// Set while `load` is notifying views. A `load` requested from inside one
    /// of those notifications is queued here and run afterwards, so row
    /// mutations never interleave.
    loading: bool,
    queued: Option<(String, String, i32)>,
    /// The navigation the current rows belong to.
    visit_id: Option<i32>,
}

impl Default for TaskListModelRust {
    fn default() -> Self {
        Self {
            error_message: QString::default(),
            rows: Vec::new(),
            db: database(),
            visit: Visit::new(),
            loading: false,
            queued: None,
            visit_id: None,
            sections: HashMap::new(),
            progress: HashMap::new(),
        }
    }
}

impl qobject::TaskListModel {
    pub fn load(mut self: Pin<&mut Self>, view: &QString, project_id: &QString, visit: i32) {
        let mut request = (String::from(view), String::from(project_id), visit);
        if self.loading {
            self.as_mut().rust_mut().queued = Some(request);
            return;
        }
        self.as_mut().rust_mut().loading = true;
        loop {
            self.as_mut().load_now(&request.0, &request.1, request.2);
            match self.as_mut().rust_mut().queued.take() {
                Some(next) => request = next,
                None => break,
            }
        }
        self.as_mut().rust_mut().loading = false;
    }

    fn load_now(mut self: Pin<&mut Self>, view_key: &str, project_id: &str, visit: i32) {
        let Some(view) = View::from_key(view_key, Some(project_id)) else {
            let message = format!("unknown view '{view_key}'");
            return self.set_error_message(QString::from(message.as_str()));
        };
        let target = {
            let this = self.as_mut().rust_mut().get_mut();
            if this.visit_id != Some(visit) {
                this.visit_id = Some(visit);
                this.visit.restart();
            }
            let db = &this.db;
            let engine = QueryEngine::new(db);
            engine
                .tasks_in(&view, local_today())
                .and_then(|members| {
                    let shown: Vec<Task> = this.rows.iter().filter_map(row_task).cloned().collect();
                    this.visit
                        .rows(&view, members, &shown, |id| db.get_task(id))
                })
                .and_then(|tasks| {
                    let keys = engine.section_keys(&view, &tasks)?;
                    let mut sections: HashMap<String, String> =
                        tasks.iter().map(|t| t.id.clone()).zip(keys).collect();
                    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
                    let progress = db.checklist_progress(&ids)?;
                    let rows = engine.list_rows(&view, tasks)?;
                    for row in &rows {
                        let key = match row {
                            ListRow::EmptyHeading(h) => {
                                matters_core::heading_section_key(&h.id, &h.title)
                            }
                            ListRow::Project(p) => {
                                matters_core::logbook_section_key(p.completed_at)
                            }
                            ListRow::Task(_) => continue,
                        };
                        sections.insert(row_key(row), key);
                    }
                    Ok((rows, sections, progress))
                })
        };
        match target {
            Ok((rows, sections, progress)) => {
                let this = self.as_mut().rust_mut().get_mut();
                let old_sections = std::mem::replace(&mut this.sections, sections);
                let old_progress = std::mem::replace(&mut this.progress, progress);
                reconcile(&mut QtRows(self.as_mut()), rows, row_key);
                // A row whose task is unchanged can still move to another
                // section (the Logbook after midnight, say) or have its
                // checklist ticked: refresh those.
                let moved: Vec<usize> = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        let key = row_key(row);
                        old_sections.get(&key) != self.sections.get(&key)
                            || old_progress.get(&key) != self.progress.get(&key)
                    })
                    .map(|(i, _)| i)
                    .collect();
                for row in moved {
                    QtRows(self.as_mut()).changed(row);
                }
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
            .position(|row| row_task(row).is_some_and(|t| t.id == id))
            .map_or(-1, qt_row)
    }

    pub fn id_at(&self, row: i32) -> QString {
        self.task_at(row)
            .map_or_else(QString::default, |task| QString::from(task.id.as_str()))
    }

    pub fn title_at(&self, row: i32) -> QString {
        self.task_at(row)
            .map_or_else(QString::default, |task| QString::from(task.title.as_str()))
    }

    pub fn evening_at(&self, row: i32) -> bool {
        self.task_at(row).is_some_and(Task::is_evening)
    }

    pub fn heading_at(&self, row: i32) -> QString {
        let heading = match usize::try_from(row).ok().and_then(|r| self.rows.get(r)) {
            Some(ListRow::Task(task)) => task.heading_id.as_deref(),
            Some(ListRow::EmptyHeading(heading)) => Some(heading.id.as_str()),
            Some(ListRow::Project(_)) | None => None,
        };
        QString::from(heading.unwrap_or_default())
    }

    fn task_at(&self, row: i32) -> Option<&Task> {
        usize::try_from(row)
            .ok()
            .and_then(|r| self.rows.get(r))
            .and_then(row_task)
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = usize::try_from(index.row()).ok();
        let (Some(row), Some(role)) = (row.and_then(|r| self.rows.get(r)), Role::from_i32(role))
        else {
            return QVariant::default();
        };
        let task = match row {
            ListRow::Task(task) => task,
            ListRow::EmptyHeading(heading) => return self.heading_data(row, heading, role),
            ListRow::Project(project) => return self.project_data(row, project, role),
        };
        match role {
            Role::Kind => QVariant::from(&QString::from("task")),
            Role::HeadingId => QVariant::from(&QString::from(
                task.heading_id.as_deref().unwrap_or_default(),
            )),
            Role::TaskId => QVariant::from(&QString::from(task.id.as_str())),
            Role::Title => QVariant::from(&QString::from(task.title.as_str())),
            Role::Notes => QVariant::from(&QString::from(task.notes.as_str())),
            Role::Done => QVariant::from(&(task.status == Status::Completed)),
            Role::StartDate => QVariant::from(&QString::from(
                task.start_date
                    .map(|d| d.to_string())
                    .unwrap_or_default()
                    .as_str(),
            )),
            Role::Evening => QVariant::from(&task.is_evening()),
            Role::Deadline => QVariant::from(&QString::from(
                task.deadline
                    .map(|d| d.to_string())
                    .unwrap_or_default()
                    .as_str(),
            )),
            Role::Someday => QVariant::from(&task.is_someday),
            Role::Section => QVariant::from(&QString::from(
                self.sections
                    .get(&task.id)
                    .map(String::as_str)
                    .unwrap_or_default(),
            )),
            Role::ChecklistDone => {
                let done = self.progress.get(&task.id).map_or(0, |p| p.done);
                QVariant::from(&i32::try_from(done).unwrap_or(i32::MAX))
            }
            Role::ChecklistTotal => {
                let total = self.progress.get(&task.id).map_or(0, |p| p.total);
                QVariant::from(&i32::try_from(total).unwrap_or(i32::MAX))
            }
            Role::DeadlineStatus => {
                QVariant::from(&QString::from(match task.deadline_status(local_today()) {
                    DeadlineStatus::None => "none",
                    DeadlineStatus::Set => "set",
                    DeadlineStatus::DueToday => "today",
                    DeadlineStatus::Overdue => "overdue",
                }))
            }
        }
    }

    /// A finished project's row in the Logbook: its kind, id (as `taskId`),
    /// title, whether it was completed, and section; the other roles are
    /// empty.
    fn project_data(&self, row: &ListRow, project: &Project, role: Role) -> QVariant {
        match role {
            Role::Kind => QVariant::from(&QString::from("project")),
            Role::TaskId => QVariant::from(&QString::from(project.id.as_str())),
            Role::Title => QVariant::from(&QString::from(project.title.as_str())),
            Role::Done => QVariant::from(&(project.status == Status::Completed)),
            Role::Section => QVariant::from(&QString::from(
                self.sections
                    .get(&row_key(row))
                    .map(String::as_str)
                    .unwrap_or_default(),
            )),
            _ => QVariant::default(),
        }
    }

    /// An empty heading's row: its kind, id, title and section; the task
    /// roles are empty.
    fn heading_data(&self, row: &ListRow, heading: &Heading, role: Role) -> QVariant {
        match role {
            Role::Kind => QVariant::from(&QString::from("heading")),
            Role::HeadingId => QVariant::from(&QString::from(heading.id.as_str())),
            Role::Title => QVariant::from(&QString::from(heading.title.as_str())),
            Role::Section => QVariant::from(&QString::from(
                self.sections
                    .get(&row_key(row))
                    .map(String::as_str)
                    .unwrap_or_default(),
            )),
            Role::Done | Role::Evening | Role::Someday => QVariant::from(&false),
            Role::ChecklistDone | Role::ChecklistTotal => QVariant::from(&0_i32),
            Role::TaskId
            | Role::Notes
            | Role::StartDate
            | Role::Deadline
            | Role::DeadlineStatus => QVariant::from(&QString::default()),
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

fn qt_row(row: usize) -> i32 {
    i32::try_from(row).expect("row index fits in i32")
}

/// Declares a [`ListSink`] adapter for a flat Qt list model, so
/// [`reconcile`] drives the `begin*`/`end*` notifications around each
/// mutation. One definition serves every model.
macro_rules! qt_list_sink {
    ($(#[$doc:meta])* $sink:ident, $model:ty, $row:ty) => {
        $(#[$doc])*
        struct $sink<'a>(Pin<&'a mut $model>);

        impl ListSink<$row> for $sink<'_> {
            fn rows(&mut self) -> &mut Vec<$row> {
                &mut self.0.as_mut().rust_mut().get_mut().rows
            }
            fn begin_remove(&mut self, row: usize) {
                let row = qt_row(row);
                self.0
                    .as_mut()
                    .begin_remove_rows(&QModelIndex::default(), row, row);
            }
            fn end_remove(&mut self) {
                self.0.as_mut().end_remove_rows();
            }
            fn begin_insert(&mut self, row: usize) {
                let row = qt_row(row);
                self.0
                    .as_mut()
                    .begin_insert_rows(&QModelIndex::default(), row, row);
            }
            fn end_insert(&mut self) {
                self.0.as_mut().end_insert_rows();
            }
            fn begin_move(&mut self, from: usize, to: usize) {
                let root = QModelIndex::default();
                let from = qt_row(from);
                let moved = self
                    .0
                    .as_mut()
                    .begin_move_rows(&root, from, from, &root, qt_row(to));
                assert!(moved, "beginMoveRows rejected a move from {from} to {to}");
            }
            fn end_move(&mut self) {
                self.0.as_mut().end_move_rows();
            }
            fn changed(&mut self, row: usize) {
                let index = self.0.index(qt_row(row), 0, &QModelIndex::default());
                self.0
                    .as_mut()
                    .data_changed(&index, &index, &QList::<i32>::default());
            }
        }
    };
}

qt_list_sink!(
    /// The task list model as a [`ListSink`].
    QtRows,
    qobject::TaskListModel,
    ListRow
);

// =============================================================================
// ChecklistModel
// =============================================================================

#[repr(i32)]
#[derive(Clone, Copy)]
enum ItemRole {
    ItemId = 0x0100, // Qt::UserRole
    Title,
    Done,
}

impl ItemRole {
    const ALL: [(ItemRole, &'static str); 3] = [
        (ItemRole::ItemId, "itemId"),
        (ItemRole::Title, "title"),
        (ItemRole::Done, "done"),
    ];

    fn from_i32(role: i32) -> Option<ItemRole> {
        ItemRole::ALL
            .iter()
            .map(|(r, _)| *r)
            .find(|r| *r as i32 == role)
    }
}

pub struct ChecklistModelRust {
    rows: Vec<ChecklistItem>,
    db: Database,
}

impl Default for ChecklistModelRust {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            db: database(),
        }
    }
}

impl qobject::ChecklistModel {
    pub fn load(mut self: Pin<&mut Self>, task_id: &QString) {
        match self.db.list_checklist_items(&String::from(task_id)) {
            Ok(items) => reconcile(&mut ChecklistRows(self.as_mut()), items, |i| i.id.clone()),
            Err(err) => eprintln!("matters: {err}"),
        }
    }

    pub fn id_at(&self, row: i32) -> QString {
        usize::try_from(row)
            .ok()
            .and_then(|r| self.rows.get(r))
            .map_or_else(QString::default, |item| QString::from(item.id.as_str()))
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = usize::try_from(index.row()).ok();
        let (Some(item), Some(role)) =
            (row.and_then(|r| self.rows.get(r)), ItemRole::from_i32(role))
        else {
            return QVariant::default();
        };
        match role {
            ItemRole::ItemId => QVariant::from(&QString::from(item.id.as_str())),
            ItemRole::Title => QVariant::from(&QString::from(item.title.as_str())),
            ItemRole::Done => QVariant::from(&item.is_completed),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (role, name) in ItemRole::ALL {
            roles.insert(role as i32, QByteArray::from(name));
        }
        roles
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
        }
    }
}

qt_list_sink!(
    /// The checklist model as a [`ListSink`].
    ChecklistRows,
    qobject::ChecklistModel,
    ChecklistItem
);
