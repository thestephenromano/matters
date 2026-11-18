//! GTK4 / Libadwaita frontend for Matters.
//!
//! The binary (`src/main.rs`) only calls [`run`]; the UI lives in this
//! library so tests can build and drive it.

mod task_row;
mod ui;
#[cfg(test)]
mod tests;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita::Application;
use matters_core::Database;

const APP_ID: &str = "org.matters.MattersGtk";

/// Open the user's database and run the application.
pub fn run() -> glib::ExitCode {
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
    app.connect_activate(move |app| {
        ui::Ui::build(Some(app), db.clone());
    });
    app.run()
}
