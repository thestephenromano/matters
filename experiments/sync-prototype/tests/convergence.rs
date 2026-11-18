use matters_sync_prototype::Request;
use matters_sync_prototype::{Command as C, Fault, Kind, Record, Replica, Result, Schedule};
use tempfile::TempDir;

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}
fn create(n: u128, kind: Kind, parent: Option<u128>) -> C {
    C::Create {
        id: id(n),
        kind,
        parent: parent.map(id),
        title: format!("entity {n}"),
    }
}
fn rename(n: u128, title: &str) -> C {
    C::Rename {
        id: id(n),
        title: title.into(),
    }
}

struct Pair {
    _dir: TempDir,
    a: Replica,
    b: Replica,
}
impl Pair {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let mut seed = Replica::open(dir.path().join("seed.db"))?;
        seed.set_test_peer(1)?;
        for c in [
            create(1, Kind::Project, None),
            create(2, Kind::Project, None),
            create(3, Kind::Task, Some(1)),
            create(4, Kind::Task, Some(1)),
        ] {
            seed.execute(&Request::new(c))?;
        }
        let mut a = Replica::open(dir.path().join("a.db"))?;
        let mut b = Replica::open(dir.path().join("b.db"))?;
        for r in seed.outbox()? {
            a.receive(&r)?;
            b.receive(&r)?;
        }
        a.set_test_peer(10)?;
        b.set_test_peer(20)?;
        Ok(Self { _dir: dir, a, b })
    }
    fn exchange(&mut self, reverse: bool) -> Result<()> {
        let mut a = self.a.outbox()?;
        let mut b = self.b.outbox()?;
        if reverse {
            a.reverse();
            b.reverse();
        }
        for r in a {
            self.b.receive(&r)?;
            self.b.receive(&r)?;
        }
        for r in b {
            self.a.receive(&r)?;
            self.a.receive(&r)?;
        }
        assert_eq!(self.a.projection()?, self.b.projection()?);
        assert_eq!(self.a.frontier(), self.b.frontier());
        Ok(())
    }
}

#[test]
fn c01_independent_fields_survive_reversed_duplicate_delivery() -> Result<()> {
    for reverse in [false, true] {
        let mut p = Pair::new()?;
        p.a.execute(&Request::new(rename(3, "renamed")))?;
        p.b.execute(&Request::new(C::Deadline {
            id: id(3),
            date: Some("2026-12-01".into()),
        }))?;
        p.exchange(reverse)?;
        let state = p.a.projection()?;
        assert_eq!(state.entities[&id(3)].title, "renamed");
        assert_eq!(
            state.entities[&id(3)].deadline.as_deref(),
            Some("2026-12-01")
        );
    }
    Ok(())
}

#[test]
fn c02_text_inserts_preserve_both_intents() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::InsertNotes {
        id: id(3),
        index: 0,
        text: "ab".into(),
    }))?;
    p.exchange(false)?;
    p.a.execute(&Request::new(C::InsertNotes {
        id: id(3),
        index: 0,
        text: "X".into(),
    }))?;
    p.b.execute(&Request::new(C::InsertNotes {
        id: id(3),
        index: 2,
        text: "Y".into(),
    }))?;
    p.exchange(true)?;
    assert_eq!(p.a.projection()?.entities[&id(3)].notes, "XabY");
    Ok(())
}

#[test]
fn c03_completion_is_one_register_with_a_fixed_peer_tie_break() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Complete {
        id: id(3),
        at: Some(1),
    }))?;
    p.exchange(false)?;
    p.a.execute(&Request::new(C::Complete {
        id: id(3),
        at: Some(999999),
    }))?;
    p.b.execute(&Request::new(C::Complete {
        id: id(3),
        at: None,
    }))?;
    p.exchange(true)?;
    assert_eq!(
        p.a.projection()?.entities[&id(3)].completed_at,
        None,
        "higher peer wins equal Lamport time; wall time irrelevant"
    );
    p.a.execute(&Request::new(C::Complete {
        id: id(3),
        at: Some(2),
    }))?;
    p.exchange(false)?;
    assert_eq!(
        p.b.projection()?.entities[&id(3)].completed_at,
        Some(2),
        "causally later edit wins"
    );
    Ok(())
}

