//! Deleting a task moves it to the Trash (SPEC §3.7): it stays recoverable,
//! keeps its state, and is destroyed only when the Trash is emptied.

use chrono::NaiveDate;
use matters_core::storage::schema::SCHEMA_V1;
use matters_core::{Area, ChecklistItem, Database, Project, QueryEngine, Status, Task, View};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()
}

fn live_views() -> Vec<View> {
    let mut views: Vec<View> = View::sidebar().into_iter().filter(|v| *v != View::Trash).collect();
    views.push(View::Project("p1".into()));
    views
}

fn listed_in(engine: &QueryEngine, id: &str) -> Vec<View> {
    let mut views = live_views();
    views.push(View::Trash);
    views
        .into_iter()
        .filter(|v| engine.tasks_in(v, today()).unwrap().iter().any(|t| t.id == id))
        .collect()
}

#[test]
fn put_back_returns_a_task_exactly_where_it_was() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0)).unwrap();
    let engine = QueryEngine::new(&db);

    let mut today_task = Task::new("today", "today", 0.0);
    today_task.start_date = Some(today());
    today_task.time_of_day = Some("evening".into());
    today_task.notes = "keep me".into();
    let mut project_task = Task::new("project", "project", 0.0);
    project_task.project_id = Some("p1".into());
    let done = Task::new("done", "done", 0.0);
    for t in [&today_task, &project_task, &done] {
        db.create_task(t).unwrap();
    }
    db.complete_task("done").unwrap();

    for id in ["today", "project", "done"] {
        let before = db.get_task(id).unwrap().unwrap();
        let home = listed_in(&engine, id);
        assert!(!home.is_empty());

        db.trash_task(id).unwrap();
        let trashed = db.get_task(id).unwrap().unwrap();
        assert!(trashed.is_trashed());
        assert_eq!(trashed.status, before.status, "{id}: trashing keeps the status");
        assert_eq!(listed_in(&engine, id), [View::Trash], "{id}: only the Trash lists it");
        assert_eq!(View::containing(&trashed, today()), View::Trash);

        db.restore_task(id).unwrap();
        let back = db.get_task(id).unwrap().unwrap();
        assert!(!back.is_trashed());
        assert_eq!(listed_in(&engine, id), home, "{id}: put back where it was");
        assert_eq!(
            (back.status, back.start_date, back.time_of_day, back.notes, back.project_id),
            (before.status, before.start_date, before.time_of_day, before.notes, before.project_id),
        );
    }
}

#[test]
fn trashed_tasks_are_hidden_from_counts_progress_and_search() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0)).unwrap();
    let engine = QueryEngine::new(&db);

    let mut a = Task::new("a", "water the roses", 0.0);
    a.project_id = Some("p1".into());
    let mut b = Task::new("b", "water the lawn", 1.0);
    b.project_id = Some("p1".into());
    db.create_task(&a).unwrap();
    db.create_task(&b).unwrap();
    db.complete_task("b").unwrap();
    let inbox = Task::new("c", "water bill", 2.0);
    db.create_task(&inbox).unwrap();

    let counts_before = engine.counts(today()).unwrap();
    db.trash_task("b").unwrap();
    db.trash_task("c").unwrap();

    let counts = engine.counts(today()).unwrap();
    assert_eq!(counts.logbook, counts_before.logbook - 1);
    assert_eq!(counts.inbox, counts_before.inbox - 1);

    let progress = engine.project_progress("p1").unwrap();
    assert_eq!((progress.completed_tasks, progress.total_tasks), (0, 1));

    let found: Vec<_> = engine.quick_find("water").unwrap().tasks.into_iter().map(|t| t.id).collect();
    assert_eq!(found, ["a"]);
}

