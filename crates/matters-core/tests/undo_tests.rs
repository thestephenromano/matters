//! One Undo for an action on several tasks (#64): a snapshot taken before
//! the action puts each task back exactly, and touches nothing else.

use jiff::civil::Date;
use matters_core::{
    Area, Database, Home, Project, QueryEngine, Schedule, Status, Tag, Tagged, Task, TaskSnapshot,
    View,
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

fn take(db: &Database, ids: &[&str]) -> TaskSnapshot {
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    TaskSnapshot::take(db, &ids).unwrap()
}

/// Every field of task `id` that a batch action could change.
fn state(db: &Database, id: &str) -> (Task, Vec<String>) {
    let mut task = db.get_task(id).unwrap().unwrap();
    // Times stamped by the operations themselves.
    task.updated_at = 0;
    let tags = db
        .tags_of(Tagged::Task(id))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    (task, tags)
}

/// Five Today tasks in project `p`; the middle three are acted on.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("a", "A", 0.0)).unwrap();
    db.create_project(&Project::new("p", "P", 0.0)).unwrap();
    db.create_project(&Project::new("q", "Q", 1.0)).unwrap();
    for id in ["t1", "t2", "t3", "t4", "t5"] {
        add(
            &db,
            id,
            Home::Project {
                project: "p".into(),
                heading: None,
            },
            Schedule::Today { evening: false },
        );
    }
    db.add_tag(&mut Tag::new("old", "old", None)).unwrap();
    db.add_tag(&mut Tag::new("new", "new", None)).unwrap();
    db.add_tag_to(Tagged::Task("t3"), "old").unwrap();
    db.add_tag_to(Tagged::Task("t3"), "new").unwrap();
    db
}

const ACTED: [&str; 3] = ["t2", "t3", "t4"];

fn assert_restored(db: &Database, before: &[(Task, Vec<String>)]) {
    for (old, tags) in before {
        let (now, now_tags) = state(db, &old.id);
        let mut expected = old.clone();
        // Restoring a finished status, or the Trash, stamps a new time.
        if old.status != Status::Open {
            expected.completed_at = now.completed_at;
        }
        if old.trashed_at.is_some() {
            expected.trashed_at = now.trashed_at;
        }
        assert_eq!(now, expected, "{} is as it was", old.id);
        assert_eq!(&now_tags, tags, "{} has its tags", old.id);
    }
}

fn snapshot_and_states(db: &Database) -> (TaskSnapshot, Vec<(Task, Vec<String>)>) {
    let before: Vec<_> = ACTED.iter().map(|id| state(db, id)).collect();
    (take(db, &ACTED), before)
}

#[test]
fn undo_puts_back_tasks_completed_or_canceled_together() {
    let db = fixture();
    db.complete_task("t3").unwrap();
    let (snapshot, before) = snapshot_and_states(&db);
    assert_eq!(snapshot.len(), 3);
    for id in ACTED {
        db.cancel_task(id).unwrap();
    }
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
    assert_eq!(
        db.get_task("t3").unwrap().unwrap().status,
        Status::Completed
    );
}

#[test]
fn undo_puts_back_trashed_tasks_in_their_places() {
    let db = fixture();
    let order = ids(&db, &View::Project("p".into()));
    let (snapshot, before) = snapshot_and_states(&db);
    for id in ACTED {
        db.trash_task(id).unwrap();
    }
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
    assert_eq!(ids(&db, &View::Project("p".into())), order);
    assert_eq!(ids(&db, &View::Today), order);
}

#[test]
fn undo_puts_back_moved_tasks_where_they_were() {
    let db = fixture();
    let order = ids(&db, &View::Project("p".into()));
    let (snapshot, before) = snapshot_and_states(&db);
    for id in ACTED {
        db.set_task_home(id, &Home::Area("a".into())).unwrap();
    }
    assert_eq!(ids(&db, &View::Area("a".into())).len(), 3);
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
    assert_eq!(
        ids(&db, &View::Project("p".into())),
        order,
        "in their old places, not at the end"
    );
    assert!(ids(&db, &View::Area("a".into())).is_empty());
}

