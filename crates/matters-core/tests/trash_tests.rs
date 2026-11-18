//! Deleting a task moves it to the Trash (SPEC §3.7): it stays recoverable,
//! keeps its state, and is destroyed only when the Trash is emptied.

use chrono::NaiveDate;
use matters_core::storage::schema::SCHEMA_V1;
use matters_core::{ChecklistItem, Database, Project, QueryEngine, Status, Task, View};

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
fn v1_databases_are_migrated_without_losing_trashed_tasks() {
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
                    ('was-done', 'was done', 'trashed', 5, 0, 0, 30);",
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
    }
}