#[test]
fn c04_schedule_is_atomic() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Schedule {
        id: id(3),
        schedule: Schedule::Someday,
    }))?;
    let scheduled = Schedule::Date {
        date: "2026-12-01".into(),
        evening: true,
    };
    p.b.execute(&Request::new(C::Schedule {
        id: id(3),
        schedule: scheduled.clone(),
    }))?;
    p.exchange(true)?;
    assert_eq!(p.a.projection()?.entities[&id(3)].schedule, scheduled);
    Ok(())
}

#[test]
fn c05_move_and_reorder_have_one_location() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(2)),
        index: 0,
    }))?;
    p.b.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(1)),
        index: 1,
    }))?;
    p.exchange(true)?;
    let state = p.a.projection()?;
    assert_eq!(state.entities[&id(3)].parent, Some(id(1)));
    assert_eq!(state.entities[&id(3)].rank, 1);
    p.a.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(2)),
        index: 0,
    }))?;
    p.exchange(false)?;
    assert_eq!(p.a.projection()?.entities[&id(3)].parent, Some(id(2)));
    Ok(())
}

#[test]
fn c06_project_trash_keeps_concurrent_child_edit_for_restore() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: true,
    }))?;
    p.b.execute(&Request::new(rename(3, "edit while offline")))?;
    p.exchange(true)?;
    assert!(p.a.projection()?.entities[&id(3)].effective_trash);
    p.a.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: false,
    }))?;
    p.exchange(false)?;
    let state = p.a.projection()?;
    assert!(!state.entities[&id(3)].effective_trash);
    assert_eq!(state.entities[&id(3)].title, "edit while offline");
    Ok(())
}

#[test]
fn c07_move_out_of_trashed_project_is_visible() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: true,
    }))?;
    p.b.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(2)),
        index: 0,
    }))?;
    p.exchange(true)?;
    assert!(!p.a.projection()?.entities[&id(3)].effective_trash);
    Ok(())
}

#[test]
fn c08_restore_parent_keeps_independent_child_trash() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: true,
    }))?;
    p.exchange(false)?;
    p.a.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: false,
    }))?;
    p.b.execute(&Request::new(C::Trash {
        id: id(3),
        trashed: true,
    }))?;
    p.exchange(true)?;
    assert!(p.a.projection()?.entities[&id(3)].effective_trash);
    assert!(!p.a.projection()?.entities[&id(1)].effective_trash);
    Ok(())
}

#[test]
fn c09_purge_wins_over_stale_edit_restore_and_move() -> Result<()> {
    for command in [
        rename(3, "stale"),
        C::Trash {
            id: id(3),
            trashed: false,
        },
        C::Move {
            id: id(3),
            parent: Some(id(2)),
            index: 0,
        },
    ] {
        let mut p = Pair::new()?;
        p.a.execute(&Request::new(C::Trash {
            id: id(3),
            trashed: true,
        }))?;
        p.exchange(false)?;
        p.a.execute(&Request::new(C::PurgeTrash))?;
        p.b.execute(&Request::new(command))?;
        p.exchange(true)?;
        let state = p.a.projection()?;
        assert!(!state.entities.contains_key(&id(3)));
        assert!(state.tombstones.contains(&id(3)));
        assert!(p
            .a
            .execute(&Request::new(create(3, Kind::Task, None)))
            .is_err());
    }
    Ok(())
}

