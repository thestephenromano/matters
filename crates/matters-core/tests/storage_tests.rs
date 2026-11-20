//! What the database keeps: each write is read back as written, and each
//! delete removes exactly the row it names.

use matters_core::{ChecklistItem, Database, Heading, Project, Status, Task};

#[test]
fn headings_are_listed_per_project_in_order_and_deleted_one_at_a_time() {
    let db = Database::open_in_memory().unwrap();
    for id in ["p1", "p2"] {
        db.create_project(&Project::new(id, id, 0.0)).unwrap();
    }
    let later = Heading::new("h1", "p1", "Planting", 2.0);
    let first = Heading::new("h2", "p1", "Digging", 1.0);
    let elsewhere = Heading::new("h3", "p2", "Painting", 0.0);
    for h in [&later, &first, &elsewhere] {
        db.create_heading(h).unwrap();
    }
    assert_eq!(
        db.list_headings_for_project("p1").unwrap(),
        [first.clone(), later.clone()]
    );

    db.delete_heading("h2").unwrap();
    assert_eq!(db.list_headings_for_project("p1").unwrap(), [later]);
    assert_eq!(db.list_headings_for_project("p2").unwrap(), [elsewhere]);
}

#[test]
fn deleting_a_checklist_item_removes_only_that_item() {
    let db = Database::open_in_memory().unwrap();
    db.create_task(&Task::new("t", "Pack", 0.0)).unwrap();
    for (i, id) in ["c1", "c2"].iter().enumerate() {
        db.create_checklist_item(&ChecklistItem::new(*id, "t", *id, i as f64))
            .unwrap();
    }
    db.delete_checklist_item("c1").unwrap();
    let left: Vec<_> = db
        .list_checklist_items("t")
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(left, ["c2"]);
}

#[test]
fn a_project_is_read_back_as_it_was_edited() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p", "Garden", 0.0))
        .unwrap();
    db.create_project(&Project::new("other", "Shed", 1.0))
        .unwrap();

    let start = jiff::civil::date(2026, 12, 1);
    let deadline = jiff::civil::date(2027, 3, 1);
    db.rename_project("p", "Garden, phase 2").unwrap();
    db.set_project_notes("p", "after the frost").unwrap();
    db.set_project_schedule("p", &matters_core::Schedule::Date(start), start)
        .unwrap();
    db.set_project_deadline("p", Some(deadline)).unwrap();

    let saved = db.get_project("p").unwrap().unwrap();
    assert_eq!(
        (
            saved.title.as_str(),
            saved.notes.as_str(),
            saved.start_date,
            saved.deadline,
            saved.is_someday,
            saved.status,
            saved.completed_at,
        ),
        (
            "Garden, phase 2",
            "after the frost",
            Some(start),
            Some(deadline),
            false,
            Status::Open,
            None,
        ),
        "edits never finish a project: finish_project does"
    );
    for missing in [
        db.set_project_notes("missing", ""),
        db.set_project_deadline("missing", None),
        db.set_task_notes("missing", ""),
        db.set_task_deadline("missing", None),
        db.cancel_task("missing"),
    ] {
        assert!(matches!(
            missing,
            Err(matters_core::MattersError::NotFound { .. })
        ));
    }
    let other = db.get_project("other").unwrap().unwrap();
    assert_eq!((other.title.as_str(), other.is_someday), ("Shed", false));
}

#[test]
fn the_shared_connection_reads_but_never_writes() {
    let db = Database::open_in_memory().unwrap();
    db.create_task(&matters_core::Task::new("t", "t", 0.0))
        .unwrap();
    let write = db.with_conn(|conn| {
        conn.execute("UPDATE tasks SET title = 'raw' WHERE id = 't'", [])?;
        Ok(())
    });
    assert!(write.is_err(), "a raw write is refused");
    let title: String = db
        .with_conn(|conn| Ok(conn.query_row("SELECT title FROM tasks", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(title, "t");
    // Typed writes still work afterwards.
    db.rename_task("t", "typed").unwrap();
    assert_eq!(db.get_task("t").unwrap().unwrap().title, "typed");
}
