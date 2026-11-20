//! Receiving engine updates from another replica (#35): merged into the
//! engine with the rows rebuilt from it, never echoed back, and kept until
//! their dependencies arrive.

use matters_core::{Database, Home, Project, Task};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn synced() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    db
}

/// Everything `from` authored after `after`, received by `to`; returns
/// the last sequence sent.
fn send(from: &Database, to: &Database, after: i64) -> i64 {
    let mut last = after;
    for (seq, bytes) in from.outgoing_updates(after).unwrap() {
        to.receive_update(&bytes).unwrap();
        last = seq;
    }
    last
}

/// The rows both replicas show, as the engine projects them.
fn rows(db: &Database) -> matters_core::crdt::Snapshot {
    let mut s = db.engine_snapshot().unwrap().unwrap();
    for t in &mut s.tasks {
        assert_eq!(db.get_task(&t.id).unwrap().unwrap().title, t.title);
        t.sort_order = 0.0;
        t.today_order = 0.0;
    }
    s.repairs.clear();
    s
}

#[test]
fn two_replicas_converge_without_echoes() {
    let (a, b) = (synced(), synced());
    a.add_project(&mut Project::new(id(1), "Garden", 0.0))
        .unwrap();
    let mut task = Task::new(id(2), "Dig", 0.0);
    Home::Project {
        project: id(1),
        heading: None,
    }
    .apply(&mut task);
    a.add_task(&mut task).unwrap();
    let sent_a = send(&a, &b, 0);
    assert_eq!(b.get_task(&id(2)).unwrap().unwrap().project_id, Some(id(1)));
    assert!(
        b.outgoing_updates(0).unwrap().is_empty(),
        "received is not outgoing"
    );

    // Concurrent edits to one task.
    a.rename_task(&id(2), "Dig deep").unwrap();
    b.set_task_deadline(&id(2), Some(jiff::civil::date(2026, 12, 1)))
        .unwrap();
    send(&a, &b, sent_a);
    send(&b, &a, 0);
    let task = a.get_task(&id(2)).unwrap().unwrap();
    assert_eq!(task.title, "Dig deep");
    assert_eq!(task.deadline, Some(jiff::civil::date(2026, 12, 1)));
    assert_eq!(rows(&a).tasks, rows(&b).tasks);
    assert_eq!(rows(&a).projects, rows(&b).projects);

    // Receiving something again changes nothing.
    let before = rows(&b);
    send(&a, &b, 0);
    assert_eq!(rows(&b).tasks, before.tasks);
}

#[test]
fn an_update_waits_for_its_dependencies_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.db");
    let a = synced();
    a.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    a.rename_task(&id(1), "two").unwrap();
    let updates = a.outgoing_updates(0).unwrap();
    assert_eq!(updates.len(), 2);
    {
        let b = Database::open(&path).unwrap();
        b.enable_sync().unwrap();
        b.receive_update(&updates[1].1).unwrap(); // the rename first
        assert!(b.get_task(&id(1)).unwrap().is_none(), "waits for its task");
    }
    let b = Database::open(&path).unwrap();
    b.receive_update(&updates[0].1).unwrap();
    assert_eq!(b.get_task(&id(1)).unwrap().unwrap().title, "two");
}

#[test]
fn rebuilding_the_rows_from_the_engine_keeps_every_view() {
    use matters_core::{QueryEngine, View};
    let db = synced();
    db.add_project(&mut Project::new(id(1), "Garden", 0.0))
        .unwrap();
    for n in 2..6 {
        let mut task = Task::new(id(n), format!("t{n}"), 0.0);
        if n % 2 == 0 {
            Home::Project {
                project: id(1),
                heading: None,
            }
            .apply(&mut task);
        }
        db.add_task(&mut task).unwrap();
    }
    db.complete_task(&id(3)).unwrap();
    db.trash_task(&id(5)).unwrap();
    let today = jiff::civil::date(2026, 11, 20);
    let views = |db: &Database| {
        let q = QueryEngine::new(db);
        [
            View::Inbox,
            View::Project(id(1)),
            View::Logbook,
            View::Trash,
        ]
        .map(|v| {
            q.tasks_in(&v, today)
                .unwrap()
                .into_iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        })
    };
    let before = views(&db);
    db.rebuild_from_engine().unwrap();
    assert_eq!(views(&db), before);

    // Rows lost or damaged outside the engine come back from it.
    db.with_raw_conn(|c| {
        c.execute_batch("DELETE FROM tasks; UPDATE projects SET title = 'damaged';")?;
        Ok(())
    })
    .unwrap();
    assert_ne!(views(&db), before);
    db.rebuild_from_engine().unwrap();
    assert_eq!(views(&db), before);
    assert_eq!(db.get_project(&id(1)).unwrap().unwrap().title, "Garden");
}

#[test]
fn bad_input_and_sync_off_are_refused_with_nothing_kept() {
    let db = synced();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    assert!(db.receive_update(b"not an engine update").is_err());
    assert_eq!(db.get_task(&id(1)).unwrap().unwrap().title, "one");
    let journal: i64 = db
        .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_journal", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(journal, 1);
    db.rename_task(&id(1), "still works").unwrap();

    let off = Database::open_in_memory().unwrap();
    assert!(off
        .receive_update(&db.outgoing_updates(0).unwrap()[0].1)
        .is_err());
    assert!(off.outgoing_updates(0).unwrap().is_empty());
}