#[test]
fn trash_lists_most_recently_trashed_first() {
    let db = Database::open_in_memory().unwrap();
    for id in ["a", "b", "c"] {
        db.create_task(&Task::new(id, id, 0.0)).unwrap();
    }
    for id in ["b", "c", "a"] {
        db.trash_task(id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // Trashing again does not move a task to the top.
    db.trash_task("b").unwrap();
    let ids: Vec<_> = QueryEngine::new(&db).trash().unwrap().into_iter().map(|t| t.id).collect();
    assert_eq!(ids, ["a", "c", "b"]);
}

#[test]
fn emptying_the_trash_destroys_only_trashed_tasks() {
    let db = Database::open_in_memory().unwrap();
    for id in ["keep", "gone-1", "gone-2"] {
        db.create_task(&Task::new(id, id, 0.0)).unwrap();
        db.create_checklist_item(&ChecklistItem::new(format!("{id}-item"), id, "step", 0.0)).unwrap();
    }
    db.trash_task("gone-1").unwrap();
    db.trash_task("gone-2").unwrap();

    assert_eq!(db.empty_trash().unwrap(), 2);
    assert!(db.get_task("keep").unwrap().is_some());
    assert!(db.get_task("gone-1").unwrap().is_none());
    assert_eq!(db.list_checklist_items("keep").unwrap().len(), 1);
    assert!(db.list_checklist_items("gone-1").unwrap().is_empty());
    assert_eq!(db.empty_trash().unwrap(), 0);
}

#[test]
fn saving_a_task_never_changes_its_trash_state() {
    let db = Database::open_in_memory().unwrap();
    db.create_task(&Task::new("t", "t", 0.0)).unwrap();

    // A copy read before the task was trashed, saved afterwards (a late notes
    // autosave, say), must not pull it out of the Trash.
    let mut stale = db.get_task("t").unwrap().unwrap();
    db.trash_task("t").unwrap();
    stale.notes = "late edit".into();
    db.update_task(&stale).unwrap();
    let now = db.get_task("t").unwrap().unwrap();
    assert!(now.is_trashed());
    assert_eq!(now.notes, "late edit");

    // Nor can a save move a live task into the Trash.
    db.restore_task("t").unwrap();
    let mut task = db.get_task("t").unwrap().unwrap();
    task.trashed_at = Some(1);
    db.update_task(&task).unwrap();
    assert!(!db.get_task("t").unwrap().unwrap().is_trashed());
}

#[test]
fn v1_databases_are_migrated_without_losing_trashed_items() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (1, 0);
             INSERT INTO tasks (id, title, status, completed_at, sort_order, created_at, updated_at)
             VALUES ('open', 'open', 'open', NULL, 0, 0, 10),
                    ('was-open', 'was open', 'trashed', NULL, 0, 0, 20),
                    ('was-done', 'was done', 'trashed', 5, 0, 0, 30);
             INSERT INTO projects (id, title, status, completed_at, sort_order, created_at, updated_at)
             VALUES ('live-project', 'live', 'open', NULL, 0, 0, 10),
                    ('old-project', 'old', 'trashed', NULL, 0, 0, 40);
             INSERT INTO areas (id, title, sort_order, created_at, updated_at)
             VALUES ('area', 'Home', 0, 0, 0);",
        )
        .unwrap();
    }

    // Opening twice checks the migration runs once.
    for _ in 0..2 {
        let db = Database::open(&path).unwrap();
        let get = |id: &str| db.get_task(id).unwrap().unwrap();
        assert!(!get("open").is_trashed());
        assert_eq!((get("was-open").status, get("was-open").trashed_at), (Status::Open, Some(20)));
        assert_eq!((get("was-done").status, get("was-done").trashed_at), (Status::Completed, Some(30)));
        let trash: Vec<_> = QueryEngine::new(&db).trash().unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(trash, ["was-done", "was-open"]);
        let projects: Vec<_> = db.list_projects().unwrap().into_iter().map(|p| p.id).collect();
        assert_eq!(projects, ["live-project"]);
        let old = db.get_project("old-project").unwrap().unwrap();
        assert_eq!((old.status, old.trashed_at), (Status::Open, Some(40)));
        assert_eq!(ids(db.list_areas().unwrap(), |a| &a.id), ["area"]);
    }
}

