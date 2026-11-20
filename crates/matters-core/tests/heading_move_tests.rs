//! Moving a heading to another project (#169): the heading goes last among
//! the target's headings, with its tasks; refused targets change nothing.

use matters_core::ordering::move_heading_to_project;
use matters_core::{Area, Database, Heading, MattersError, Project, QueryEngine, Task, View};

fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    for (i, id) in ["p", "q"].into_iter().enumerate() {
        db.create_project(&Project::new(id, id, i as f64)).unwrap();
    }
    for (id, project) in [("h", "p"), ("other", "p"), ("q1", "q")] {
        db.add_heading(&mut Heading::new(id, project, id, 0.0))
            .unwrap();
    }
    for (id, heading) in [
        ("t1", Some("h")),
        ("t2", Some("h")),
        ("loose", None),
        ("o", Some("other")),
    ] {
        let mut t = Task::new(id, id, 0.0);
        t.project_id = Some("p".into());
        t.heading_id = heading.map(str::to_string);
        db.add_task(&mut t).unwrap();
    }
    db
}

fn listed(db: &Database, project: &str) -> Vec<String> {
    let engine = QueryEngine::new(db);
    let view = View::Project(project.into());
    let tasks = engine
        .tasks_in(&view, jiff::civil::date(2026, 11, 20))
        .unwrap();
    tasks
        .into_iter()
        .map(|t| format!("{}:{}", t.heading_id.unwrap_or_default(), t.id))
        .collect()
}

fn headings(db: &Database, project: &str) -> Vec<String> {
    db.list_headings_for_project(project)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect()
}

#[test]
fn a_heading_moves_with_its_tasks_to_the_end_of_another_project() {
    let db = fixture();
    move_heading_to_project(&db, "h", "q").unwrap();

    assert_eq!(headings(&db, "p"), ["other"]);
    assert_eq!(headings(&db, "q"), ["q1", "h"], "last among the headings");
    assert_eq!(listed(&db, "p"), [":loose", "other:o"]);
    assert_eq!(
        listed(&db, "q"),
        ["h:t1", "h:t2"],
        "its tasks, in their order"
    );
    for id in ["t1", "t2"] {
        assert_eq!(
            db.get_task(id).unwrap().unwrap().project_id.as_deref(),
            Some("q")
        );
    }
}

#[test]
fn a_heading_moves_to_a_project_with_no_headings() {
    let db = fixture();
    db.create_project(&Project::new("empty", "empty", 2.0))
        .unwrap();
    move_heading_to_project(&db, "h", "empty").unwrap();
    assert_eq!(headings(&db, "empty"), ["h"]);
}

#[test]
fn a_move_to_a_missing_trashed_or_the_same_project_is_refused() {
    let db = fixture();
    db.create_project(&Project::new("gone", "gone", 2.0))
        .unwrap();
    db.trash_project("gone").unwrap();
    db.create_area(&Area::new("old", "old", 0.0)).unwrap();
    let mut boxed = Project::new("boxed", "boxed", 3.0);
    boxed.area_id = Some("old".into());
    db.create_project(&boxed).unwrap();
    db.trash_area("old").unwrap();

    for target in ["missing", "gone", "boxed"] {
        assert!(
            matches!(
                move_heading_to_project(&db, "h", target),
                Err(MattersError::NotFound {
                    entity: "project",
                    ..
                })
            ),
            "{target}"
        );
    }
    assert!(matches!(
        move_heading_to_project(&db, "h", "p"),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        move_heading_to_project(&db, "missing", "q"),
        Err(MattersError::NotFound {
            entity: "heading",
            ..
        })
    ));
    assert_eq!(headings(&db, "p"), ["h", "other"]);
    assert_eq!(
        db.get_task("t1").unwrap().unwrap().project_id.as_deref(),
        Some("p")
    );
}
