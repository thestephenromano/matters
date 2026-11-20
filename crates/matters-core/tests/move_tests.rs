//! Moving tasks and projects (#148, part of #54): a task is filed in the
//! Inbox, an area, or a project and one of its headings, at the end of the
//! destination; moves that would break the hierarchy are refused, as are
//! new tasks that would store one. A whole-entity save never changes where
//! a task or project lives.

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

    db.set_task_home("a", &Home::Area("home".into())).unwrap();
    assert_eq!(ids(&db, View::Inbox), ["b"], "it leaves the Inbox at once");
    assert_eq!(ids(&db, View::Area("home".into())), ["x", "a"]);

    db.set_task_home("a", &project("p", Some("h1"))).unwrap();
    assert_eq!(ids(&db, View::Area("home".into())), ["x"]);
    assert_eq!(ids(&db, View::Project("p".into())), ["y", "a", "z"]);
    assert_eq!(home_of(&db, "a"), project("p", Some("h1")));

    db.set_task_home("a", &project("p", None)).unwrap();
    assert_eq!(
        ids(&db, View::Project("p".into())),
        ["y", "a", "z"],
        "above the headings, after the tasks already there"
    );
    assert_eq!(home_of(&db, "a"), project("p", None));

    db.set_task_home("y", &project("p", Some("h2"))).unwrap();
    assert_eq!(
        ids(&db, View::Project("p".into())),
        ["a", "z", "y"],
        "within its project, to the end of a heading"
    );

    db.set_task_home("y", &project("q", Some("qh"))).unwrap();
    assert_eq!(ids(&db, View::Project("p".into())), ["a", "z"]);
    assert_eq!(ids(&db, View::Project("q".into())), ["y"]);

    db.set_task_home("y", &Home::Inbox).unwrap();
    assert_eq!(ids(&db, View::Inbox), ["b", "y"]);
    assert_eq!(home_of(&db, "y"), Home::Inbox);
}

#[test]
fn a_moved_task_keeps_its_schedule_deadline_notes_and_checklist() {
    let db = fixture();
    db.set_task_schedule("a", &Schedule::Today { evening: true }, today())
        .unwrap();
    db.set_task_deadline("a", Some(jiff::civil::date(2026, 12, 1)))
        .unwrap();
    db.set_task_notes("a", "bring gloves").unwrap();
    let task = db.get_task("a").unwrap().unwrap();
    db.create_checklist_item(&ChecklistItem::new("c", "a", "rake", 0.0))
        .unwrap();

    db.set_task_home("a", &project("p", Some("h1"))).unwrap();

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

    db.set_task_home("a", &Home::Inbox).unwrap();
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

    db.set_task_home("a", &Home::Inbox).unwrap();
    db.set_task_home("z", &project("p", Some("h2"))).unwrap();

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
        match db.set_task_home("a", &home) {
            Err(MattersError::NotFound { entity: e, .. }) => assert_eq!(e, entity, "{home:?}"),
            other => panic!("{home:?}: expected NotFound, got {other:?}"),
        }
    }
    assert!(
        matches!(
            db.set_task_home("a", &project("p", Some("qh"))),
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
                db.set_task_home(id, &Home::Area("home".into())),
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
fn a_new_task_with_a_broken_hierarchy_is_refused() {
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
    }

    let mut missing = Task::new("new missing", "missing", 9.0);
    missing.project_id = Some("p".into());
    missing.heading_id = Some("missing".into());
    assert!(matches!(
        db.create_task(&missing),
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

// Review follow-up (#149): only the guarded moves change where something
// lives; a whole-entity save keeps it.

#[test]
fn a_project_in_the_trash_cannot_be_refiled() {
    let db = fixture();
    db.create_area(&Area::new("old", "Old", 2.0)).unwrap();
    let mut boxed = Project::new("boxed", "Boxed", 2.0);
    boxed.area_id = Some("old".into());
    db.create_project(&boxed).unwrap();
    db.trash_area("old").unwrap();
    db.trash_project("q").unwrap();

    for (project, area) in [
        ("boxed", None),
        ("boxed", Some("home")),
        ("q", Some("home")),
        ("q", None),
    ] {
        assert!(
            matches!(
                db.set_project_area(project, area),
                Err(MattersError::NotFound {
                    entity: "project",
                    ..
                })
            ),
            "{project} to {area:?}"
        );
    }
    assert_eq!(
        db.get_project("boxed").unwrap().unwrap().area_id.as_deref(),
        Some("old")
    );
    db.restore_area("old").unwrap();
    assert!(
        sidebar(&db).contains(&("boxed".into(), Some("old".into()))),
        "Put Back still brings the project back with its area"
    );
}

#[test]
fn saving_a_task_never_changes_its_home() {
    let db = fixture();
    db.create_project(&Project::new("gone", "Gone", 3.0))
        .unwrap();
    db.trash_project("gone").unwrap();
    let before = db.get_task("y").unwrap().unwrap();

    // Only set_task_home writes a home: every field edit leaves it.
    db.set_task_notes("y", "edited").unwrap();
    db.set_task_schedule("y", &Schedule::Someday, today())
        .unwrap();
    db.set_task_deadline("y", Some(today())).unwrap();
    db.rename_task("y", "renamed").unwrap();
    let saved = db.get_task("y").unwrap().unwrap();
    assert_eq!(saved.notes, "edited");
    assert_eq!(Home::of(&saved).unwrap(), project("p", None));
    assert_eq!(saved.sort_order, before.sort_order);
    assert_eq!(ids(&db, View::Project("p".into())), ["y", "z"]);
}

#[test]
fn saving_a_project_never_changes_its_area() {
    let db = fixture();
    db.trash_area("work").unwrap();
    // Only set_project_area files a project: every field edit leaves it.
    db.set_project_notes("p", "edited").unwrap();
    db.set_project_schedule("p", &Schedule::Someday, today())
        .unwrap();
    db.set_project_deadline("p", Some(today())).unwrap();
    db.rename_project("p", "renamed").unwrap();
    let saved = db.get_project("p").unwrap().unwrap();
    assert_eq!(saved.area_id, None);
    assert_eq!(saved.notes, "edited");
    assert_eq!(sidebar(&db), [("p".into(), None), ("q".into(), None)]);
}

#[test]
fn a_task_stored_before_the_hierarchy_rule_stays_editable() {
    let db = fixture();
    // Earlier versions accepted any combination of ids; such rows must not
    // lock the task against edits.
    db.with_raw_conn(|c| {
        c.execute(
            "UPDATE tasks SET area_id = 'home', project_id = 'p', heading_id = 'qh' WHERE id = 'a'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let task = db.get_task("a").unwrap().unwrap();
    assert!(Home::of(&task).is_err());
    assert_eq!(Home::listed(&task), project("p", Some("qh")));
    assert_eq!(View::containing(&task, today()), View::Project("p".into()));

    db.set_task_notes("a", "still editable").unwrap();
    assert_eq!(db.get_task("a").unwrap().unwrap().notes, "still editable");

    db.set_task_home("a", &Home::Area("home".into())).unwrap();
    assert_eq!(home_of(&db, "a"), Home::Area("home".into()));
}