#[test]
fn a_trashed_project_takes_its_tasks_with_it_and_put_back_restores_them() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0)).unwrap();
    db.create_project(&Project::new("p2", "Kitchen", 1.0)).unwrap();
    let engine = QueryEngine::new(&db);

    let mut open = Task::new("open", "dig the beds", 0.0);
    open.project_id = Some("p1".into());
    open.start_date = Some(today());
    let mut done = Task::new("done", "buy seeds", 1.0);
    done.project_id = Some("p1".into());
    let mut alone = Task::new("alone", "trashed before the project", 2.0);
    alone.project_id = Some("p1".into());
    let mut other = Task::new("other", "paint", 0.0);
    other.project_id = Some("p2".into());
    for t in [&open, &done, &alone, &other] {
        db.create_task(t).unwrap();
    }
    db.complete_task("done").unwrap();
    db.trash_task("alone").unwrap();

    let homes: Vec<_> = ["open", "done"].iter().map(|id| listed_in(&engine, id)).collect();
    let counts_before = engine.counts(today()).unwrap();

    db.trash_project("p1").unwrap();

    // The project leaves the sidebar and Quick Find, and shows in the Trash.
    let live: Vec<_> = db.list_projects().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(live, ["p2"]);
    let trashed: Vec<_> = db.list_trashed_projects().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(trashed, ["p1"]);
    assert!(engine.quick_find("Garden").unwrap().projects.is_empty());

    // Its tasks leave every view, counts and search; the Trash lists the
    // project in their place, not the tasks.
    for id in ["open", "done", "alone"] {
        assert!(listed_in(&engine, id).is_empty(), "{id} should be listed nowhere");
        assert!(engine.quick_find(&db.get_task(id).unwrap().unwrap().title).unwrap().tasks.is_empty());
    }
    let counts = engine.counts(today()).unwrap();
    assert_eq!(counts.today, counts_before.today - 1);
    assert_eq!(counts.logbook, counts_before.logbook - 1);
    // Another project's tasks are untouched.
    assert_eq!(listed_in(&engine, "other"), [View::Anytime]);
    assert_eq!(engine.tasks_in(&View::Project("p2".into()), today()).unwrap().len(), 1);

    db.restore_project("p1").unwrap();
    assert_eq!(db.list_projects().unwrap().len(), 2);
    assert!(db.list_trashed_projects().unwrap().is_empty());
    for (id, home) in ["open", "done"].iter().zip(&homes) {
        assert_eq!(&listed_in(&engine, id), home, "{id}: put back where it was");
    }
    assert_eq!(db.get_task("done").unwrap().unwrap().status, Status::Completed);
    // A task trashed on its own before the project stays in the Trash.
    assert_eq!(listed_in(&engine, "alone"), [View::Trash]);
    assert_eq!(engine.counts(today()).unwrap(), counts_before);
}

#[test]
fn emptying_the_trash_destroys_trashed_projects_with_their_contents() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("gone", "Old plans", 0.0)).unwrap();
    db.create_project(&Project::new("kept", "Current", 1.0)).unwrap();
    db.create_heading(&matters_core::Heading::new("h", "gone", "Phase 1", 0.0)).unwrap();
    for (id, project) in [("g1", "gone"), ("g2", "gone"), ("k1", "kept")] {
        let mut t = Task::new(id, id, 0.0);
        t.project_id = Some(project.into());
        db.create_task(&t).unwrap();
        db.create_checklist_item(&ChecklistItem::new(format!("{id}-item"), id, "step", 0.0)).unwrap();
    }
    db.create_task(&Task::new("loose", "loose", 0.0)).unwrap();
    db.trash_task("loose").unwrap();
    db.trash_project("gone").unwrap();

    assert_eq!(db.empty_trash().unwrap(), 3, "g1, g2 and loose");
    assert!(db.get_project("gone").unwrap().is_none());
    assert!(db.list_headings_for_project("gone").unwrap().is_empty());
    for id in ["g1", "g2", "loose"] {
        assert!(db.get_task(id).unwrap().is_none(), "{id}");
    }
    assert!(db.list_checklist_items("g1").unwrap().is_empty());
    assert!(db.get_project("kept").unwrap().is_some());
    assert!(db.get_task("k1").unwrap().is_some());
    assert_eq!(db.list_checklist_items("k1").unwrap().len(), 1);
}

