use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, CheckButton, Entry, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow,
    SelectionMode, Separator,
};
use libadwaita::prelude::*;
use libadwaita::{
    ActionRow, Application, ApplicationWindow, HeaderBar, NavigationPage, NavigationSplitView,
};
use matters_core::{Database, QueryEngine, Task};
use std::cell::RefCell;
use std::rc::Rc;
use uuid::Uuid;

const APP_ID: &str = "org.matters.MattersGtk";

struct AppState {
    db: Database,
    current_view: String,
    current_project_id: Option<String>,
}

fn main() {
    let app = Application::builder().application_id(APP_ID).build();

    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    let state = Rc::new(RefCell::new(AppState {
        db: Database::default(),
        current_view: "today".to_string(),
        current_project_id: None,
    }));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Matters (GTK)")
        .default_width(960)
        .default_height(680)
        .build();

    let split_view = NavigationSplitView::new();

    // =========================================================================
    // SIDEBAR
    // =========================================================================
    let sidebar_box = GtkBox::new(Orientation::Vertical, 6);
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);

    // Nav list
    let nav_list = ListBox::new();
    nav_list.set_selection_mode(SelectionMode::Single);
    nav_list.add_css_class("navigation-sidebar");

    let views = [
        ("inbox", "📥 Inbox"),
        ("today", "⭐ Today"),
        ("upcoming", "📅 Upcoming"),
        ("anytime", "📦 Anytime"),
        ("someday", "🗃️ Someday"),
        ("logbook", "📓 Logbook"),
    ];

    for (id, label) in views {
        let row = ActionRow::builder().title(label).build();
        unsafe {
            row.set_data("view_id", id.to_string());
        }
        nav_list.append(&row);
    }

    sidebar_box.append(&nav_list);

    let sep = Separator::new(Orientation::Horizontal);
    sep.set_margin_top(8);
    sep.set_margin_bottom(8);
    sidebar_box.append(&sep);

    // Projects Header
    let proj_header_box = GtkBox::new(Orientation::Horizontal, 8);
    let proj_title_lbl = Label::builder()
        .label("PROJECTS")
        .halign(Align::Start)
        .hexpand(true)
        .css_classes(["dim-label", "caption", "bold"])
        .build();
    let add_proj_btn = gtk4::Button::builder().label("+").build();
    proj_header_box.append(&proj_title_lbl);
    proj_header_box.append(&add_proj_btn);
    sidebar_box.append(&proj_header_box);

    let proj_list = ListBox::new();
    proj_list.set_selection_mode(SelectionMode::Single);
    proj_list.add_css_class("navigation-sidebar");
    sidebar_box.append(&proj_list);

    let sidebar_scroll = ScrolledWindow::builder().child(&sidebar_box).build();
    let sidebar_page = NavigationPage::builder()
        .title("Matters")
        .child(&sidebar_scroll)
        .build();
    split_view.set_sidebar(Some(&sidebar_page));

    // =========================================================================
    // CONTENT AREA
    // =========================================================================
    let content_box = GtkBox::new(Orientation::Vertical, 16);
    content_box.set_margin_top(16);
    content_box.set_margin_bottom(24);
    content_box.set_margin_start(28);
    content_box.set_margin_end(28);

    let header_bar = HeaderBar::new();

    // Quick Task Input
    let new_task_entry = Entry::builder()
        .placeholder_text("New to-do... (Press Enter to add)")
        .hexpand(true)
        .build();
    content_box.append(&new_task_entry);

    // Task List Box
    let task_list_box = ListBox::new();
    task_list_box.set_selection_mode(SelectionMode::None);
    task_list_box.add_css_class("boxed-list");

    let task_scroll = ScrolledWindow::builder()
        .child(&task_list_box)
        .vexpand(true)
        .hexpand(true)
        .build();
    content_box.append(&task_scroll);

    let content_page_box = GtkBox::new(Orientation::Vertical, 0);
    content_page_box.append(&header_bar);
    content_page_box.append(&content_box);

    let content_page = NavigationPage::builder()
        .title("Today")
        .child(&content_page_box)
        .build();
    split_view.set_content(Some(&content_page));

    window.set_content(Some(&split_view));

    // =========================================================================
    // REFRESH & EVENT LOGIC
    // =========================================================================
    let refresh_tasks = {
        let state = state.clone();
        let task_list_box = task_list_box.clone();
        let content_page = content_page.clone();

        Rc::new(move || {
            // Clear current list
            while let Some(child) = task_list_box.first_child() {
                task_list_box.remove(&child);
            }

            let s = state.borrow();
            let engine = QueryEngine::new(&s.db);
            let today = chrono::Local::now().date_naive();

            let (title, tasks) = match s.current_view.as_str() {
                "inbox" => ("Inbox", engine.inbox().unwrap_or_default()),
                "today" => {
                    let td = engine.today(today).unwrap_or_default();
                    let mut all = td.daytime;
                    all.extend(td.evening);
                    ("Today", all)
                }
                "upcoming" => {
                    let up = engine.upcoming(today).unwrap_or_default();
                    let mut all = Vec::new();
                    for g in up {
                        all.extend(g.tasks);
                    }
                    ("Upcoming", all)
                }
                "anytime" => ("Anytime", engine.anytime(today).unwrap_or_default()),
                "someday" => ("Someday", engine.someday_tasks().unwrap_or_default()),
                "logbook" => ("Logbook", engine.logbook(50).unwrap_or_default()),
                "project" => {
                    if let Some(pid) = &s.current_project_id {
                        let p_title = s.db.get_project(pid).ok().flatten().map(|p| p.title).unwrap_or_else(|| "Project".to_string());
                        let p_tasks = s.db.list_tasks_for_project(pid).unwrap_or_default();
                        content_page.set_title(&p_title);
                        ("", p_tasks)
                    } else {
                        ("Project", Vec::new())
                    }
                }
                _ => ("Tasks", Vec::new()),
            };

            if s.current_view != "project" {
                content_page.set_title(title);
            }

            for task in tasks {
                let row = ListBoxRow::new();
                let row_box = GtkBox::new(Orientation::Horizontal, 12);
                row_box.set_margin_top(8);
                row_box.set_margin_bottom(8);
                row_box.set_margin_start(10);
                row_box.set_margin_end(10);

                let is_completed = task.status == matters_core::Status::Completed;

                // Round checkbox
                let check = CheckButton::new();
                check.add_css_class("selection-mode");
                check.set_active(is_completed);

                let tid = task.id.clone();
                let db_clone = s.db.clone();
                let refresh_self = state.clone();
                let list_box_clone = task_list_box.clone();
                let page_clone = content_page.clone();

                check.connect_toggled(move |btn| {
                    if btn.is_active() {
                        let _ = db_clone.complete_task(&tid);
                    } else {
                        let _ = db_clone.uncomplete_task(&tid);
                    }
                });

                // Title label
                let title_lbl = Label::builder()
                    .label(&task.title)
                    .halign(Align::Start)
                    .hexpand(true)
                    .build();

                if is_completed {
                    title_lbl.add_css_class("dim-label");
                }

                row_box.append(&check);
                row_box.append(&title_lbl);

                // Evening badge
                if task.time_of_day.as_deref() == Some("evening") {
                    let eve_badge = Label::builder()
                        .label("🌙 Evening")
                        .css_classes(["badge", "warning"])
                        .build();
                    row_box.append(&eve_badge);
                }

                // Date badge
                if let Some(d) = task.start_date {
                    let d_lbl = Label::builder()
                        .label(&d.format("%b %d").to_string())
                        .css_classes(["caption", "dim-label"])
                        .build();
                    row_box.append(&d_lbl);
                }

                row.set_child(Some(&row_box));
                task_list_box.append(&row);
            }
        })
    };

    // Nav List selection
    {
        let state = state.clone();
        let refresh = refresh_tasks.clone();
        nav_list.connect_row_selected(move |_, row_opt| {
            if let Some(row) = row_opt {
                unsafe {
                    if let Some(view_id) = row.data::<String>("view_id") {
                        let mut s = state.borrow_mut();
                        s.current_view = view_id.as_ref().clone();
                        s.current_project_id = None;
                    }
                }
                refresh();
            }
        });
    }

    // New Task Entry handler
    {
        let state = state.clone();
        let refresh = refresh_tasks.clone();
        new_task_entry.connect_activate(move |entry| {
            let text = entry.text().trim().to_string();
            if !text.is_empty() {
                let s = state.borrow();
                let today = chrono::Local::now().date_naive();
                let mut task = Task::new(Uuid::new_v4().to_string(), text, 0.0);

                if s.current_view == "today" {
                    task.start_date = Some(today);
                } else if s.current_view == "someday" {
                    task.is_someday = true;
                } else if s.current_view == "project" {
                    task.project_id = s.current_project_id.clone();
                }

                let _ = s.db.create_task(&task);
                entry.set_text("");
                drop(s);
                refresh();
            }
        });
    }

    // Initial load
    refresh_tasks();

    window.present();
}
