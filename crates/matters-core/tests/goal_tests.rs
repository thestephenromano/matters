//! Goal references (#212, ADR 0004): a project, or a repeating series'
//! newest occurrence, names the goal in the Goals app it serves. Engine
//! cases are named after ADR 0004's merge cases G01–G09.

use jiff::civil::{date, Date};
use matters_core::crdt::{Anchor, Command, Document, Kind, Snapshot};
use matters_core::repeat::{occurrence_id, Frequency, RepeatRule};
use matters_core::{Database, Home, MattersError, Project, Status, Task, TaskSnapshot};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

/// Goal IDs as the Goals app makes them; any UUID version is accepted.
fn goal(n: u128) -> String {
    uuid::Uuid::from_u128(0x7000_0000_0000_7000_8000_0000_0000_0000 | n).to_string()
}

fn day(n: i8) -> Date {
    date(2026, 11, n)
}

// ---------------------------------------------------------------- engine

const PROJECT: u128 = 1;
const TASK: u128 = 2;
const HEADING: u128 = 3;

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

fn set_goal(n: u128, g: Option<u128>) -> Command {
    Command::SetGoal {
        id: id(n),
        goal: g.map(goal),
    }
}

/// A project with a heading, and a task in the Inbox. A edits as peer 1,
/// B as peer 2.
fn pair() -> (Document, Document) {
    let mut a = Document::new(1).unwrap();
    a.apply(
        &[
            create(PROJECT, Kind::Project, None, "Garden"),
            create(HEADING, Kind::Heading, Some(PROJECT), "Spring"),
            create(TASK, Kind::Task, None, "Water"),
        ],
        10,
    )
    .unwrap();
    let b = Document::import(&a.snapshot().unwrap(), 2).unwrap();
    (a, b)
}

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

fn project_goal(s: &Snapshot, n: u128) -> Option<String> {
    s.projects
        .iter()
        .find(|p| p.id == id(n))
        .expect("project")
        .goal_id
        .clone()
}

fn task_goal(s: &Snapshot, n: u128) -> Option<String> {
    s.tasks
        .iter()
        .find(|t| t.id == id(n))
        .expect("task")
        .goal_id
        .clone()
}

#[test]
fn a_goal_is_set_cleared_and_kept_through_a_snapshot() {
    let (mut a, _) = pair();
    assert_eq!(project_goal(&a.project().unwrap(), PROJECT), None);
    a.apply(&[set_goal(PROJECT, Some(1)), set_goal(TASK, Some(2))], 20)
        .unwrap();
    let s = a.project().unwrap();
    assert_eq!(project_goal(&s, PROJECT), Some(goal(1)));
    assert_eq!(task_goal(&s, TASK), Some(goal(2)));
    let copy = Document::import(&a.snapshot().unwrap(), 3).unwrap();
    assert_eq!(copy.project().unwrap(), s);
    a.apply(&[set_goal(PROJECT, None)], 30).unwrap();
    let s = a.project().unwrap();
    assert_eq!(project_goal(&s, PROJECT), None);
    assert!(s.repairs.is_empty());
    assert_eq!(
        s.projects[0].updated_at, 30,
        "setting a goal is an edit of the project"
    );
}

#[test]
fn a_goal_that_is_not_an_id_or_on_a_heading_is_refused() {
    let (mut a, _) = pair();
    for bad in ["not-a-goal", "7000000000007000800000000000000", ""] {
        let refused = a.apply(
            &[Command::SetGoal {
                id: id(PROJECT),
                goal: Some(bad.into()),
            }],
            20,
        );
        assert!(matches!(refused, Err(MattersError::Validation(_))), "{bad}");
    }
    // Canonical text is lower case: the same UUID in capitals is refused.
    let upper = goal(0xabc).to_uppercase();
    assert_ne!(upper, goal(0xabc));
    assert!(matches!(
        a.apply(
            &[Command::SetGoal {
                id: id(PROJECT),
                goal: Some(upper),
            }],
            20
        ),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        a.apply(&[set_goal(HEADING, Some(1))], 20),
        Err(MattersError::Validation(_))
    ));
}

