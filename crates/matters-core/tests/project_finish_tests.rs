//! Completing, canceling and reopening projects (#152, part of #55).
//! Finishing a project closes its open tasks only as the caller says;
//! reopening puts back exactly what finishing changed. A finished project
//! leaves the sidebar, its tasks leave the other views, and the Logbook
//! lists it as one row.

use jiff::civil::Date;
use matters_core::storage::schema::{
    MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5, MIGRATION_V6, SCHEMA_V1,
};
use matters_core::{
    logbook_section_key, Area, Database, ListRow, MattersError, Outcome, Project, QueryEngine,
    Status, Task, View,
};

fn today() -> Date {
    jiff::civil::date(2026, 11, 19)
}

fn task_in(db: &Database, id: &str, project: &str) -> Task {
    let mut task = Task::new(id, id, 0.0);
    task.project_id = Some(project.into());
    db.add_task(&mut task).unwrap();
    task
}

/// Project `p` with open tasks `a` and `b`, `done` finished before, and an
/// open task `binned` in the Trash.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "Garden", 0.0))
        .unwrap();
    for id in ["a", "b", "done", "binned"] {
        task_in(&db, id, "p");
    }
    db.complete_task("done").unwrap();
    db.trash_task("binned").unwrap();
    db
}

fn task(db: &Database, id: &str) -> Task {
    db.get_task(id).unwrap().unwrap()
}

fn project(db: &Database, id: &str) -> Project {
    db.get_project(id).unwrap().unwrap()
}

fn status(db: &Database, id: &str) -> Status {
    task(db, id).status
}

#[test]
fn a_project_is_finished_with_a_choice_for_its_open_tasks() {
    for (outcome, open_tasks) in [
        (Outcome::Completed, Outcome::Completed),
        (Outcome::Completed, Outcome::Canceled),
        (Outcome::Canceled, Outcome::Canceled),
        (Outcome::Canceled, Outcome::Completed),
    ] {
        let db = fixture();
        let done_before = task(&db, "done");

        db.finish_project("p", outcome, open_tasks).unwrap();

        let p = project(&db, "p");
        assert_eq!(p.status, Status::from(outcome));
        assert!(p.completed_at.is_some());
        for id in ["a", "b"] {
            let t = task(&db, id);
            assert_eq!(t.status, Status::from(open_tasks), "{id}");
            assert_eq!(
                t.completed_at, p.completed_at,
                "{id} closed with the project"
            );
        }
        let done = task(&db, "done");
        assert_eq!(
            (done.status, done.completed_at),
            (done_before.status, done_before.completed_at),
            "a task finished before keeps its state"
        );
        assert_eq!(
            status(&db, "binned"),
            Status::Open,
            "the Trash is left alone"
        );
    }
}

#[test]
fn reopening_puts_back_exactly_what_finishing_closed() {
    let db = fixture();
    let done_before = task(&db, "done");
    db.finish_project("p", Outcome::Completed, Outcome::Canceled)
        .unwrap();

    db.reopen_project("p").unwrap();

    let p = project(&db, "p");
    assert_eq!((p.status, p.completed_at), (Status::Open, None));
    for id in ["a", "b"] {
        let t = task(&db, id);
        assert_eq!((t.status, t.completed_at), (Status::Open, None), "{id}");
    }
    assert_eq!(task(&db, "done"), done_before);

    // The mark is spent: finishing and reopening again changes only what
    // the second finish closed.
    db.complete_task("a").unwrap();
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    db.reopen_project("p").unwrap();
    assert_eq!(status(&db, "a"), Status::Completed);
    assert_eq!(status(&db, "b"), Status::Open);
}

#[test]
fn a_task_changed_by_hand_after_finishing_is_left_as_it_is_on_reopening() {
    let db = fixture();
    task_in(&db, "c", "p");
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();

    // Reopened by hand, then finished by hand: the user's own decision.
    db.uncomplete_task("a").unwrap();
    db.complete_task("a").unwrap();
    // Ticked again though already done.
    db.complete_task("b").unwrap();
    // Reopened, then canceled.
    db.uncomplete_task("c").unwrap();
    db.cancel_task("c").unwrap();

    db.reopen_project("p").unwrap();
    assert_eq!(status(&db, "a"), Status::Completed);
    assert_eq!(status(&db, "b"), Status::Completed);
    assert_eq!(status(&db, "c"), Status::Canceled);
}

#[test]
fn a_save_that_keeps_a_task_closed_keeps_its_mark() {
    let db = fixture();
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    db.set_task_notes("a", "edited while the project was finished")
        .unwrap();

    db.reopen_project("p").unwrap();
    assert_eq!(status(&db, "a"), Status::Open);
}

