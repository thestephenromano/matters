//! The engine document (#35): typed commands, concurrent merges and the
//! rows read back. Case names follow ADR 0001's concurrency fixtures.

use matters_core::crdt::{Anchor, Command, Document, Kind, Snapshot};
use matters_core::{MattersError, Status};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

const AREA: u128 = 1;
const PROJECT: u128 = 2;
const HEADING: u128 = 3;
const TASK: u128 = 4;
const OTHER: u128 = 5;
const ITEM: u128 = 6;

fn create(n: u128, kind: Kind, parent: Option<u128>, title: &str) -> Command {
    Command::Create {
        id: id(n),
        kind,
        parent: parent.map(id),
        at: Anchor::Last,
        title: title.into(),
        created_at: 1,
    }
}

/// A shared genesis: area, project in it with a heading, a task under the
/// heading with a checklist item, and a loose task in the Inbox. Replica A
/// edits as peer 1, B as peer 2.
fn pair() -> (Document, Document) {
    let mut a = Document::new(1).unwrap();
    a.apply(
        &[
            create(AREA, Kind::Area, None, "Home"),
            create(PROJECT, Kind::Project, Some(AREA), "Garden"),
            create(HEADING, Kind::Heading, Some(PROJECT), "Spring"),
            create(TASK, Kind::Task, Some(HEADING), "Dig"),
            create(ITEM, Kind::ChecklistItem, Some(TASK), "Spade"),
            create(OTHER, Kind::Task, None, "Call"),
        ],
        10,
    )
    .unwrap();
    let b = Document::import(&a.snapshot().unwrap(), 2).unwrap();
    (a, b)
}

/// Exchange everything both ways, twice (duplicates are harmless).
fn sync(a: &mut Document, b: &mut Document) {
    for _ in 0..2 {
        let (va, vb) = (a.version(), b.version());
        let to_b = a.updates_since(&vb).unwrap();
        let to_a = b.updates_since(&va).unwrap();
        a.merge(&to_a).unwrap();
        b.merge(&to_b).unwrap();
    }
}

fn converged(a: &mut Document, b: &mut Document) -> Snapshot {
    sync(a, b);
    let (sa, sb) = (a.project().unwrap(), b.project().unwrap());
    assert_eq!(sa, sb, "replicas agree");
    sa
}

fn task(s: &Snapshot, n: u128) -> &matters_core::Task {
    s.tasks.iter().find(|t| t.id == id(n)).expect("task")
}

#[test]
fn rows_follow_the_tree() {
    let (a, _) = pair();
    let s = a.project().unwrap();
    assert_eq!(s.areas.len(), 1);
    assert_eq!(s.projects[0].area_id, Some(id(AREA)));
    assert_eq!(s.headings[0].project_id, id(PROJECT));
    let dig = task(&s, TASK);
    assert_eq!(
        (
            dig.area_id.clone(),
            dig.project_id.clone(),
            dig.heading_id.clone()
        ),
        (None, Some(id(PROJECT)), Some(id(HEADING)))
    );
    assert_eq!(s.checklist_items[0].task_id, id(TASK));
    let call = task(&s, OTHER);
    assert_eq!(
        (call.area_id.clone(), call.project_id.clone()),
        (None, None)
    );
    assert!(
        dig.today_order < call.today_order,
        "Today keeps creation order"
    );
    assert_eq!(dig.status, Status::Open);
    assert!(s.repairs.is_empty() && s.recovery.is_empty());
}

#[test]
fn c01_independent_fields_survive() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::Rename {
            id: id(TASK),
            title: "Dig deep".into(),
        }],
        20,
    )
    .unwrap();
    let due = jiff::civil::date(2026, 12, 1);
    b.apply(
        &[Command::SetDeadline {
            id: id(TASK),
            deadline: Some(due),
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(task(&s, TASK).title, "Dig deep");
    assert_eq!(task(&s, TASK).deadline, Some(due));
}

#[test]
fn c02_concurrent_note_edits_keep_both_intents() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetNotes {
            id: id(TASK),
            notes: "ab".into(),
        }],
        20,
    )
    .unwrap();
    sync(&mut a, &mut b);
    a.apply(
        &[Command::SetNotes {
            id: id(TASK),
            notes: "Xab".into(),
        }],
        30,
    )
    .unwrap();
    b.apply(
        &[Command::SetNotes {
            id: id(TASK),
            notes: "abY".into(),
        }],
        30,
    )
    .unwrap();
    assert_eq!(task(&converged(&mut a, &mut b), TASK).notes, "XabY");
}