#[test]
fn g01_concurrent_links_resolve_to_one_goal() {
    let (mut a, mut b) = pair();
    a.apply(&[set_goal(PROJECT, Some(1))], 20).unwrap();
    b.apply(&[set_goal(PROJECT, Some(2))], 20).unwrap();
    let s = converged(&mut a, &mut b);
    let won = project_goal(&s, PROJECT);
    assert!(won == Some(goal(1)) || won == Some(goal(2)), "{won:?}");
    // A causally later link always wins.
    b.apply(&[set_goal(PROJECT, Some(3))], 30).unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(project_goal(&s, PROJECT), Some(goal(3)));
}

#[test]
fn g02_a_link_and_a_concurrent_clear_resolve_the_same_everywhere() {
    let (mut a, mut b) = pair();
    a.apply(&[set_goal(PROJECT, Some(1))], 20).unwrap();
    sync(&mut a, &mut b);
    a.apply(&[set_goal(PROJECT, Some(2))], 30).unwrap();
    b.apply(&[set_goal(PROJECT, None)], 30).unwrap();
    let s = converged(&mut a, &mut b);
    let won = project_goal(&s, PROJECT);
    assert!(won.is_none() || won == Some(goal(2)), "{won:?}");
    a.apply(&[set_goal(PROJECT, None)], 40).unwrap();
    assert_eq!(project_goal(&converged(&mut a, &mut b), PROJECT), None);
}

#[test]
fn g03_a_trashed_project_keeps_a_concurrent_link() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetTrashed {
            id: id(PROJECT),
            trashed_at: Some(20),
        }],
        20,
    )
    .unwrap();
    b.apply(&[set_goal(PROJECT, Some(1))], 20).unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(s.projects[0].trashed_at, Some(20));
    assert_eq!(project_goal(&s, PROJECT), Some(goal(1)));
    a.apply(
        &[Command::SetTrashed {
            id: id(PROJECT),
            trashed_at: None,
        }],
        30,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(
        (s.projects[0].trashed_at, project_goal(&s, PROJECT)),
        (None, Some(goal(1))),
        "putting it back shows the goal"
    );
}

#[test]
fn g04_a_finished_project_keeps_a_concurrent_link() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::SetStatus {
            id: id(PROJECT),
            status: Status::Completed,
            completed_at: Some(20),
            closed_by_project: false,
        }],
        20,
    )
    .unwrap();
    b.apply(&[set_goal(PROJECT, Some(1))], 20).unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(s.projects[0].status, Status::Completed);
    assert_eq!(project_goal(&s, PROJECT), Some(goal(1)));
}

#[test]
fn g05_a_purged_project_takes_its_link_with_it() {
    let (mut a, mut b) = pair();
    a.apply(
        &[Command::Purge {
            ids: vec![id(PROJECT)],
        }],
        20,
    )
    .unwrap();
    b.apply(&[set_goal(PROJECT, Some(1))], 20).unwrap();
    let s = converged(&mut a, &mut b);
    assert!(s.projects.is_empty());
    assert!(s.tombstones.contains(&id(PROJECT)));
}

/// Occurrence `n` of series 900, for `on`, in the Inbox.
fn occurrence(n: u128, on: Date, extra: Vec<Command>) -> Vec<Command> {
    let mut commands = vec![
        create(n, Kind::Task, None, "Run"),
        Command::SetOccurrence {
            id: id(n),
            series: id(900),
            date: on,
        },
    ];
    commands.extend(extra);
    commands
}

fn weekly() -> RepeatRule {
    RepeatRule {
        every: 1,
        frequency: Frequency::Daily,
        after_completion: true,
        start: day(1),
        until: None,
        left: None,
        paused: false,
    }
}