#[test]
fn finishing_or_reopening_the_wrong_project_is_refused() {
    let db = fixture();
    db.create_area(&Area::new("home", "Home", 0.0)).unwrap();
    let mut boxed = Project::new("boxed", "Boxed", 1.0);
    boxed.area_id = Some("home".into());
    db.create_project(&boxed).unwrap();
    db.trash_area("home").unwrap();
    db.create_project(&Project::new("gone", "Gone", 2.0))
        .unwrap();
    db.trash_project("gone").unwrap();

    for id in ["missing", "gone", "boxed"] {
        for result in [
            db.finish_project(id, Outcome::Completed, Outcome::Completed),
            db.reopen_project(id),
        ] {
            assert!(
                matches!(
                    result,
                    Err(MattersError::NotFound {
                        entity: "project",
                        ..
                    })
                ),
                "{id}: {result:?}"
            );
        }
    }

    assert!(matches!(
        db.reopen_project("p"),
        Err(MattersError::Validation(_))
    ));
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    let finished = project(&db, "p");
    assert!(matches!(
        db.finish_project("p", Outcome::Canceled, Outcome::Canceled),
        Err(MattersError::Validation(_))
    ));
    assert_eq!(project(&db, "p"), finished, "nothing changed");
}

fn ids(db: &Database, view: &View) -> Vec<String> {
    QueryEngine::new(db)
        .tasks_in(view, today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect()
}

#[test]
fn a_finished_projects_tasks_leave_the_other_views_and_their_counts() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "Garden", 0.0))
        .unwrap();
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    // Open tasks can still be added to a finished project, from its view.
    let days = [
        ("today", Some(today()), false),
        ("upcoming", Some(jiff::civil::date(2026, 12, 1)), false),
        ("anytime", None, false),
        ("someday", None, true),
    ];
    for (id, start, someday) in days {
        let mut t = Task::new(id, id, 0.0);
        t.project_id = Some("p".into());
        t.start_date = start;
        t.is_someday = someday;
        db.add_task(&mut t).unwrap();
    }
    let views = [View::Today, View::Upcoming, View::Anytime, View::Someday];
    let engine = QueryEngine::new(&db);

    for view in &views {
        assert!(ids(&db, view).is_empty(), "{view:?}");
    }
    let counts = engine.counts(today()).unwrap();
    assert_eq!(
        (
            counts.today,
            counts.upcoming,
            counts.anytime,
            counts.someday
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        ids(&db, &View::Project("p".into())).len(),
        4,
        "listed in the project"
    );

    db.reopen_project("p").unwrap();
    for (view, id) in views
        .iter()
        .zip(["today", "upcoming", "anytime", "someday"])
    {
        assert!(ids(&db, view).contains(&id.to_string()), "{view:?}");
    }
    let counts = engine.counts(today()).unwrap();
    assert_eq!(
        (
            counts.today,
            counts.upcoming,
            counts.anytime,
            counts.someday
        ),
        (1, 1, 2, 1),
        "Anytime lists today's task too"
    );
}

#[test]
fn the_sidebar_lists_open_projects_and_finished_ones_are_listed_apart() {
    let db = Database::open_in_memory().unwrap();
    for (i, id) in ["p", "q", "r"].into_iter().enumerate() {
        db.create_project(&Project::new(id, id, i as f64)).unwrap();
    }
    db.finish_project("q", Outcome::Completed, Outcome::Completed)
        .unwrap();
    db.finish_project("r", Outcome::Canceled, Outcome::Canceled)
        .unwrap();
    db.with_raw_conn(|c| {
        c.execute_batch("UPDATE projects SET completed_at = 100 WHERE id = 'q';")?;
        Ok(())
    })
    .unwrap();

    let open: Vec<String> = db
        .list_projects()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(open, ["p"]);
    let finished: Vec<String> = db
        .list_finished_projects()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(finished, ["r", "q"], "most recently finished first");

    db.trash_project("r").unwrap();
    let finished: Vec<String> = db
        .list_finished_projects()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(finished, ["q"], "not the Trash");
}