#[test]
fn c10_unknown_children_survive_purge_in_inbox() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Trash {
        id: id(1),
        trashed: true,
    }))?;
    p.exchange(false)?;
    p.a.execute(&Request::new(C::PurgeTrash))?;
    p.b.execute(&Request::new(create(5, Kind::Task, Some(1))))?;
    p.b.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(2)),
        index: 0,
    }))?;
    p.exchange(true)?;
    let state = p.a.projection()?;
    assert_eq!(state.entities[&id(5)].parent, None);
    assert!(!state.entities[&id(5)].effective_trash);
    assert!(!state.entities.contains_key(&id(3)));
    Ok(())
}

#[test]
fn c11_deleted_heading_preserves_children_in_project() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(create(5, Kind::Heading, Some(1))))?;
    p.a.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(5)),
        index: 0,
    }))?;
    p.exchange(false)?;
    p.a.execute(&Request::new(C::Delete { id: id(5) }))?;
    p.b.execute(&Request::new(rename(3, "kept")))?;
    p.exchange(true)?;
    let state = p.a.projection()?;
    assert_eq!(state.entities[&id(3)].parent, Some(id(1)));
    assert_eq!(state.entities[&id(3)].title, "kept");
    Ok(())
}

#[test]
fn c12_duplicate_tag_names_keep_original_memberships_on_rename() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::Tag {
        id: id(10),
        name: "work".into(),
    }))?;
    p.a.execute(&Request::new(C::TagMembership {
        task: id(3),
        tag: id(10),
        present: true,
    }))?;
    p.b.execute(&Request::new(C::Tag {
        id: id(11),
        name: "work".into(),
    }))?;
    p.b.execute(&Request::new(C::TagMembership {
        task: id(4),
        tag: id(11),
        present: true,
    }))?;
    p.exchange(true)?;
    let state = p.a.projection()?;
    assert_eq!(state.tags.len(), 1);
    assert!(state.memberships.contains(&(id(4), id(10))));
    p.b.execute(&Request::new(C::Tag {
        id: id(11),
        name: "Work".into(),
    }))?;
    p.exchange(false)?;
    let state = p.a.projection()?;
    assert_eq!(state.tags.len(), 2);
    assert!(state.memberships.contains(&(id(4), id(11))));
    Ok(())
}

#[test]
fn c13_check_is_idempotent_and_delete_is_permanent() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(create(5, Kind::Checklist, Some(3))))?;
    p.exchange(false)?;
    for replica in [&mut p.a, &mut p.b] {
        replica.execute(&Request::new(C::Check {
            id: id(5),
            checked: true,
        }))?;
    }
    p.exchange(true)?;
    assert!(p.a.projection()?.entities[&id(5)].checked);
    p.a.execute(&Request::new(C::Delete { id: id(5) }))?;
    p.b.execute(&Request::new(C::Check {
        id: id(5),
        checked: false,
    }))?;
    p.exchange(true)?;
    assert!(!p.a.projection()?.entities.contains_key(&id(5)));
    Ok(())
}

#[test]
fn c14_pending_import_survives_snapshot_restart_and_local_write() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut sender = Replica::open(dir.path().join("sender.db"))?;
    let first = sender.execute(&Request::new(create(1, Kind::Task, None)))?;
    let second = sender.execute(&Request::new(rename(1, "after dependency")))?;
    let path = dir.path().join("receiver.db");
    let mut receiver = Replica::open(&path)?;
    receiver.receive(&second)?;
    receiver.checkpoint()?;
    drop(receiver);
    let mut receiver = Replica::open(&path)?;
    assert!(receiver.projection()?.entities.is_empty());
    receiver.execute(&Request::new(create(2, Kind::Task, None)))?;
    receiver.receive(&first)?;
    assert_eq!(
        receiver.projection()?.entities[&id(1)].title,
        "after dependency"
    );
    assert_eq!(
        receiver.outbox()?.len(),
        1,
        "imports must not echo to outbox"
    );
    Ok(())
}

