//! GTK4 / Libadwaita frontend.
//!
//! View membership, task placement and scheduling rules come from
//! `matters_core` (`View`, `Schedule`, `QueryEngine`); this file only builds
//! widgets and turns user actions into core calls. Every failed command is
//! shown as a toast.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    glib, Align, Box as GtkBox, CheckButton, Entry, Label, ListBox, ListBoxRow, Orientation,
    ScrolledWindow, SelectionMode, Separator,
};
use libadwaita::prelude::*;
use libadwaita::{
    ActionRow, AlertDialog, Application, ApplicationWindow, HeaderBar, NavigationPage,
    NavigationSplitView, ResponseAppearance, Toast, ToastOverlay,
};
use matters_core::{Database, Project, QueryEngine, Status, Task, View};
use uuid::Uuid;

const APP_ID: &str = "org.matters.MattersGtk";

fn main() -> glib::ExitCode {
    // Open the user's database before any UI exists. If it cannot be opened,
    // refuse to start rather than run against a store that would not persist.
    let db = match Database::open_default() {
        Ok(db) => db,
        Err(err) => {
            let location = Database::default_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".to_string());
            eprintln!("matters-gtk: cannot open database at {location}: {err}");
            return glib::ExitCode::FAILURE;
        }
    };

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| Ui::build(app, db.clone()));
    app.run()
}

fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

struct State {
    view: View,
    /// Projects in sidebar order; a project row's index selects from here.
    projects: Vec<Project>,
}

struct Ui {
    db: Database,
    state: RefCell<State>,
    window: ApplicationWindow,
    toasts: ToastOverlay,
    nav_list: ListBox,
    nav_counts: Vec<Label>,
    proj_list: ListBox,
    task_list: ListBox,
    content_page: NavigationPage,
}

impl Ui {
    fn build(app: &Application, db: Database) {
        let window = ApplicationWindow::builder()
            .application(app)
            .title("Matters (GTK)")
            .default_width(960)
            .default_height(680)
            .build();

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
        for view in View::sidebar() {
            let icon = match view {
                View::Inbox => "📥",
                View::Today => "⭐",
                View::Upcoming => "📅",
                View::Anytime => "📦",
                View::Someday => "🗃️",
                View::Logbook => "📓",
                View::Project(_) => "",
            };
            let row = ActionRow::builder()
                .title(format!("{icon} {}", view.title()))
                .activatable(true)
                .build();
            let count = Label::builder().css_classes(["dim-label", "caption"]).build();
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
            state: RefCell::new(State { view: View::Today, projects: Vec::new() }),
            window: window.clone(),
            toasts,
            nav_list: nav_list.clone(),
            nav_counts,
            proj_list: proj_list.clone(),
            task_list,
            content_page,
        });

        // ---- Events ---------------------------------------------------------
        {
            let ui = ui.clone();
            nav_list.connect_row_activated(move |_, row| {
                let index = usize::try_from(row.index()).ok();
                if let Some(view) = index.and_then(|i| View::sidebar().get(i).cloned()) {
                    ui.select(view);
                }
            });
        }
        {
            let ui = ui.clone();
            proj_list.connect_row_activated(move |_, row| {
                let index = usize::try_from(row.index()).ok();
                let project = index.and_then(|i| ui.state.borrow().projects.get(i).map(|p| p.id.clone()));
                if let Some(id) = project {
                    ui.select(View::Project(id));
                }
            });
        }
        {
            let ui = ui.clone();
            new_task_entry.connect_activate(move |entry| {
                let title = entry.text().trim().to_string();
                if title.is_empty() {
                    return;
                }
                let task = ui.state.borrow().view.new_task(Uuid::new_v4().to_string(), title, today());
                if ui.check(ui.db.create_task(&task)) {
                    entry.set_text("");
                }
                ui.refresh();
            });
        }
        {
            let ui = ui.clone();
            add_proj_btn.connect_clicked(move |_| ui.prompt_new_project());
        }

        ui.select(View::Today);
        window.present();
    }

    /// Show `result`'s error, if any, as a toast. Returns whether it succeeded.
    fn check<T>(&self, result: matters_core::Result<T>) -> bool {
        match result {
            Ok(_) => true,
            Err(err) => {
                eprintln!("matters-gtk: {err}");
                self.toasts.add_toast(Toast::new(&err.to_string()));
                false
            }
        }
    }

    fn select(self: &Rc<Self>, view: View) {
        self.state.borrow_mut().view = view;
        self.refresh();
    }

    fn prompt_new_project(self: &Rc<Self>) {
        let dialog = AlertDialog::new(Some("New Project"), None);
        let entry = Entry::builder().placeholder_text("Project title").activates_default(true).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("create", "Create");
        dialog.set_response_appearance("create", ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");

        let ui = self.clone();
        dialog.connect_response(Some("create"), move |_, _| {
            let title = entry.text().trim().to_string();
            if title.is_empty() {
                return;
            }
            let project = Project::new(Uuid::new_v4().to_string(), title, 0.0);
            if ui.check(ui.db.create_project(&project)) {
                ui.select(View::Project(project.id));
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Re-read everything the window shows from the database.
    fn refresh(self: &Rc<Self>) {
        let engine = QueryEngine::new(&self.db);
        let view = self.state.borrow().view.clone();

        // Sidebar counts.
        if let Some(counts) = self.check_value(engine.counts(today())) {
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
        let projects = self.check_value(self.db.list_projects()).unwrap_or_default();
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
                let row = View::sidebar().iter().position(|v| v == other);
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

        // Tasks.
        while let Some(child) = self.task_list.first_child() {
            self.task_list.remove(&child);
        }
        for task in self.check_value(engine.tasks_in(&view, today())).unwrap_or_default() {
            self.task_list.append(&self.task_row(&task));
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

    fn task_row(self: &Rc<Self>, task: &Task) -> ListBoxRow {
        let row_box = GtkBox::new(Orientation::Horizontal, 12);
        row_box.set_margin_top(8);
        row_box.set_margin_bottom(8);
        row_box.set_margin_start(10);
        row_box.set_margin_end(10);

        let is_completed = task.status == Status::Completed;
        let check = CheckButton::new();
        check.add_css_class("selection-mode");
        check.set_active(is_completed);
        {
            let ui = self.clone();
            let id = task.id.clone();
            check.connect_toggled(move |btn| {
                let result = if btn.is_active() {
                    ui.db.complete_task(&id)
                } else {
                    ui.db.uncomplete_task(&id)
                };
                ui.check(result);
                // Rebuild after this handler returns: the list is about to drop
                // the row this checkbox lives in.
                let ui = ui.clone();
                glib::idle_add_local_once(move || ui.refresh());
            });
        }
        row_box.append(&check);

        let title = Label::builder()
            .label(task.title.as_str())
            .halign(Align::Start)
            .hexpand(true)
            .build();
        if is_completed {
            title.add_css_class("dim-label");
        }
        row_box.append(&title);

        if task.is_evening() {
            row_box.append(
                &Label::builder()
                    .label("🌙 Evening")
                    .css_classes(["caption", "warning"])
                    .build(),
            );
        }
        if let Some(date) = task.start_date {
            row_box.append(
                &Label::builder()
                    .label(date.format("%b %d").to_string().as_str())
                    .css_classes(["caption", "dim-label"])
                    .build(),
            );
        }

        let row = ListBoxRow::new();
        row.set_child(Some(&row_box));
        row
    }
}
