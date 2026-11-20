//! When finished tasks move to the Logbook (#67): the lists keep the tasks
//! finished after the cutoff, shown done; the Logbook and its badge have
//! the rest; the other badges count open tasks only. The settings that
//! choose the cutoff, and how a visit keeps ticked tasks, per policy.

use jiff::civil::Date;
use matters_core::{
    Area, Database, Home, LogPolicy, QueryEngine, Schedule, Settings, SettingsFile, Status, Task,
    View, Visit,
};

fn today() -> Date {
    jiff::civil::date(2026, 11, 20)
}

/// The cutoff the tests log up to.
const CUTOFF: i64 = 1_000_000;

/// A task per placing view, one finished before the cutoff and one after
/// in each, plus one open; and one finished at an unknown time.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("home", "Home", 0.0)).unwrap();
    let places: [(&str, Schedule, Home); 5] = [
        ("inbox", Schedule::None, Home::Inbox),
        ("today", Schedule::Today { evening: false }, Home::Inbox),
        (
            "upcoming",
            Schedule::Date(today().tomorrow().unwrap()),
            Home::Inbox,
        ),
        ("anytime", Schedule::None, Home::Area("home".into())),
        ("someday", Schedule::Someday, Home::Inbox),
    ];
    for (place, schedule, home) in places {
        for state in ["open", "before", "after"] {
            let id = format!("{place}-{state}");
            let mut t = Task::new(&id, &id, 0.0);
            home.apply(&mut t);
            t.apply_schedule(&schedule, today());
            db.add_task(&mut t).unwrap();
        }
        finish_at(&db, &format!("{place}-before"), "completed", Some(CUTOFF));
        finish_at(&db, &format!("{place}-after"), "canceled", Some(CUTOFF + 1));
    }
    db.add_task(&mut Task::new("unknown", "unknown", 0.0))
        .unwrap();
    finish_at(&db, "unknown", "completed", None);
    db
}

/// Stage a task finished at `at` (or at an unknown time).
fn finish_at(db: &Database, id: &str, status: &str, at: Option<i64>) {
    db.with_raw_conn(|conn| {
        conn.execute(
            "UPDATE tasks SET status = ?1, completed_at = ?2 WHERE id = ?3",
            (status, at, id),
        )?;
        Ok(())
    })
    .unwrap();
}

fn ids(engine: &QueryEngine, view: &View) -> Vec<String> {
    let mut ids: Vec<String> = engine
        .tasks_in(view, today())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    ids.sort();
    ids
}

#[test]
fn every_finished_task_is_logged_by_default() {
    let db = fixture();
    let engine = QueryEngine::new(&db);
    for (view, place) in [
        (View::Inbox, "inbox"),
        (View::Today, "today"),
        (View::Upcoming, "upcoming"),
        (View::Anytime, "anytime"),
        (View::Someday, "someday"),
    ] {
        assert_eq!(ids(&engine, &view), [format!("{place}-open")], "{view:?}");
    }
    assert_eq!(ids(&engine, &View::Logbook).len(), 11);
    assert_eq!(
        engine.logged_until(None).counts(today()).unwrap().logbook,
        11
    );
}

#[test]
fn tasks_finished_after_the_cutoff_stay_in_their_lists_until_logged() {
    let db = fixture();
    let engine = QueryEngine::new(&db).logged_until(Some(CUTOFF));
    for (view, place) in [
        (View::Inbox, "inbox"),
        (View::Today, "today"),
        (View::Upcoming, "upcoming"),
        (View::Anytime, "anytime"),
        (View::Someday, "someday"),
    ] {
        assert_eq!(
            ids(&engine, &view),
            [format!("{place}-after"), format!("{place}-open")],
            "{view:?}: open, and finished (completed or canceled) after the cutoff"
        );
    }
    let mut logged = ids(&engine, &View::Logbook);
    logged.retain(|id| !id.ends_with("-before"));
    assert_eq!(
        logged,
        ["unknown"],
        "the Logbook: finished up to the cutoff, or at an unknown time"
    );
    assert_eq!(ids(&engine, &View::Logbook).len(), 6);

    let counts = engine.counts(today()).unwrap();
    assert_eq!(
        (
            counts.inbox,
            counts.today,
            counts.upcoming,
            counts.anytime,
            counts.someday
        ),
        (1, 1, 1, 1, 1),
        "badges count the open tasks only"
    );
    assert_eq!(counts.logbook, 6, "the Logbook's badge matches its list");
}

