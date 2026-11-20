//! Repeating tasks as rows (#61): a series' newest occurrence carries the
//! rule and passes it on; occurrences are created on schedule or after
//! completion, never twice, and are copies of the one before.

use jiff::civil::{date, time, Date};
use matters_core::repeat::{occurrence_id, Frequency, RepeatRule};
use matters_core::{
    ChecklistItem, Database, Home, MattersError, Outcome, Project, Status, Tag, Tagged, Task,
};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn day(n: i8) -> Date {
    date(2026, 11, n)
}

fn rule(frequency: Frequency) -> RepeatRule {
    RepeatRule {
        every: 1,
        frequency,
        after_completion: false,
        start: day(1),
        until: None,
        left: None,
        paused: false,
    }
}

fn weekly(days: &[i8]) -> RepeatRule {
    rule(Frequency::Weekly {
        days: days.to_vec(),
    })
}

/// Task `n`, dated `on`.
fn task_on(db: &Database, n: u128, on: Option<Date>) -> String {
    let mut task = Task::new(id(n), format!("task {n}"), n as f64);
    task.start_date = on;
    db.add_task(&mut task).unwrap();
    task.id
}

fn get(db: &Database, id: &str) -> Task {
    db.get_task(id).unwrap().unwrap()
}

/// Every task of `series`, by date.
fn series(db: &Database, series: &str) -> Vec<Task> {
    let ids: Vec<String> = db
        .with_raw_conn(|c| {
            let mut stmt =
                c.prepare("SELECT id FROM tasks WHERE repeat_series = ?1 ORDER BY repeat_date")?;
            let rows = stmt.query_map([series], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .unwrap();
    ids.iter().map(|i| get(db, i)).collect()
}

#[test]
fn a_repeating_task_starts_a_series_on_its_date() {
    let db = Database::open_in_memory().unwrap();
    // Saturday 21st, repeating on Mondays and Thursdays.
    let t = task_on(&db, 1, Some(day(21)));
    db.set_task_repeat(&t, Some(weekly(&[1, 4])), day(20))
        .unwrap();
    let task = get(&db, &t);
    assert_eq!(
        task.series.as_deref(),
        Some(t.as_str()),
        "named by its first"
    );
    assert_eq!(task.occurrence, Some(day(21)));
    let rule = task.repeat.unwrap();
    assert_eq!(rule.start, day(21), "from the task's date");

    // An undated or Someday task is scheduled for today.
    let inbox = task_on(&db, 2, None);
    db.set_task_repeat(&inbox, Some(rule.clone()), day(20))
        .unwrap();
    assert_eq!(get(&db, &inbox).start_date, Some(day(20)));
    let mut later = Task::new(id(3), "someday", 3.0);
    later.is_someday = true;
    db.add_task(&mut later).unwrap();
    db.set_task_repeat(&later.id, Some(rule), day(20)).unwrap();
    let later = get(&db, &later.id);
    assert_eq!((later.start_date, later.is_someday), (Some(day(20)), false));

    // A rule that can't be used is refused, changing nothing.
    let bad = weekly(&[]);
    assert!(matches!(
        db.set_task_repeat(&t, Some(bad), day(20)),
        Err(MattersError::Validation(_))
    ));
    assert!(get(&db, &t).repeat.is_some());
    assert!(db
        .set_task_repeat(&id(99), Some(weekly(&[1])), day(20))
        .is_err());
}

#[test]
fn the_next_occurrence_comes_on_its_date_as_a_copy() {
    let db = Database::open_in_memory().unwrap();
    let mut garden = Project::new(id(10), "Garden", 0.0);
    db.add_project(&mut garden).unwrap();
    let t = task_on(&db, 1, Some(day(21)));
    db.set_task_home(
        &t,
        &Home::Project {
            project: id(10),
            heading: None,
        },
    )
    .unwrap();
    db.set_task_notes(&t, "water the beds").unwrap();
    db.set_task_deadline(&t, Some(day(23))).unwrap();
    db.set_task_reminder(&t, Some(time(8, 0, 0, 0))).unwrap();
    let mut tag = Tag::new(id(30), "outside", None);
    db.add_tag(&mut tag).unwrap();
    db.add_tag_to(Tagged::Task(&t), &tag.id).unwrap();
    let mut item = ChecklistItem::new(id(20), t.clone(), "hose", 0.0);
    db.add_checklist_item(&mut item).unwrap();
    db.set_checklist_item_done(&id(20), true).unwrap();
    db.set_task_repeat(&t, Some(weekly(&[1, 4])), day(21))
        .unwrap();

    assert!(
        db.advance_repeats(day(22)).unwrap().is_empty(),
        "Sunday: nothing due"
    );
    let created = db.advance_repeats(day(23)).unwrap();
    let next_id = occurrence_id(&t, day(23)).unwrap();
    assert_eq!(created, std::slice::from_ref(&next_id));

    let next = get(&db, &next_id);
    let head = get(&db, &t);
    assert_eq!(next.title, "task 1");
    assert_eq!(next.notes, "water the beds");
    assert_eq!(next.project_id, Some(id(10)));
    assert_eq!(next.start_date, Some(day(23)));
    assert_eq!(
        next.deadline,
        Some(day(25)),
        "two days after its date, as before"
    );
    assert_eq!(next.reminder, Some(time(8, 0, 0, 0)));
    assert_eq!(next.status, Status::Open);
    assert_eq!(next.series, Some(t.clone()));
    assert_eq!(next.occurrence, Some(day(23)));
    assert!(next.repeat.is_some(), "the rule passed on");
    assert_eq!(head.repeat, None);
    assert_eq!(head.status, Status::Open, "the one before stays as it was");
    let items = db.list_checklist_items(&next_id).unwrap();
    assert_eq!(items.len(), 1);
    assert!(!items[0].is_completed, "the checklist starts unticked");
    let tags = db.tags_of_tasks(&[next_id.as_str()]).unwrap();
    assert_eq!(tags[&next_id].len(), 1, "the tags come along");

    // Again the same day: nothing more.
    assert!(db.advance_repeats(day(23)).unwrap().is_empty());
    // Thursday.
    assert_eq!(
        db.advance_repeats(day(26)).unwrap(),
        [occurrence_id(&t, day(26)).unwrap()]
    );
    assert_eq!(series(&db, &t).len(), 3);
}

#[test]
fn after_days_away_only_the_latest_due_is_created() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    // Away from the 2nd to the 12th.
    let created = db.advance_repeats(day(12)).unwrap();
    assert_eq!(created, [occurrence_id(&t, day(12)).unwrap()]);
    assert_eq!(series(&db, &t).len(), 2);
}

#[test]
fn completing_an_after_completion_repeat_creates_the_next_with_it() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(10)));
    let every_3_days = RepeatRule {
        every: 3,
        after_completion: true,
        ..rule(Frequency::Daily)
    };
    db.set_task_repeat(&t, Some(every_3_days), day(10)).unwrap();
    // Nothing on schedule.
    assert!(db.advance_repeats(day(28)).unwrap().is_empty());

    // Done late, on the 14th: the next is three days after that.
    db.complete_task_on(&t, day(14)).unwrap();
    let next_id = occurrence_id(&t, day(17)).unwrap();
    let next = get(&db, &next_id);
    assert_eq!(next.start_date, Some(day(17)));
    assert!(next.repeat.is_some());
    assert_eq!(get(&db, &t).status, Status::Completed);

    // Completing the old one again, or reopening and completing it, makes
    // nothing: it no longer carries the rule.
    db.uncomplete_task(&t).unwrap();
    db.complete_task_on(&t, day(14)).unwrap();
    assert_eq!(series(&db, &t).len(), 2);

    // The next, done on its day.
    db.complete_task_on(&next_id, day(17)).unwrap();
    assert!(db
        .get_task(&occurrence_id(&t, day(20)).unwrap())
        .unwrap()
        .is_some());
    assert_eq!(series(&db, &t).len(), 3);
}

