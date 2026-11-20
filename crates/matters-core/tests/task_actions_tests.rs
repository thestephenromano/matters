//! Cancel, duplicate and convert a task (#65).

use jiff::civil::Date;
use matters_core::{
    Area, ChecklistItem, Database, Home, MattersError, Project, QueryEngine, Schedule, Status,
    Tag, Tagged, Task, View,
};

fn today() -> Date {
    jiff::civil::date(2026, 11, 20)
}

fn ids(db: &Database, view: &View) -> Vec<String> {
    QueryEngine::new(db)
        .tasks_in(view, today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect()
}

fn add(db: &Database, id: &str, home: Home, schedule: Schedule) {
    let mut t = Task::new(id, id, 0.0);
    home.apply(&mut t);
    t.apply_schedule(&schedule, today());
    db.add_task(&mut t).unwrap();
}

fn tag(db: &Database, id: &str) {
    db.add_tag(&mut Tag::new(id, id, None)).unwrap();
}

fn checklist(db: &Database, task: &str, items: &[(&str, bool)]) {
    for (i, (title, done)) in items.iter().enumerate() {
        let id = format!("{task}-item-{i}");
        db.add_checklist_item(&mut ChecklistItem::new(&id, task, *title, 0.0))
            .unwrap();
        if *done {
            db.set_checklist_item_done(&id, true).unwrap();
        }
    }
}

fn in_project(project: &str) -> Home {
    Home::Project {
        project: project.into(),
        heading: None,
    }
}

// ---- Cancel ----------------------------------------------------------------

#[test]
fn a_canceled_task_is_finished_like_a_completed_one() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "P", 0.0)).unwrap();
    add(&db, "done", in_project("p"), Schedule::Today { evening: false });
    add(&db, "dropped", in_project("p"), Schedule::Today { evening: false });
    add(&db, "open", in_project("p"), Schedule::Today { evening: false });
    db.complete_task("done").unwrap();
    db.cancel_task("dropped").unwrap();

    assert_eq!(ids(&db, &View::Today), ["open"], "both leave Today");
    let mut logbook = ids(&db, &View::Logbook);
    logbook.sort();
    assert_eq!(logbook, ["done", "dropped"], "both are in the Logbook");
    let counts = QueryEngine::new(&db).counts(today()).unwrap();
    assert_eq!((counts.today, counts.logbook), (1, 2));

    let mut project = ids(&db, &View::Project("p".into()));
    project.sort();
    assert_eq!(
        project,
        ["done", "dropped", "open"],
        "the project lists both"
    );
    let progress = QueryEngine::new(&db).project_progress("p").unwrap();
    assert_eq!(
        (progress.completed_tasks, progress.total_tasks),
        (2, 3),
        "both count as finished"
    );

    let status = db.get_task("dropped").unwrap().unwrap().status;
    assert_eq!(status, Status::Canceled, "shown apart by its status");
    db.uncomplete_task("dropped").unwrap();
    assert_eq!(
        db.get_task("dropped").unwrap().unwrap().status,
        Status::Open,
        "reopened"
    );
}

#[test]
fn an_area_lists_its_canceled_tasks() {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("a", "A", 0.0)).unwrap();
    add(&db, "x", Home::Area("a".into()), Schedule::None);
    db.cancel_task("x").unwrap();
    assert_eq!(ids(&db, &View::Area("a".into())), ["x"]);
}

// ---- Duplicate -------------------------------------------------------------

#[test]
fn a_copy_is_open_below_the_original_with_its_details() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "P", 0.0)).unwrap();
    for id in ["before", "orig", "after"] {
        add(&db, id, in_project("p"), Schedule::Today { evening: true });
    }
    db.set_task_notes("orig", "the notes").unwrap();
    db.set_task_deadline("orig", Some(today())).unwrap();
    tag(&db, "t");
    db.add_tag_to(Tagged::Task("orig"), "t").unwrap();
    checklist(&db, "orig", &[("one", true), ("two", false)]);
    db.complete_task("orig").unwrap();

    let copy = db.duplicate_task("orig").unwrap();
    let original = db.get_task("orig").unwrap().unwrap();
    let dup = db.get_task(&copy).unwrap().unwrap();
    assert_eq!(dup.status, Status::Open, "open, whatever the original");
    assert_eq!(dup.completed_at, None);
    assert_eq!(
        (
            &dup.title,
            &dup.notes,
            &dup.project_id,
            dup.start_date,
            &dup.time_of_day,
            dup.deadline
        ),
        (
            &original.title,
            &original.notes,
            &original.project_id,
            original.start_date,
            &original.time_of_day,
            original.deadline
        )
    );
    assert_eq!(
        original.status,
        Status::Completed,
        "the original is unchanged"
    );

    db.uncomplete_task("orig").unwrap();
    let project = ids(&db, &View::Project("p".into()));
    assert_eq!(project, ["before", "orig", &copy, "after"], "just below it");
    let today_list = ids(&db, &View::Today);
    assert_eq!(today_list, ["before", "orig", &copy, "after"], "in Today too");

    let tags: Vec<String> = db
        .tags_of(Tagged::Task(&copy))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(tags, ["t"]);
    let items: Vec<(String, bool)> = db
        .list_checklist_items(&copy)
        .unwrap()
        .into_iter()
        .map(|i| (i.title, i.is_completed))
        .collect();
    assert_eq!(
        items,
        [("one".to_string(), false), ("two".to_string(), false)],
        "the checklist, in order, unticked"
    );
    assert_eq!(
        db.list_checklist_items("orig").unwrap().len(),
        2,
        "the original keeps its own"
    );
}

