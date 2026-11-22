//! The main window: sidebar, task list and the commands behind them.
//!
//! View membership, task placement and scheduling rules come from
//! `matters_core`; this module builds widgets and turns user actions into
//! core calls. Every failed command is shown as a toast.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    glib, Align, Box as GtkBox, Entry, Label, ListBox, Orientation, ScrolledWindow, SelectionMode,
    Separator,
};
use libadwaita::prelude::*;
use libadwaita::{
    ActionRow, AlertDialog, Application, ApplicationWindow, HeaderBar, NavigationPage,
    NavigationSplitView, ResponseAppearance, Toast, ToastOverlay,
};
use matters_core::list_diff::{reconcile, ListSink};
use matters_core::{
    local_today, Database, DayTracker, Project, QueryEngine, Schedule, Task, View, Visit,
};
use uuid::Uuid;

use crate::sync::SyncPanel;
use crate::task_row::TaskRow;

/// The sidebar views GTK offers. GTK has no way to delete a task, so it has no
/// Trash either: Put Back and Empty Trash are Qt-only (SPEC §3.7).
pub(crate) fn nav_views() -> Vec<View> {
    View::sidebar()
        .into_iter()
        .filter(|v| *v != View::Trash)
        .collect()
}

pub(crate) struct State {
    pub(crate) view: View,
    /// The date the window was last built for.
    day: DayTracker,
    /// Projects in sidebar order; a project row's index selects from here.
    pub(crate) projects: Vec<Project>,
    /// Keeps tasks ticked during this visit in place (see `matters_core::Visit`).
    visit: Visit,
    /// The tasks the list currently shows.
    pub(crate) shown: Vec<Task>,
}

pub(crate) struct Ui {
    pub(crate) db: Database,
    pub(crate) state: RefCell<State>,
    pub(crate) window: ApplicationWindow,
    toasts: ToastOverlay,
    pub(crate) nav_list: ListBox,
    pub(crate) nav_counts: Vec<Label>,
    pub(crate) proj_list: ListBox,
    #[cfg_attr(not(test), allow(dead_code))] // read by tests
    pub(crate) new_task_entry: Entry,
    task_list: ListBox,
    /// One widget per entry of `State::shown`, in the same order.
    pub(crate) task_rows: RefCell<Vec<TaskRow>>,
    pub(crate) content_page: NavigationPage,
    /// Sync (#42): its button in the sidebar, and its window.
    pub(crate) sync: Rc<SyncPanel>,
}