#[test]
fn completing_a_scheduled_or_paused_repeat_creates_nothing() {
    let db = Database::open_in_memory().unwrap();
    let scheduled = task_on(&db, 1, Some(day(10)));
    db.set_task_repeat(&scheduled, Some(rule(Frequency::Daily)), day(10))
        .unwrap();
    db.complete_task_on(&scheduled, day(10)).unwrap();
    let paused = task_on(&db, 2, Some(day(10)));
    let rule = RepeatRule {
        after_completion: true,
        paused: true,
        ..rule(Frequency::Daily)
    };
    db.set_task_repeat(&paused, Some(rule), day(10)).unwrap();
    db.complete_task_on(&paused, day(10)).unwrap();
    assert_eq!(series(&db, &scheduled).len(), 1, "it waits for its date");
    assert_eq!(series(&db, &paused).len(), 1);
    assert!(get(&db, &scheduled).repeat.is_some(), "still carrying it");
}

#[test]
fn an_occurrence_that_exists_is_never_made_twice() {
    // As after a crash between creating an occurrence and passing the rule
    // on, or with the occurrence received from another device first.
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    let mut first = Task::new(occurrence_id(&t, day(3)).unwrap(), "already here", 9.0);
    first.start_date = Some(day(3));
    db.add_task(&mut first).unwrap();

    assert!(db.advance_repeats(day(3)).unwrap().is_empty());
    assert_eq!(get(&db, &first.id).title, "already here", "left as it is");
    assert_eq!(get(&db, &t).repeat, None, "the rule has passed on");
}