#[test]
fn c15_clone_opens_with_new_actor_and_preserves_old_outbox() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("a.db");
    let copy = dir.path().join("b.db");
    let mut original = Replica::open(&path)?;
    original.execute(&Request::new(create(1, Kind::Task, None)))?;
    let old_peer = original.peer();
    let old_outbox = original.outbox()?;
    drop(original);
    std::fs::copy(&path, &copy)?;
    let mut a = Replica::open(&path)?;
    let mut b = Replica::open(&copy)?;
    assert_ne!(a.peer(), old_peer);
    assert_ne!(a.peer(), b.peer());
    assert_eq!(a.outbox()?, old_outbox);
    let ar = a.execute(&Request::new(rename(1, "from a")))?;
    let br = b.execute(&Request::new(C::Deadline {
        id: id(1),
        date: Some("2026-12-02".into()),
    }))?;
    a.receive(&br)?;
    b.receive(&ar)?;
    assert_eq!(a.projection()?, b.projection()?);
    assert_eq!(a.projection()?.entities[&id(1)].title, "from a");
    Ok(())
}

#[test]
fn c16_invalid_commands_leave_journal_and_projection_unchanged() -> Result<()> {
    let mut p = Pair::new()?;
    let before = p.a.projection()?;
    let count = p.a.journal_len()?;
    for c in [
        create(3, Kind::Task, None),
        create(9, Kind::Project, Some(3)),
        C::Move {
            id: id(1),
            parent: Some(id(3)),
            index: 0,
        },
    ] {
        assert!(p.a.execute(&Request::new(c)).is_err());
        assert_eq!(p.a.projection()?, before);
        assert_eq!(p.a.journal_len()?, count);
    }
    let bad = Record {
        id: id(99),
        bytes: vec![0, 1, 2],
    };
    assert!(p.a.receive(&bad).is_err());
    assert_eq!(p.a.projection()?, before);
    Ok(())
}

#[test]
fn c17_today_order_is_independent_of_home_placement() -> Result<()> {
    let mut p = Pair::new()?;
    p.a.execute(&Request::new(C::MoveToday {
        id: id(4),
        index: 0,
    }))?;
    p.b.execute(&Request::new(C::Move {
        id: id(3),
        parent: Some(id(2)),
        index: 0,
    }))?;
    p.exchange(true)?;
    let state = p.a.projection()?;
    assert_eq!(state.today_order, vec![id(4), id(3)]);
    assert_eq!(state.entities[&id(3)].parent, Some(id(2)));
    Ok(())
}

#[test]
fn failed_commit_does_not_advance_memory_disk_or_outbox() -> Result<()> {
    let mut p = Pair::new()?;
    let before = p.a.projection()?;
    let frontier = p.a.frontier();
    let journal = p.a.journal_len()?;
    assert!(p
        .a
        .execute_with_fault(&Request::new(rename(3, "rollback")), Fault::BeforeCommit)
        .is_err());
    assert_eq!(p.a.projection()?, before);
    assert_eq!(p.a.frontier(), frontier);
    assert_eq!(p.a.journal_len()?, journal);
    p.a.execute(&Request::new(rename(4, "later")))?;
    assert_eq!(p.a.projection()?.entities[&id(3)].title, "entity 3");
    Ok(())
}

#[test]
fn lost_ack_after_commit_survives_restart_and_projection_rebuild() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("replica.db");
    let mut r = Replica::open(&path)?;
    r.execute(&Request::new(create(1, Kind::Task, None)))?;
    assert!(r
        .execute_with_fault(&Request::new(rename(1, "durable")), Fault::AfterCommit)
        .is_err());
    let expected = r.projection()?;
    drop(r);
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute("DELETE FROM projection", [])?;
    drop(conn);
    let r = Replica::open(&path)?;
    assert_eq!(r.projection()?, expected);
    assert_eq!(r.outbox()?.len(), 2);
    Ok(())
}

