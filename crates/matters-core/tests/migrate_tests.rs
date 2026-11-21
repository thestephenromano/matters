//! Bringing an existing database into sync (#41): every entity and state
//! migrates with its IDs and views unchanged, a backup is kept, a crash
//! leaves the original, and a second device receives exactly the same
//! data, never a second lineage.

use std::process::Command;

use jiff::civil::date;
use matters_core::repeat::{Frequency, RepeatRule};
use matters_core::storage::membership::MemoryKeyStore;
use matters_core::{
    Area, ChecklistItem, Database, Heading, Home, Outcome, Project, Schedule, Tag, Tagged, Task,
};
use matters_sync::authority::{parse, Body, Route};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

/// A database with every kind of entity in every state the app makes.
fn representative(db: &Database) {
    let today = date(2026, 11, 21);
    db.add_area(&mut Area::new(id(1), "Home", 0.0)).unwrap();
    db.add_area(&mut Area::new(id(2), "Old area", 0.0)).unwrap();
    db.trash_area(&id(2)).unwrap();
    let mut garden = Project::new(id(10), "Garden", 0.0);
    garden.area_id = Some(id(1));
    db.add_project(&mut garden).unwrap();
    db.set_project_notes(&id(10), "beds by the fence").unwrap();
    db.set_project_deadline(&id(10), Some(date(2026, 12, 24)))
        .unwrap();
    db.set_project_goal(&id(10), Some(&id(900))).unwrap();
    db.add_project(&mut Project::new(id(11), "Finished", 0.0))
        .unwrap();
    db.add_project(&mut Project::new(id(12), "Trashed", 0.0))
        .unwrap();
    db.add_heading(&mut Heading::new(id(20), id(10), "Spring", 0.0))
        .unwrap();
    db.add_heading(&mut Heading::new(id(21), id(10), "Summer", 0.0))
        .unwrap();
    let tasks: [(u128, &str, Home); 9] = [
        (30, "inbox", Home::Inbox),
        (31, "in the area", Home::Area(id(1))),
        (
            32,
            "in the project",
            Home::Project {
                project: id(10),
                heading: None,
            },
        ),
        (
            33,
            "under a heading",
            Home::Project {
                project: id(10),
                heading: Some(id(20)),
            },
        ),
        (
            34,
            "second under it",
            Home::Project {
                project: id(10),
                heading: Some(id(20)),
            },
        ),
        (
            35,
            "closes with its project",
            Home::Project {
                project: id(11),
                heading: None,
            },
        ),
        (36, "trashed", Home::Inbox),
        (37, "repeats", Home::Inbox),
        (38, "evening", Home::Inbox),
    ];
    for (n, title, home) in tasks {
        db.add_task(&mut Task::new(id(n), title, 0.0)).unwrap();
        db.set_task_home(&id(n), &home).unwrap();
    }
    db.set_task_notes(&id(30), "first line\nsecond line")
        .unwrap();
    db.set_task_schedule(&id(30), &Schedule::Today { evening: false }, today)
        .unwrap();
    db.set_task_schedule(&id(38), &Schedule::Today { evening: true }, today)
        .unwrap();
    db.set_task_schedule(&id(31), &Schedule::Someday, today)
        .unwrap();
    db.set_task_schedule(&id(32), &Schedule::Date(date(2026, 12, 1)), today)
        .unwrap();
    db.set_task_deadline(&id(32), Some(date(2026, 12, 5)))
        .unwrap();
    db.set_task_reminder(&id(33), Some(jiff::civil::time(9, 30, 0, 0)))
        .unwrap();
    db.complete_task(&id(34)).unwrap();
    db.cancel_task(&id(31)).unwrap();
    db.trash_task(&id(36)).unwrap();
    db.set_task_repeat(
        &id(37),
        Some(RepeatRule {
            every: 1,
            frequency: Frequency::Daily,
            after_completion: false,
            start: today,
            until: None,
            left: None,
            paused: false,
        }),
        today,
    )
    .unwrap();
    db.finish_project(&id(11), Outcome::Completed, Outcome::Completed)
        .unwrap();
    db.trash_project(&id(12)).unwrap();
    for (n, title, done) in [(40, "spade", true), (41, "seeds", false)] {
        db.add_checklist_item(&mut ChecklistItem::new(id(n), id(33), title, 0.0))
            .unwrap();
        if done {
            db.set_checklist_item_done(&id(n), true).unwrap();
        }
    }
    db.add_tag(&mut Tag::new(id(50), "Errands", None)).unwrap();
    db.add_tag(&mut Tag::new(id(51), "Shop", Some(id(50))))
        .unwrap();
    db.add_tag(&mut Tag::new(id(52), "Calls", None)).unwrap();
    db.add_tag_to(Tagged::Task(&id(30)), &id(51)).unwrap();
    db.add_tag_to(Tagged::Project(&id(10)), &id(52)).unwrap();
    db.add_tag_to(Tagged::Area(&id(1)), &id(50)).unwrap();
}