#[test]
fn a_series_ends_after_its_count_or_its_last_day() {
    let db = Database::open_in_memory().unwrap();
    let twice_more = RepeatRule {
        left: Some(2),
        ..rule(Frequency::Daily)
    };
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(twice_more), day(2)).unwrap();
    for d in 3..=6 {
        db.advance_repeats(day(d)).unwrap();
    }
    let all = series(&db, &t);
    assert_eq!(all.len(), 3, "the first and two more");
    assert_eq!(all[2].repeat.as_ref().and_then(|r| r.left), Some(0));

    let until = RepeatRule {
        until: Some(day(4)),
        ..rule(Frequency::Daily)
    };
    let u = task_on(&db, 2, Some(day(2)));
    db.set_task_repeat(&u, Some(until), day(2)).unwrap();
    for d in 3..=8 {
        db.advance_repeats(day(d)).unwrap();
    }
    let dates: Vec<_> = series(&db, &u).iter().map(|t| t.start_date).collect();
    assert_eq!(dates, [Some(day(2)), Some(day(3)), Some(day(4))]);
}

#[test]
fn a_paused_trashed_or_finished_series_waits() {
    let db = Database::open_in_memory().unwrap();
    let paused = RepeatRule {
        paused: true,
        ..rule(Frequency::Daily)
    };
    let p = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&p, Some(paused), day(2)).unwrap();
    let trashed = task_on(&db, 2, Some(day(2)));
    db.set_task_repeat(&trashed, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    db.trash_task(&trashed).unwrap();
    let mut done = Project::new(id(10), "Done", 0.0);
    db.add_project(&mut done).unwrap();
    let inside = task_on(&db, 3, Some(day(2)));
    db.set_task_home(
        &inside,
        &Home::Project {
            project: id(10),
            heading: None,
        },
    )
    .unwrap();
    db.set_task_repeat(&inside, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    db.finish_project(&id(10), Outcome::Completed, Outcome::Completed)
        .unwrap();
    assert!(db.advance_repeats(day(5)).unwrap().is_empty());

    // Put back, it catches up with the latest date.
    db.restore_task(&trashed).unwrap();
    assert_eq!(
        db.advance_repeats(day(5)).unwrap(),
        [occurrence_id(&trashed, day(5)).unwrap()]
    );
}

#[test]
fn stopping_a_repeat_ends_the_series() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    db.set_task_repeat(&t, None, day(2)).unwrap();
    assert!(db.advance_repeats(day(9)).unwrap().is_empty());
    let task = get(&db, &t);
    assert_eq!(task.repeat, None);
    assert_eq!(task.series, Some(t.clone()), "still an occurrence of it");
}

