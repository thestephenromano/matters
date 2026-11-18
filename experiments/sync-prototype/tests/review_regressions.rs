use matters_sync_prototype::Request;
use matters_sync_prototype::{Admission, Record};
use matters_sync_prototype::{Command as C, Fault, Kind, Replica, Result};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}
fn create(n: u128) -> C {
    C::Create {
        id: id(n),
        kind: Kind::Task,
        parent: None,
        title: format!("task {n}"),
    }
}
fn home(r: &Replica) -> Result<Vec<String>> {
    let mut entities: Vec<_> = r.projection()?.entities.into_values().collect();
    entities.sort_by_key(|e| e.rank);
    Ok(entities.into_iter().map(|e| e.id).collect())
}

#[test]
fn remove_merged_tag_clears_every_known_alias() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut a = Replica::open(dir.path().join("a.db"))?;
    let mut b = Replica::open(dir.path().join("b.db"))?;
    b.receive(&a.execute(&Request::new(create(1)))?)?;
    for (r, tag) in [(&mut a, 10), (&mut b, 11)] {
        r.execute(&Request::new(C::Tag {
            id: id(tag),
            name: "work".into(),
        }))?;
        r.execute(&Request::new(C::TagMembership {
            task: id(1),
            tag: id(tag),
            present: true,
        }))?;
    }
    for record in b.outbox()? {
        a.receive(&record)?;
    }
    for record in a.outbox()? {
        b.receive(&record)?;
    }
    let removal = a.execute(&Request::new(C::TagMembership {
        task: id(1),
        tag: id(10),
        present: false,
    }))?;
    b.receive(&removal)?;
    assert!(a.projection()?.memberships.is_empty());
    assert_eq!(a.projection()?, b.projection()?);
    Ok(())
}

#[test]
fn home_move_uses_visible_rank_after_purge() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut r = Replica::open(dir.path().join("r.db"))?;
    for n in 1..=3 {
        r.execute(&Request::new(create(n)))?;
    }
    r.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: true,
    }))?;
    r.execute(&Request::new(C::PurgeTrash))?;
    r.execute(&Request::new(C::Move {
        id: id(2),
        parent: None,
        index: 1,
    }))?;
    assert_eq!(home(&r)?, vec![id(3), id(2)]);
    Ok(())
}

#[test]
fn today_move_uses_visible_rank_after_purge() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut r = Replica::open(dir.path().join("r.db"))?;
    for n in 1..=3 {
        r.execute(&Request::new(create(n)))?;
    }
    r.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: true,
    }))?;
    r.execute(&Request::new(C::PurgeTrash))?;
    r.execute(&Request::new(C::MoveToday {
        id: id(2),
        index: 1,
    }))?;
    assert_eq!(r.projection()?.today_order, vec![id(3), id(2)]);
    Ok(())
}

#[test]
fn lost_ack_retry_does_not_insert_notes_twice() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut r = Replica::open(dir.path().join("r.db"))?;
    r.execute(&Request::new(create(1)))?;
    let insert = Request::new(C::InsertNotes {
        id: id(1),
        index: 0,
        text: "once".into(),
    });
    assert!(r.execute_with_fault(&insert, Fault::AfterCommit).is_err());
    r.execute(&insert)?;
    assert_eq!(r.projection()?.entities[&id(1)].notes, "once");
    Ok(())
}

#[test]
fn open_actually_reads_a_saved_checkpoint() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let mut r = Replica::open(&path)?;
    r.execute(&Request::new(create(1)))?;
    r.checkpoint()?;
    drop(r);
    let c = rusqlite::Connection::open(&path)?;
    c.execute("UPDATE checkpoint SET bytes=x'000102'", [])?;
    drop(c);
    assert!(
        Replica::open(path).is_err(),
        "an unread checkpoint cannot validate snapshot recovery"
    );
    Ok(())
}

#[test]
fn rejected_record_is_retained_for_diagnosis() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let mut r = Replica::open(&path)?;
    let bad = matters_sync_prototype::Record {
        id: id(99),
        bytes: vec![0, 1, 2],
    };
    assert!(r.receive(&bad).is_err());
    drop(r);
    let c = rusqlite::Connection::open(path)?;
    let tables: i64 = c.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name='inbox'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(tables, 1, "rejected bytes need a durable admission record");
    Ok(())
}