/// Everything the app shows of the representative data, field by field
/// except the list keys (whose order is compared through the lists).
fn shown(db: &Database) -> Vec<String> {
    let mut out = Vec::new();
    for a in db
        .list_areas()
        .unwrap()
        .iter()
        .chain(&db.list_trashed_areas().unwrap())
    {
        out.push(format!(
            "area {} {} {:?} {:?}",
            a.id,
            a.title,
            a.trashed_at,
            db.tags_of(Tagged::Area(&a.id))
                .unwrap()
                .iter()
                .map(|t| &t.id)
                .collect::<Vec<_>>()
        ));
    }
    for p in db
        .list_projects()
        .unwrap()
        .iter()
        .chain(&db.list_finished_projects().unwrap())
        .chain(&db.list_trashed_projects().unwrap())
    {
        out.push(format!(
            "project {} {} {:?} {} {:?} {:?} {:?} {:?} {:?}",
            p.id,
            p.title,
            p.area_id,
            p.notes,
            p.deadline,
            p.status,
            p.completed_at,
            p.trashed_at,
            db.tags_of(Tagged::Project(&p.id))
                .unwrap()
                .iter()
                .map(|t| &t.id)
                .collect::<Vec<_>>()
        ));
        for h in db.list_headings_for_project(&p.id).unwrap() {
            out.push(format!("heading {} {} {}", h.id, h.project_id, h.title));
        }
    }
    for n in 30..=38 {
        let t = db.get_task(&id(n)).unwrap().unwrap();
        out.push(format!(
            "task {} {} {:?} {:?} {:?} {} {:?} {:?} {} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {}",
            t.id,
            t.title,
            t.area_id,
            t.project_id,
            t.heading_id,
            t.notes,
            t.start_date,
            t.time_of_day,
            t.is_someday,
            t.deadline,
            t.reminder,
            t.repeat,
            t.status,
            t.completed_at,
            t.trashed_at,
            t.created_at,
            t.goal_id,
            t.updated_at
        ));
        out.push(format!(
            "  tags {:?}",
            db.tags_of(Tagged::Task(&t.id))
                .unwrap()
                .iter()
                .map(|t| &t.id)
                .collect::<Vec<_>>()
        ));
        for c in db.list_checklist_items(&t.id).unwrap() {
            out.push(format!("  item {} {} {}", c.id, c.title, c.is_completed));
        }
    }
    for t in db.list_tags().unwrap() {
        out.push(format!("tag {} {} {:?}", t.id, t.name, t.parent_id));
    }
    out
}

#[test]
fn a_representative_database_migrates_with_ids_and_views_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    let db = Database::open(&path).unwrap();
    representative(&db);
    let before = shown(&db);
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    assert!(db.sync_enabled());
    assert_eq!(shown(&db), before, "every view unchanged");
    // The migration is this device's first sealed records.
    assert!(!db.outgoing_records().unwrap().is_empty());
    // A consistent backup of the original sits beside it.
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".pre-sync-"))
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = Database::open(backups[0].path()).unwrap();
    assert!(!backup.sync_enabled());
    assert_eq!(shown(&backup), before);
    // It survives a restart: the rows are rebuilt from the engine and match.
    drop(db);
    let db = Database::open(&path).unwrap();
    db.rebuild_from_engine().unwrap();
    assert_eq!(shown(&db), before);
}

#[test]
fn a_second_device_receives_the_same_data_not_a_second_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    representative(&a);
    let before = shown(&a);
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    let shown_invitation = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &shown_invitation).unwrap();
    let b_id = parse(&request).unwrap().message.signer;
    a.admit(&request, &shown_invitation).unwrap();
    let envelopes: Vec<Vec<u8>> = a
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == b_id)
        })
        .map(|o| o.bytes)
        .collect();
    b.finish_enrollment(&b_keys, &a.sync_controls().unwrap(), &envelopes)
        .unwrap();
    for record in a.outgoing_records().unwrap() {
        b.receive_record(&record.bytes).unwrap();
    }
    assert!(b.pending_records().unwrap().is_empty());
    assert_eq!(shown(&b), before, "the same IDs, fields and lists");
}