#[test]
fn c03_status_is_one_register() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetStatus {
            id: id(TASK),
            status: Status::Completed,
            completed_at: Some(99),
            closed_by_project: false,
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[Command::SetStatus {
            id: id(TASK),
            status: Status::Open,
            completed_at: Some(5),
            closed_by_project: true,
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    let t = task(&s, TASK);
    match t.status {
        Status::Open => assert_eq!(t.completed_at, None, "an open task has no finish time"),
        _ => assert_eq!(t.completed_at, Some(99)),
    }
    assert!(
        !s.closed_by_project.contains(&id(TASK)),
        "only a closed task is marked"
    );
}

#[test]
fn c04_schedule_is_atomic() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetSchedule {
            id: id(OTHER),
            start_date: None,
            evening: false,
            someday: true,
        }],
        20,
    )
    .unwrap();
    let day = jiff::civil::date(2026, 11, 20);
    b.apply(
        &[Command::SetSchedule {
            id: id(OTHER),
            start_date: Some(day),
            evening: true,
            someday: false,
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    let t = task(&s, OTHER);
    let mixed = t.is_someday && (t.start_date.is_some() || t.time_of_day.is_some());
    assert!(!mixed, "one complete When wins: {t:?}");
}

#[test]
fn c05_move_and_reorder_leave_one_location() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::Move {
            id: id(OTHER),
            parent: Some(id(AREA)),
            at: Anchor::First,
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[Command::Move {
            id: id(OTHER),
            parent: None,
            at: Anchor::First,
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(s.tasks.iter().filter(|t| t.id == id(OTHER)).count(), 1);
}

#[test]
fn c06_trashing_a_project_keeps_a_concurrent_child_edit() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetTrashed {
            id: id(PROJECT),
            trashed_at: Some(20),
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[Command::Rename {
            id: id(TASK),
            title: "edited".into(),
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(s.projects[0].trashed_at, Some(20));
    let t = task(&s, TASK);
    assert_eq!(
        (t.title.as_str(), t.trashed_at),
        ("edited", None),
        "trash is inherited"
    );
}

#[test]
fn c09_permanent_deletion_wins_over_stale_edits_and_moves() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::Purge {
            ids: vec![id(OTHER)],
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[
            Command::Rename {
                id: id(OTHER),
                title: "late".into(),
            },
            Command::Move {
                id: id(OTHER),
                parent: Some(id(AREA)),
                at: Anchor::Last,
            },
        ],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert!(s.tasks.iter().all(|t| t.id != id(OTHER)));
    assert!(s.tombstones.contains(&id(OTHER)));
    assert!(matches!(
        a.apply(
            &[Command::Rename {
                id: id(OTHER),
                title: "x".into()
            }],
            30
        ),
        Err(MattersError::Validation(_))
    ));
}

#[test]
fn c10_unknown_children_of_a_purged_project_survive() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::Purge {
            ids: vec![id(PROJECT), id(HEADING), id(TASK), id(ITEM)],
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[
            create(70, Kind::Task, Some(PROJECT), "new in project"),
            create(71, Kind::Heading, Some(PROJECT), "new heading"),
        ],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    let t = task(&s, 70);
    assert_eq!(
        t.area_id,
        Some(id(AREA)),
        "lifted to the nearest legal ancestor"
    );
    assert!(s.recovery.contains(&id(71)), "a heading needs a project");
    assert!(s.headings.iter().all(|h| h.id != id(71)));
}

#[test]
fn c11_deleted_heading_keeps_its_tasks_in_the_project_until_undone() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetRemoved {
            id: id(HEADING),
            removed: true,
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[Command::Rename {
            id: id(TASK),
            title: "kept".into(),
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert!(s.headings.is_empty());
    let t = task(&s, TASK);
    assert_eq!(
        (t.project_id.clone(), t.heading_id.clone(), t.title.as_str()),
        (Some(id(PROJECT)), None, "kept")
    );
    a.apply(
        &[Command::SetRemoved {
            id: id(HEADING),
            removed: false,
        }],
        30,
    )
    .unwrap();
    let s = a.project().unwrap();
    assert_eq!(task(&s, TASK).heading_id, Some(id(HEADING)));
}

#[test]
fn c17_today_order_is_independent_of_home_placement() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::PlaceInToday {
            id: id(OTHER),
            at: Anchor::First,
        }],
        20,
    )
    .unwrap();
    b.apply(
        &[Command::Move {
            id: id(TASK),
            parent: None,
            at: Anchor::First,
        }],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert!(task(&s, OTHER).today_order < task(&s, TASK).today_order);
    assert_eq!(task(&s, TASK).project_id, None, "the home move holds");
}

#[test]
fn c18_concurrent_creations_under_one_uuid_keep_a_conflict_copy() {
    let (mut a, mut b) = pair();
    a.apply(&[create(60, Kind::Task, None, "from A")], 20)
        .unwrap();
    b.apply(&[create(60, Kind::Task, None, "from B")], 20)
        .unwrap();
    let s = converged(&mut a, &mut b);
    let copies: Vec<_> = s
        .tasks
        .iter()
        .filter(|t| t.title.starts_with("from"))
        .collect();
    assert_eq!(copies.len(), 2, "no edit disappears");
    assert_eq!(copies.iter().filter(|t| t.id == id(60)).count(), 1);
    assert!(s.repairs.iter().any(|r| r.starts_with("identity-copy:")));
    // Both copies stay editable on both replicas.
    let copy = copies.iter().find(|t| t.id != id(60)).unwrap().id.clone();
    a.apply(
        &[Command::Rename {
            id: copy.clone(),
            title: "copy".into(),
        }],
        30,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert!(s.tasks.iter().any(|t| t.id == copy && t.title == "copy"));
}

#[test]
fn invalid_commands_are_refused() {
    let (mut a, _) = pair();
    let refused = [
        create(AREA, Kind::Area, None, "again"),
        create(50, Kind::Heading, None, "no project"),
        create(51, Kind::ChecklistItem, Some(AREA), "not in a task"),
        Command::Create {
            id: "not-a-uuid".into(),
            kind: Kind::Task,
            parent: None,
            at: Anchor::Last,
            title: "x".into(),
            created_at: 1,
        },
        Command::Rename {
            id: id(999),
            title: "x".into(),
        },
        Command::SetDone {
            id: id(TASK),
            done: true,
        },
        Command::SetNotes {
            id: id(HEADING),
            notes: "x".into(),
        },
        Command::Move {
            id: id(OTHER),
            parent: Some(id(ITEM)),
            at: Anchor::Last,
        },
        Command::Move {
            id: id(OTHER),
            parent: None,
            at: Anchor::After(id(OTHER)),
        },
        Command::Move {
            id: id(OTHER),
            parent: None,
            at: Anchor::After(id(TASK)),
        },
        Command::PlaceInToday {
            id: id(PROJECT),
            at: Anchor::First,
        },
    ];
    for command in refused {
        let mut b = Document::import(&a.snapshot().unwrap(), 3).unwrap();
        assert!(
            b.apply(std::slice::from_ref(&command), 20).is_err(),
            "{command:?}"
        );
    }
    // A conflict-copy identity can't be claimed by a new entity.
    let reserved = "00000000-0000-8000-8000-000000000000";
    assert!(a
        .apply(
            &[Command::Create {
                id: reserved.into(),
                kind: Kind::Task,
                parent: None,
                at: Anchor::Last,
                title: "x".into(),
                created_at: 1,
            }],
            20
        )
        .is_err());
}

#[test]
fn order_follows_anchors() {
    let (mut a, _) = pair();
    a.apply(
        &[
            create(20, Kind::Task, None, "b"),
            create(21, Kind::Task, None, "c"),
            Command::Move {
                id: id(21),
                parent: None,
                at: Anchor::After(id(OTHER)),
            },
            Command::Move {
                id: id(20),
                parent: None,
                at: Anchor::First,
            },
        ],
        20,
    )
    .unwrap();
    let s = a.project().unwrap();
    let mut inbox: Vec<_> = s
        .tasks
        .iter()
        .filter(|t| t.project_id.is_none() && t.area_id.is_none())
        .collect();
    inbox.sort_by(|x, y| x.sort_order.total_cmp(&y.sort_order));
    let titles: Vec<_> = inbox.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["b", "Call", "c"]);
}

#[test]
fn a_snapshot_round_trips() {
    let (mut a, _) = pair();
    a.apply(
        &[Command::SetNotes {
            id: id(TASK),
            notes: "notes".into(),
        }],
        20,
    )
    .unwrap();
    let copy = Document::import(&a.snapshot().unwrap(), 9).unwrap();
    assert_eq!(copy.project().unwrap(), a.project().unwrap());
}

fn today(s: &Snapshot) -> Vec<String> {
    let mut tasks: Vec<_> = s.tasks.iter().collect();
    tasks.sort_by(|x, y| x.today_order.total_cmp(&y.today_order));
    tasks.into_iter().map(|t| t.title.clone()).collect()
}

#[test]
fn today_order_follows_anchors() {
    let (mut a, _) = pair();
    a.apply(&[create(30, Kind::Task, None, "Third")], 20)
        .unwrap();
    assert_eq!(today(&a.project().unwrap()), ["Dig", "Call", "Third"]);
    a.apply(
        &[Command::PlaceInToday {
            id: id(30),
            at: Anchor::After(id(TASK)),
        }],
        20,
    )
    .unwrap();
    assert_eq!(today(&a.project().unwrap()), ["Dig", "Third", "Call"]);
    a.apply(
        &[Command::PlaceInToday {
            id: id(TASK),
            at: Anchor::Last,
        }],
        20,
    )
    .unwrap();
    assert_eq!(today(&a.project().unwrap()), ["Third", "Call", "Dig"]);
    a.apply(
        &[Command::PlaceInToday {
            id: id(TASK),
            at: Anchor::First,
        }],
        20,
    )
    .unwrap();
    assert_eq!(today(&a.project().unwrap()), ["Dig", "Third", "Call"]);
    for anchor in [Anchor::After(id(TASK)), Anchor::After(id(999))] {
        let mut b = Document::import(&a.snapshot().unwrap(), 3).unwrap();
        assert!(
            b.apply(
                &[Command::PlaceInToday {
                    id: id(TASK),
                    at: anchor.clone()
                }],
                20
            )
            .is_err(),
            "{anchor:?}"
        );
    }
}

#[test]
fn checklist_items_are_done_and_moves_stamp_the_time() {
    let (mut a, _) = pair();
    a.apply(
        &[Command::SetDone {
            id: id(ITEM),
            done: true,
        }],
        20,
    )
    .unwrap();
    a.apply(
        &[Command::Move {
            id: id(OTHER),
            parent: Some(id(AREA)),
            at: Anchor::Last,
        }],
        30,
    )
    .unwrap();
    let s = a.project().unwrap();
    assert!(s.checklist_items[0].is_completed);
    assert_eq!(s.checklist_items[0].updated_at, 20);
    assert_eq!(task(&s, OTHER).updated_at, 30);
    assert_eq!(task(&s, TASK).updated_at, 10);
}

#[test]
fn a_purged_identity_is_never_created_again() {
    let (mut a, _) = pair();
    a.apply(
        &[Command::Purge {
            ids: vec![id(OTHER)],
        }],
        20,
    )
    .unwrap();
    assert!(a
        .apply(&[create(OTHER, Kind::Task, None, "again")], 30)
        .is_err());
}

#[test]
fn the_version_moves_with_each_commit_and_agrees_after_sync() {
    let (mut a, mut b) = pair();
    assert_eq!(a.version(), b.version());
    let before = a.version();
    a.apply(
        &[Command::Rename {
            id: id(TASK),
            title: "x".into(),
        }],
        20,
    )
    .unwrap();
    assert_ne!(a.version(), before);
    sync(&mut a, &mut b);
    assert_eq!(a.version(), b.version());
}

#[test]
fn c19_today_skips_references_that_no_longer_name_a_task() {
    // Same UUID: a project on A (peer 1, the smaller tree ID, keeps it) and
    // a task on B, whose creation added the UUID to Today.
    let (mut a, mut b) = pair();
    a.apply(&[create(61, Kind::Project, None, "project")], 20)
        .unwrap();
    b.apply(&[create(61, Kind::Task, None, "task")], 20)
        .unwrap();
    let s = converged(&mut a, &mut b);
    assert!(s.projects.iter().any(|p| p.id == id(61)));
    assert!(s.repairs.contains(&format!("today-non-task:{}", id(61))));
    let copy = s.tasks.iter().find(|t| t.title == "task").unwrap();
    assert_ne!(copy.id, id(61));

    // Same UUID, both tasks: Today lists it once.
    let (mut a, mut b) = pair();
    a.apply(&[create(62, Kind::Task, None, "one")], 20).unwrap();
    b.apply(&[create(62, Kind::Task, None, "two")], 20).unwrap();
    let s = converged(&mut a, &mut b);
    assert!(s.repairs.contains(&format!("today-duplicate:{}", id(62))));
}
