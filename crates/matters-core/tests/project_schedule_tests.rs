//! A project's When and deadline (#160, part of #56): a Someday or future
//! project holds its tasks back, projects are listed as rows where their
//! dates put them, and the counts match the lists.

use jiff::civil::{date, Date};
use matters_core::{
    logbook_section_key, project_section_key, Database, DeadlineStatus, ListRow, Outcome, Project,
    QueryEngine, Schedule, Status, Task, View,
};

fn today() -> Date {
    date(2026, 11, 19)
}

fn project(db: &Database, id: &str, schedule: Schedule) {
    let mut p = Project::new(id, id, 0.0);
    p.apply_schedule(&schedule, today());
    db.create_project(&p).unwrap();
}

/// A task in `project` (or none) with its own schedule.
fn task(db: &Database, id: &str, project: Option<&str>, schedule: Schedule) {
    let mut t = Task::new(id, id, 0.0);
    t.project_id = project.map(str::to_string);
    t.apply_schedule(&schedule, today());
    db.add_task(&mut t).unwrap();
}

/// The rows of `view` on `day`, tasks by id and projects as `<id>`.
fn rows(db: &Database, view: &View, day: Date) -> Vec<String> {
    let engine = QueryEngine::new(db);
    let tasks = engine.tasks_in(view, day).unwrap();
    engine
        .list_rows(view, tasks, day)
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id,
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
            ListRow::Event(e) => format!("({})", e.key()),
        })
        .collect()
}

/// Each view's count, checked against its rows.
fn count(db: &Database, view: &View, day: Date) -> usize {
    let counts = QueryEngine::new(db).counts(day).unwrap();
    let n = match view {
        View::Today => counts.today,
        View::Upcoming => counts.upcoming,
        View::Anytime => counts.anytime,
        View::Someday => counts.someday,
        other => panic!("no count for {other:?}"),
    };
    assert_eq!(
        n,
        rows(db, view, day).len(),
        "{view:?}: the count matches the list"
    );
    n
}

/// Tasks of every kind of schedule in project `p`.
fn tasks_in_p(db: &Database) {
    task(db, "p-today", Some("p"), Schedule::Today { evening: false });
    task(db, "p-anytime", Some("p"), Schedule::None);
    task(db, "p-later", Some("p"), Schedule::Date(date(2026, 12, 5)));
    task(db, "p-someday", Some("p"), Schedule::Someday);
}

#[test]
fn a_someday_project_holds_its_tasks_back_and_is_listed_in_someday() {
    let db = Database::open_in_memory().unwrap();
    project(&db, "p", Schedule::Someday);
    tasks_in_p(&db);
    task(&db, "loose", None, Schedule::Someday);

    assert!(rows(&db, &View::Today, today()).is_empty());
    assert!(rows(&db, &View::Anytime, today()).is_empty());
    assert!(
        rows(&db, &View::Upcoming, today()).is_empty(),
        "not even on a date of their own"
    );
    assert_eq!(
        rows(&db, &View::Someday, today()),
        ["<p>", "p-someday", "loose"]
    );
    assert_eq!(
        rows(&db, &View::Project("p".into()), today()).len(),
        4,
        "still in the project"
    );
    for view in [View::Today, View::Upcoming, View::Anytime, View::Someday] {
        count(&db, &view, today());
    }

    // Brought back from Someday, the project lets its tasks go.
    db.set_project_schedule("p", &Schedule::None, today())
        .unwrap();
    assert_eq!(rows(&db, &View::Today, today()), ["p-today"]);
    assert_eq!(rows(&db, &View::Anytime, today()), ["p-today", "p-anytime"]);
    assert_eq!(rows(&db, &View::Upcoming, today()), ["p-later"]);
    assert_eq!(rows(&db, &View::Someday, today()), ["p-someday", "loose"]);
}

#[test]
fn a_project_starting_later_holds_its_tasks_back_until_it_starts() {
    let db = Database::open_in_memory().unwrap();
    let start = date(2026, 12, 1);
    project(&db, "p", Schedule::Date(start));
    tasks_in_p(&db);

    assert!(rows(&db, &View::Today, today()).is_empty());
    assert!(rows(&db, &View::Anytime, today()).is_empty());
    assert_eq!(
        rows(&db, &View::Upcoming, today()),
        ["<p>", "p-later"],
        "the project on the day it starts, a task on its own date"
    );
    for view in [View::Today, View::Upcoming, View::Anytime, View::Someday] {
        count(&db, &view, today());
    }

    // The day it starts, it is no longer upcoming and its tasks are free.
    assert_eq!(rows(&db, &View::Today, start), ["p-today"]);
    assert_eq!(rows(&db, &View::Anytime, start), ["p-today", "p-anytime"]);
    assert_eq!(rows(&db, &View::Upcoming, start), ["p-later"]);
}