#[test]
fn saving_a_project_never_changes_its_trash_state() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "p", 0.0)).unwrap();
    let mut stale = db.get_project("p").unwrap().unwrap();
    db.trash_project("p").unwrap();
    stale.title = "renamed".into();
    db.update_project(&stale).unwrap();
    assert!(db.get_project("p").unwrap().unwrap().is_trashed());

    db.restore_project("p").unwrap();
    let mut project = db.get_project("p").unwrap().unwrap();
    project.trashed_at = Some(1);
    db.update_project(&project).unwrap();
    assert!(!db.get_project("p").unwrap().unwrap().is_trashed());
}

/// Area `a1` holds project `p1` (tasks `p1-open`, `p1-done`, and `p1-alone`,
/// which was trashed on its own), project `p2` (trashed on its own) with task
/// `p2-task`, and task `a1-task` filed directly in the area. Area `a2` holds
/// `a2-task`, and `loose` is an unrelated Inbox task.
fn area_fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("a1", "Home", 0.0)).unwrap();
    db.create_area(&Area::new("a2", "Work", 1.0)).unwrap();
    for (id, title) in [("p1", "Garden"), ("p2", "Attic")] {
        let mut p = Project::new(id, title, 0.0);
        p.area_id = Some("a1".into());
        db.create_project(&p).unwrap();
    }
    let tasks: [(&str, Option<&str>, Option<&str>); 6] = [
        ("p1-open", Some("p1"), None),
        ("p1-done", Some("p1"), None),
        ("p1-alone", Some("p1"), None),
        ("p2-task", Some("p2"), None),
        ("a1-task", None, Some("a1")),
        ("a2-task", None, Some("a2")),
    ];
    for (id, project, area) in tasks {
        let mut t = Task::new(id, format!("{id} chores"), 0.0);
        t.project_id = project.map(str::to_string);
        t.area_id = area.map(str::to_string);
        db.create_task(&t).unwrap();
        db.create_checklist_item(&ChecklistItem::new(format!("{id}-item"), id, "step", 0.0)).unwrap();
    }
    db.create_task(&Task::new("loose", "loose chores", 0.0)).unwrap();
    db.complete_task("p1-done").unwrap();
    db.trash_task("p1-alone").unwrap();
    db.trash_project("p2").unwrap();
    db
}

fn ids<T>(items: Vec<T>, id: impl Fn(&T) -> &str) -> Vec<String> {
    items.iter().map(|x| id(x).to_string()).collect()
}