#[test]
fn g06_a_goal_change_on_an_older_occurrence_does_not_reach_the_next() {
    let (mut a, mut b) = pair();
    a.apply(
        &occurrence(
            501,
            day(21),
            vec![
                Command::SetRepeat {
                    id: id(501),
                    rule: Some(weekly()),
                },
                set_goal(501, Some(1)),
            ],
        ),
        20,
    )
    .unwrap();
    sync(&mut a, &mut b);
    // A completes O1 and creates O2 from it, as `complete_task_on` records
    // it: O1 gives up its rule, O2 is a copy with the goal handed on.
    let mut handed = vec![
        Command::SetStatus {
            id: id(501),
            status: Status::Completed,
            completed_at: Some(30),
            closed_by_project: false,
        },
        Command::SetRepeat {
            id: id(501),
            rule: None,
        },
    ];
    handed.extend(occurrence(
        502,
        day(22),
        vec![
            Command::SetRepeat {
                id: id(502),
                rule: Some(weekly()),
            },
            set_goal(502, Some(1)),
        ],
    ));
    a.apply(&handed, 30).unwrap();
    b.apply(&[set_goal(501, Some(2))], 30).unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(task_goal(&s, 501), Some(goal(2)), "B's change lands on O1");
    assert_eq!(task_goal(&s, 502), Some(goal(1)), "and does not reach O2");
}

#[test]
fn g07_twin_occurrences_merge_only_when_their_goals_match() {
    // Same goal on both twins: they merge.
    let (mut a, mut b) = pair();
    let twin = |doc: &mut Document, g: Option<u128>| {
        doc.apply(&occurrence(510, day(21), vec![set_goal(510, g)]), 20)
            .unwrap();
    };
    twin(&mut a, Some(1));
    twin(&mut b, Some(1));
    let s = converged(&mut a, &mut b);
    assert_eq!(s.tasks.iter().filter(|t| t.title == "Run").count(), 1);
    assert!(s.repairs.contains(&format!("occurrence-merge:{}", id(510))));
    assert_eq!(task_goal(&s, 510), Some(goal(1)));

    // Different goals mean a concurrent edit: they stay conflict copies.
    let (mut a, mut b) = pair();
    twin(&mut a, Some(1));
    twin(&mut b, Some(2));
    let s = converged(&mut a, &mut b);
    let mut goals: Vec<_> = s
        .tasks
        .iter()
        .filter(|t| t.title == "Run")
        .map(|t| t.goal_id.clone())
        .collect();
    goals.sort();
    assert_eq!(goals, [Some(goal(1)), Some(goal(2))]);
    assert!(s.repairs.iter().any(|r| r.starts_with("identity-copy:")));
    assert!(!s.repairs.iter().any(|r| r.starts_with("occurrence-merge:")));
}

#[test]
fn g08_other_edits_leave_the_goal_alone() {
    let (mut a, mut b) = pair();
    a.apply(&[set_goal(PROJECT, Some(1))], 20).unwrap();
    b.apply(
        &[
            Command::Rename {
                id: id(PROJECT),
                title: "Vegetables".into(),
            },
            Command::SetDeadline {
                id: id(PROJECT),
                deadline: Some(day(30)),
            },
        ],
        20,
    )
    .unwrap();
    let s = converged(&mut a, &mut b);
    assert_eq!(s.projects[0].title, "Vegetables");
    assert_eq!(project_goal(&s, PROJECT), Some(goal(1)));
}