/// Set when something was finished, to order the Logbook.
fn finished_at(db: &Database, table: &str, id: &str, at: i64) {
    db.with_raw_conn(|c| {
        c.execute(
            &format!("UPDATE {table} SET completed_at = ?1 WHERE id = ?2"),
            rusqlite::params![at, id],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn the_logbook_lists_a_finished_project_as_one_row_among_the_tasks() {
    let db = fixture();
    for id in ["early", "late"] {
        let mut t = Task::new(id, id, 0.0);
        db.add_task(&mut t).unwrap();
        db.complete_task(id).unwrap();
    }
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    db.create_project(&Project::new("old", "Old", 1.0)).unwrap();
    db.finish_project("old", Outcome::Canceled, Outcome::Canceled)
        .unwrap();
    finished_at(&db, "tasks", "late", 300);
    finished_at(&db, "projects", "p", 200);
    finished_at(&db, "tasks", "early", 100);
    finished_at(&db, "projects", "old", 50);

    let engine = QueryEngine::new(&db);
    let tasks = engine.tasks_in(&View::Logbook, today()).unwrap();
    let rows: Vec<String> = engine
        .list_rows(&View::Logbook, tasks, today())
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id,
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
        })
        .collect();
    assert_eq!(
        rows,
        ["late", "<p>", "early", "<old>"],
        "not the project's tasks"
    );
    assert_eq!(engine.counts(today()).unwrap().logbook, rows.len());

    db.reopen_project("p").unwrap();
    let tasks = engine.tasks_in(&View::Logbook, today()).unwrap();
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    assert_eq!(
        ids,
        ["done", "late", "early"],
        "a task finished before is listed on its own again"
    );
    assert_eq!(engine.counts(today()).unwrap().logbook, 4);
}

#[test]
fn a_task_finished_at_the_same_moment_as_a_project_comes_first() {
    let db = Database::open_in_memory().unwrap();
    let mut t = Task::new("t", "t", 0.0);
    db.add_task(&mut t).unwrap();
    db.complete_task("t").unwrap();
    db.create_project(&Project::new("p", "P", 0.0)).unwrap();
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    finished_at(&db, "tasks", "t", 100);
    finished_at(&db, "projects", "p", 100);

    let engine = QueryEngine::new(&db);
    let tasks = engine.tasks_in(&View::Logbook, today()).unwrap();
    let rows = engine.list_rows(&View::Logbook, tasks, today()).unwrap();
    assert!(
        matches!(rows.as_slice(), [ListRow::Task(t), ListRow::Project(p)] if t.id == "t" && p.id == "p"),
        "{rows:?}"
    );
}

#[test]
fn a_task_reopened_during_a_logbook_visit_keeps_its_place_among_projects() {
    let db = Database::open_in_memory().unwrap();
    for id in ["t1", "t2"] {
        let mut t = Task::new(id, id, 0.0);
        db.add_task(&mut t).unwrap();
        db.complete_task(id).unwrap();
    }
    db.create_project(&Project::new("p", "P", 0.0)).unwrap();
    db.finish_project("p", Outcome::Completed, Outcome::Completed)
        .unwrap();
    finished_at(&db, "tasks", "t1", 300);
    finished_at(&db, "projects", "p", 200);
    finished_at(&db, "tasks", "t2", 100);
    let engine = QueryEngine::new(&db);
    let mut tasks = engine.tasks_in(&View::Logbook, today()).unwrap();
    // t2 reopened while the Logbook shows it: kept in place, no finish time.
    tasks[1].completed_at = None;
    tasks[1].status = Status::Open;

    let rows: Vec<String> = engine
        .list_rows(&View::Logbook, tasks, today())
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id,
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
        })
        .collect();
    assert_eq!(rows, ["t1", "<p>", "t2"]);
}

#[test]
fn logbook_section_keys_name_the_day() {
    assert_eq!(logbook_section_key(None), "done:");
    let noon = jiff::civil::date(2026, 11, 19)
        .at(12, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::system())
        .unwrap()
        .timestamp()
        .as_millisecond();
    assert_eq!(logbook_section_key(Some(noon)), "done:2026-11-19");
}

#[test]
fn a_v6_database_is_migrated_with_no_task_marked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        for migration in [
            MIGRATION_V2,
            MIGRATION_V3,
            MIGRATION_V4,
            MIGRATION_V5,
            MIGRATION_V6,
        ] {
            conn.execute_batch(migration).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO schema_migrations (version, applied_at)
             VALUES (1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0);
             INSERT INTO projects (id, title, status, completed_at, sort_order, created_at, updated_at)
             VALUES ('p', 'Old project', 'completed', 10, 0, 0, 10);
             INSERT INTO tasks (id, project_id, title, status, completed_at, sort_order, created_at, updated_at)
             VALUES ('t', 'p', 'Closed long ago', 'completed', 10, 0, 0, 10);",
        )
        .unwrap();
    }

    for _ in 0..2 {
        let db = Database::open(&path).unwrap();
        let marked: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM tasks WHERE closed_by_project = 1",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(marked, 0);
        let version: i64 = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert!(version >= 7, "v7 applied (later migrations may follow)");
    }
    // Reopening a project finished before the mark existed reopens only it.
    let db = Database::open(&path).unwrap();
    db.reopen_project("p").unwrap();
    assert_eq!(project(&db, "p").status, Status::Open);
    assert_eq!(status(&db, "t"), Status::Completed);
}