#[test]
fn only_one_occurrence_carries_the_rule() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    let next = db.advance_repeats(day(3)).unwrap().remove(0);
    // Changing the rule on the older one takes it back from the newer.
    db.set_task_repeat(&t, Some(weekly(&[1])), day(3)).unwrap();
    assert!(get(&db, &t).repeat.is_some());
    assert_eq!(get(&db, &next).repeat, None);
    let carriers = series(&db, &t)
        .iter()
        .filter(|t| t.repeat.is_some())
        .count();
    assert_eq!(carriers, 1);
}

#[test]
fn a_duplicate_is_not_part_of_the_series() {
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    let copy = get(&db, &db.duplicate_task(&t).unwrap());
    assert_eq!(
        (copy.repeat, copy.series, copy.occurrence),
        (None, None, None)
    );
}

#[test]
fn occurrences_are_listed_and_logged_like_any_task() {
    use matters_core::{QueryEngine, View};
    let db = Database::open_in_memory().unwrap();
    let t = task_on(&db, 1, Some(day(2)));
    db.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    let next = db.advance_repeats(day(3)).unwrap().remove(0);
    let engine = QueryEngine::new(&db);
    let today: Vec<String> = engine
        .tasks_in(&View::Today, day(3))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert!(
        today.contains(&t) && today.contains(&next),
        "the overdue one and today's"
    );
    db.complete_task_on(&next, day(3)).unwrap();
    let logged: Vec<String> = engine
        .tasks_in(&View::Logbook, day(3))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert!(logged.contains(&next));
}

#[test]
fn occurrences_sync_with_their_series_and_rule() {
    let a = Database::open_in_memory().unwrap();
    a.enable_sync().unwrap();
    let b = Database::open_in_memory().unwrap();
    b.enable_sync().unwrap();
    let t = task_on(&a, 1, Some(day(2)));
    a.set_task_repeat(&t, Some(rule(Frequency::Daily)), day(2))
        .unwrap();
    let next = a.advance_repeats(day(3)).unwrap().remove(0);

    let engine = a.engine_snapshot().unwrap().unwrap();
    let in_engine = |id: &str| engine.tasks.iter().find(|t| t.id == id).unwrap().clone();
    let (head, new) = (in_engine(&t), in_engine(&next));
    assert_eq!(head.repeat, None);
    assert_eq!(head.series, Some(t.clone()));
    assert_eq!(new.series, Some(t.clone()));
    assert_eq!(new.occurrence, Some(day(3)));
    assert!(new.repeat.is_some());

    for (_, bytes) in a.outgoing_updates(0).unwrap() {
        b.receive_update(&bytes).unwrap();
    }
    let on_b = get(&b, &next);
    assert_eq!(on_b.occurrence, Some(day(3)));
    assert!(on_b.repeat.is_some());
    assert_eq!(get(&b, &t).repeat, None);
    // b, catching up the same day, makes nothing new.
    assert!(b.advance_repeats(day(3)).unwrap().is_empty());
}

#[test]
fn two_devices_derive_the_same_occurrence() {
    let make = || {
        let db = Database::open_in_memory().unwrap();
        let mut task = Task::new(id(1), "shared", 0.0);
        task.start_date = Some(day(2));
        db.add_task(&mut task).unwrap();
        db.set_task_repeat(&task.id, Some(rule(Frequency::Daily)), day(2))
            .unwrap();
        db
    };
    let (a, b) = (make(), make());
    assert_eq!(
        a.advance_repeats(day(3)).unwrap(),
        b.advance_repeats(day(3)).unwrap()
    );
}

fn task_count(db: &Database) -> i64 {
    db.with_raw_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
        .unwrap()
}

