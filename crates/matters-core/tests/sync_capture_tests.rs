//! Recording typed writes in the sync engine (#35): with sync on, each
//! write's engine commands commit with its SQL change, and the engine's
//! rows match the database's.

use std::collections::BTreeMap;

use matters_core::{
    Area, ChecklistItem, Database, Heading, Home, MattersError, Project, Schedule, Status, Task,
};

fn today() -> jiff::civil::Date {
    jiff::civil::date(2026, 11, 20)
}

/// The engine's rows equal the database's, field for field, and every list
/// has the same order.
fn check(db: &Database) {
    let s = db.engine_snapshot().unwrap().expect("sync is on");
    let counts: (i64, i64, i64, i64, i64) = db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT (SELECT COUNT(*) FROM areas), (SELECT COUNT(*) FROM projects),
                        (SELECT COUNT(*) FROM headings), (SELECT COUNT(*) FROM tasks),
                        (SELECT COUNT(*) FROM checklist_items)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        counts,
        (
            s.areas.len() as i64,
            s.projects.len() as i64,
            s.headings.len() as i64,
            s.tasks.len() as i64,
            s.checklist_items.len() as i64
        )
    );
    // Order keys differ (ranks against keys); compare everything else, then
    // the order within each list.
    for a in &s.areas {
        let mut row = db.get_area(&a.id).unwrap().unwrap();
        row.sort_order = a.sort_order;
        assert_eq!(&row, a);
    }
    for p in &s.projects {
        let mut row = db.get_project(&p.id).unwrap().unwrap();
        row.sort_order = p.sort_order;
        assert_eq!(&row, p);
    }
    for h in &s.headings {
        let mut row = db.get_heading(&h.id).unwrap().unwrap();
        row.sort_order = h.sort_order;
        assert_eq!(&row, h);
    }
    for t in &s.tasks {
        let mut row = db.get_task(&t.id).unwrap().unwrap();
        row.sort_order = t.sort_order;
        row.today_order = t.today_order;
        assert_eq!(&row, t);
        let mut items = db.list_checklist_items(&t.id).unwrap();
        let mut mine: Vec<_> = s
            .checklist_items
            .iter()
            .filter(|i| i.task_id == t.id)
            .cloned()
            .collect();
        mine.sort_by(|x, y| x.sort_order.total_cmp(&y.sort_order));
        for (row, item) in items.iter_mut().zip(&mine) {
            row.sort_order = item.sort_order;
        }
        assert_eq!(items, mine);
    }
    // Home lists in the database's order against the engine's.
    let lists = |tasks: Vec<Task>| {
        let mut by_list: BTreeMap<_, Vec<(f64, String)>> = BTreeMap::new();
        for t in tasks {
            by_list
                .entry((
                    t.area_id.clone(),
                    t.project_id.clone(),
                    t.heading_id.clone(),
                ))
                .or_default()
                .push((t.sort_order, t.id.clone()));
        }
        by_list
            .into_values()
            .map(|mut l| {
                l.sort_by(|x, y| x.0.total_cmp(&y.0));
                l.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let rows: Vec<Task> = s
        .tasks
        .iter()
        .map(|t| db.get_task(&t.id).unwrap().unwrap())
        .collect();
    assert_eq!(lists(rows), lists(s.tasks.clone()));
}

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn synced() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    db
}

#[test]
fn every_covered_write_keeps_the_engine_equal_to_the_rows() {
    let db = synced();
    db.add_area(&mut Area::new(id(1), "Home", 0.0)).unwrap();
    check(&db);
    let mut garden = Project::new(id(2), "Garden", 0.0);
    garden.area_id = Some(id(1));
    db.add_project(&mut garden).unwrap();
    db.add_project(&mut Project::new(id(3), "Shed", 0.0))
        .unwrap();
    db.add_heading(&mut Heading::new(id(4), id(2), "Spring", 0.0))
        .unwrap();
    check(&db);
    for (n, home) in [
        (10, Home::Inbox),
        (11, Home::Area(id(1))),
        (
            12,
            Home::Project {
                project: id(2),
                heading: None,
            },
        ),
        (
            13,
            Home::Project {
                project: id(2),
                heading: Some(id(4)),
            },
        ),
        (14, Home::Inbox),
    ] {
        let mut task = Task::new(id(n), format!("task {n}"), 0.0);
        home.apply(&mut task);
        db.add_task(&mut task).unwrap();
    }
    db.add_checklist_item(&mut ChecklistItem::new(id(20), id(13), "rake", 0.0))
        .unwrap();
    check(&db);

    db.rename_task(&id(10), "renamed").unwrap();
    db.set_task_notes(&id(10), "some notes").unwrap();
    db.set_task_schedule(&id(10), &Schedule::Today { evening: true }, today())
        .unwrap();
    db.set_task_deadline(&id(11), Some(today())).unwrap();
    db.complete_task(&id(11)).unwrap();
    db.cancel_task(&id(12)).unwrap();
    db.complete_task(&id(14)).unwrap();
    db.uncomplete_task(&id(14)).unwrap();
    db.trash_task(&id(13)).unwrap();
    db.trash_task(&id(13)).unwrap(); // already trashed: no change
    check(&db);
    db.restore_task(&id(13)).unwrap();
    db.set_task_home(
        &id(10),
        &Home::Project {
            project: id(2),
            heading: Some(id(4)),
        },
    )
    .unwrap();
    db.set_task_home(&id(13), &Home::Inbox).unwrap();
    db.set_task_home(&id(13), &Home::Inbox).unwrap(); // already there
    db.set_project_notes(&id(2), "plan").unwrap();
    db.set_project_schedule(&id(2), &Schedule::Someday, today())
        .unwrap();
    db.set_project_deadline(&id(3), Some(today())).unwrap();
    check(&db);
    let s = db.engine_snapshot().unwrap().unwrap();
    let task = |n| s.tasks.iter().find(|t| t.id == id(n)).unwrap().clone();
    assert_eq!(task(10).heading_id, Some(id(4)));
    assert_eq!(task(12).status, Status::Canceled);
}

#[test]
fn a_reopened_database_rebuilds_the_engine_from_its_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let db = Database::open(&path).unwrap();
        db.enable_sync().unwrap();
        db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
        db.set_task_notes(&id(1), "kept").unwrap();
    }
    let db = Database::open(&path).unwrap();
    assert!(db.sync_enabled());
    check(&db);
    db.rename_task(&id(1), "again").unwrap();
    check(&db);
    let journal: i64 = db
        .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_journal", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(journal, 3);
}