#[test]
fn a_task_finished_exactly_at_the_cutoff_is_logged() {
    let db = fixture();
    let engine = QueryEngine::new(&db).logged_until(Some(CUTOFF + 1));
    assert_eq!(ids(&engine, &View::Inbox), ["inbox-open"]);
    assert!(ids(&engine, &View::Logbook).contains(&"inbox-after".to_string()));
    let engine = QueryEngine::new(&db).logged_until(Some(CUTOFF - 1));
    assert_eq!(
        ids(&engine, &View::Inbox),
        ["inbox-after", "inbox-before", "inbox-open"]
    );
    assert_eq!(ids(&engine, &View::Logbook), ["unknown"]);
}

#[test]
fn the_logbook_limit_query_honours_the_cutoff() {
    let db = fixture();
    let engine = QueryEngine::new(&db).logged_until(Some(CUTOFF));
    let recent: Vec<String> = engine
        .logbook(100)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(recent.len(), 6);
    assert!(recent.iter().all(|id| !id.ends_with("-after")));
}

#[test]
fn the_cutoff_follows_the_policy() {
    let day_start = matters_core::clock::start_of_local_day_millis(today());
    let mut settings = Settings::default();
    assert_eq!(settings.logbook.policy, LogPolicy::WhenLeaving);
    assert_eq!(settings.logged_until(today()), None);

    settings.set_log_policy(LogPolicy::Immediately, 5);
    assert_eq!(settings.logged_until(today()), None);

    settings.set_log_policy(LogPolicy::Manually, 5);
    assert_eq!(
        settings.logbook.logged_at, 5,
        "choosing logs what is finished"
    );
    assert_eq!(settings.logged_until(today()), Some(5));
    settings.log_completed(day_start + 9);
    assert_eq!(settings.logged_until(today()), Some(day_start + 9));

    settings.set_log_policy(LogPolicy::Daily, 7);
    assert_eq!(
        settings.logged_until(today()),
        Some(day_start),
        "the start of today, past the last logging"
    );
    settings.log_completed(day_start + 3);
    assert_eq!(
        settings.logged_until(today()),
        Some(day_start + 3),
        "Log Completed logs today's too"
    );
    assert_eq!(
        settings.logged_until(today().tomorrow().unwrap()),
        Some(matters_core::clock::start_of_local_day_millis(
            today().tomorrow().unwrap()
        )),
        "the next day logs the day before"
    );
}

#[test]
fn policies_have_stable_keys_and_only_the_default_keeps_ticked_tasks() {
    for policy in LogPolicy::ALL {
        assert_eq!(LogPolicy::from_key(policy.key()), Some(policy));
        assert_eq!(policy.keeps_ticked(), policy == LogPolicy::WhenLeaving);
    }
    assert_eq!(
        LogPolicy::ALL.map(LogPolicy::key),
        ["when-leaving", "immediately", "daily", "manually"]
    );
    assert_eq!(LogPolicy::from_key("weekly"), None);
}

#[test]
fn without_keeping_a_ticked_task_leaves_at_once() {
    let db = Database::open_in_memory().unwrap();
    for id in ["a", "b"] {
        let mut t = Task::new(id, id, 0.0);
        t.apply_schedule(&Schedule::Today { evening: false }, today());
        db.add_task(&mut t).unwrap();
    }
    let mut visit = Visit::new();
    let show = |visit: &mut Visit, shown: &[Task]| -> Vec<Task> {
        let members = QueryEngine::new(&db)
            .tasks_in(&View::Today, today())
            .unwrap();
        visit
            .rows(&View::Today, members, shown, |id| db.get_task(id))
            .unwrap()
    };
    let shown = show(&mut visit, &[]);
    db.complete_task("a").unwrap();
    let kept = show(&mut visit, &shown);
    assert_eq!(kept.len(), 2, "kept by default");
    assert_eq!(kept[0].status, Status::Completed);

    visit.keep_ticked(false);
    let now = show(&mut visit, &kept);
    assert_eq!(
        now.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["b"],
        "turned off, what was kept goes"
    );
    db.complete_task("b").unwrap();
    assert!(show(&mut visit, &now).is_empty());

    visit.keep_ticked(true);
    db.uncomplete_task("b").unwrap();
    let back = show(&mut visit, &[]);
    db.complete_task("b").unwrap();
    assert_eq!(show(&mut visit, &back).len(), 1, "on again, it keeps");
}