#[test]
fn undo_puts_back_when_and_deadline_exactly() {
    let db = fixture();
    let yesterday = today().yesterday().unwrap();
    // An overdue evening task: no Schedule names it, so it is restored by
    // its fields.
    db.set_task_schedule("t2", &Schedule::Date(yesterday), today())
        .unwrap();
    let _ = matters_core::ordering::place_in_today(&db, today(), "t2", true, None);
    db.set_task_schedule("t4", &Schedule::Someday, today())
        .unwrap();
    db.set_task_deadline("t3", Some(today())).unwrap();
    let (snapshot, before) = snapshot_and_states(&db);
    assert!(before[0].0.is_evening(), "t2 is an overdue evening task");

    for id in ACTED {
        db.set_task_schedule(id, &Schedule::Date(today().tomorrow().unwrap()), today())
            .unwrap();
        db.set_task_deadline(id, None).unwrap();
    }
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
}

#[test]
fn undo_takes_off_only_the_tags_the_action_gave() {
    let db = fixture();
    let (snapshot, before) = snapshot_and_states(&db);
    for id in ACTED {
        db.add_tag_to(Tagged::Task(id), "new").unwrap();
    }
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
    let t3: Vec<String> = db
        .tags_of(Tagged::Task("t3"))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(t3, ["old", "new"], "t3 had `new` already");

    // A tag taken off comes back.
    let snapshot = take(&db, &["t3"]);
    db.remove_tag_from(Tagged::Task("t3"), "old").unwrap();
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before[1..2]);
}

#[test]
fn a_snapshot_counts_the_tasks_it_remembers() {
    let db = fixture();
    let none = take(&db, &[]);
    assert!(none.is_empty());
    assert_eq!(none.len(), 0);
    let some = take(&db, &["t1", "t2"]);
    assert!(!some.is_empty());
    assert_eq!(some.len(), 2);
}

#[test]
fn undo_leaves_untouched_tasks_and_skips_deleted_ones() {
    let db = fixture();
    let others: Vec<_> = ["t1", "t5"].iter().map(|id| state(&db, id)).collect();
    let snapshot = take(&db, &["t2", "missing"]);
    assert_eq!(snapshot.len(), 1, "a missing id is skipped");
    db.trash_task("t2").unwrap();
    db.empty_trash().unwrap();
    db.delete_tag("old").unwrap();
    snapshot.restore(&db, today()).unwrap();
    assert_eq!(db.get_task("t2").unwrap(), None, "deleted stays deleted");
    for (old, tags) in &others {
        assert_eq!(&state(&db, &old.id), &(old.clone(), tags.clone()));
    }
}

#[test]
fn undo_puts_a_task_back_in_the_trash() {
    let db = fixture();
    db.trash_task("t2").unwrap();
    let (snapshot, before) = snapshot_and_states(&db);
    db.restore_task("t2").unwrap();
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
}

#[test]
fn a_task_left_in_the_trash_is_not_touched() {
    let db = fixture();
    db.trash_task("t2").unwrap();
    let before = db.get_task("t2").unwrap().unwrap();
    let snapshot = take(&db, &["t2"]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    snapshot.restore(&db, today()).unwrap();
    let after = db.get_task("t2").unwrap().unwrap();
    assert_eq!(
        (after.trashed_at, after.updated_at),
        (before.trashed_at, before.updated_at),
        "not taken out and put back"
    );
}

#[test]
fn a_task_that_cant_go_back_doesnt_stop_the_others() {
    let db = fixture();
    let (snapshot, before) = snapshot_and_states(&db);
    for id in ACTED {
        db.set_task_home(id, &Home::Area("a".into())).unwrap();
        db.complete_task(id).unwrap();
    }
    db.trash_project("p").unwrap();
    // Their project is in the Trash: they can't be filed there again...
    assert!(snapshot.restore(&db, today()).is_err());
    // ...but what else they had comes back, task by task, as far as it can.
    db.restore_project("p").unwrap();
    snapshot.restore(&db, today()).unwrap();
    assert_restored(&db, &before);
}
