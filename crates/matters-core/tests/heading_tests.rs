//! Headings in projects (#22): a project lists its tasks above the headings,
//! then each heading's; headings are added, renamed, ordered and deleted;
//! moving a task past a heading files it under that heading.

use jiff::civil::Date;
use matters_core::ordering::{
    add_task_below, move_heading, move_task, place_heading, place_in_heading, place_task,
};
use matters_core::{Database, Heading, MattersError, Project, QueryEngine, Task, View};

fn today() -> Date {
    jiff::civil::date(2026, 11, 18)
}

fn project() -> View {
    View::Project("p".into())
}

/// Project `p` with headings and tasks: `tasks` are (id, heading) pairs,
/// created in order, all with the same key so the first move renumbers.
fn fixture(headings: &[&str], tasks: &[(&str, Option<&str>)]) -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "Garden", 0.0))
        .unwrap();
    db.create_project(&Project::new("q", "Shed", 1.0)).unwrap();
    for h in headings {
        db.add_heading(&mut Heading::new(*h, "p", *h, 0.0)).unwrap();
    }
    for (i, (id, heading)) in tasks.iter().enumerate() {
        let mut t = Task::new(*id, *id, 0.0);
        t.project_id = Some("p".into());
        t.heading_id = heading.map(str::to_string);
        t.created_at = i as i64;
        db.create_task(&t).unwrap();
    }
    db
}