#[test]
fn a_device_with_its_own_data_cannot_join_without_an_explicit_import() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    b.add_task(&mut Task::new(id(1), "b's own", 0.0)).unwrap();
    let keys = MemoryKeyStore::default();
    let shown_invitation = a.invite().unwrap();
    let request = b.start_enrollment(&keys, &shown_invitation).unwrap();
    a.admit(&request, &shown_invitation).unwrap();
    let refused = b.finish_enrollment(&keys, &a.sync_controls().unwrap(), &[]);
    assert!(refused.is_err());
    // Nothing of B's was replaced or lost.
    assert_eq!(b.get_task(&id(1)).unwrap().unwrap().title, "b's own");
    assert!(!b.sync_enabled());
}

/// Child process for the crash test: migrate, crashing before the commit.
#[test]
#[ignore]
fn migration_crash_worker() {
    let Ok(path) = std::env::var("MATTERS_CRASH_DB") else {
        return;
    };
    let db = Database::open(&path).unwrap();
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_during_migration_leaves_the_original_and_can_be_redone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    let before = {
        let db = Database::open(&path).unwrap();
        representative(&db);
        shown(&db)
    };
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "migration_crash_worker",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("MATTERS_CRASH_DB", &path)
        .env("MATTERS_CRASH_AT", "collection-before-commit")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let db = Database::open(&path).unwrap();
    assert!(!db.sync_enabled(), "nothing half-converted");
    assert_eq!(shown(&db), before);
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    assert_eq!(shown(&db), before);
}

#[test]
fn a_database_from_a_newer_matters_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let db = Database::open(&path).unwrap();
        db.add_task(&mut Task::new(id(1), "kept", 0.0)).unwrap();
    }
    let newer = matters_core::storage::db::LATEST_SCHEMA + 1;
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
            [newer],
        )
        .unwrap();
    }
    match Database::open(&path) {
        Err(matters_core::MattersError::NewerSchema { found, supported }) => {
            assert_eq!((found, supported), (newer, newer - 1));
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    let title: String = conn
        .query_row("SELECT title FROM tasks WHERE id = ?1", [id(1)], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, "kept");
}

#[test]
fn a_device_sets_its_own_data_aside_joins_and_imports_what_is_new() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.add_task(&mut Task::new(id(1), "shared, A's version", 0.0))
        .unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    // B was once a copy of A, then gained its own work.
    let b = Database::open(dir.path().join("b.db")).unwrap();
    b.add_task(&mut Task::new(id(1), "shared, B's old copy", 0.0))
        .unwrap();
    b.add_task(&mut Task::new(id(2), "B's own", 1.0)).unwrap();
    b.complete_task(&id(2)).unwrap();
    b.add_checklist_item(&mut ChecklistItem::new(id(3), id(2), "step", 0.0))
        .unwrap();
    b.add_tag(&mut Tag::new(id(4), "B's tag", None)).unwrap();
    b.add_tag_to(Tagged::Task(&id(2)), &id(4)).unwrap();
    let aside = b.set_aside_local_data().unwrap();
    assert!(b.get_task(&id(2)).unwrap().is_none(), "set aside, not lost");
    assert_eq!(
        Database::open(&aside)
            .unwrap()
            .get_task(&id(2))
            .unwrap()
            .unwrap()
            .title,
        "B's own"
    );
    // B joins, receives A's data, then imports what A does not have.
    let keys = MemoryKeyStore::default();
    let invitation = a.invite().unwrap();
    let request = b.start_enrollment(&keys, &invitation).unwrap();
    let b_id = parse(&request).unwrap().message.signer;
    a.admit(&request, &invitation).unwrap();
    let envelopes: Vec<Vec<u8>> = a
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == b_id)
        })
        .map(|o| o.bytes)
        .collect();
    b.finish_enrollment(&keys, &a.sync_controls().unwrap(), &envelopes)
        .unwrap();
    for record in a.outgoing_records().unwrap() {
        b.receive_record(&record.bytes).unwrap();
    }
    let report = b.import_missing_from(&aside).unwrap();
    assert_eq!(
        report.skipped, 1,
        "the shared task keeps the collection's version"
    );
    assert_eq!(
        b.get_task(&id(1)).unwrap().unwrap().title,
        "shared, A's version"
    );
    let own = b.get_task(&id(2)).unwrap().unwrap();
    assert_eq!(
        (own.title.as_str(), own.completed_at.is_some()),
        ("B's own", true)
    );
    assert_eq!(b.list_checklist_items(&id(2)).unwrap().len(), 1);
    assert_eq!(b.tags_of(Tagged::Task(&id(2))).unwrap()[0].id, id(4));
    // The import is B's own recorded work: A receives it.
    for o in b.outgoing_controls().unwrap() {
        let _ = a.apply_control(&o.bytes, None);
    }
    for record in b.outgoing_records().unwrap() {
        a.receive_record(&record.bytes).unwrap();
    }
    assert_eq!(a.get_task(&id(2)).unwrap().unwrap().title, "B's own");
    assert!(a.pending_records().unwrap().is_empty());
}