#[test]
fn retries_of_create_batch_and_notes_survive_restart_and_reject_changed_intent() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let request = Request::batch([
        create(1),
        C::InsertNotes {
            id: id(1),
            index: 0,
            text: "once".into(),
        },
    ]);
    let encoded = serde_json::to_string(&request)?;
    let mut r = Replica::open(&path)?;
    assert!(r.execute_with_fault(&request, Fault::AfterCommit).is_err());
    let expected = r.outbox()?[0].clone();
    drop(r);
    let mut r = Replica::open(path)?;
    let request: Request = serde_json::from_str(&encoded)?;
    assert_eq!(r.execute(&request)?, expected);
    assert_eq!(r.execute(&request)?, expected);
    assert_eq!(r.journal_len()?, 1);
    assert_eq!(r.projection()?.entities[&id(1)].notes, "once");
    let mut changed = request.clone();
    changed.commands.push(C::Rename {
        id: id(1),
        title: "different".into(),
    });
    assert!(r.execute(&changed).is_err());
    // Identical commands with a new request ID represent another deliberate edit.
    r.execute(&Request::new(C::InsertNotes {
        id: id(1),
        index: 0,
        text: "once".into(),
    }))?;
    assert_eq!(r.projection()?.entities[&id(1)].notes, "onceonce");
    Ok(())
}

#[test]
fn retry_after_rollback_commits_once() -> Result<()> {
    let mut r = Replica::open_in_memory()?;
    let request = Request::new(create(1));
    assert!(r.execute_with_fault(&request, Fault::BeforeCommit).is_err());
    assert_eq!(r.journal_len()?, 0);
    let saved = r.execute(&request)?;
    assert_eq!(r.execute(&request)?, saved);
    assert_eq!(r.journal_len()?, 1);
    Ok(())
}

#[test]
fn reorder_slots_exclude_purged_prefix_middle_and_suffix() -> Result<()> {
    for deleted in [1, 3, 5] {
        let visible: Vec<_> = (1..=5).filter(|n| *n != deleted).collect();
        for moved in &visible {
            for position in 0..visible.len() {
                let mut r = Replica::open_in_memory()?;
                r.execute(&Request::batch((1..=5).map(create)))?;
                r.execute(&Request::batch([
                    C::Trash {
                        id: id(deleted),
                        trashed: true,
                    },
                    C::PurgeTrash,
                ]))?;
                let mut expected: Vec<_> = visible
                    .iter()
                    .filter(|n| *n != moved)
                    .map(|n| id(*n))
                    .collect();
                expected.insert(position, id(*moved));
                r.execute(&Request::new(C::Move {
                    id: id(*moved),
                    parent: None,
                    index: position,
                }))?;
                r.execute(&Request::new(C::MoveToday {
                    id: id(*moved),
                    index: position,
                }))?;
                assert_eq!(home(&r)?, expected);
                assert_eq!(r.projection()?.today_order, expected);
                let before = r.journal_len()?;
                assert!(r
                    .execute(&Request::new(C::MoveToday {
                        id: id(*moved),
                        index: visible.len()
                    }))
                    .is_err());
                assert!(r
                    .execute(&Request::new(C::Move {
                        id: id(*moved),
                        parent: None,
                        index: visible.len()
                    }))
                    .is_err());
                assert_eq!(r.journal_len()?, before);
            }
        }
    }
    Ok(())
}

#[test]
fn home_reorders_children_lifted_through_a_deleted_heading() -> Result<()> {
    let mut r = Replica::open_in_memory()?;
    r.execute(&Request::batch([
        C::Create {
            id: id(10),
            kind: Kind::Project,
            parent: None,
            title: "project".into(),
        },
        C::Create {
            id: id(11),
            kind: Kind::Heading,
            parent: Some(id(10)),
            title: "heading".into(),
        },
        C::Create {
            id: id(1),
            kind: Kind::Task,
            parent: Some(id(11)),
            title: "nested 1".into(),
        },
        C::Create {
            id: id(2),
            kind: Kind::Task,
            parent: Some(id(11)),
            title: "nested 2".into(),
        },
        C::Create {
            id: id(3),
            kind: Kind::Task,
            parent: Some(id(10)),
            title: "direct".into(),
        },
        C::Delete { id: id(11) },
    ]))?;
    r.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(10)),
        index: 1,
    }))?;
    let state = r.projection()?;
    for n in 1..=3 {
        assert_eq!(state.entities[&id(n)].parent, Some(id(10)));
    }
    assert_eq!(state.entities[&id(1)].rank, 0);
    assert_eq!(state.entities[&id(3)].rank, 1);
    assert_eq!(state.entities[&id(2)].rank, 2);
    Ok(())
}

