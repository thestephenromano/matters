//! Moving tasks and projects (#148, part of #54): a task is filed in the
//! Inbox, an area, or a project and one of its headings, at the end of the
//! destination; moves that would break the hierarchy are refused, as are
//! writes that would store one.

use jiff::civil::Date;
use matters_core::{
    Area, ChecklistItem, Database, Heading, Home, MattersError, Project, QueryEngine, Schedule,
    Task, View,
};

fn today() -> Date {
    jiff::civil::date(2026, 11, 19)
}

/// Areas `home` and `work`; project `p` (headings `h1`, `h2`) and `q`
/// (heading `qh`); tasks `a`, `b` in the Inbox, `x` in `home`, `y` in `p`
/// above its headings and `z` under `h2`.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("home", "Home", 0.0)).unwrap();
    db.create_area(&Area::new("work", "Work", 1.0)).unwrap();
    db.create_project(&Project::new("p", "Garden", 0.0))
        .unwrap();
    db.create_project(&Project::new("q", "Shed", 1.0)).unwrap();
    for (id, project) in [("h1", "p"), ("h2", "p"), ("qh", "q")] {
        db.add_heading(&mut Heading::new(id, project, id, 0.0))
            .unwrap();
    }
    for (id, home) in [
        ("a", Home::Inbox),
        ("b", Home::Inbox),
        ("x", Home::Area("home".into())),
        ("y", project("p", None)),
        ("z", project("p", Some("h2"))),
    ] {
        let mut task = Task::new(id, id, 0.0);
        home.apply(&mut task);
        db.add_task(&mut task).unwrap();
    }
    db
}

fn project(id: &str, heading: Option<&str>) -> Home {
    Home::Project {
        project: id.into(),
        heading: heading.map(str::to_string),
    }
}