impl Ui {
    /// Build and show the window. `app` is `None` in tests. `sync` is the
    /// sync session, with the flag it sets when its view changes; `None`
    /// when this device can't sync.
    pub(crate) fn build(
        app: Option<&Application>,
        db: Database,
        sync: Option<(
            matters_sync_app::SyncApp,
            std::sync::Arc<std::sync::atomic::AtomicBool>,
        )>,
    ) -> Rc<Ui> {
        let window = ApplicationWindow::builder()
            .title("Matters (GTK)")
            .default_width(960)
            .default_height(680)
            .build();
        if let Some(app) = app {
            window.set_application(Some(app));
        }

        // ---- Sidebar --------------------------------------------------------
        let sidebar_box = GtkBox::new(Orientation::Vertical, 6);
        sidebar_box.set_margin_top(12);
        sidebar_box.set_margin_bottom(12);
        sidebar_box.set_margin_start(12);
        sidebar_box.set_margin_end(12);

        let nav_list = ListBox::new();
        nav_list.set_selection_mode(SelectionMode::Single);
        nav_list.add_css_class("navigation-sidebar");
        let mut nav_counts = Vec::new();
        for view in nav_views() {
            let icon = match view {
                View::Inbox => "📥",
                View::Today => "⭐",
                View::Upcoming => "📅",
                View::Anytime => "📦",
                View::Someday => "🗃️",
                View::Logbook => "📓",
                View::Trash | View::Project(_) | View::Area(_) => "",
            };
            let row = ActionRow::builder()
                .title(format!("{icon} {}", view.title()))
                .activatable(true)
                .build();
            let count = Label::builder()
                .css_classes(["dim-label", "caption"])
                .build();
            row.add_suffix(&count);
            nav_counts.push(count);
            nav_list.append(&row);
        }
        sidebar_box.append(&nav_list);

        let sep = Separator::new(Orientation::Horizontal);
        sep.set_margin_top(8);
        sep.set_margin_bottom(8);
        sidebar_box.append(&sep);

        let proj_header = GtkBox::new(Orientation::Horizontal, 8);
        proj_header.append(
            &Label::builder()
                .label("PROJECTS")
                .halign(Align::Start)
                .hexpand(true)
                .css_classes(["dim-label", "caption", "bold"])
                .build(),
        );
        let add_proj_btn = gtk4::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Project")
            .css_classes(["flat"])
            .build();
        proj_header.append(&add_proj_btn);
        sidebar_box.append(&proj_header);

        let proj_list = ListBox::new();
        proj_list.set_selection_mode(SelectionMode::Single);
        proj_list.add_css_class("navigation-sidebar");
        sidebar_box.append(&proj_list);

        // Sync (#42), at the foot of the sidebar.
        let (sync_app, sync_dirty) = match sync {
            Some((app, dirty)) => (Some(app), Some(dirty)),
            None => (None, None),
        };
        let sync = SyncPanel::build(sync_app, &window);
        sync.button.set_halign(Align::Start);
        sync.button.set_margin_top(8);
        sidebar_box.append(&sync.button);

        let sidebar_page = NavigationPage::builder()
            .title("Matters")
            .child(&ScrolledWindow::builder().child(&sidebar_box).build())
            .build();

        // ---- Content --------------------------------------------------------
        let content_box = GtkBox::new(Orientation::Vertical, 16);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(24);
        content_box.set_margin_start(28);
        content_box.set_margin_end(28);

        let new_task_entry = Entry::builder()
            .placeholder_text("New to-do... (Press Enter to add)")
            .hexpand(true)
            .build();
        content_box.append(&new_task_entry);

        let task_list = ListBox::new();
        task_list.set_selection_mode(SelectionMode::None);
        task_list.add_css_class("boxed-list");
        content_box.append(
            &ScrolledWindow::builder()
                .child(&task_list)
                .vexpand(true)
                .hexpand(true)
                .build(),
        );

        let content_page_box = GtkBox::new(Orientation::Vertical, 0);
        content_page_box.append(&HeaderBar::new());
        content_page_box.append(&content_box);
        let content_page = NavigationPage::builder()
            .title(View::Today.title())
            .child(&content_page_box)
            .build();

        let split_view = NavigationSplitView::new();
        split_view.set_sidebar(Some(&sidebar_page));
        split_view.set_content(Some(&content_page));
        let toasts = ToastOverlay::new();
        toasts.set_child(Some(&split_view));
        window.set_content(Some(&toasts));

        let ui = Rc::new(Ui {
            db,
            state: RefCell::new(State {
                view: View::Today,
                day: DayTracker::new(local_today()),
                projects: Vec::new(),
                visit: Visit::new(),
                shown: Vec::new(),
            }),
            window: window.clone(),
            toasts,
            nav_list: nav_list.clone(),
            nav_counts,
            proj_list: proj_list.clone(),
            new_task_entry: new_task_entry.clone(),
            task_list,
            task_rows: RefCell::new(Vec::new()),
            content_page,
            sync,
        });
        // What other devices changed shows once a round has finished.
        if let Some(dirty) = sync_dirty {
            let weak = Rc::downgrade(&ui);
            ui.sync.follow(dirty, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.refresh();
                }
            });
        }

        // ---- Events ---------------------------------------------------------
        {
            let ui = ui.clone();
            nav_list.connect_row_activated(move |_, row| {
                let index = usize::try_from(row.index()).ok();
                if let Some(view) = index.and_then(|i| nav_views().get(i).cloned()) {
                    ui.select(view);
                }
            });
        }
        {
            let ui = ui.clone();
            proj_list.connect_row_activated(move |_, row| {
                let index = usize::try_from(row.index()).ok();
                let project =
                    index.and_then(|i| ui.state.borrow().projects.get(i).map(|p| p.id.clone()));
                if let Some(id) = project {
                    ui.select(View::Project(id));
                }
            });
        }
        {
            let ui = ui.clone();
            new_task_entry.connect_activate(move |entry| {
                let title = entry.text();
                let Some(mut task) = ui.state.borrow().view.new_task(
                    Uuid::new_v4().to_string(),
                    &title,
                    local_today(),
                ) else {
                    return; // a blank title (the entry is hidden where tasks can't be captured)
                };
                if ui.check(ui.db.add_task(&mut task)) {
                    entry.set_text("");
                }
                ui.refresh();
            });
        }
        {
            let ui = ui.clone();
            add_proj_btn.connect_clicked(move |_| ui.prompt_new_project());
        }

        // Today, Upcoming and the counts depend on the date. Check it every
        // minute; a repeating check also catches resume and clock changes.
        {
            let ui = ui.clone();
            glib::timeout_add_seconds_local(60, move || {
                if ui.state.borrow().day.day() != local_today() {
                    ui.refresh();
                }
                glib::ControlFlow::Continue
            });
        }

        ui.select(View::Today);
        window.present();
        ui
    }

    /// Show `result`'s error, if any, as a toast. Returns whether it succeeded.
    fn check<T>(&self, result: matters_core::Result<T>) -> bool {
        match result {
            Ok(_) => {
                // A local change: send it soon (#42).
                if let Some(app) = &self.sync.app {
                    app.sync_now();
                }
                true
            }
            Err(err) => {
                eprintln!("matters-gtk: {err}");
                self.toasts.add_toast(Toast::new(&err.to_string()));
                false
            }
        }
    }

    /// Navigate to `view`; this always starts a new visit.
    pub(crate) fn select(self: &Rc<Self>, view: View) {
        {
            let mut state = self.state.borrow_mut();
            state.view = view;
            state.visit.restart();
        }
        self.refresh();
    }

    fn prompt_new_project(self: &Rc<Self>) {
        let dialog = AlertDialog::new(Some("New Project"), None);
        let entry = Entry::builder()
            .placeholder_text("Project title")
            .activates_default(true)
            .build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("create", "Create");
        dialog.set_response_appearance("create", ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");

        let ui = self.clone();
        dialog.connect_response(Some("create"), move |_, _| ui.create_project(&entry.text()));
        dialog.present(Some(&self.window));
    }

    /// Create a project and open it. Blank titles are ignored.
    pub(crate) fn create_project(self: &Rc<Self>, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let mut project = Project::new(Uuid::new_v4().to_string(), title, 0.0);
        if self.check(self.db.add_project(&mut project)) {
            self.select(View::Project(project.id));
        }
    }

    // ---- Task commands (called from task rows) ------------------------------
    //
    // These run inside a row's signal handler, so the list is refreshed from
    // the main loop afterwards rather than re-entrantly.

    pub(crate) fn set_done(self: &Rc<Self>, id: &str, done: bool) {
        let result = if done {
            self.db.complete_task(id)
        } else {
            self.db.uncomplete_task(id)
        };
        self.check(result);
        self.refresh_soon();
    }

    pub(crate) fn schedule(self: &Rc<Self>, id: &str, schedule: &Schedule) {
        let result = gone_task_is_fine(self.db.set_task_schedule(id, schedule, local_today()));
        self.check(result);
        self.refresh_soon();
    }

    /// Save notes from the main loop: this can be triggered by a focus change
    /// while `refresh` is replacing rows.
    pub(crate) fn save_notes(self: &Rc<Self>, id: &str, notes: &str) {
        let (ui, id, notes) = (self.clone(), id.to_string(), notes.to_string());
        glib::idle_add_local_once(move || {
            let unchanged = ui
                .state
                .borrow()
                .shown
                .iter()
                .any(|t| t.id == id && t.notes == notes);
            if !unchanged {
                let result = gone_task_is_fine(ui.db.set_task_notes(&id, &notes));
                ui.check(result);
                ui.refresh();
            }
        });
    }

    fn refresh_soon(self: &Rc<Self>) {
        let ui = self.clone();
        glib::idle_add_local_once(move || ui.refresh());
    }

    /// Re-read everything the window shows from the database.
    pub(crate) fn refresh(self: &Rc<Self>) {
        let engine = QueryEngine::new(&self.db);
        let today = local_today();
        self.state.borrow_mut().day.advance(today);
        let view = self.state.borrow().view.clone();
        self.new_task_entry.set_visible(view.accepts_new_tasks());

        // Sidebar counts.
        if let Some(counts) = self.check_value(engine.counts(today)) {
            let values = [
                counts.inbox,
                counts.today,
                counts.upcoming,
                counts.anytime,
                counts.someday,
                counts.logbook,
            ];
            for (label, n) in self.nav_counts.iter().zip(values) {
                label.set_label(&if n > 0 { n.to_string() } else { String::new() });
            }
        }

        // Projects list.
        let projects = self
            .check_value(self.db.list_projects())
            .unwrap_or_default();
        while let Some(child) = self.proj_list.first_child() {
            self.proj_list.remove(&child);
        }
        for project in &projects {
            // Titles are user text, not Pango markup.
            let row = ActionRow::builder()
                .title(project.title.as_str())
                .use_markup(false)
                .activatable(true)
                .build();
            self.proj_list.append(&row);
        }

        // Sidebar selection mirrors the current view.
        match &view {
            View::Project(id) => {
                self.nav_list.unselect_all();
                let row = projects.iter().position(|p| &p.id == id);
                match row.and_then(|i| self.proj_list.row_at_index(i as i32)) {
                    Some(row) => self.proj_list.select_row(Some(&row)),
                    None => self.proj_list.unselect_all(),
                }
            }
            other => {
                self.proj_list.unselect_all();
                let row = nav_views().iter().position(|v| v == other);
                if let Some(row) = row.and_then(|i| self.nav_list.row_at_index(i as i32)) {
                    self.nav_list.select_row(Some(&row));
                }
            }
        }

        // Header.
        let title = match &view {
            View::Project(id) => projects
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.title.clone())
                .unwrap_or_else(|| view.title().to_string()),
            other => other.title().to_string(),
        };
        self.content_page.set_title(&title);
        self.state.borrow_mut().projects = projects;

        // Tasks: update the existing rows in place, keyed by task id.
        let target = {
            let mut state = self.state.borrow_mut();
            let State { visit, shown, .. } = &mut *state;
            let db = &self.db;
            engine
                .tasks_in(&view, today)
                .and_then(|members| visit.rows(&view, members, shown, |id| db.get_task(id)))
        };
        if let Some(target) = self.check_value(target) {
            let mut state = self.state.borrow_mut();
            let mut widgets = self.task_rows.borrow_mut();
            let mut sink = RowSink {
                ui: self,
                tasks: &mut state.shown,
                widgets: &mut widgets,
                pending: Pending::None,
            };
            reconcile(&mut sink, target, |t| t.id.clone());
        }
    }

    fn check_value<T>(&self, result: matters_core::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                self.check::<()>(Err(err));
                None
            }
        }
    }
}