#[test]
fn tag_removal_preserves_other_names_tasks_and_unseen_concurrent_links() -> Result<()> {
    let mut a = Replica::open_in_memory()?;
    let mut b = Replica::open_in_memory()?;
    let seed = a.execute(&Request::batch([
        create(1),
        create(2),
        C::Tag {
            id: id(10),
            name: "work".into(),
        },
        C::Tag {
            id: id(11),
            name: "work".into(),
        },
        C::Tag {
            id: id(12),
            name: "Work".into(),
        },
        C::TagMembership {
            task: id(1),
            tag: id(10),
            present: true,
        },
        C::TagMembership {
            task: id(1),
            tag: id(11),
            present: true,
        },
        C::TagMembership {
            task: id(1),
            tag: id(12),
            present: true,
        },
        C::TagMembership {
            task: id(2),
            tag: id(11),
            present: true,
        },
    ]))?;
    b.receive(&seed)?;
    let unseen = b.execute(&Request::batch([
        C::Tag {
            id: id(13),
            name: "work".into(),
        },
        C::TagMembership {
            task: id(1),
            tag: id(13),
            present: true,
        },
    ]))?;
    let remove = a.execute(&Request::new(C::TagMembership {
        task: id(1),
        tag: id(10),
        present: false,
    }))?;
    let state = a.projection()?;
    assert_eq!(state.memberships, [(id(1), id(12)), (id(2), id(10))].into());
    a.receive(&unseen)?;
    b.receive(&remove)?;
    assert!(a.projection()?.memberships.contains(&(id(1), id(10))));
    assert_eq!(a.projection()?, b.projection()?);
    Ok(())
}

#[test]
fn snapshot_loads_prefix_pending_bytes_and_journal_tail() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let mut sender = Replica::open_in_memory()?;
    let first = sender.execute(&Request::new(create(1)))?;
    let second = sender.execute(&Request::new(C::Rename {
        id: id(1),
        title: "resolved".into(),
    }))?;
    let mut r = Replica::open(&path)?;
    r.execute(&Request::new(create(2)))?;
    assert_eq!(r.receive(&second)?, Admission::Pending);
    r.checkpoint()?;
    r.execute(&Request::new(create(3)))?;
    drop(r);
    let mut r = Replica::open(path)?;
    assert!(r.restore_stats().snapshot_loaded);
    assert_eq!(r.restore_stats().replayed_records, 1);
    assert_eq!(r.inbox()?[0].status, Admission::Pending);
    assert_eq!(r.receive(&first)?, Admission::Applied);
    assert_eq!(r.projection()?.entities[&id(1)].title, "resolved");
    assert_eq!(r.inbox()?[0].status, Admission::Applied);
    assert_eq!(r.outbox()?.len(), 2);
    assert_eq!(r.journal_len()?, 4);
    Ok(())
}

#[test]
fn quarantine_and_dependency_wait_are_durable_and_observable() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let mut sender = Replica::open_in_memory()?;
    let mut first = sender.execute(&Request::new(create(1)))?;
    let second = sender.execute(&Request::new(C::Rename {
        id: id(1),
        title: "resolved".into(),
    }))?;
    let bad = Record {
        id: first.id.clone(),
        bytes: vec![0, 1, 2],
    };
    let mut r = Replica::open(&path)?;
    assert!(r.receive(&bad).is_err());
    assert!(r.receive(&bad).is_err());
    assert_eq!(r.receive(&second)?, Admission::Pending);
    r.checkpoint()?;
    drop(r);
    let mut r = Replica::open(path)?;
    let inbox = r.inbox()?;
    assert_eq!(inbox.len(), 2);
    assert_eq!(inbox[0].status, Admission::Quarantined);
    assert_eq!(inbox[0].record, bad);
    assert!(!inbox[0].reason.is_empty());
    assert_eq!(inbox[1].status, Admission::Pending);
    assert!(!inbox[1].reason.is_empty());
    assert!(r.projection()?.entities.is_empty());
    // An explicitly redelivered valid dependency uses a new transport identity.
    first.id = id(999);
    assert_eq!(r.receive(&first)?, Admission::Applied);
    assert_eq!(r.inbox()?[1].status, Admission::Applied);
    assert_eq!(r.projection()?.entities[&id(1)].title, "resolved");
    Ok(())
}

