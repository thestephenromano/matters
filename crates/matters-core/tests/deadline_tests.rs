//! Deadlines (#20): typed deadlines and where a task stands against one.

use jiff::civil::Date;
use matters_core::{Database, DateOrder, DeadlineStatus, QueryEngine, Schedule, Task, View};

/// Wednesday 18 November 2026.
fn today() -> Date {
    jiff::civil::date(2026, 11, 18)
}

fn day(m: i8, d: i8) -> Date {
    jiff::civil::date(2026, m, d)
}

fn parse(input: &str) -> Option<Date> {
    Schedule::parse_typed_deadline(input, today(), DateOrder::MonthFirst)
}

#[test]
fn typed_deadlines_are_days() {
    assert_eq!(parse("today"), Some(today()));
    assert_eq!(parse("fri"), Some(day(11, 20)));
    assert_eq!(parse("in 3 days"), Some(day(11, 21)));
    assert_eq!(parse("dec 24"), Some(day(12, 24)));
    for not_a_deadline in [
        "tonight",
        "this evening",
        "someday",
        "yesterday",
        "tomorrow 5pm",
        "banana",
        "",
    ] {
        assert_eq!(parse(not_a_deadline), None, "{not_a_deadline:?}");
    }
}

#[test]
fn deadline_status_on_a_given_day() {
    let mut task = Task::new("t", "t", 0.0);
    assert_eq!(task.deadline_status(today()), DeadlineStatus::None);
    task.deadline = Some(day(11, 20));
    assert_eq!(task.deadline_status(today()), DeadlineStatus::Set);
    task.deadline = Some(today());
    assert_eq!(task.deadline_status(today()), DeadlineStatus::DueToday);
    task.deadline = Some(day(11, 10));
    assert_eq!(task.deadline_status(today()), DeadlineStatus::Overdue);
    task.status = matters_core::Status::Completed;
    assert_eq!(
        task.deadline_status(today()),
        DeadlineStatus::Set,
        "a finished task is never overdue"
    );
}

#[test]
fn a_deadline_due_today_brings_an_inbox_task_into_today() {
    let db = Database::open_in_memory().unwrap();
    db.create_task(&Task::new("t", "t", 0.0)).unwrap();
    let engine = QueryEngine::new(&db);
    let in_today = || {
        engine
            .tasks_in(&View::Today, today())
            .unwrap()
            .iter()
            .any(|t| t.id == "t")
    };
    assert!(!in_today());

    db.set_task_deadline("t", Some(today())).unwrap();
    assert!(in_today(), "due today: listed in Today");
    assert!(
        engine
            .tasks_in(&View::Inbox, today())
            .unwrap()
            .iter()
            .any(|t| t.id == "t"),
        "and still in the Inbox"
    );
}