/// The project as listed: each task with its section key's heading id.
fn listed(db: &Database) -> Vec<(String, String)> {
    let engine = QueryEngine::new(db);
    let tasks = engine.tasks_in(&project(), today()).unwrap();
    let keys = engine.section_keys(&project(), &tasks).unwrap();
    tasks
        .into_iter()
        .zip(keys)
        .map(|(t, key)| {
            let heading = key.split(':').nth(1).unwrap_or("").to_string();
            (t.id, heading)
        })
        .collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn heading_ids(db: &Database) -> Vec<String> {
    db.list_headings_for_project("p")
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect()
}

#[test]
fn a_project_lists_its_tasks_above_the_headings_then_each_heading() {
    let db = fixture(
        &["plan", "build"],
        &[
            ("b1", Some("build")),
            ("loose", None),
            ("p1", Some("plan")),
            ("b2", Some("build")),
        ],
    );
    assert_eq!(
        listed(&db),
        pairs(&[
            ("loose", ""),
            ("p1", "plan"),
            ("b1", "build"),
            ("b2", "build")
        ])
    );
    let engine = QueryEngine::new(&db);
    let tasks = engine.tasks_in(&project(), today()).unwrap();
    assert_eq!(
        engine.section_keys(&project(), &tasks).unwrap()[1],
        "heading:plan:plan",
        "the key carries the heading's id and title"
    );
}

#[test]
fn headings_are_added_at_the_end_renamed_and_reordered_with_their_tasks() {
    let db = fixture(
        &["plan", "build"],
        &[("p1", Some("plan")), ("b1", Some("build"))],
    );
    let mut late = Heading::new("late", "p", "  tidy   up ", 0.0);
    db.add_heading(&mut late).unwrap();
    assert_eq!(late.title, "tidy up");
    assert_eq!(heading_ids(&db), ["plan", "build", "late"]);

    db.rename_heading("plan", " Planning ").unwrap();
    assert_eq!(db.get_heading("plan").unwrap().unwrap().title, "Planning");
    assert!(matches!(
        db.rename_heading("plan", "  "),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        db.rename_heading("gone", "x"),
        Err(MattersError::NotFound { .. })
    ));
    let mut blank = Heading::new("blank", "p", " ", 0.0);
    assert!(db.add_heading(&mut blank).is_err());
    let mut orphan = Heading::new("orphan", "no-such-project", "x", 0.0);
    assert!(db.add_heading(&mut orphan).is_err());
    assert_eq!(heading_ids(&db), ["plan", "build", "late"]);

    place_heading(&db, "plan", Some("build")).unwrap();
    assert_eq!(heading_ids(&db), ["build", "plan", "late"]);
    assert_eq!(
        listed(&db),
        pairs(&[("b1", "build"), ("p1", "plan")]),
        "tasks follow their heading"
    );
    move_heading(&db, "late", -2).unwrap();
    assert_eq!(heading_ids(&db), ["late", "build", "plan"]);
    move_heading(&db, "late", -1).unwrap();
    assert_eq!(
        heading_ids(&db),
        ["late", "build", "plan"],
        "the top stops a move"
    );
    assert!(
        place_heading(&db, "plan", Some("other-project-heading")).is_err(),
        "only among its own project's headings"
    );
}

#[test]
fn deleting_a_heading_keeps_its_tasks_and_undo_puts_them_back() {
    let db = fixture(
        &["plan", "build"],
        &[
            ("loose", None),
            ("p1", Some("plan")),
            ("p2", Some("plan")),
            ("b1", Some("build")),
        ],
    );
    db.trash_task("p2").unwrap();
    let deleted = db.delete_heading("plan").unwrap();
    assert_eq!(deleted.heading.title, "plan");
    assert_eq!(deleted.task_ids, ["p1", "p2"]);
    assert_eq!(heading_ids(&db), ["build"]);
    assert_eq!(
        listed(&db),
        pairs(&[("loose", ""), ("p1", ""), ("b1", "build")]),
        "the tasks stay in the project, above the headings"
    );
    assert!(matches!(
        db.delete_heading("plan"),
        Err(MattersError::NotFound { .. })
    ));

    db.restore_heading(&deleted).unwrap();
    assert_eq!(heading_ids(&db), ["plan", "build"]);
    assert_eq!(
        listed(&db),
        pairs(&[("loose", ""), ("p1", "plan"), ("b1", "build")])
    );
    db.restore_task("p2").unwrap();
    assert_eq!(
        db.get_task("p2").unwrap().unwrap().heading_id.as_deref(),
        Some("plan"),
        "a trashed task comes back under it too"
    );
}

#[test]
fn undo_leaves_a_task_filed_elsewhere_since() {
    let db = fixture(&["plan", "build"], &[("p1", Some("plan"))]);
    let deleted = db.delete_heading("plan").unwrap();
    place_in_heading(&db, "p", today(), "p1", Some("build"), None).unwrap();
    db.restore_heading(&deleted).unwrap();
    assert_eq!(listed(&db), pairs(&[("p1", "build")]));
}

#[test]
fn moving_past_a_heading_files_the_task_under_it() {
    let db = fixture(
        &["plan", "empty", "build"],
        &[
            ("loose", None),
            ("p1", Some("plan")),
            ("p2", Some("plan")),
            ("b1", Some("build")),
        ],
    );
    // Down from the part above the headings: the top of "plan".
    move_task(&db, &project(), today(), "loose", 1).unwrap();
    assert_eq!(
        listed(&db),
        pairs(&[
            ("loose", "plan"),
            ("p1", "plan"),
            ("p2", "plan"),
            ("b1", "build")
        ])
    );
    // Within "plan", then past its end into the empty heading.
    move_task(&db, &project(), today(), "loose", 2).unwrap();
    assert_eq!(listed(&db)[2], ("loose".into(), "plan".into()));
    move_task(&db, &project(), today(), "loose", 1).unwrap();
    assert_eq!(listed(&db)[2], ("loose".into(), "empty".into()));
    // On into "build", above its task.
    move_task(&db, &project(), today(), "loose", 1).unwrap();
    assert_eq!(
        listed(&db)[2..],
        pairs(&[("loose", "build"), ("b1", "build")])
    );
    // Up: past the empty heading, to the end of "plan".
    move_task(&db, &project(), today(), "loose", -1).unwrap();
    move_task(&db, &project(), today(), "loose", -1).unwrap();
    assert_eq!(
        listed(&db),
        pairs(&[
            ("p1", "plan"),
            ("p2", "plan"),
            ("loose", "plan"),
            ("b1", "build")
        ])
    );
    // Up from the top of the first heading: the part above the headings.
    move_task(&db, &project(), today(), "p1", -1).unwrap();
    assert_eq!(listed(&db)[0], ("p1".into(), "".into()));
    // The ends of the project stop a move.
    move_task(&db, &project(), today(), "p1", -1).unwrap();
    assert_eq!(listed(&db)[0], ("p1".into(), "".into()));
    move_task(&db, &project(), today(), "b1", 1).unwrap();
    assert_eq!(listed(&db).last().unwrap(), &("b1".into(), "build".into()));
}

#[test]
fn a_big_step_stops_at_the_end_of_the_list() {
    let db = fixture(
        &["h"],
        &[("a", Some("h")), ("b", Some("h")), ("c", Some("h"))],
    );
    move_task(&db, &project(), today(), "a", 9).unwrap();
    assert_eq!(listed(&db), pairs(&[("b", "h"), ("c", "h"), ("a", "h")]));
    move_task(&db, &project(), today(), "a", -9).unwrap();
    assert_eq!(listed(&db)[0].0, "a");
}

#[test]
fn placing_under_a_heading_needs_a_neighbour_there() {
    let db = fixture(
        &["plan", "build"],
        &[("p1", Some("plan")), ("b1", Some("build"))],
    );
    db.add_heading(&mut Heading::new("shed-h", "q", "Shed heading", 0.0))
        .unwrap();
    assert!(
        place_in_heading(&db, "p", today(), "p1", Some("build"), Some("p1")).is_err(),
        "a task can't follow itself"
    );
    assert!(
        place_in_heading(&db, "p", today(), "p1", Some("plan"), Some("b1")).is_err(),
        "after must be under the target heading"
    );
    assert!(
        place_in_heading(&db, "p", today(), "p1", Some("shed-h"), None).is_err(),
        "another project's heading"
    );
    assert!(
        place_task(&db, &project(), today(), "p1", Some("b1")).is_err(),
        "place_task never changes a task's heading"
    );
    assert_eq!(
        listed(&db),
        pairs(&[("p1", "plan"), ("b1", "build")]),
        "refused moves change nothing"
    );

    place_in_heading(&db, "p", today(), "p1", Some("build"), Some("b1")).unwrap();
    assert_eq!(listed(&db), pairs(&[("b1", "build"), ("p1", "build")]));
    place_in_heading(&db, "p", today(), "p1", None, None).unwrap();
    assert_eq!(listed(&db), pairs(&[("p1", ""), ("b1", "build")]));
}

#[test]
fn a_task_added_below_another_joins_its_heading() {
    let db = fixture(&["plan"], &[("p1", Some("plan")), ("p2", Some("plan"))]);
    let mut new = project().new_task("new", "new", today()).unwrap();
    add_task_below(&db, &project(), today(), &mut new, "p1").unwrap();
    assert_eq!(
        listed(&db),
        pairs(&[("p1", "plan"), ("new", "plan"), ("p2", "plan")])
    );
}

#[test]
fn an_empty_heading_has_a_row_where_its_tasks_would_be() {
    use matters_core::ListRow;
    let db = fixture(
        &["plan", "empty", "build", "last"],
        &[("loose", None), ("p1", Some("plan")), ("b1", Some("build"))],
    );
    let engine = QueryEngine::new(&db);
    let tasks = engine.tasks_in(&project(), today()).unwrap();
    let rows: Vec<String> = engine
        .list_rows(&project(), tasks, today())
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id,
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::Event(e) => format!("({})", e.key()),
        })
        .collect();
    assert_eq!(rows, ["loose", "p1", "[empty]", "b1", "[last]"]);

    // Other views have only tasks.
    let inbox = engine.tasks_in(&View::Inbox, today()).unwrap();
    assert!(engine
        .list_rows(&View::Inbox, inbox, today())
        .unwrap()
        .iter()
        .all(|row| matches!(row, ListRow::Task(_))));
}

#[test]
fn rows_keep_the_order_of_the_tasks_given_within_each_heading() {
    use matters_core::ListRow;
    let db = fixture(&["h"], &[("a", Some("h")), ("b", Some("h")), ("x", None)]);
    let engine = QueryEngine::new(&db);
    // The order Visit gives (a ticked task kept in place), not the query's.
    let mut tasks = engine.tasks_in(&project(), today()).unwrap();
    tasks.reverse();
    let rows: Vec<String> = engine
        .list_rows(&project(), tasks, today())
        .unwrap()
        .into_iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id,
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::Event(e) => format!("({})", e.key()),
        })
        .collect();
    assert_eq!(rows, ["x", "b", "a"]);
}
