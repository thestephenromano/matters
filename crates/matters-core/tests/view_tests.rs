//! The shared view contract every frontend relies on: view keys, where a task
//! captured in a view lands, how "When" strings are parsed and applied, and
//! that sidebar counts match the lists they badge.

use jiff::civil::Date;
use matters_core::{Database, Project, QueryEngine, Schedule, Status, Task, View};

fn today() -> Date {
    jiff::civil::date(2026, 11, 17)
}

fn all_views(project_id: &str) -> Vec<View> {
    let mut views = View::sidebar().to_vec();
    views.push(View::Project(project_id.to_string()));
    views
}

#[test]
fn view_keys_round_trip() {
    for view in all_views("p1") {
        let back = View::from_key(view.key(), view.project_id());
        assert_eq!(back.as_ref(), Some(&view));
    }
    assert_eq!(View::from_key("project", None), None);
    assert_eq!(View::from_key("project", Some("")), None);
    assert_eq!(View::from_key("archive", None), None);
}

#[test]
fn captured_task_lands_where_the_view_says() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0))
        .unwrap();
    let engine = QueryEngine::new(&db);

    for view in all_views("p1") {
        let id = format!("t-{}", view.key());
        let Some(task) = view.new_task(id, "captured", today()) else {
            // The Logbook and the Trash hold no new work.
            assert!(
                matches!(view, View::Logbook | View::Trash),
                "{view:?} should accept new tasks"
            );
            assert!(!view.accepts_new_tasks());
            continue;
        };
        assert!(view.accepts_new_tasks(), "{view:?}");
        db.create_task(&task).unwrap();

        // Anytime lists only area/project tasks, so captures there are filed
        // in the Inbox; every other view shows the task it was typed into.
        let expected_home = match view {
            View::Anytime => View::Inbox,
            ref other => other.clone(),
        };
        let listed = engine.tasks_in(&expected_home, today()).unwrap();
        assert!(
            listed.iter().any(|t| t.id == task.id),
            "task captured in {:?} should be listed in {:?}",
            view,
            expected_home
        );
    }
}

#[test]
fn schedule_parse_accepts_only_schedules() {
    assert_eq!(
        Schedule::parse("today"),
        Some(Schedule::Today { evening: false })
    );
    assert_eq!(
        Schedule::parse("evening"),
        Some(Schedule::Today { evening: true })
    );
    assert_eq!(Schedule::parse("someday"), Some(Schedule::Someday));
    assert_eq!(Schedule::parse("none"), Some(Schedule::None));
    assert_eq!(
        Schedule::parse("2026-12-01"),
        Some(Schedule::Date(jiff::civil::date(2026, 12, 1)))
    );
    // View names and junk are not schedules.
    for not_a_schedule in [
        "",
        "inbox",
        "upcoming",
        "anytime",
        "logbook",
        "project",
        "2026-13-40",
    ] {
        assert_eq!(Schedule::parse(not_a_schedule), None, "{not_a_schedule:?}");
    }
}

#[test]
fn apply_schedule_writes_all_fields_together() {
    let mut task = Task::new("t", "x", 0.0);

    task.apply_schedule(&Schedule::Someday, today());
    assert!(task.is_someday);
    assert_eq!(task.start_date, None);

    task.apply_schedule(&Schedule::Today { evening: true }, today());
    assert!(!task.is_someday, "scheduling for today must clear Someday");
    assert_eq!(task.start_date, Some(today()));
    assert!(task.is_evening());

    task.apply_schedule(&Schedule::Today { evening: false }, today());
    assert!(
        !task.is_evening(),
        "moving to daytime must clear This Evening"
    );

    let date = jiff::civil::date(2026, 12, 24);
    task.apply_schedule(&Schedule::Date(date), today());
    assert_eq!(
        (
            task.start_date,
            task.time_of_day.as_deref(),
            task.is_someday
        ),
        (Some(date), None, false)
    );

    task.apply_schedule(&Schedule::None, today());
    assert_eq!(
        (
            task.start_date,
            task.time_of_day.as_deref(),
            task.is_someday
        ),
        (None, None, false)
    );
}

#[test]
fn counts_match_listed_tasks() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0))
        .unwrap();
    let engine = QueryEngine::new(&db);

    let d = |y, m, day| jiff::civil::date(y, m, day);
    let mut n = 0;
    let mut add = |f: &dyn Fn(&mut Task)| {
        n += 1;
        let mut t = Task::new(format!("t{n}"), format!("task {n}"), 0.0);
        f(&mut t);
        db.create_task(&t).unwrap();
        t
    };

    add(&|_| {}); // inbox
    add(&|t| t.start_date = Some(today())); // today
    add(&|t| t.apply_schedule(&Schedule::Today { evening: true }, today())); // evening
    add(&|t| t.start_date = Some(d(2026, 11, 1))); // overdue -> today
    add(&|t| t.deadline = Some(today())); // undated, deadline today -> inbox and today
    add(&|t| t.start_date = Some(d(2026, 12, 1))); // upcoming
    add(&|t| t.project_id = Some("p1".into())); // anytime
    add(&|t| {
        t.project_id = Some("p1".into());
        t.start_date = Some(today());
    }); // today + anytime
    add(&|t| t.is_someday = true); // someday
    let done = add(&|t| t.project_id = Some("p1".into()));
    db.complete_task(&done.id).unwrap(); // logbook
    let trashed = add(&|t| t.project_id = Some("p1".into()));
    db.trash_task(&trashed.id).unwrap(); // trash only
    let trashed_done = add(&|t| t.start_date = Some(today()));
    db.complete_task(&trashed_done.id).unwrap();
    db.trash_task(&trashed_done.id).unwrap(); // trash only, not the logbook

    let counts = engine.counts(today()).unwrap();
    let len = |v: View| engine.tasks_in(&v, today()).unwrap().len();
    assert_eq!(counts.inbox, len(View::Inbox));
    assert_eq!(counts.today, len(View::Today));
    assert_eq!(counts.upcoming, len(View::Upcoming));
    assert_eq!(counts.anytime, len(View::Anytime));
    assert_eq!(counts.someday, len(View::Someday));
    assert_eq!(counts.logbook, len(View::Logbook));
    assert_eq!(
        (
            counts.inbox,
            counts.today,
            counts.upcoming,
            counts.anytime,
            counts.someday,
            counts.logbook
        ),
        (2, 5, 1, 2, 1, 1)
    );

    // The project view lists open and completed tasks, never trashed ones.
    let project = engine
        .tasks_in(&View::Project("p1".into()), today())
        .unwrap();
    assert_eq!(project.len(), 3);
    assert!(project.iter().all(|t| !t.is_trashed()));
    assert_eq!(len(View::Trash), 2);
}

