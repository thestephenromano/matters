//! GTK4 / Libadwaita frontend for Matters.
//!
//! The binary (`src/main.rs`) only calls [`run`]; the UI lives in this
//! library so tests can build and drive it.

mod sync;
mod task_row;
#[cfg(test)]
mod tests;
mod ui;

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

    // Sync (#42), when the desktop has a keyring for this device's keys.
    // Without one, Matters works as before and Sync says why it can't.
    let (dirty, notice) = sync::SyncPanel::notice();
    let sync = match matters_keyring::Oo7KeyStore::open() {
        Ok(keys) => {
            let data_dir = Database::default_path()
                .ok()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                .unwrap_or_else(std::env::temp_dir);
            let settings = matters_core::SettingsFile::in_config_dir().ok();
            let endpoint = settings.as_ref().and_then(|f| f.load().0.sync.endpoint);
            let save = move |endpoint: Option<String>| {
                if let Some(file) = &settings {
                    let mut current = file.load().0;
                    current.sync.endpoint = endpoint;
                    if let Err(err) = file.save(&current) {
                        eprintln!("matters-gtk: the relay's address was not saved: {err}");
                    }
                }
            };
            match matters_sync_app::SyncApp::open(
                std::sync::Arc::new(db.clone()),
                std::sync::Arc::new(keys),
                endpoint,
                data_dir,
                notice,
                save,
            ) {
                Ok(app) => Some((app, dirty)),
                Err(err) => {
                    eprintln!("matters-gtk: sync is unavailable: {err}");
                    None
                }
            }
        }
        Err(err) => {
            eprintln!("matters-gtk: sync is unavailable without a keyring: {err}");
            None
        }
    };

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        ui::Ui::build(Some(app), db.clone(), sync.clone());
    });
    app.run()
}
