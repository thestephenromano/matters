//! Task titles (#27): always visible, one line, edited in place.

use chrono::NaiveDate;
use matters_core::{Database, MattersError, Project, Schedule, Task, View};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 18).unwrap()
}

#[test]
fn titles_are_one_tidy_line() {
    assert_eq!(
        Task::clean_title("  Call   Sam ").as_deref(),
        Some("Call Sam")
    );
    assert_eq!(
        Task::clean_title("Buy\nmilk\r\nand\teggs").as_deref(),
        Some("Buy milk and eggs")
    );
    for blank in ["", "   ", "\n\t "] {
        assert_eq!(Task::clean_title(blank), None, "{blank:?}");
    }
}

#[test]
fn blank_captures_create_nothing() {
    for view in [
        View::Inbox,
        View::Today,
        View::Upcoming,
        View::Anytime,
        View::Someday,
        View::Project("p".into()),
    ] {
        assert!(view.new_task("t", "  \n ", today()).is_none(), "{view:?}");
        let task = view.new_task("t", " Pay\nrent ", today()).unwrap();
        assert_eq!(task.title, "Pay rent", "{view:?}");
    }
}

#[test]
fn renaming_changes_only_the_title() {
    let db = Database::open_in_memory().unwrap();
    let mut task = Task::new("t", "Draft", 3.0);
    task.notes = "keep".into();
    task.apply_schedule(&Schedule::Today { evening: true }, today());
    db.create_task(&task).unwrap();

    db.rename_task("t", "  Final\nreport ").unwrap();
    let renamed = db.get_task("t").unwrap().unwrap();
    assert_eq!(renamed.title, "Final report");
    assert_eq!(
        (
            renamed.notes,
            renamed.start_date,
            renamed.time_of_day,
            renamed.sort_order
        ),
        (
            task.notes,
            task.start_date,
            task.time_of_day,
            task.sort_order
        )
    );
}

#[test]
fn a_blank_rename_is_refused_and_keeps_the_name() {
    let db = Database::open_in_memory().unwrap();
    db.create_task(&Task::new("t", "Draft", 0.0)).unwrap();
    assert!(matches!(
        db.rename_task("t", " \n "),
        Err(MattersError::Validation(_))
    ));
    assert_eq!(db.get_task("t").unwrap().unwrap().title, "Draft");
    assert!(matches!(
        db.rename_task("missing", "x"),
        Err(MattersError::NotFound { .. })
    ));
}

#[test]
fn renaming_a_project_cleans_the_name_and_refuses_blank_ones() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "Draft", 0.0)).unwrap();
    db.rename_project("p", "  Kitchen\n renovation ").unwrap();
    assert_eq!(
        db.get_project("p").unwrap().unwrap().title,
        "Kitchen renovation"
    );
    assert!(matches!(
        db.rename_project("p", " "),
        Err(MattersError::Validation(_))
    ));
    assert_eq!(
        db.get_project("p").unwrap().unwrap().title,
        "Kitchen renovation"
    );
    assert!(matches!(
        db.rename_project("missing", "x"),
        Err(MattersError::NotFound { .. })
    ));
}