#[test]
fn record_identity_rejects_conflicting_bytes() -> Result<()> {
    let mut p = Pair::new()?;
    let first = p.a.execute(&Request::new(rename(3, "one")))?;
    p.b.receive(&first)?;
    let before = p.b.journal_len()?;
    let mut second = p.a.execute(&Request::new(rename(3, "two")))?;
    second.id = first.id;
    assert!(p.b.receive(&second).is_err());
    assert_eq!(p.b.journal_len()?, before);
    assert_eq!(p.b.projection()?.entities[&id(3)].title, "one");
    Ok(())
}

fn copies_of(state: &matters_sync_prototype::Projection, original: &str) -> Vec<String> {
    state
        .entities
        .values()
        .filter(|e| e.identity_conflict.as_deref() == Some(original))
        .map(|e| e.id.clone())
        .collect()
}

#[test]
fn c18_concurrent_creates_under_one_uuid_keep_an_original_and_a_conflict_copy() -> Result<()> {
    for reverse in [false, true] {
        let mut p = Pair::new()?;
        p.a.execute(&Request::new(create(9, Kind::Project, None)))?;
        p.a.execute(&Request::new(create(10, Kind::Task, Some(9))))?;
        p.b.execute(&Request::new(create(9, Kind::Project, None)))?;
        p.b.execute(&Request::new(rename(9, "b's project")))?;
        p.b.execute(&Request::new(create(11, Kind::Task, Some(9))))?;
        p.exchange(reverse)?;
        let state = p.a.projection()?;
        // The smaller tree ID (peer 10) keeps the UUID; B's node is a copy
        // with its own title and child, so neither writer's edits vanish.
        let copies = copies_of(&state, &id(9));
        assert_eq!(copies.len(), 1);
        let copy = &state.entities[&copies[0]];
        assert_eq!(state.entities[&id(9)].title, "entity 9");
        assert_eq!(copy.title, "b's project");
        assert_eq!(copy.kind, Kind::Project);
        assert_eq!(state.entities[&id(10)].parent, Some(id(9)));
        assert_eq!(state.entities[&id(11)].parent, Some(copy.id.clone()));
        assert!(state
            .repairs
            .contains(&format!("identity-copy:{}", copy.id)));
        // The copy is an ordinary entity under its derived identity.
        p.b.execute(&Request::new(C::Rename {
            id: copy.id.clone(),
            title: "kept".into(),
        }))?;
        p.b.execute(&Request::new(C::Trash {
            id: copy.id.clone(),
            trashed: true,
        }))?;
        p.exchange(!reverse)?;
        let state = p.a.projection()?;
        assert_eq!(state.entities[&copies[0]].title, "kept");
        assert!(state.entities[&copies[0]].explicit_trash);
        // The UUID stays taken locally.
        assert!(p
            .a
            .execute(&Request::new(create(9, Kind::Task, None)))
            .is_err());
    }
    Ok(())
}

#[test]
fn c19_today_keeps_the_first_task_reference_and_skips_non_tasks() -> Result<()> {
    for reverse in [false, true] {
        let mut p = Pair::new()?;
        p.a.execute(&Request::new(create(9, Kind::Task, None)))?;
        p.b.execute(&Request::new(create(9, Kind::Task, None)))?;
        p.a.execute(&Request::new(create(12, Kind::Project, None)))?;
        p.b.execute(&Request::new(create(12, Kind::Task, None)))?;
        p.exchange(reverse)?;
        let state = p.a.projection()?;
        assert_eq!(state.today_order.iter().filter(|t| **t == id(9)).count(), 1);
        assert!(state
            .repairs
            .contains(&format!("today-duplicate:{}", id(9))));
        // A's project keeps UUID 12, so B's Today reference names a project.
        assert_eq!(state.entities[&id(12)].kind, Kind::Project);
        assert!(!state.today_order.contains(&id(12)));
        assert!(state
            .repairs
            .contains(&format!("today-non-task:{}", id(12))));
    }
    Ok(())
}