/// The frontends set the keeping rule before every load; setting it on
/// again must not forget the tasks kept so far.
#[test]
fn keeping_set_before_each_load_keeps_a_ticked_task_until_leaving() {
    let db = Database::open_in_memory().unwrap();
    for id in ["a", "b"] {
        let mut t = Task::new(id, id, 0.0);
        t.apply_schedule(&Schedule::Today { evening: false }, today());
        db.add_task(&mut t).unwrap();
    }
    let mut visit = Visit::new();
    // As a frontend loads: keeping set, then the rows from those shown.
    let show = |visit: &mut Visit, shown: &[Task]| -> Vec<Task> {
        visit.keep_ticked(true);
        let members = QueryEngine::new(&db)
            .tasks_in(&View::Today, today())
            .unwrap();
        visit
            .rows(&View::Today, members, shown, |id| db.get_task(id))
            .unwrap()
    };
    let ids = |tasks: &[Task]| -> Vec<String> { tasks.iter().map(|t| t.id.clone()).collect() };
    let first = show(&mut visit, &[]);
    db.complete_task("a").unwrap();
    let second = show(&mut visit, &first);
    assert_eq!(ids(&second), ["a", "b"]);
    let third = show(&mut visit, &second);
    assert_eq!(ids(&third), ["a", "b"], "still kept on the next load");
    visit.restart();
    assert_eq!(ids(&show(&mut visit, &third)), ["b"], "gone once left");
}

#[test]
fn settings_survive_a_save_and_a_missing_file_is_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("nested").join("settings.toml"));
    assert_eq!(file.load(), (Settings::default(), None));

    let mut settings = Settings::default();
    settings.set_log_policy(LogPolicy::Manually, 42);
    file.save(&settings).unwrap();
    assert_eq!(file.load(), (settings, None));
    let text = std::fs::read_to_string(file.path()).unwrap();
    assert!(text.contains("policy = \"manually\""), "{text}");
    assert!(
        !file.path().with_extension("toml.partial").exists(),
        "no partial file is left"
    );
}

#[test]
fn missing_keys_take_defaults_and_unknown_ones_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    std::fs::write(
        file.path(),
        "future = 1\n[logbook]\npolicy = \"daily\"\ncolour = \"red\"\n",
    )
    .unwrap();
    let (settings, problem) = file.load();
    assert_eq!(problem, None);
    assert_eq!(settings.logbook.policy, LogPolicy::Daily);
    assert_eq!(settings.logbook.logged_at, 0);
}

#[test]
fn an_unreadable_file_gives_the_defaults_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    std::fs::write(file.path(), "[logbook]\npolicy = \"weekly\"\n").unwrap();
    let (settings, problem) = file.load();
    assert_eq!(settings, Settings::default());
    let problem = problem.expect("a message");
    assert!(
        problem.contains(&file.path().display().to_string()),
        "{problem}"
    );
    assert!(problem.contains("defaults"), "{problem}");

    // Not a file at all.
    let file = SettingsFile::new(dir.path());
    let (settings, problem) = file.load();
    assert_eq!(settings, Settings::default());
    assert!(problem.is_some());

    // Saving replaces the unreadable file.
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    file.save(&Settings::default()).unwrap();
    assert_eq!(file.load(), (Settings::default(), None));
}

#[test]
fn the_default_file_is_in_the_config_directory() {
    let file = SettingsFile::in_config_dir().unwrap();
    assert!(
        file.path().ends_with("matters/settings.toml"),
        "{}",
        file.path().display()
    );
}