#[test]
fn a_write_that_fails_to_commit_leaves_the_engine_as_the_rows() {
    let db = synced();
    db.add_task(&mut Task::new(id(1), "before", 0.0)).unwrap();
    db.with_raw_conn(|c| {
        c.execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON sync_journal
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )?;
        Ok(())
    })
    .unwrap();
    assert!(db.rename_task(&id(1), "after").is_err());
    assert_eq!(db.get_task(&id(1)).unwrap().unwrap().title, "before");
    check(&db);
    db.with_raw_conn(|c| Ok(c.execute_batch("DROP TRIGGER refuse;")?))
        .unwrap();
    db.rename_task(&id(1), "after").unwrap();
    check(&db);
}

#[test]
fn an_engine_refusal_rolls_the_write_back() {
    let db = synced();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    // A row the engine never saw: its commands are refused, so the write is.
    db.with_raw_conn(|c| {
        c.execute(
            "INSERT INTO tasks (id, title, sort_order, created_at, updated_at) VALUES (?1, 'ghost', 9, 0, 0)",
            [id(2)],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        db.rename_task(&id(2), "x"),
        Err(MattersError::NotFound { .. })
    ));
    assert_eq!(db.get_task(&id(2)).unwrap().unwrap().title, "ghost");
}

#[test]
fn sync_is_off_by_default_and_turns_on_only_for_an_empty_database() {
    let db = Database::open_in_memory().unwrap();
    assert!(!db.sync_enabled());
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    db.rename_task(&id(1), "two").unwrap();
    let journal: i64 = db
        .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_journal", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(journal, 0);
    assert!(db.engine_snapshot().unwrap().is_none());
    assert!(db.enable_sync().is_err(), "existing data needs #41");

    let db = synced();
    assert!(db.enable_sync().is_err(), "already on");
}

fn journal(db: &Database) -> i64 {
    db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_journal", [], |r| r.get(0))?))
        .unwrap()
}

#[test]
fn rows_created_with_every_field_set_match() {
    let db = synced();
    let mut area = Area::new(id(1), "Old", 0.0);
    area.trashed_at = Some(5);
    db.create_area(&area).unwrap();
    db.create_area(&Area::new(id(2), "Home", 1.0)).unwrap();
    let mut project = Project::new(id(3), "Garden", 1.0);
    project.area_id = Some(id(2));
    project.notes = "plan".into();
    project.apply_schedule(&Schedule::Today { evening: true }, today());
    project.deadline = Some(today());
    project.status = Status::Completed;
    project.completed_at = Some(7);
    project.trashed_at = Some(8);
    db.create_project(&project).unwrap();
    db.create_project(&Project::new(id(4), "First", 0.0))
        .unwrap();
    let mut first = Task::new(id(10), "first", 2.0);
    first.today_order = 2.0;
    db.create_task(&first).unwrap();
    let mut task = Task::new(id(11), "full", 1.0);
    task.notes = "notes".into();
    task.apply_schedule(&Schedule::Someday, today());
    task.deadline = Some(today());
    task.status = Status::Canceled;
    task.completed_at = Some(9);
    task.trashed_at = Some(10);
    task.today_order = 1.0;
    db.create_task(&task).unwrap();
    let mut item = ChecklistItem::new(id(20), id(11), "done", 1.0);
    item.is_completed = true;
    db.create_checklist_item(&item).unwrap();
    db.create_checklist_item(&ChecklistItem::new(id(21), id(11), "before", 0.0))
        .unwrap();
    db.add_heading(&mut Heading::new(id(30), id(3), "b", 0.0))
        .unwrap();
    db.create_heading(&Heading::new(id(31), id(3), "a", 0.5))
        .unwrap();
    check(&db);
    let s = db.engine_snapshot().unwrap().unwrap();
    let mut today_order: Vec<_> = s
        .tasks
        .iter()
        .map(|t| (t.today_order, t.id.clone()))
        .collect();
    today_order.sort_by(|x, y| x.0.total_cmp(&y.0));
    assert_eq!(today_order[0].1, id(11), "placed by its Today order");
    let mut areas: Vec<_> = s
        .areas
        .iter()
        .map(|a| (a.sort_order, a.id.clone()))
        .collect();
    areas.sort_by(|x, y| x.0.total_cmp(&y.0));
    assert_eq!(areas[0].1, id(1));
}

#[test]
fn moving_a_task_into_this_evening_records_its_when() {
    let db = synced();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    db.set_task_schedule(&id(1), &Schedule::Today { evening: false }, today())
        .unwrap();
    matters_core::ordering::place_in_today(&db, today(), &id(1), true, None).unwrap();
    check(&db);
    let s = db.engine_snapshot().unwrap().unwrap();
    assert_eq!(s.tasks[0].time_of_day.as_deref(), Some("evening"));
}

#[test]
fn writes_that_change_nothing_journal_nothing() {
    let db = synced();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    db.trash_task(&id(1)).unwrap();
    let before = journal(&db);
    db.trash_task(&id(1)).unwrap();
    db.restore_task(&id(1)).unwrap();
    db.restore_task(&id(1)).unwrap();
    db.complete_task(&id(404)).unwrap();
    db.uncomplete_task(&id(404)).unwrap();
    db.set_task_home(&id(1), &Home::Inbox).unwrap();
    assert_eq!(
        journal(&db),
        before + 1,
        "only the restore changed anything"
    );
    check(&db);
}

#[test]
fn each_open_edits_as_a_new_engine_actor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    let first = {
        let db = Database::open(&path).unwrap();
        db.enable_sync().unwrap();
        db.engine_peer().unwrap()
    };
    let second = Database::open(&path).unwrap().engine_peer().unwrap();
    let third = Database::open(&path).unwrap().engine_peer().unwrap();
    assert_ne!(first, second);
    assert_ne!(second, third);
    assert_eq!(Database::open_in_memory().unwrap().engine_peer(), None);
}