#[test]
fn upcoming_lists_a_project_at_the_head_of_the_day_it_starts() {
    let db = Database::open_in_memory().unwrap();
    task(&db, "early", None, Schedule::Date(date(2026, 11, 25)));
    task(&db, "same-day", None, Schedule::Date(date(2026, 12, 1)));
    task(&db, "late", None, Schedule::Date(date(2026, 12, 9)));
    project(&db, "p", Schedule::Date(date(2026, 12, 1)));
    project(&db, "q", Schedule::Date(date(2026, 12, 20)));

    assert_eq!(
        rows(&db, &View::Upcoming, today()),
        ["early", "<p>", "same-day", "late", "<q>"]
    );
    count(&db, &View::Upcoming, today());

    let p = db.get_project("p").unwrap().unwrap();
    assert_eq!(project_section_key(&View::Upcoming, &p), "date:2026-12-01");
    assert_eq!(project_section_key(&View::Today, &p), "");
    assert_eq!(project_section_key(&View::Someday, &p), "");

    // A finished project sits under the day it was finished in the Logbook.
    db.finish_project("q", Outcome::Completed, Outcome::Completed)
        .unwrap();
    let q = db.get_project("q").unwrap().unwrap();
    assert_eq!(
        project_section_key(&View::Logbook, &q),
        logbook_section_key(q.completed_at)
    );
    assert!(project_section_key(&View::Logbook, &q).len() > "done:".len());
}

#[test]
fn today_lists_a_project_whose_deadline_has_come() {
    let db = Database::open_in_memory().unwrap();
    for (id, deadline) in [
        ("due", date(2026, 11, 19)),
        ("overdue", date(2026, 11, 2)),
        ("later", date(2026, 11, 30)),
        ("done", date(2026, 11, 19)),
        ("binned", date(2026, 11, 19)),
    ] {
        let mut p = Project::new(id, id, 0.0);
        p.deadline = Some(deadline);
        db.create_project(&p).unwrap();
    }
    db.finish_project("done", Outcome::Completed, Outcome::Completed)
        .unwrap();
    db.trash_project("binned").unwrap();
    // The project's deadline is not its tasks': this one is not due.
    task(&db, "in-due", Some("due"), Schedule::None);
    task(&db, "mine", None, Schedule::Today { evening: false });

    assert_eq!(
        rows(&db, &View::Today, today()),
        ["<due>", "<overdue>", "mine"]
    );
    count(&db, &View::Today, today());

    let due = db.get_project("due").unwrap().unwrap();
    assert_eq!(due.deadline_status(today()), DeadlineStatus::DueToday);
    let overdue = db.get_project("overdue").unwrap().unwrap();
    assert_eq!(overdue.deadline_status(today()), DeadlineStatus::Overdue);
    let done = db.get_project("done").unwrap().unwrap();
    assert_eq!(
        done.deadline_status(today()),
        DeadlineStatus::Set,
        "finished: just shown"
    );
}

#[test]
fn a_finished_or_trashed_project_holds_nothing_back_and_is_listed_nowhere_but_the_logbook() {
    let db = Database::open_in_memory().unwrap();
    project(&db, "p", Schedule::Someday);
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    project(&db, "q", Schedule::Date(date(2026, 12, 1)));
    db.trash_project("q").unwrap();

    for view in [View::Today, View::Upcoming, View::Anytime, View::Someday] {
        assert!(rows(&db, &view, today()).is_empty(), "{view:?}");
        count(&db, &view, today());
    }
}

#[test]
fn a_project_save_never_changes_its_status() {
    let db = Database::open_in_memory().unwrap();
    project(&db, "p", Schedule::None);
    task(&db, "t", Some("p"), Schedule::None);
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();

    // A new When for the finished project.
    db.set_project_schedule("p", &Schedule::Someday, today())
        .unwrap();

    let saved = db.get_project("p").unwrap().unwrap();
    assert_eq!(saved.status, Status::Completed);
    assert!(saved.completed_at.is_some());
    assert!(saved.is_someday, "the When was saved");

    // And it still reopens with its task.
    db.reopen_project("p").unwrap();
    assert_eq!(db.get_task("t").unwrap().unwrap().status, Status::Open);
}

#[test]
fn a_projects_schedule_is_stored_like_a_tasks() {
    let mut p = Project::new("p", "p", 0.0);
    p.apply_schedule(&Schedule::Today { evening: true }, today());
    assert_eq!(
        (p.start_date, p.time_of_day.as_deref(), p.is_someday),
        (Some(today()), Some("evening"), false)
    );
    p.apply_schedule(&Schedule::Someday, today());
    assert_eq!(
        (p.start_date, p.time_of_day.as_deref(), p.is_someday),
        (None, None, true)
    );
}
