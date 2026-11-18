//! Per-user database location and open-failure behaviour.
//!
//! These tests mutate process environment variables, so they live in their own
//! test binary and run as a single test function to avoid racing each other.

use matters_core::{Database, MattersError};

#[test]
fn default_location_follows_xdg_and_open_failures_surface() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();

    std::env::set_var("HOME", &home);

    // An absolute XDG_DATA_HOME is used.
    std::env::set_var("XDG_DATA_HOME", &xdg);
    assert_eq!(
        Database::default_path().unwrap(),
        xdg.join("matters").join("matters.db")
    );

    // An empty or relative XDG_DATA_HOME is invalid per the XDG spec and must be
    // ignored in favour of $HOME/.local/share, never used as a relative path.
    let home_default = home.join(".local/share/matters/matters.db");
    for invalid in ["", "relative/dir"] {
        std::env::set_var("XDG_DATA_HOME", invalid);
        let path = Database::default_path().unwrap();
        assert_eq!(path, home_default, "XDG_DATA_HOME={invalid:?}");
        assert!(path.is_absolute());
    }

    // open_default persists to disk at the default path.
    std::env::set_var("XDG_DATA_HOME", &xdg);
    {
        let db = Database::open_default().expect("open default db");
        let task = matters_core::Task::new("t1", "persisted", 0.0);
        db.create_task(&task).unwrap();
    }
    let reopened = Database::open(xdg.join("matters/matters.db")).unwrap();
    assert_eq!(reopened.get_task("t1").unwrap().unwrap().title, "persisted");

    // When the location is unusable, the error is returned to the caller rather
    // than replaced by a throwaway in-memory database.
    let blocker = tmp.path().join("not-a-dir");
    std::fs::write(&blocker, b"").unwrap();
    std::env::set_var("XDG_DATA_HOME", &blocker);
    match Database::open_default() {
        Err(MattersError::Io(_)) => {}
        Err(other) => panic!("expected an IO error, got {other:?}"),
        Ok(_) => panic!("open_default must fail when the data dir is unusable"),
    }

    // A corrupt database file is an error, not an empty store.
    let corrupt_dir = tmp.path().join("corrupt");
    std::fs::create_dir_all(corrupt_dir.join("matters")).unwrap();
    std::fs::write(
        corrupt_dir.join("matters/matters.db"),
        b"this is not a sqlite database, just some bytes that fill the header area",
    )
    .unwrap();
    std::env::set_var("XDG_DATA_HOME", &corrupt_dir);
    assert!(Database::open_default().is_err());
}