/// As a damaged or hostile peer would: `goal` on PROJECT set straight in
/// the engine to `value`.
fn damaged_goal(doc: &mut Document, value: loro::LoroValue) {
    let raw = loro::LoroDoc::new();
    raw.set_peer_id(99).unwrap();
    raw.import(&doc.snapshot().unwrap()).unwrap();
    let home = raw.get_tree("home");
    let node = home
        .nodes()
        .into_iter()
        .find(|node| {
            home.get_meta(*node)
                .unwrap()
                .get("id")
                .and_then(|v| v.into_value().ok())
                .and_then(|v| v.into_string().ok())
                .is_some_and(|s| *s == id(PROJECT))
        })
        .expect("project node");
    home.get_meta(node).unwrap().insert("goal", value).unwrap();
    raw.commit();
    doc.merge(
        &raw.export(loro::ExportMode::updates(&doc.version()))
            .unwrap(),
    )
    .unwrap();
}

#[test]
fn g09_a_damaged_goal_projects_as_none_with_a_repair() {
    let damaged: [loro::LoroValue; 4] = [
        "\"not-a-goal\"".into(),
        "not json".into(),
        "42".into(),
        loro::LoroValue::I64(42),
    ];
    for value in damaged {
        let (mut a, _) = pair();
        damaged_goal(&mut a, value.clone());
        let s = a.project().unwrap();
        assert_eq!(project_goal(&s, PROJECT), None, "{value:?}");
        assert!(
            s.repairs.contains(&format!("goal-invalid:{}", id(PROJECT))),
            "{value:?}"
        );
        assert_eq!(s.tasks.len(), 1, "the rest still projects");
    }
    // A stored null is simply no goal.
    let (mut a, _) = pair();
    damaged_goal(&mut a, "null".into());
    let s = a.project().unwrap();
    assert_eq!(project_goal(&s, PROJECT), None);
    assert!(s.repairs.is_empty());
}

// ---------------------------------------------------------------- storage

fn task_on(db: &Database, n: u128, on: Option<Date>) -> String {
    let mut task = Task::new(id(n), format!("task {n}"), n as f64);
    task.start_date = on;
    db.add_task(&mut task).unwrap();
    task.id
}

fn get(db: &Database, id: &str) -> Task {
    db.get_task(id).unwrap().unwrap()
}

/// With sync on, the engine's rows carry the same goals as the database's.
fn engine_agrees(db: &Database) {
    let s = db.engine_snapshot().unwrap().expect("sync is on");
    for p in &s.projects {
        assert_eq!(db.get_project(&p.id).unwrap().unwrap().goal_id, p.goal_id);
    }
    for t in &s.tasks {
        assert_eq!(get(db, &t.id).goal_id, t.goal_id, "task {}", t.id);
    }
}

#[test]
fn a_project_serves_a_goal_until_it_is_cleared() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    let mut garden = Project::new(id(10), "Garden", 0.0);
    db.add_project(&mut garden).unwrap();
    db.set_project_goal(&garden.id, Some(&goal(1))).unwrap();
    assert_eq!(
        db.get_project(&garden.id).unwrap().unwrap().goal_id,
        Some(goal(1))
    );
    engine_agrees(&db);
    db.set_project_goal(&garden.id, None).unwrap();
    assert_eq!(db.get_project(&garden.id).unwrap().unwrap().goal_id, None);
    engine_agrees(&db);

    // Created with a goal, as a restore or an import would.
    let mut run = Project::new(id(11), "Half marathon", 1.0);
    run.goal_id = Some(goal(2));
    db.create_project(&run).unwrap();
    assert_eq!(
        db.get_project(&run.id).unwrap().unwrap().goal_id,
        Some(goal(2))
    );
    engine_agrees(&db);
    db.rebuild_from_engine().unwrap();
    assert_eq!(
        db.get_project(&run.id).unwrap().unwrap().goal_id,
        Some(goal(2)),
        "rebuilt rows keep it"
    );
}