#[test]
fn invalid_domain_import_is_quarantined_without_changing_valid_state() -> Result<()> {
    let mut sender = Replica::open_in_memory()?;
    let record = sender.execute(&Request::new(create(1)))?;
    let mut r = Replica::open_in_memory()?;
    r.receive(&record)?;
    let before = r.projection()?;
    let doc = loro::LoroDoc::new();
    doc.import(&record.bytes)?;
    let version = doc.oplog_vv();
    let tree = doc.get_tree("home");
    let node = tree.nodes()[0];
    tree.get_meta(node)?
        .insert("kind", serde_json::to_string(&Kind::Heading)?)?;
    doc.commit();
    let bad = Record {
        id: id(999),
        bytes: doc.export(loro::ExportMode::updates(&version))?,
    };
    assert!(r.receive(&bad).is_err());
    assert_eq!(r.projection()?, before);
    assert_eq!(r.inbox()?.last().unwrap().record, bad);
    assert_eq!(r.inbox()?.last().unwrap().status, Admission::Quarantined);
    Ok(())
}

#[test]
fn received_bytes_resume_after_sql_admission_failure() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let mut r = Replica::open(&path)?;
    let mut sender = Replica::open_in_memory()?;
    let record = sender.execute(&Request::new(create(1)))?;
    let c = rusqlite::Connection::open(&path)?;
    c.execute_batch(
        "CREATE TRIGGER deny_write BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT,'test'); END;",
    )?;
    assert!(r.receive(&record).is_err());
    assert_eq!(r.inbox()?[0].status, Admission::Received);
    c.execute_batch("DROP TRIGGER deny_write")?;
    drop(c);
    drop(r);
    let r = Replica::open(path)?;
    assert_eq!(r.inbox()?[0].status, Admission::Applied);
    assert_eq!(r.journal_len()?, 1);
    assert!(r.outbox()?.is_empty());
    Ok(())
}

#[test]
fn normal_and_fixture_ordering_policies_are_separate() -> Result<()> {
    let mut normal = Replica::open_in_memory()?;
    assert_eq!(normal.ordering_jitter_bytes(), 8);
    normal.execute(&Request::batch([create(1), create(2)]))?;
    assert_eq!(normal.ordering_jitter_bytes(), 8);
    let mut fixture = Replica::open_in_memory()?;
    fixture.set_test_peer(10)?;
    fixture.execute(&Request::batch([create(1), create(2)]))?;
    assert_eq!(fixture.ordering_jitter_bytes(), 0);
    Ok(())
}

#[test]
fn quarantine_rejects_changed_bytes_for_an_existing_transport_id() -> Result<()> {
    let mut r = Replica::open_in_memory()?;
    let mut sender = Replica::open_in_memory()?;
    let record = sender.execute(&Request::new(create(1)))?;
    assert!(r
        .receive(&Record {
            id: record.id.clone(),
            bytes: vec![0, 1, 2]
        })
        .is_err());
    assert!(r.receive(&record).is_err());
    assert_eq!(r.inbox()?.len(), 2);
    assert!(r
        .inbox()?
        .iter()
        .all(|e| e.status == Admission::Quarantined));
    assert_eq!(r.journal_len()?, 0);
    Ok(())
}

#[test]
fn legacy_checkpoint_migrates_without_losing_its_pending_imports() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("r.db");
    let mut sender = Replica::open_in_memory()?;
    let first = sender.execute(&Request::new(create(1)))?;
    let second = sender.execute(&Request::new(C::Rename {
        id: id(1),
        title: "resolved".into(),
    }))?;
    let mut r = Replica::open(&path)?;
    r.receive(&second)?;
    r.checkpoint()?;
    drop(r);
    let c = rusqlite::Connection::open(&path)?;
    c.execute_batch("DROP TABLE inbox; ALTER TABLE journal DROP COLUMN request_body; ALTER TABLE checkpoint DROP COLUMN pending; ALTER TABLE checkpoint DROP COLUMN through_seq; UPDATE prototype_meta SET value='matters-sync-prototype/0' WHERE key='format';")?;
    drop(c);
    let mut r = Replica::open(path)?;
    assert!(r.restore_stats().snapshot_loaded);
    assert_eq!(r.restore_stats().replayed_records, 1);
    assert_eq!(r.inbox()?[0].status, Admission::Pending);
    r.receive(&first)?;
    assert_eq!(r.projection()?.entities[&id(1)].title, "resolved");
    Ok(())
}