#[test]
fn the_last_task_is_copied_to_the_end() {
    let db = Database::open_in_memory().unwrap();
    add(&db, "only", Home::Inbox, Schedule::None);
    let copy = db.duplicate_task("only").unwrap();
    assert_eq!(ids(&db, &View::Inbox), ["only".to_string(), copy]);
}

#[test]
fn a_trashed_or_missing_task_is_not_copied() {
    let db = Database::open_in_memory().unwrap();
    add(&db, "gone", Home::Inbox, Schedule::None);
    db.trash_task("gone").unwrap();
    for id in ["gone", "nothing"] {
        assert!(matches!(
            db.duplicate_task(id),
            Err(MattersError::NotFound { .. })
        ));
    }
    assert!(ids(&db, &View::Inbox).is_empty());
}

// ---- Convert to a project --------------------------------------------------

#[test]
fn a_task_becomes_a_project_of_its_checklist() {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("a", "A", 0.0)).unwrap();
    add(
        &db,
        "plan",
        Home::Area("a".into()),
        Schedule::Date(today().tomorrow().unwrap()),
    );
    db.set_task_notes("plan", "why").unwrap();
    db.set_task_deadline("plan", Some(today())).unwrap();
    tag(&db, "t");
    db.add_tag_to(Tagged::Task("plan"), "t").unwrap();
    checklist(&db, "plan", &[("first", false), ("second", true)]);

    let id = db.convert_task_to_project("plan").unwrap();
    let project = db.get_project(&id).unwrap().unwrap();
    assert_eq!(
        (
            project.title.as_str(),
            project.notes.as_str(),
            project.area_id.as_deref()
        ),
        ("plan", "why", Some("a"))
    );
    assert_eq!(project.start_date, today().tomorrow().ok());
    assert_eq!(project.deadline, Some(today()));
    assert_eq!(project.status, Status::Open);
    let tags: Vec<String> = db
        .tags_of(Tagged::Project(&id))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(tags, ["t"], "the tags");

    let tasks: Vec<(String, Status)> = QueryEngine::new(&db)
        .project_tasks(&id)
        .unwrap()
        .into_iter()
        .map(|t| (t.title, t.status))
        .collect();
    assert_eq!(
        tasks,
        [
            ("first".to_string(), Status::Open),
            ("second".to_string(), Status::Completed)
        ],
        "the checklist, in order, ticked items done"
    );

    assert_eq!(db.get_task("plan").unwrap(), None, "the task is gone");
    assert!(db.list_checklist_items("plan").unwrap().is_empty());
    let last = db.list_projects().unwrap().last().unwrap().id.clone();
    assert_eq!(last, id, "last in the sidebar");
}

#[test]
fn a_project_task_becomes_a_project_in_that_projects_area() {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("a", "A", 0.0)).unwrap();
    let mut outer = Project::new("outer", "Outer", 0.0);
    outer.area_id = Some("a".into());
    db.create_project(&outer).unwrap();
    add(&db, "sub", in_project("outer"), Schedule::Someday);
    let id = db.convert_task_to_project("sub").unwrap();
    let project = db.get_project(&id).unwrap().unwrap();
    assert_eq!(project.area_id.as_deref(), Some("a"));
    assert!(project.is_someday, "its When");
    assert!(QueryEngine::new(&db).project_tasks(&id).unwrap().is_empty());

    add(&db, "loose", Home::Inbox, Schedule::Today { evening: true });
    let id = db.convert_task_to_project("loose").unwrap();
    let project = db.get_project(&id).unwrap().unwrap();
    assert_eq!(project.area_id, None, "no area");
    assert_eq!(project.time_of_day.as_deref(), Some("evening"));
}

#[test]
fn only_an_open_live_task_becomes_a_project() {
    let db = Database::open_in_memory().unwrap();
    add(&db, "done", Home::Inbox, Schedule::None);
    db.complete_task("done").unwrap();
    add(&db, "binned", Home::Inbox, Schedule::None);
    db.trash_task("binned").unwrap();
    let before = db.list_projects().unwrap().len();
    assert!(matches!(
        db.convert_task_to_project("done"),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        db.convert_task_to_project("binned"),
        Err(MattersError::NotFound { .. })
    ));
    assert_eq!(db.list_projects().unwrap().len(), before, "nothing made");
    assert!(db.get_task("done").unwrap().is_some(), "nothing removed");
}