fn preview_days(db: &Database, from: Date, until: Date) -> Vec<(Date, String)> {
    db.repeat_previews(from, until)
        .unwrap()
        .into_iter()
        .map(|p| (p.day, p.task.title))
        .collect()
}

#[test]
fn upcoming_previews_future_occurrences_without_creating_them() {
    let db = Database::open_in_memory().unwrap();
    let daily = task_on(&db, 1, Some(day(21)));
    db.set_task_repeat(&daily, Some(rule(Frequency::Daily)), day(21))
        .unwrap();
    let before = task_count(&db);
    assert_eq!(
        preview_days(&db, day(22), day(25)),
        [
            (day(22), "task 1".to_string()),
            (day(23), "task 1".to_string()),
            (day(24), "task 1".to_string())
        ],
        "from the day after its own, up to the window's end"
    );
    assert_eq!(task_count(&db), before, "nothing created");
    // The window can start later than the series.
    assert_eq!(preview_days(&db, day(24), day(25)).len(), 1);
    // Or include the task's own day, which it holds already.
    assert_eq!(
        preview_days(&db, day(21), day(23)),
        [(day(22), "task 1".to_string())]
    );

    // As many as are left.
    let twice = task_on(&db, 2, Some(day(21)));
    let rule2 = RepeatRule {
        left: Some(2),
        ..rule(Frequency::Daily)
    };
    db.set_task_repeat(&twice, Some(rule2), day(21)).unwrap();
    let of_two: Vec<Date> = preview_days(&db, day(22), day(30))
        .into_iter()
        .filter(|(_, title)| title == "task 2")
        .map(|(d, _)| d)
        .collect();
    assert_eq!(of_two, [day(22), day(23)]);
}

#[test]
fn paused_after_completion_and_trashed_series_have_no_preview() {
    let db = Database::open_in_memory().unwrap();
    let paused = task_on(&db, 1, Some(day(21)));
    db.set_task_repeat(
        &paused,
        Some(RepeatRule {
            paused: true,
            ..rule(Frequency::Daily)
        }),
        day(21),
    )
    .unwrap();
    let after = task_on(&db, 2, Some(day(21)));
    db.set_task_repeat(
        &after,
        Some(RepeatRule {
            after_completion: true,
            ..rule(Frequency::Daily)
        }),
        day(21),
    )
    .unwrap();
    let trashed = task_on(&db, 3, Some(day(21)));
    db.set_task_repeat(&trashed, Some(rule(Frequency::Daily)), day(21))
        .unwrap();
    db.trash_task(&trashed).unwrap();
    assert!(preview_days(&db, day(22), day(30)).is_empty());
}

#[test]
fn previews_sit_at_the_end_of_their_day() {
    use matters_core::repeat::{merge_previews, Preview};
    use matters_core::ListRow;
    let dated = |n: u128, title: &str, on: Date| {
        let mut t = Task::new(id(n), title, 0.0);
        t.start_date = Some(on);
        t
    };
    let rows = vec![
        ListRow::Task(dated(1, "a", day(22))),
        ListRow::Task(dated(2, "b", day(24))),
    ];
    let preview = |n: u128, title: &str, on: Date| Preview {
        day: on,
        task: dated(n, title, day(21)),
    };
    let previews = vec![
        preview(13, "z", day(25)),
        preview(12, "y", day(23)),
        preview(11, "x", day(22)),
        preview(10, "w", day(22)),
    ];
    let merged: Vec<String> = merge_previews(rows, previews)
        .iter()
        .map(|row| match row {
            ListRow::Task(t) => t.title.clone(),
            ListRow::Preview(p) => format!("~{} {}", p.task.title, p.day),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        merged,
        [
            "a",
            "~w 2026-11-22",
            "~x 2026-11-22",
            "~y 2026-11-23",
            "b",
            "~z 2026-11-25"
        ]
    );
    let p = preview(11, "x", day(22));
    assert_eq!(p.key(), format!("repeat:{}:2026-11-22", id(11)));
}