#[test]
fn c20_derived_identities_are_reserved() -> Result<()> {
    let mut r = Replica::open_in_memory()?;
    let reserved = "00000000-0000-8000-8000-000000000001";
    assert!(r
        .execute(&Request::new(C::Create {
            id: reserved.into(),
            kind: Kind::Task,
            parent: None,
            title: "claim".into(),
        }))
        .is_err());
    assert!(r.projection()?.entities.is_empty());
    Ok(())
}

/// Seeded concurrent histories with UUIDs drawn from a small pool, so
/// collisions are common. Projection must always succeed, agree across
/// delivery orders, and create exactly one copy per extra node per UUID.
#[test]
fn c21_merged_valid_histories_always_project_and_agree() -> Result<()> {
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }
    const KINDS: [Kind; 5] = [
        Kind::Area,
        Kind::Project,
        Kind::Heading,
        Kind::Task,
        Kind::Checklist,
    ];
    let mut collisions = 0;
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut replicas = Vec::new();
        for peer in 0..3 {
            let mut r = Replica::open_in_memory()?;
            r.set_test_peer(10 + peer)?;
            replicas.push(r);
        }
        let pick = |rng: &mut Rng, pool: usize| id(1 + rng.below(pool) as u128);
        for _ in 0..80 {
            let r = rng.below(3);
            let command = match rng.below(9) {
                0..=2 => C::Create {
                    id: pick(&mut rng, 6),
                    kind: KINDS[rng.below(5)],
                    parent: (rng.below(3) != 0).then(|| pick(&mut rng, 6)),
                    title: "t".into(),
                },
                3 => C::Move {
                    id: pick(&mut rng, 6),
                    parent: (rng.below(3) != 0).then(|| pick(&mut rng, 6)),
                    index: 0,
                },
                4 => C::MoveToday {
                    id: pick(&mut rng, 6),
                    index: 0,
                },
                5 => C::Trash {
                    id: pick(&mut rng, 6),
                    trashed: rng.below(2) == 0,
                },
                6 if rng.below(2) == 0 => C::PurgeTrash,
                6 => C::Delete {
                    id: pick(&mut rng, 6),
                },
                7 => C::Rename {
                    id: pick(&mut rng, 6),
                    title: format!("{seed}"),
                },
                _ => {
                    let from = rng.below(3);
                    if from != r {
                        for record in replicas[from].outbox()? {
                            let _ = replicas[r].receive(&record);
                        }
                    }
                    continue;
                }
            };
            let _ = replicas[r].execute(&Request::new(command));
        }
        let mut records = Vec::new();
        for r in &replicas {
            records.extend(r.outbox()?.into_iter().map(|record| record.bytes));
        }
        let mut views = Vec::new();
        for reverse in [false, true] {
            let doc = loro::LoroDoc::new();
            let mut ordered = records.clone();
            if reverse {
                ordered.reverse();
            }
            for bytes in &ordered {
                doc.import(bytes)?;
            }
            let view = matters_sync_prototype::project_document(&doc)?;
            let tree = doc.get_tree("home");
            let mut per_uuid = std::collections::BTreeMap::<String, usize>::new();
            for node in tree.nodes() {
                let meta = tree.get_meta(node)?.get_deep_value();
                let loro::LoroValue::Map(meta) = meta else {
                    panic!("metadata map")
                };
                let Some(loro::LoroValue::String(uuid)) = meta.get("id") else {
                    panic!("metadata id")
                };
                *per_uuid.entry(uuid.to_string()).or_default() += 1;
            }
            let extra: usize = per_uuid.values().map(|n| n - 1).sum();
            let copies = view
                .repairs
                .iter()
                .filter(|r| r.starts_with("identity-copy:"))
                .count();
            assert_eq!(copies, extra, "seed {seed}");
            collisions += extra;
            views.push(view);
        }
        assert_eq!(views[0], views[1], "seed {seed}");
    }
    assert!(collisions > 0, "the pool must force collisions");
    Ok(())
}