#[test]
fn bad_goals_and_missing_projects_are_refused() {
    let db = Database::open_in_memory().unwrap();
    let mut garden = Project::new(id(10), "Garden", 0.0);
    db.add_project(&mut garden).unwrap();
    db.set_project_goal(&garden.id, Some(&goal(1))).unwrap();
    assert!(matches!(
        db.set_project_goal(&garden.id, Some("someday")),
        Err(MattersError::Validation(_))
    ));
    assert_eq!(
        db.get_project(&garden.id).unwrap().unwrap().goal_id,
        Some(goal(1)),
        "a refused goal changes nothing"
    );
    assert!(matches!(
        db.set_project_goal(&id(99), Some(&goal(1))),
        Err(MattersError::NotFound { .. })
    ));
    let mut bad = Project::new(id(12), "Bad", 2.0);
    bad.goal_id = Some("someday".into());
    assert!(matches!(
        db.create_project(&bad),
        Err(MattersError::Validation(_))
    ));
    let mut bad = Task::new(id(13), "Bad", 3.0);
    bad.goal_id = Some("someday".into());
    assert!(matches!(
        db.create_task(&bad),
        Err(MattersError::Validation(_))
    ));
}

#[test]
fn only_a_series_newest_occurrence_takes_a_goal() {
    let db = Database::open_in_memory().unwrap();
    let single = task_on(&db, 1, Some(day(21)));
    assert!(matches!(
        db.set_series_goal(&single, Some(&goal(1))),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        db.set_series_goal(&id(99), Some(&goal(1))),
        Err(MattersError::NotFound { .. })
    ));
    db.set_task_repeat(&single, Some(weekly()), day(21))
        .unwrap();
    db.set_series_goal(&single, Some(&goal(1))).unwrap();
    assert_eq!(get(&db, &single).goal_id, Some(goal(1)));
    assert!(matches!(
        db.set_series_goal(&single, Some("someday")),
        Err(MattersError::Validation(_))
    ));
}

#[test]
fn the_goal_passes_to_the_next_occurrence_and_the_old_one_keeps_it() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    let t = task_on(&db, 1, Some(day(10)));
    db.set_task_repeat(&t, Some(weekly()), day(10)).unwrap();
    db.set_series_goal(&t, Some(&goal(1))).unwrap();
    db.complete_task_on(&t, day(10)).unwrap();
    let next = occurrence_id(&t, day(11)).unwrap();
    assert_eq!(get(&db, &next).goal_id, Some(goal(1)), "handed on");
    assert_eq!(get(&db, &t).goal_id, Some(goal(1)), "the done one keeps it");
    engine_agrees(&db);

    // A new goal for the series is set on its newest occurrence only; the
    // finished one no longer carries the rule and keeps what it served.
    assert!(db.set_series_goal(&t, Some(&goal(2))).is_err());
    db.set_series_goal(&next, Some(&goal(2))).unwrap();
    assert_eq!(get(&db, &t).goal_id, Some(goal(1)));
    db.complete_task_on(&next, day(11)).unwrap();
    let third = occurrence_id(&t, day(12)).unwrap();
    assert_eq!(get(&db, &third).goal_id, Some(goal(2)));
    engine_agrees(&db);
}

#[test]
fn a_scheduled_series_hands_its_goal_on_too() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(10)));
    let daily = RepeatRule {
        after_completion: false,
        ..weekly()
    };
    db.set_task_repeat(&t, Some(daily), day(10)).unwrap();
    db.set_series_goal(&t, Some(&goal(1))).unwrap();
    let made = db.advance_repeats(day(12)).unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(get(&db, &made[0]).goal_id, Some(goal(1)));
}

#[test]
fn copies_and_conversions_start_without_a_goal() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(10)));
    db.set_task_repeat(&t, Some(weekly()), day(10)).unwrap();
    db.set_series_goal(&t, Some(&goal(1))).unwrap();
    // A duplicate doesn't repeat, so it serves its project's goal instead.
    let copy = db.duplicate_task(&t).unwrap();
    assert_eq!(get(&db, &copy).goal_id, None);
    let project = db.convert_task_to_project(&t).unwrap();
    assert_eq!(db.get_project(&project).unwrap().unwrap().goal_id, None);
}