#[test]
fn equal_sort_order_lists_in_a_stable_order() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0))
        .unwrap();
    for id in ["c", "a", "b"] {
        let mut t = Task::new(id, id, 0.0);
        t.project_id = Some("p1".into());
        t.created_at = 1_000; // identical creation time and sort order
        db.create_task(&t).unwrap();
    }
    let ids: Vec<_> = QueryEngine::new(&db)
        .tasks_in(&View::Project("p1".into()), today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}

#[test]
fn containing_view_lists_the_task() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Garden", 0.0))
        .unwrap();
    db.create_area(&matters_core::Area::new("a1", "Home", 0.0))
        .unwrap();
    let engine = QueryEngine::new(&db);

    let d = |m, day| jiff::civil::date(2026, m, day);
    type Setup<'a> = Box<dyn Fn(&mut Task) + 'a>;
    let setups: Vec<(&str, Setup<'_>)> = vec![
        ("inbox", Box::new(|_| {})),
        ("today", Box::new(|t| t.start_date = Some(today()))),
        ("overdue", Box::new(|t| t.start_date = Some(d(11, 1)))),
        (
            "evening",
            Box::new(|t| t.apply_schedule(&Schedule::Today { evening: true }, today())),
        ),
        ("deadline", Box::new(|t| t.deadline = Some(today()))),
        ("future deadline", Box::new(|t| t.deadline = Some(d(12, 1)))),
        ("upcoming", Box::new(|t| t.start_date = Some(d(12, 1)))),
        ("someday", Box::new(|t| t.is_someday = true)),
        ("area", Box::new(|t| t.area_id = Some("a1".into()))),
        (
            "area today",
            Box::new(|t| {
                t.area_id = Some("a1".into());
                t.start_date = Some(today());
            }),
        ),
        ("project", Box::new(|t| t.project_id = Some("p1".into()))),
        (
            "project someday",
            Box::new(|t| {
                t.project_id = Some("p1".into());
                t.is_someday = true;
            }),
        ),
        ("done", Box::new(|t| t.status = Status::Completed)),
        ("trashed", Box::new(|t| t.trashed_at = Some(1))),
        (
            "trashed project done",
            Box::new(|t| {
                t.project_id = Some("p1".into());
                t.status = Status::Completed;
                t.trashed_at = Some(1);
            }),
        ),
    ];

    for (name, setup) in setups {
        let mut task = Task::new(format!("t-{name}"), name, 0.0);
        setup(&mut task);
        if task.status == Status::Completed {
            task.completed_at = Some(1);
        }
        db.create_task(&task).unwrap();

        let view = View::containing(&task, today());
        let listed = engine.tasks_in(&view, today()).unwrap();
        assert!(
            listed.iter().any(|t| t.id == task.id),
            "{name}: View::containing chose {view:?}, which does not list the task"
        );
    }
}

#[test]
fn logbook_lists_every_finished_task_its_badge_counts() {
    let db = Database::open_in_memory().unwrap();
    let engine = QueryEngine::new(&db);
    for i in 0..60 {
        let id = format!("done-{i:02}");
        db.create_task(&Task::new(id.as_str(), id.as_str(), 0.0))
            .unwrap();
        db.complete_task(&id).unwrap();
    }
    let listed = engine.tasks_in(&View::Logbook, today()).unwrap();
    assert_eq!(listed.len(), 60);
    assert_eq!(engine.counts(today()).unwrap().logbook, listed.len());
    assert_eq!(
        engine.logbook(10).unwrap().len(),
        10,
        "the bounded slice still works"
    );
}

#[test]
fn an_undated_task_due_today_is_found_in_today() {
    // The Inbox lists it too, but Today is where its deadline puts it.
    let mut task = Task::new("t", "Pay rent", 0.0);
    task.deadline = Some(today());
    assert_eq!(View::containing(&task, today()), View::Today);
    task.deadline = Some(jiff::civil::date(2026, 11, 3));
    assert_eq!(View::containing(&task, today()), View::Today, "overdue");
    task.deadline = Some(jiff::civil::date(2026, 12, 1));
    assert_eq!(View::containing(&task, today()), View::Inbox, "not due yet");
}

#[test]
fn views_have_their_titles() {
    let titles: Vec<&str> = all_views("p1").iter().map(View::title).collect();
    assert_eq!(
        titles,
        ["Inbox", "Today", "Upcoming", "Anytime", "Someday", "Logbook", "Trash", "Project"]
    );
    assert_eq!(View::Area("a1".into()).title(), "Area");
}