fn ids(db: &Database, view: View) -> Vec<String> {
    QueryEngine::new(db)
        .tasks_in(&view, today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect()
}

fn home_of(db: &Database, id: &str) -> Home {
    Home::of(&db.get_task(id).unwrap().unwrap()).unwrap()
}

#[test]
fn a_task_moves_to_the_end_of_each_kind_of_home() {
    let db = fixture();

    db.move_task("a", &Home::Area("home".into())).unwrap();
    assert_eq!(ids(&db, View::Inbox), ["b"], "it leaves the Inbox at once");
    assert_eq!(ids(&db, View::Area("home".into())), ["x", "a"]);

    db.move_task("a", &project("p", Some("h1"))).unwrap();
    assert_eq!(ids(&db, View::Area("home".into())), ["x"]);
    assert_eq!(ids(&db, View::Project("p".into())), ["y", "a", "z"]);
    assert_eq!(home_of(&db, "a"), project("p", Some("h1")));

    db.move_task("a", &project("p", None)).unwrap();
    assert_eq!(
        ids(&db, View::Project("p".into())),
        ["y", "a", "z"],
        "above the headings, after the tasks already there"
    );
    assert_eq!(home_of(&db, "a"), project("p", None));

    db.move_task("y", &project("p", Some("h2"))).unwrap();
    assert_eq!(
        ids(&db, View::Project("p".into())),
        ["a", "z", "y"],
        "within its project, to the end of a heading"
    );

    db.move_task("y", &project("q", Some("qh"))).unwrap();
    assert_eq!(ids(&db, View::Project("p".into())), ["a", "z"]);
    assert_eq!(ids(&db, View::Project("q".into())), ["y"]);

    db.move_task("y", &Home::Inbox).unwrap();
    assert_eq!(ids(&db, View::Inbox), ["b", "y"]);
    assert_eq!(home_of(&db, "y"), Home::Inbox);
}

#[test]
fn a_moved_task_keeps_its_schedule_deadline_notes_and_checklist() {
    let db = fixture();
    let mut task = db.get_task("a").unwrap().unwrap();
    task.apply_schedule(&Schedule::Today { evening: true }, today());
    task.deadline = Some(jiff::civil::date(2026, 12, 1));
    task.notes = "bring gloves".into();
    db.update_task(&task).unwrap();
    db.create_checklist_item(&ChecklistItem::new("c", "a", "rake", 0.0))
        .unwrap();

    db.move_task("a", &project("p", Some("h1"))).unwrap();

    let moved = db.get_task("a").unwrap().unwrap();
    assert_eq!(
        (
            moved.start_date,
            moved.time_of_day,
            moved.deadline,
            moved.notes
        ),
        (task.start_date, task.time_of_day, task.deadline, task.notes)
    );
    assert_eq!(db.list_checklist_items("a").unwrap().len(), 1);
    assert!(ids(&db, View::Today).contains(&"a".to_string()));

    db.move_task("a", &Home::Inbox).unwrap();
    assert_eq!(
        ids(&db, View::Inbox),
        ["b"],
        "a scheduled task is listed where its schedule puts it"
    );
    assert!(ids(&db, View::Today).contains(&"a".to_string()));
}

#[test]
fn moving_a_task_to_where_it_is_changes_nothing() {
    let db = fixture();
    let before = db.get_task("a").unwrap().unwrap();

    db.move_task("a", &Home::Inbox).unwrap();
    db.move_task("z", &project("p", Some("h2"))).unwrap();

    assert_eq!(ids(&db, View::Inbox), ["a", "b"]);
    assert_eq!(db.get_task("a").unwrap().unwrap(), before);
}

#[test]
fn a_move_to_a_missing_or_trashed_home_is_refused() {
    let db = fixture();
    db.create_area(&Area::new("old", "Old", 2.0)).unwrap();
    db.trash_area("old").unwrap();
    let mut boxed = Project::new("boxed", "Boxed", 2.0);
    boxed.area_id = Some("work".into());
    db.create_project(&boxed).unwrap();
    db.create_project(&Project::new("gone", "Gone", 3.0))
        .unwrap();
    db.trash_project("gone").unwrap();
    db.trash_area("work").unwrap();
    let before = db.get_task("a").unwrap().unwrap();

    for (home, entity) in [
        (Home::Area("missing".into()), "area"),
        (Home::Area("old".into()), "area"),
        (project("missing", None), "project"),
        (project("gone", None), "project"),
        (project("boxed", None), "project"),
        (project("p", Some("missing")), "heading"),
    ] {
        match db.move_task("a", &home) {
            Err(MattersError::NotFound { entity: e, .. }) => assert_eq!(e, entity, "{home:?}"),
            other => panic!("{home:?}: expected NotFound, got {other:?}"),
        }
    }
    assert!(
        matches!(
            db.move_task("a", &project("p", Some("qh"))),
            Err(MattersError::Validation(_))
        ),
        "a heading of another project"
    );
    assert_eq!(db.get_task("a").unwrap().unwrap(), before);
}

#[test]
fn a_task_in_the_trash_cannot_be_moved() {
    let db = fixture();
    db.trash_task("a").unwrap();
    db.trash_project("p").unwrap();

    for id in ["a", "y", "missing"] {
        assert!(
            matches!(
                db.move_task(id, &Home::Area("home".into())),
                Err(MattersError::NotFound { entity: "task", .. })
            ),
            "{id}"
        );
    }
    assert_eq!(home_of(&db, "a"), Home::Inbox);
    assert_eq!(home_of(&db, "y"), project("p", None));
}

/// Breaks a task's home.
type Breaks = fn(&mut Task);

#[test]
fn a_write_that_would_break_the_hierarchy_is_refused() {
    let db = fixture();
    let broken: [(&str, Breaks); 3] = [
        ("area and project", |t| {
            t.area_id = Some("home".into());
            t.project_id = Some("p".into());
        }),
        ("heading without a project", |t| {
            t.heading_id = Some("h1".into());
        }),
        ("heading of another project", |t| {
            t.project_id = Some("q".into());
            t.heading_id = Some("h1".into());
        }),
    ];
    for (name, breaks) in broken {
        let mut new = Task::new(format!("new {name}"), name, 9.0);
        breaks(&mut new);
        assert!(
            matches!(db.create_task(&new), Err(MattersError::Validation(_))),
            "create: {name}"
        );
        assert!(db.get_task(&new.id).unwrap().is_none(), "{name}");

        let before = db.get_task("a").unwrap().unwrap();
        let mut edited = before.clone();
        breaks(&mut edited);
        edited.title = "edited".into();
        assert!(
            matches!(db.update_task(&edited), Err(MattersError::Validation(_))),
            "update: {name}"
        );
        assert_eq!(db.get_task("a").unwrap().unwrap(), before, "{name}");
    }

    let mut missing = db.get_task("y").unwrap().unwrap();
    missing.heading_id = Some("missing".into());
    assert!(matches!(
        db.update_task(&missing),
        Err(MattersError::NotFound {
            entity: "heading",
            ..
        })
    ));
}

#[test]
fn home_reads_a_tasks_ids_and_apply_writes_them() {
    let mut task = Task::new("t", "t", 0.0);
    for home in [
        Home::Inbox,
        Home::Area("home".into()),
        project("p", None),
        project("p", Some("h1")),
        Home::Area("work".into()),
        Home::Inbox,
    ] {
        home.apply(&mut task);
        assert_eq!(Home::of(&task).unwrap(), home);
    }
}

fn sidebar(db: &Database) -> Vec<(String, Option<String>)> {
    db.list_projects()
        .unwrap()
        .into_iter()
        .map(|p| (p.id, p.area_id))
        .collect()
}

#[test]
fn a_project_filed_in_an_area_goes_to_the_end_of_its_projects() {
    let db = fixture();
    let mut r = Project::new("r", "Roof", 2.0);
    r.area_id = Some("home".into());
    db.create_project(&r).unwrap();
    let home = Some("home".to_string());

    db.set_project_area("p", Some("home")).unwrap();
    assert_eq!(
        sidebar(&db),
        [
            ("q".into(), None),
            ("r".into(), home.clone()),
            ("p".into(), home.clone())
        ]
    );

    db.set_project_area("r", Some("home")).unwrap();
    assert_eq!(
        sidebar(&db)[1],
        ("r".into(), home.clone()),
        "filing it where it is keeps its place"
    );

    db.set_project_area("r", None).unwrap();
    assert_eq!(
        sidebar(&db),
        [
            ("q".into(), None),
            ("p".into(), home.clone()),
            ("r".into(), None)
        ]
    );
}