/// Applies `reconcile`'s row operations to the task `ListBox`.
struct RowSink<'a> {
    ui: &'a Rc<Ui>,
    tasks: &'a mut Vec<Task>,
    widgets: &'a mut Vec<TaskRow>,
    pending: Pending,
}

enum Pending {
    None,
    Remove(usize),
    Insert(usize),
    Move(usize, usize),
}

fn list_index(i: usize) -> i32 {
    i32::try_from(i).expect("row index fits in i32")
}

impl ListSink<Task> for RowSink<'_> {
    fn rows(&mut self) -> &mut Vec<Task> {
        self.tasks
    }
    fn begin_remove(&mut self, row: usize) {
        self.pending = Pending::Remove(row);
    }
    fn end_remove(&mut self) {
        if let Pending::Remove(row) = std::mem::replace(&mut self.pending, Pending::None) {
            let widget = self.widgets.remove(row);
            self.ui.task_list.remove(&widget.row);
        }
    }
    fn begin_insert(&mut self, row: usize) {
        self.pending = Pending::Insert(row);
    }
    fn end_insert(&mut self) {
        if let Pending::Insert(row) = std::mem::replace(&mut self.pending, Pending::None) {
            let widget = TaskRow::new(self.ui, &self.tasks[row]);
            self.ui.task_list.insert(&widget.row, list_index(row));
            self.widgets.insert(row, widget);
        }
    }
    fn begin_move(&mut self, from: usize, to: usize) {
        self.pending = Pending::Move(from, to);
    }
    fn end_move(&mut self) {
        if let Pending::Move(from, to) = std::mem::replace(&mut self.pending, Pending::None) {
            let widget = self.widgets.remove(from);
            self.ui.task_list.remove(&widget.row);
            self.ui.task_list.insert(&widget.row, list_index(to));
            self.widgets.insert(to, widget);
        }
    }
    fn changed(&mut self, row: usize) {
        self.widgets[row].update(&self.tasks[row]);
    }
}

/// A task edit that finds the task already gone (deleted while its notes
/// were being saved, say) changes nothing, as before field-by-field writes.
fn gone_task_is_fine(result: matters_core::Result<()>) -> matters_core::Result<()> {
    match result {
        Err(matters_core::MattersError::NotFound { entity: "task", .. }) => Ok(()),
        other => other,
    }
}
