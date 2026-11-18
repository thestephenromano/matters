//! Areas in the app (#15): the area view, capturing into it, finding a task
//! there, renaming, and filing projects in areas.

use jiff::civil::Date;
use matters_core::ordering::place_task;
use matters_core::{Area, Database, MattersError, Project, QueryEngine, Status, Task, View};

fn today() -> Date {
    jiff::civil::date(2026, 11, 18)
}

fn ids(db: &Database, view: &View) -> Vec<String> {
    QueryEngine::new(db)
        .tasks_in(view, today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect()
}

fn home() -> View {
    View::Area("home".into())
}

/// Area "home" with two loose tasks, a project "garden" filed in it with a
/// task, a finished loose task and a trashed one.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("home", "Home", 0.0)).unwrap();
    let mut garden = Project::new("garden", "Garden", 0.0);
    garden.area_id = Some("home".into());
    db.create_project(&garden).unwrap();
    for (i, (id, area, project)) in [
        ("loose-1", Some("home"), None),
        ("loose-2", Some("home"), None),
        ("in-garden", None, Some("garden")),
        ("finished", Some("home"), None),
        ("binned", Some("home"), None),
    ]
    .iter()
    .enumerate()
    {
        let mut t = Task::new(*id, *id, i as f64);
        t.area_id = area.map(str::to_string);
        t.project_id = project.map(str::to_string);
        db.create_task(&t).unwrap();
    }
    db.complete_task("finished").unwrap();
    db.trash_task("binned").unwrap();
    db
}

#[test]
fn an_area_lists_the_tasks_filed_directly_in_it() {
    let db = fixture();
    assert_eq!(
        ids(&db, &home()),
        ["loose-1", "loose-2", "finished"],
        "not its projects' tasks, nor trashed ones"
    );
}

#[test]
fn a_task_captured_in_an_area_is_filed_there() {
    let db = fixture();
    let mut task = home().new_task("new", "new", today()).unwrap();
    assert_eq!(task.area_id.as_deref(), Some("home"));
    assert_eq!(task.project_id, None);
    db.add_task(&mut task).unwrap();
    assert_eq!(ids(&db, &home()).last().map(String::as_str), Some("new"));
    assert_eq!(
        View::containing(&task, today()),
        home(),
        "Quick Find opens the area"
    );
}

#[test]
fn area_views_have_a_key_and_are_arranged_by_hand() {
    assert_eq!(View::from_key("area", Some("home")), Some(home()));
    assert_eq!(View::from_key("area", Some("")), None);
    assert_eq!(home().key(), "area");
    assert_eq!(home().container_id(), Some("home"));
    assert!(home().is_reorderable());

    let db = fixture();
    place_task(&db, &home(), today(), "loose-2", None).unwrap();
    assert_eq!(ids(&db, &home()), ["loose-2", "loose-1", "finished"]);
}

#[test]
fn renaming_an_area_cleans_the_name_and_refuses_blank_ones() {
    let db = fixture();
    db.rename_area("home", "  My\nhome ").unwrap();
    assert_eq!(db.get_area("home").unwrap().unwrap().title, "My home");
    assert!(matches!(
        db.rename_area("home", "  "),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        db.rename_area("nope", "x"),
        Err(MattersError::NotFound { .. })
    ));
}

#[test]
fn projects_are_filed_in_live_areas_only() {
    let db = fixture();
    db.create_area(&Area::new("work", "Work", 1.0)).unwrap();
    db.create_project(&Project::new("solo", "Solo", 1.0))
        .unwrap();

    db.set_project_area("solo", Some("work")).unwrap();
    assert_eq!(
        db.get_project("solo").unwrap().unwrap().area_id.as_deref(),
        Some("work")
    );
    db.set_project_area("solo", None).unwrap();
    assert_eq!(db.get_project("solo").unwrap().unwrap().area_id, None);

    db.trash_area("work").unwrap();
    assert!(
        db.set_project_area("solo", Some("work")).is_err(),
        "not into a trashed area"
    );
    assert!(db.set_project_area("solo", Some("missing")).is_err());
    assert!(db.set_project_area("missing", None).is_err());
    assert_eq!(
        db.get_project("solo").unwrap().unwrap().status,
        Status::Open
    );
}