#[test]
fn area_project_heading_and_checklist_writes_keep_the_engine_equal() {
    use matters_core::Outcome;
    let db = synced();
    db.add_area(&mut Area::new(id(1), "Home", 0.0)).unwrap();
    db.add_area(&mut Area::new(id(2), "Work", 0.0)).unwrap();
    db.add_project(&mut Project::new(id(3), "Garden", 0.0))
        .unwrap();
    db.add_heading(&mut Heading::new(id(4), id(3), "Spring", 0.0))
        .unwrap();
    for (n, heading) in [
        (10, Some(id(4))),
        (11, Some(id(4))),
        (12, None),
        (13, Some(id(4))),
    ] {
        let mut task = Task::new(id(n), format!("task {n}"), 0.0);
        Home::Project {
            project: id(3),
            heading,
        }
        .apply(&mut task);
        db.add_task(&mut task).unwrap();
    }
    db.add_checklist_item(&mut ChecklistItem::new(id(20), id(10), "rake", 0.0))
        .unwrap();
    db.add_checklist_item(&mut ChecklistItem::new(id(21), id(10), "hoe", 0.0))
        .unwrap();
    check(&db);

    db.rename_area(&id(1), "House").unwrap();
    db.rename_project(&id(3), "Yard").unwrap();
    db.rename_heading(&id(4), "Summer").unwrap();
    db.set_project_area(&id(3), Some(&id(1))).unwrap();
    db.set_project_area(&id(3), Some(&id(1))).unwrap(); // already there
    db.rename_checklist_item(&id(20), "big rake").unwrap();
    db.set_checklist_item_done(&id(21), true).unwrap();
    check(&db);

    db.complete_task(&id(11)).unwrap();
    db.finish_project(&id(3), Outcome::Completed, Outcome::Canceled)
        .unwrap();
    check(&db);
    let s = db.engine_snapshot().unwrap().unwrap();
    assert!(s.closed_by_project.contains(&id(10)));
    assert!(!s.closed_by_project.contains(&id(11)), "finished before");
    db.reopen_project(&id(3)).unwrap();
    check(&db);
    assert!(db
        .engine_snapshot()
        .unwrap()
        .unwrap()
        .closed_by_project
        .is_empty());

    let deleted = db.delete_heading(&id(4)).unwrap();
    check(&db);
    db.restore_heading(&deleted).unwrap();
    check(&db);

    let item = db.get_checklist_item(&id(20)).unwrap().unwrap();
    db.delete_checklist_item(&id(20)).unwrap();
    db.delete_checklist_item(&id(20)).unwrap(); // already gone
    check(&db);
    db.create_checklist_item(&item).unwrap(); // Undo
    check(&db);

    db.trash_project(&id(3)).unwrap();
    db.trash_area(&id(2)).unwrap();
    check(&db);
    db.restore_project(&id(3)).unwrap();
    db.restore_area(&id(2)).unwrap();
    db.set_project_area(&id(3), None).unwrap();
    check(&db);
}

