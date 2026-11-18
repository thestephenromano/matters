//! Dates keep the stored form they have always had, `YYYY-MM-DD` zero-padded,
//! whatever date library the core uses (#123: chrono to jiff).

use jiff::civil::date;
use matters_core::{Database, DateOrder, Schedule, Task};

#[test]
fn dates_are_stored_as_zero_padded_iso_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let db = Database::open(&path).unwrap();
        let mut task = Task::new("t", "dentist", 0.0);
        task.start_date = Some(date(2026, 1, 5));
        task.deadline = Some(date(2027, 2, 9));
        db.create_task(&task).unwrap();
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    let (start, deadline): (String, String) = conn
        .query_row(
            "SELECT start_date, deadline FROM tasks WHERE id = 't'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (start.as_str(), deadline.as_str()),
        ("2026-01-05", "2027-02-09")
    );
}

#[test]
fn dates_stored_by_earlier_versions_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let db = Database::open(&path).unwrap();
        db.create_task(&Task::new("t", "dentist", 0.0)).unwrap();
    }
    {
        // Text as earlier versions (chrono's "%Y-%m-%d") wrote it.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE tasks SET start_date = '2026-11-05', deadline = '2026-12-24' WHERE id = 't'",
            [],
        )
        .unwrap();
    }
    let task = Database::open(&path)
        .unwrap()
        .get_task("t")
        .unwrap()
        .unwrap();
    assert_eq!(task.start_date, Some(date(2026, 11, 5)));
    assert_eq!(task.deadline, Some(date(2026, 12, 24)));
}

#[test]
fn a_frontend_schedule_date_is_iso() {
    assert_eq!(
        Schedule::parse("2026-01-05"),
        Some(Schedule::Date(date(2026, 1, 5)))
    );
    assert_eq!(Schedule::parse("2026-02-30"), None, "not a day");
    assert_eq!(Schedule::parse("someday"), Some(Schedule::Someday));
}

#[test]
fn a_29_february_already_past_is_refused_when_next_year_has_none() {
    // In March of a leap year, "feb 29" has passed, and next year has no
    // 29 February: refused rather than moved to another day.
    let today = date(2028, 3, 1);
    assert_eq!(
        Schedule::parse_typed("feb 29", today, DateOrder::MonthFirst),
        None
    );
    // A day that next year has moves there.
    assert_eq!(
        Schedule::parse_typed("feb 28", today, DateOrder::MonthFirst),
        Some(Schedule::Date(date(2029, 2, 28)))
    );
}