#[test]
fn a_trashed_area_takes_its_projects_and_tasks_and_put_back_restores_them() {
    let db = area_fixture();
    let engine = QueryEngine::new(&db);
    let views = |id: &str| listed_in(&engine, id);
    let before: Vec<_> = ["p1-open", "p1-done", "a1-task", "a2-task"].iter().map(|id| views(id)).collect();
    let counts_before = engine.counts(today()).unwrap();
    assert_eq!(ids(QueryEngine::new(&db).trash().unwrap(), |t| &t.id), ["p1-alone"]);
    assert_eq!(ids(db.list_trashed_projects().unwrap(), |p| &p.id), ["p2"]);

    db.trash_area("a1").unwrap();

    // The area stands for everything in it: it is the only Home item the
    // Trash lists, and nothing of it shows anywhere else.
    assert_eq!(ids(db.list_areas().unwrap(), |a| &a.id), ["a2"]);
    assert_eq!(ids(db.list_trashed_areas().unwrap(), |a| &a.id), ["a1"]);
    assert!(db.list_projects().unwrap().is_empty());
    assert!(db.list_trashed_projects().unwrap().is_empty(), "p2 is inside the trashed area");
    assert!(engine.trash().unwrap().is_empty(), "p1-alone is inside the trashed area");
    for id in ["p1-open", "p1-done", "p1-alone", "p2-task", "a1-task"] {
        assert!(views(id).is_empty(), "{id} should be listed nowhere");
    }
    let found = engine.quick_find("chores").unwrap();
    let mut found_ids = ids(found.tasks, |t| &t.id);
    found_ids.sort();
    assert_eq!(found_ids, ["a2-task", "loose"]);
    assert!(engine.quick_find("Home").unwrap().areas.is_empty());
    assert!(engine.quick_find("Garden").unwrap().projects.is_empty());
    let counts = engine.counts(today()).unwrap();
    assert_eq!(counts.anytime, counts_before.anytime - 2, "p1-open and a1-task");
    assert_eq!(counts.logbook, counts_before.logbook - 1, "p1-done");
    assert_eq!(views("a2-task"), [View::Anytime], "another area is untouched");

    db.restore_area("a1").unwrap();
    assert_eq!(db.list_areas().unwrap().len(), 2);
    assert_eq!(ids(db.list_projects().unwrap(), |p| &p.id), ["p1"]);
    for (id, home) in ["p1-open", "p1-done", "a1-task", "a2-task"].iter().zip(&before) {
        assert_eq!(&views(id), home, "{id}: put back where it was");
    }
    // What was trashed on its own before the area is still in the Trash.
    assert_eq!(ids(engine.trash().unwrap(), |t| &t.id), ["p1-alone"]);
    assert_eq!(ids(db.list_trashed_projects().unwrap(), |p| &p.id), ["p2"]);
    assert_eq!(engine.counts(today()).unwrap(), counts_before);
}

#[test]
fn emptying_the_trash_destroys_a_trashed_area_with_everything_in_it() {
    let db = area_fixture();
    db.trash_area("a1").unwrap();

    // p1-open, p1-done, p1-alone, p2-task and a1-task.
    assert_eq!(db.empty_trash().unwrap(), 5);
    assert!(db.get_area("a1").unwrap().is_none());
    for id in ["p1", "p2"] {
        assert!(db.get_project(id).unwrap().is_none(), "{id}");
    }
    for id in ["p1-open", "p1-done", "p1-alone", "p2-task", "a1-task"] {
        assert!(db.get_task(id).unwrap().is_none(), "{id}");
        assert!(db.list_checklist_items(id).unwrap().is_empty(), "{id} checklist");
    }
    // Nothing outside the area is touched, and nothing is left orphaned.
    assert!(db.get_area("a2").unwrap().is_some());
    assert_eq!(db.get_task("a2-task").unwrap().unwrap().area_id.as_deref(), Some("a2"));
    assert!(db.get_task("loose").unwrap().is_some());
    assert_eq!(db.list_checklist_items("a2-task").unwrap().len(), 1);
}

#[test]
fn saving_an_area_never_changes_its_trash_state() {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("a", "a", 0.0)).unwrap();
    let mut stale = db.get_area("a").unwrap().unwrap();
    db.trash_area("a").unwrap();
    stale.title = "renamed".into();
    db.update_area(&stale).unwrap();
    let now = db.get_area("a").unwrap().unwrap();
    assert!(now.is_trashed());
    assert_eq!(now.title, "renamed");

    db.restore_area("a").unwrap();
    let mut area = db.get_area("a").unwrap().unwrap();
    area.trashed_at = Some(1);
    db.update_area(&area).unwrap();
    assert!(!db.get_area("a").unwrap().unwrap().is_trashed());
}
