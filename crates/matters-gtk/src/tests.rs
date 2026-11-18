//! Widget-level tests of the real GTK UI.
//!
//! GTK 4 has no public API for synthesizing pointer or keyboard input, so the
//! tests drive the UI through its widgets: activating rows and entries,
//! toggling check buttons, clicking buttons and editing text buffers. Each
//! goes through the same signal handlers that user input triggers.
//!
//! GTK must be initialised on one thread with a display, so everything runs in
//! one `#[test]` against a private headless Broadway display (`gtk4-broadwayd`,
//! shipped with GTK 4) and an in-memory database.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::prelude::*;
use matters_core::{Database, QueryEngine, Status, View};

use crate::ui::Ui;

/// A private Broadway display, stopped (and its socket removed) on drop.
struct Broadway {
    child: Child,
    socket: PathBuf,
}

impl Broadway {
    fn start() -> Broadway {
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR must be set for Broadway"),
        );
        // Display N serves HTTP on 8080 + N and its clients on broadway{N+1}.socket.
        let display = (40..100)
            .find(|n| {
                TcpListener::bind(("127.0.0.1", 8080 + n)).is_ok()
                    && !runtime.join(format!("broadway{}.socket", n + 1)).exists()
            })
            .expect("no free Broadway display number");
        let socket = runtime.join(format!("broadway{}.socket", display + 1));

