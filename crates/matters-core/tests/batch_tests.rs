//! Atomic batches (#203): many operations in one transaction, recorded for
//! sync as one update and one sealed record, or not at all.

use matters_core::storage::sealing::SealingIdentity;
use matters_core::{Database, Home, MattersError, Project, Task, View, Writer, MAX_BATCH_BYTES};
use matters_sync::{generate_signing_key, ContentKey};

/// A stable UUID for a short name: synced databases take only UUIDs.
fn id(name: &str) -> String {
    let n = name.bytes().fold(0u128, |n, b| n << 8 | u128::from(b));
    uuid::Uuid::from_u128(n).to_string()
}

fn today() -> jiff::civil::Date {
    jiff::civil::date(2026, 11, 22)
}

fn synced() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    db
}

fn sealed() -> Database {
    let db = synced();
    let (signing_key, _) = generate_signing_key([7; 32]);
    db.start_sealing(SealingIdentity {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
        document: [4; 16],
        epoch: 0,
        manifest: [5; 48],
        author: [6; 48],
        content_key: ContentKey::new([9; 32]),
        signing_key,
    })
    .unwrap();
    db
}

fn count(db: &Database, table: &str) -> i64 {
    db.with_conn(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .unwrap()
}

/// Every row of the synced tables, and the journal, as text.
fn state(db: &Database) -> Vec<String> {
    db.with_conn(|c| {
        let mut out = Vec::new();
        for table in [
            "areas",
            "projects",
            "headings",
            "tasks",
            "checklist_items",
            "sync_journal",
        ] {
            let mut stmt = c.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
            let columns = stmt.column_count();
            let rows = stmt.query_map([], |r| {
                let mut row = format!("{table}:");
                for i in 0..columns {
                    row += &format!("{:?}|", r.get_ref(i)?);
                }
                Ok(row)
            })?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    })
    .unwrap()
}

fn inbox(db: &Database) -> Vec<String> {
    matters_core::QueryEngine::new(db)
        .tasks_in(&View::Inbox, today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect()
}

#[test]
fn create_move_and_reorder_in_one_batch_is_one_record_the_engine_agrees_with() {
    let db = sealed();
    let (journal, records) = (count(&db, "sync_journal"), count(&db, "sync_records"));

    db.batch(|w| {
        let mut project = Project::new(id("p"), "Project", 0.0);
        w.add_project(&mut project)?;
        for name in ["a", "b", "c"] {
            w.add_task(&mut Task::new(id(name), name.to_uppercase(), 0.0))?;
        }
        w.set_task_home(
            &id("b"),
            &Home::Project {
                project: id("p"),
                heading: None,
            },
        )?;
        // Reads what the batch did so far: "c" is in the Inbox, after "a".
        w.place_task(&View::Inbox, today(), &id("c"), None)?;
        w.rename_task(&id("a"), "Renamed")
    })
    .unwrap();

    assert_eq!(inbox(&db), [id("c"), id("a")]);
    assert_eq!(
        db.get_task(&id("b"))
            .unwrap()
            .unwrap()
            .project_id
            .as_deref(),
        Some(id("p").as_str())
    );
    assert_eq!(db.get_task(&id("a")).unwrap().unwrap().title, "Renamed");
    assert_eq!(count(&db, "sync_journal"), journal + 1);
    assert_eq!(count(&db, "sync_records"), records + 1);
    db.verify_rows_match_engine().unwrap();
}

#[test]
fn one_refused_change_leaves_the_rows_the_journal_and_the_engine_as_they_were() {
    let db = sealed();
    for name in ["a", "b"] {
        db.add_task(&mut Task::new(id(name), name, 0.0)).unwrap();
    }
    let rows = state(&db);
    let engine = db.engine_snapshot().unwrap();

    let refused = db.batch(|w| {
        w.rename_task(&id("a"), "Changed")?;
        w.trash_task(&id("b"))?;
        w.add_task(&mut Task::new(id("c"), "New", 0.0))?;
        w.rename_task(&id("missing"), "Nothing")
    });

    assert!(
        matches!(refused, Err(MattersError::NotFound { .. })),
        "{refused:?}"
    );
    assert_eq!(state(&db), rows);
    assert_eq!(db.engine_snapshot().unwrap(), engine);
    db.verify_rows_match_engine().unwrap();
    // And writing carries on from there.
    db.rename_task(&id("a"), "After").unwrap();
    db.verify_rows_match_engine().unwrap();
}

#[test]
fn a_batch_whose_caller_ignores_a_failed_change_still_changes_nothing() {
    let db = synced();
    db.add_task(&mut Task::new(id("a"), "A", 0.0)).unwrap();
    let rows = state(&db);

    // Ignored: a change that failed in its SQL, one refused before any
    // SQL (a blank title), and a refused reorder.
    let ignoring: [fn(&mut Writer<'_>) -> matters_core::Result<()>; 3] = [
        |w| w.set_task_home(&id("a"), &Home::Area(id("missing"))),
        |w| w.rename_task(&id("a"), "   "),
        |w| w.place_task(&View::Inbox, today(), &id("missing"), None),
    ];
    for ignored in ignoring {
        let refused = db.batch(|w| {
            w.rename_task(&id("a"), "Changed")?;
            let _ = ignored(w);
            Ok(())
        });
        assert!(
            matches!(refused, Err(MattersError::Validation(_))),
            "{refused:?}"
        );
        assert_eq!(state(&db), rows);
    }
    db.verify_rows_match_engine().unwrap();
}

#[test]
fn a_batch_too_large_for_one_record_is_refused_whole() {
    let db = synced();
    db.add_task(&mut Task::new(id("kept"), "Kept", 0.0))
        .unwrap();
    let rows = state(&db);
    let notes = "n".repeat(MAX_BATCH_BYTES / 4);

    let refused = db.batch(|w| {
        for i in 0..5 {
            let id = id(&format!("big-{i}"));
            w.add_task(&mut Task::new(&id, "Big", 0.0))?;
            w.set_task_notes(&id, &notes)?;
        }
        Ok(())
    });

    match refused {
        Err(MattersError::BatchTooLarge { bytes, limit }) => {
            assert_eq!(limit, MAX_BATCH_BYTES);
            assert!(bytes > limit);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(state(&db), rows);
    db.verify_rows_match_engine().unwrap();
    // The same changes, sent in smaller batches, go through.
    for i in 0..5 {
        let id = id(&format!("big-{i}"));
        db.batch(|w| {
            w.add_task(&mut Task::new(&id, "Big", 0.0))?;
            w.set_task_notes(&id, &notes)
        })
        .unwrap();
    }
    db.verify_rows_match_engine().unwrap();
}

#[test]
fn with_sync_off_a_batch_is_still_all_or_nothing() {
    let db = Database::open_in_memory().unwrap();
    let refused = db.batch(|w| {
        w.add_task(&mut Task::new(id("a"), "A", 0.0))?;
        w.rename_task(&id("missing"), "Nothing")
    });
    assert!(refused.is_err());
    assert_eq!(count(&db, "tasks"), 0);
    db.batch(|w| w.add_task(&mut Task::new(id("a"), "A", 0.0)))
        .unwrap();
    assert_eq!(count(&db, "tasks"), 1);
    assert_eq!(count(&db, "sync_journal"), 0);
}

#[test]
fn a_retried_request_returns_its_first_result_and_changes_nothing() {
    let db = synced();
    let mut runs = 0;
    let mut send = |digest: [u8; 32]| {
        db.batch_once("request-1", &digest, |w| {
            runs += 1;
            w.add_task(&mut Task::new(id(&format!("t{runs}")), "Once", 0.0))?;
            Ok(b"created t1".to_vec())
        })
    };

    assert_eq!(send([1; 32]).unwrap(), b"created t1");
    let journal = count(&db, "sync_journal");
    // The answer was lost; the caller sends the same request again.
    assert_eq!(send([1; 32]).unwrap(), b"created t1");
    // The same ID for other changes is a caller's mistake.
    let reused = send([2; 32]);
    assert!(
        matches!(reused, Err(MattersError::Validation(_))),
        "{reused:?}"
    );

    assert_eq!(runs, 1);
    assert_eq!(count(&db, "tasks"), 1);
    assert_eq!(count(&db, "sync_journal"), journal);
    db.verify_rows_match_engine().unwrap();
}

#[test]
fn a_refused_request_is_not_remembered() {
    let db = synced();
    let refused = db.batch_once("request-1", &[1; 32], |w| {
        w.rename_task(&id("missing"), "Nothing")?;
        Ok(Vec::new())
    });
    assert!(refused.is_err());
    // Nothing was stored, so the corrected request can use the same ID.
    db.add_task(&mut Task::new(id("a"), "A", 0.0)).unwrap();
    let done = db.batch_once("request-1", &[1; 32], |w| {
        w.rename_task(&id("a"), "Renamed")?;
        Ok(b"ok".to_vec())
    });
    assert_eq!(done.unwrap(), b"ok");
    assert_eq!(db.get_task(&id("a")).unwrap().unwrap().title, "Renamed");
}

#[test]
fn request_ids_are_remembered_for_a_day() {
    let db = synced();
    let hour = 60 * 60 * 1000;
    let age = |hours: i64| {
        db.with_raw_conn(|c| {
            c.execute(
                "UPDATE batch_requests SET created_at = ?1",
                [matters_core::clock::now_millis() - hours * hour],
            )?;
            Ok(())
        })
        .unwrap();
    };
    let send = |digest: [u8; 32]| db.batch_once("request-1", &digest, |_| Ok(digest.to_vec()));

    assert_eq!(send([1; 32]).unwrap(), [1; 32]);
    age(23);
    // Still remembered: another body under the same ID is refused.
    assert!(send([2; 32]).is_err());
    age(25);
    // Forgotten: the ID is free again.
    assert_eq!(send([2; 32]).unwrap(), [2; 32]);
}

#[test]
fn queries_in_a_batch_see_its_own_changes_and_match_the_database() {
    use matters_core::{Area, ChecklistItem, QueryEngine, Tag, Tagged};
    let db = synced();
    db.add_tag(&mut Tag::new(id("tag"), "Errand", None))
        .unwrap();

    db.batch(|w| {
        w.add_area(&mut Area::new(id("area"), "Home", 0.0))?;
        w.add_project(&mut Project::new(id("p"), "Garden", 0.0))?;
        w.add_task(&mut Task::new(id("t"), "Dig", 0.0))?;
        w.add_tag_to(Tagged::Task(&id("t")), &id("tag"))?;
        w.add_checklist_item(&mut ChecklistItem::new(id("c"), id("t"), "Spade", 0.0))?;
        w.set_checklist_item_done(&id("c"), true)?;

        // Not committed yet, but the batch's queries see all of it.
        let q = w.query();
        assert_eq!(q.task(&id("t"))?.map(|t| t.title), Some("Dig".into()));
        assert_eq!(q.project(&id("p"))?.map(|p| p.title), Some("Garden".into()));
        assert_eq!(q.area(&id("area"))?.map(|a| a.title), Some("Home".into()));
        assert_eq!(q.task(&id("p"))?, None);
        assert_eq!(q.project(&id("t"))?, None);
        assert_eq!(q.area(&id("t"))?, None);
        assert_eq!(q.tag_named("  errand ")?.map(|t| t.id), Some(id("tag")));
        assert_eq!(q.tag_named("missing")?, None);
        assert_eq!(q.tag_named("   ")?, None);
        let ids = [id("t")];
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(q.tags_of_tasks(&ids)?[&id("t")][0].name, "Errand");
        let progress = &q.checklist_progress(&ids)?[&id("t")];
        assert_eq!((progress.done, progress.total), (1, 1));
        assert_eq!(q.inbox()?.len(), 1);
        Ok(())
    })
    .unwrap();

    // And outside it, the same reads through the database agree.
    let q = QueryEngine::new(&db);
    let ids = [id("t")];
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    assert_eq!(q.task(&id("t")).unwrap(), db.get_task(&id("t")).unwrap());
    assert_eq!(
        q.project(&id("p")).unwrap(),
        db.get_project(&id("p")).unwrap()
    );
    assert_eq!(
        q.area(&id("area")).unwrap(),
        db.get_area(&id("area")).unwrap()
    );
    assert_eq!(
        q.tags_of_tasks(&ids).unwrap(),
        db.tags_of_tasks(&ids).unwrap()
    );
    assert_eq!(
        q.checklist_progress(&ids).unwrap(),
        db.checklist_progress(&ids).unwrap()
    );
    assert_eq!(
        q.tag_named("ERRAND").unwrap(),
        db.tag_named("errand").unwrap()
    );
}