#[test]
fn only_a_deleted_heading_or_checklist_item_can_be_created_again() {
    let db = synced();
    db.add_project(&mut Project::new(id(1), "p", 0.0)).unwrap();
    let mut task = Task::new(id(2), "t", 0.0);
    task.project_id = Some(id(1));
    db.add_task(&mut task).unwrap();
    let item = ChecklistItem::new(id(3), id(2), "i", 0.0);
    db.create_checklist_item(&item).unwrap();
    // A live item: the row insert itself is refused.
    assert!(db.create_checklist_item(&item).is_err());
    check(&db);
}

#[test]
fn undoing_a_heading_deletion_leaves_tasks_that_moved_away() {
    let db = synced();
    db.add_project(&mut Project::new(id(1), "p", 0.0)).unwrap();
    db.add_heading(&mut Heading::new(id(2), id(1), "h", 0.0))
        .unwrap();
    for n in [10, 11] {
        let mut task = Task::new(id(n), format!("t{n}"), 0.0);
        Home::Project {
            project: id(1),
            heading: Some(id(2)),
        }
        .apply(&mut task);
        db.add_task(&mut task).unwrap();
    }
    let deleted = db.delete_heading(&id(2)).unwrap();
    db.set_task_home(&id(11), &Home::Inbox).unwrap();
    db.restore_heading(&deleted).unwrap();
    check(&db);
    assert_eq!(db.get_task(&id(11)).unwrap().unwrap().project_id, None);
}