        let child = Command::new("gtk4-broadwayd")
            .arg(format!(":{display}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("gtk4-broadwayd not found: install GTK 4 (it ships the Broadway daemon)");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "gtk4-broadwayd did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::env::set_var("GDK_BACKEND", "broadway");
        std::env::set_var("BROADWAY_DISPLAY", format!(":{display}"));
        Broadway { child, socket }
    }
}

impl Drop for Broadway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Run the main loop until it is idle and `delay` has passed.
fn pump(delay: Duration) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + delay;
    loop {
        while context.iteration(false) {}
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn settle() {
    pump(Duration::from_millis(50));
}

fn titles(ui: &Ui) -> Vec<String> {
    ui.state
        .borrow()
        .shown
        .iter()
        .map(|t| t.title.clone())
        .collect()
}

fn row_index(ui: &Ui, title: &str) -> usize {
    titles(ui)
        .iter()
        .position(|t| t == title)
        .unwrap_or_else(|| panic!("'{title}' not listed in {:?}", titles(ui)))
}

fn add_task(ui: &Rc<Ui>, title: &str) {
    ui.new_task_entry.set_text(title);
    ui.new_task_entry.emit_activate();
    settle();
}

fn activate_nav(ui: &Ui, view: &View) {
    let index = crate::ui::nav_views()
        .iter()
        .position(|v| v == view)
        .unwrap();
    ui.nav_list
        .row_at_index(index as i32)
        .unwrap()
        .emit_activate();
    settle();
}

#[test]
fn gtk_ui_end_to_end() {
    let _display = Broadway::start();
    libadwaita::init().expect("initialise libadwaita");
    let ui = Ui::build(None, Database::open_in_memory().unwrap());
    settle();

    adding_a_task_lists_it_in_the_current_view(&ui);
    ticking_keeps_the_task_until_the_view_is_left(&ui);
    sidebar_views_and_counts(&ui);
    projects_can_be_created_and_opened(&ui);
    notes_save_and_the_row_stays_open(&ui);
    when_buttons_reschedule(&ui);
}

fn adding_a_task_lists_it_in_the_current_view(ui: &Rc<Ui>) {
    activate_nav(ui, &View::Today);
    add_task(ui, "gtk-add");
    assert!(titles(ui).contains(&"gtk-add".to_string()));
    assert_eq!(ui.new_task_entry.text(), "", "entry clears after adding");
    let today = QueryEngine::new(&ui.db)
        .tasks_in(&View::Today, matters_core::local_today())
        .unwrap();
    assert!(
        today.iter().any(|t| t.title == "gtk-add"),
        "stored as a Today task"
    );

    // The Logbook holds no new work: no entry, and nothing is created.
    activate_nav(ui, &View::Logbook);
    // WidgetExt: shown on screen (EntryExt::is_visible is about password text).
    assert!(
        !WidgetExt::is_visible(&ui.new_task_entry),
        "no quick-add entry in the Logbook"
    );
    add_task(ui, "gtk-logbook");
    let all = QueryEngine::new(&ui.db)
        .quick_find("gtk-logbook")
        .unwrap()
        .tasks;
    assert!(all.is_empty(), "nothing captured in the Logbook");
    activate_nav(ui, &View::Today);
    assert!(WidgetExt::is_visible(&ui.new_task_entry));
    ui.new_task_entry.set_text("");
}

fn ticking_keeps_the_task_until_the_view_is_left(ui: &Rc<Ui>) {
    activate_nav(ui, &View::Today);
    add_task(ui, "gtk-tick");
    let i = row_index(ui, "gtk-tick");
    let row = ui.task_rows.borrow()[i].row.clone();

    let check = ui.task_rows.borrow()[i].check.clone();
    check.set_active(true);
    settle();
    let i = row_index(ui, "gtk-tick");
    assert_eq!(ui.state.borrow().shown[i].status, Status::Completed);
    assert_eq!(
        ui.task_rows.borrow()[i].row,
        row,
        "same row widget, kept in place"
    );

    check.set_active(false);
    settle();
    assert_eq!(
        ui.state.borrow().shown[row_index(ui, "gtk-tick")].status,
        Status::Open
    );

    check.set_active(true);
    settle();
    activate_nav(ui, &View::Inbox);
    activate_nav(ui, &View::Today);
    assert!(
        !titles(ui).contains(&"gtk-tick".to_string()),
        "gone after leaving"
    );
}

fn sidebar_views_and_counts(ui: &Rc<Ui>) {
    activate_nav(ui, &View::Someday);
    assert_eq!(ui.state.borrow().view, View::Someday);
    assert_eq!(ui.content_page.title(), "Someday");
    add_task(ui, "gtk-someday");

    let someday = crate::ui::nav_views()
        .iter()
        .position(|v| *v == View::Someday)
        .unwrap();
    let logbook = crate::ui::nav_views()
        .iter()
        .position(|v| *v == View::Logbook)
        .unwrap();
    assert_eq!(ui.nav_counts[someday].label(), "1");
    assert_eq!(ui.nav_counts[logbook].label(), "1", "the ticked task");
}

fn projects_can_be_created_and_opened(ui: &Rc<Ui>) {
    ui.create_project("Garden");
    settle();
    assert_eq!(ui.content_page.title(), "Garden", "a new project opens");
    let View::Project(id) = ui.state.borrow().view.clone() else {
        panic!("expected a project view");
    };
    add_task(ui, "gtk-in-project");
    assert_eq!(
        ui.db
            .get_task(&ui.state.borrow().shown[0].id)
            .unwrap()
            .unwrap()
            .project_id,
        Some(id.clone()),
        "captured into the project"
    );

    activate_nav(ui, &View::Inbox);
    assert_eq!(
        ui.proj_list.row_at_index(0).unwrap().index(),
        0,
        "listed in the sidebar"
    );
    ui.proj_list.row_at_index(0).unwrap().emit_activate();
    settle();
    assert_eq!(ui.state.borrow().view, View::Project(id));
    assert_eq!(titles(ui), ["gtk-in-project"]);
}

fn notes_save_and_the_row_stays_open(ui: &Rc<Ui>) {
    activate_nav(ui, &View::Today);
    add_task(ui, "gtk-notes");
    let i = row_index(ui, "gtk-notes");
    let (row, notes) = {
        let rows = ui.task_rows.borrow();
        (rows[i].row.clone(), rows[i].notes.clone())
    };
    row.set_expanded(true);
    notes.buffer().insert_at_cursor("first");
    pump(Duration::from_millis(900)); // past the save delay

    let i = row_index(ui, "gtk-notes");
    assert_eq!(ui.state.borrow().shown[i].notes, "first", "notes saved");
    assert_eq!(
        ui.task_rows.borrow()[i].row,
        row,
        "same row widget after the save"
    );
    assert!(row.is_expanded(), "still expanded");

    // Another task arriving while the notes are open leaves this row alone.
    add_task(ui, "gtk-notes-neighbour");
    let i = row_index(ui, "gtk-notes");
    assert_eq!(ui.task_rows.borrow()[i].row, row);
    assert!(row.is_expanded());
    let buffer = notes.buffer();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
        "first"
    );
}

fn when_buttons_reschedule(ui: &Rc<Ui>) {
    activate_nav(ui, &View::Today);
    add_task(ui, "gtk-defer");
    let i = row_index(ui, "gtk-defer");
    let someday = ui.task_rows.borrow()[i]
        .when_buttons
        .iter()
        .find(|b| b.label().as_deref() == Some("Someday"))
        .cloned()
        .unwrap();
    someday.emit_clicked();
    settle();
    assert!(
        !titles(ui).contains(&"gtk-defer".to_string()),
        "leaves Today at once"
    );
    activate_nav(ui, &View::Someday);
    assert!(titles(ui).contains(&"gtk-defer".to_string()));
}