#[test]
fn undo_puts_a_series_goal_back() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(10)));
    db.set_task_repeat(&t, Some(weekly()), day(10)).unwrap();
    db.set_series_goal(&t, Some(&goal(1))).unwrap();
    let before = TaskSnapshot::take(&db, std::slice::from_ref(&t)).unwrap();
    db.set_series_goal(&t, Some(&goal(2))).unwrap();
    before.restore(&db, day(10)).unwrap();
    assert_eq!(get(&db, &t).goal_id, Some(goal(1)));
    let cleared = TaskSnapshot::take(&db, std::slice::from_ref(&t)).unwrap();
    db.set_series_goal(&t, None).unwrap();
    cleared.restore(&db, day(10)).unwrap();
    assert_eq!(get(&db, &t).goal_id, Some(goal(1)));
}

#[test]
fn undo_leaves_the_goal_of_an_occurrence_that_has_passed_its_rule_on() {
    // Only the newest occurrence changes the series' goal, so undoing the
    // completion of an older one restores its status and leaves its goal.
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(10)));
    db.set_task_repeat(&t, Some(weekly()), day(10)).unwrap();
    let before = TaskSnapshot::take(&db, std::slice::from_ref(&t)).unwrap();
    db.set_series_goal(&t, Some(&goal(1))).unwrap();
    db.complete_task_on(&t, day(10)).unwrap();
    assert!(get(&db, &t).repeat.is_none(), "handed on");
    before.restore(&db, day(10)).unwrap();
    let old = get(&db, &t);
    assert_eq!(old.status, Status::Open);
    assert_eq!(old.goal_id, Some(goal(1)));
}

#[test]
fn goals_reach_another_replica() {
    let (a, b) = (
        Database::open_in_memory().unwrap(),
        Database::open_in_memory().unwrap(),
    );
    a.enable_sync().unwrap();
    b.enable_sync().unwrap();
    let mut garden = Project::new(id(10), "Garden", 0.0);
    a.add_project(&mut garden).unwrap();
    a.set_project_goal(&garden.id, Some(&goal(1))).unwrap();
    let t = task_on(&a, 1, Some(day(10)));
    a.set_task_home(
        &t,
        &Home::Project {
            project: garden.id.clone(),
            heading: None,
        },
    )
    .unwrap();
    a.set_task_repeat(&t, Some(weekly()), day(10)).unwrap();
    a.set_series_goal(&t, Some(&goal(2))).unwrap();
    for (_, bytes) in a.outgoing_updates(0).unwrap() {
        b.receive_update(&bytes).unwrap();
    }
    assert_eq!(
        b.get_project(&garden.id).unwrap().unwrap().goal_id,
        Some(goal(1))
    );
    assert_eq!(get(&b, &t).goal_id, Some(goal(2)));
}

#[test]
fn existing_databases_gain_goal_columns_without_losing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let db = Database::open(&path).unwrap();
        let mut garden = Project::new(id(10), "Garden", 0.0);
        db.add_project(&mut garden).unwrap();
        task_on(&db, 1, Some(day(10)));
        // Back to the schema before goal references.
        db.with_raw_conn(|c| {
            c.execute_batch(
                "ALTER TABLE projects DROP COLUMN goal_id;
                 ALTER TABLE tasks DROP COLUMN goal_id;
                 DELETE FROM schema_migrations WHERE version >= 15;",
            )?;
            Ok(())
        })
        .unwrap();
    }
    for _ in 0..2 {
        let db = Database::open(&path).unwrap();
        let garden = db.get_project(&id(10)).unwrap().unwrap();
        assert_eq!((garden.title.as_str(), garden.goal_id), ("Garden", None));
        assert_eq!(get(&db, &id(1)).goal_id, None);
        assert_eq!(get(&db, &id(1)).title, "task 1");
    }
}
