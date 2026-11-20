//! Reminders (#60): when a task is due for one, that each is shown once,
//! and that finishing or trashing a task cancels it. The clock and time zone
//! are always given, never read: these are the "injected clock" tests,
//! covering restarts, suspend (time jumping ahead), clocks set back, time
//! zone changes and daylight-saving transitions.

use jiff::civil::{date, time, Date, Time};
use jiff::tz::{Offset, TimeZone};
use jiff::Timestamp;
use matters_core::reminders::{due, parse_stored_time, stored_time, to_minute, Reminder};
use matters_core::{Database, Project, Schedule, Task, TaskSnapshot};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn day() -> Date {
    date(2026, 11, 21)
}

/// UTC+1, fixed, so instants are easy to read.
fn zone() -> TimeZone {
    TimeZone::fixed(Offset::constant(1))
}

/// The instant of `hh:mm` on `on`, in `zone()`.
fn at(on: Date, hour: i8, minute: i8) -> Timestamp {
    on.at(hour, minute, 0, 0)
        .to_zoned(zone())
        .unwrap()
        .timestamp()
}

fn ms(t: Timestamp) -> i64 {
    t.as_millisecond()
}

/// Task `n` on `on`, reminded at `reminder`.
fn reminded(db: &Database, n: u128, on: Date, reminder: Time) -> String {
    let mut task = Task::new(id(n), format!("task {n}"), n as f64);
    task.start_date = Some(on);
    db.add_task(&mut task).unwrap();
    db.set_task_reminder(&task.id, Some(reminder)).unwrap();
    task.id
}

fn due_ids(db: &Database, now: Timestamp) -> Vec<String> {
    let (due, _) = db.reminders_due(now, &zone()).unwrap();
    due.into_iter().map(|r| r.task_id).collect()
}

/// Show what is due at `now`, as a frontend does.
fn show(db: &Database, now: Timestamp) -> Vec<Reminder> {
    let (due, _) = db.reminders_due(now, &zone()).unwrap();
    db.mark_reminders_shown(&due, ms(now)).unwrap();
    due
}

#[test]
fn times_are_stored_to_the_minute() {
    assert_eq!(stored_time(time(9, 5, 0, 0)), "09:05");
    assert_eq!(stored_time(time(23, 59, 59, 0)), "23:59");
    assert_eq!(parse_stored_time("09:05"), Some(time(9, 5, 0, 0)));
    assert_eq!(parse_stored_time("00:00"), Some(Time::midnight()));
    for bad in [
        "9:05", "09:5", "24:00", "09:60", "aa:bb", "09:05:00", "", "0905",
    ] {
        assert_eq!(parse_stored_time(bad), None, "{bad:?}");
    }
    assert_eq!(to_minute(time(9, 5, 42, 7)), time(9, 5, 0, 0));
}

#[test]
fn a_task_is_due_for_its_reminder_only_while_open_dated_and_live() {
    let mut task = Task::new(id(1), "Call", 0.0);
    assert_eq!(due(&task), None, "no reminder");
    task.reminder = Some(time(9, 30, 15, 0));
    assert_eq!(due(&task), None, "no date");
    task.start_date = Some(day());
    assert_eq!(due(&task), Some(day().at(9, 30, 0, 0)), "to the minute");

    let mut someday = task.clone();
    someday.is_someday = true;
    assert_eq!(due(&someday), None);
    let mut done = task.clone();
    done.status = matters_core::Status::Completed;
    assert_eq!(due(&done), None);
    let mut canceled = task.clone();
    canceled.status = matters_core::Status::Canceled;
    assert_eq!(due(&canceled), None);
    let mut trashed = task.clone();
    trashed.trashed_at = Some(1);
    assert_eq!(due(&trashed), None);
}

#[test]
fn a_reminder_is_due_from_its_time_and_shown_once() {
    let db = Database::open_in_memory().unwrap();
    let call = reminded(&db, 1, day(), time(9, 0, 0, 0));

    let (due, next) = db.reminders_due(at(day(), 8, 59), &zone()).unwrap();
    assert!(due.is_empty(), "not yet");
    assert_eq!(next, Some(at(day(), 9, 0)), "the next one");

    let shown = show(&db, at(day(), 9, 0));
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].task_id, call);
    assert_eq!(shown[0].title, "task 1");
    assert_eq!(shown[0].due, day().at(9, 0, 0, 0));
    assert_eq!(shown[0].due_at, at(day(), 9, 0));
    assert!(!shown[0].is_late(at(day(), 9, 0)));

    // Never again: later, or after the clock is set back.
    assert!(due_ids(&db, at(day(), 9, 1)).is_empty());
    assert!(due_ids(&db, at(day(), 8, 0)).is_empty());
    assert!(due_ids(&db, at(day(), 23, 0)).is_empty());
    let (_, next) = db.reminders_due(at(day(), 8, 0), &zone()).unwrap();
    assert_eq!(next, None);
    // Showing it twice records it once.
    db.mark_reminders_shown(&shown, ms(at(day(), 9, 2)))
        .unwrap();
}

#[test]
fn a_shown_reminder_stays_shown_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    {
        let db = Database::open(&path).unwrap();
        reminded(&db, 1, day(), time(9, 0, 0, 0));
        reminded(&db, 2, day(), time(10, 0, 0, 0));
        assert_eq!(show(&db, at(day(), 9, 0)).len(), 1);
    }
    let db = Database::open(&path).unwrap();
    assert!(due_ids(&db, at(day(), 9, 30)).is_empty());
    assert_eq!(due_ids(&db, at(day(), 10, 0)), [id(2)]);
}

#[test]
fn a_reminder_missed_while_closed_or_asleep_is_due_late() {
    let db = Database::open_in_memory().unwrap();
    reminded(&db, 1, day(), time(9, 0, 0, 0));
    reminded(&db, 2, day().yesterday().unwrap(), time(18, 0, 0, 0));
    // The computer slept from 08:00 to 11:00: both are due at once, the
    // earliest first, and both are late.
    let woke = at(day(), 11, 0);
    let due = show(&db, woke);
    assert_eq!(
        due.iter().map(|r| r.task_id.clone()).collect::<Vec<_>>(),
        [id(2), id(1)]
    );
    assert!(due.iter().all(|r| r.is_late(woke)));
    // A minute is the line between on time and late.
    let r = &due[1];
    assert!(!r.is_late(at(day(), 9, 1)));
    assert!(r.is_late(
        at(day(), 9, 1)
            .checked_add(jiff::SignedDuration::from_secs(1))
            .unwrap()
    ));
}

#[test]
fn reminders_at_the_same_time_are_ordered_by_task() {
    let db = Database::open_in_memory().unwrap();
    reminded(&db, 3, day(), time(9, 0, 0, 0));
    reminded(&db, 1, day(), time(9, 0, 0, 0));
    reminded(&db, 2, day(), time(8, 0, 0, 0));
    assert_eq!(due_ids(&db, at(day(), 9, 0)), [id(2), id(1), id(3)]);
}

#[test]
fn finishing_trashing_or_undating_a_task_cancels_its_reminder() {
    let db = Database::open_in_memory().unwrap();
    for n in 1..=6 {
        reminded(&db, n, day(), time(9, 0, 0, 0));
    }
    let mut garden = Project::new(id(10), "Garden", 0.0);
    db.add_project(&mut garden).unwrap();
    db.set_task_home(
        &id(6),
        &matters_core::Home::Project {
            project: id(10),
            heading: None,
        },
    )
    .unwrap();
    db.complete_task(&id(1)).unwrap();
    db.cancel_task(&id(2)).unwrap();
    db.trash_task(&id(3)).unwrap();
    db.set_task_schedule(&id(4), &Schedule::Someday, day())
        .unwrap();
    db.set_task_schedule(&id(5), &Schedule::None, day())
        .unwrap();
    db.trash_project(&id(10)).unwrap();
    let now = at(day(), 9, 0);
    assert!(due_ids(&db, now).is_empty(), "every one is cancelled");

    // Reopened before it was shown, it is due again.
    db.uncomplete_task(&id(1)).unwrap();
    db.restore_task(&id(3)).unwrap();
    assert_eq!(due_ids(&db, now), [id(1), id(3)]);
}

#[test]
fn a_new_time_or_day_is_a_new_reminder() {
    let db = Database::open_in_memory().unwrap();
    let call = reminded(&db, 1, day(), time(9, 0, 0, 0));
    show(&db, at(day(), 9, 0));
    db.set_task_reminder(&call, Some(time(15, 0, 0, 0)))
        .unwrap();
    assert!(due_ids(&db, at(day(), 14, 59)).is_empty());
    assert_eq!(show(&db, at(day(), 15, 0)).len(), 1);
    // Moved to tomorrow, the same time is due tomorrow.
    let tomorrow = day().tomorrow().unwrap();
    db.set_task_schedule(&call, &Schedule::Date(tomorrow), day())
        .unwrap();
    assert!(due_ids(&db, at(day(), 23, 0)).is_empty());
    assert_eq!(
        due_ids(&db, at(tomorrow, 15, 0)),
        std::slice::from_ref(&call)
    );
    // Cleared, nothing is due.
    db.set_task_reminder(&call, None).unwrap();
    assert!(due_ids(&db, at(tomorrow, 16, 0)).is_empty());
    assert_eq!(db.get_task(&call).unwrap().unwrap().reminder, None);
}

#[test]
fn a_reminder_follows_the_wall_clock_across_time_zones() {
    let db = Database::open_in_memory().unwrap();
    reminded(&db, 1, day(), time(9, 0, 0, 0));
    let tokyo = TimeZone::fixed(Offset::constant(9));
    let lisbon = TimeZone::fixed(Offset::constant(0));
    // 09:00 local is a different instant in each zone.
    let (_, next_tokyo) = db.reminders_due(at(day(), 0, 0), &tokyo).unwrap();
    let (_, next_lisbon) = db.reminders_due(at(day(), 0, 0), &lisbon).unwrap();
    assert_eq!(
        next_tokyo,
        Some(
            day()
                .at(9, 0, 0, 0)
                .to_zoned(tokyo.clone())
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        next_lisbon,
        Some(
            day()
                .at(9, 0, 0, 0)
                .to_zoned(lisbon.clone())
                .unwrap()
                .timestamp()
        )
    );

    // Shown at 09:00 in Tokyo, it isn't due again after flying to Lisbon,
    // where 09:00 comes nine hours later.
    let shown_at = day()
        .at(9, 0, 0, 0)
        .to_zoned(tokyo.clone())
        .unwrap()
        .timestamp();
    let (due, _) = db.reminders_due(shown_at, &tokyo).unwrap();
    db.mark_reminders_shown(&due, ms(shown_at)).unwrap();
    let later = day()
        .at(9, 30, 0, 0)
        .to_zoned(lisbon.clone())
        .unwrap()
        .timestamp();
    assert!(db.reminders_due(later, &lisbon).unwrap().0.is_empty());
}

#[test]
fn daylight_saving_gaps_and_repeats_have_one_instant() {
    let new_york = TimeZone::get("America/New_York").unwrap();
    let db = Database::open_in_memory().unwrap();
    // 02:30 doesn't happen on 8 March 2026: due just after the gap, at
    // 03:30 EDT (07:30 UTC).
    reminded(&db, 1, date(2026, 3, 8), time(2, 30, 0, 0));
    // 01:30 happens twice on 1 November 2026: due the first time, 01:30
    // EDT (05:30 UTC).
    reminded(&db, 2, date(2026, 11, 1), time(1, 30, 0, 0));
    let (due, _) = db
        .reminders_due("2026-11-02T00:00Z".parse().unwrap(), &new_york)
        .unwrap();
    let instants: Vec<String> = due.iter().map(|r| r.due_at.to_string()).collect();
    assert_eq!(instants, ["2026-03-08T07:30:00Z", "2026-11-01T05:30:00Z"]);
}

#[test]
fn copies_and_undo_keep_the_reminder() {
    let db = Database::open_in_memory().unwrap();
    let call = reminded(&db, 1, day(), time(9, 0, 0, 0));
    let copy = db.duplicate_task(&call).unwrap();
    assert_eq!(
        db.get_task(&copy).unwrap().unwrap().reminder,
        Some(time(9, 0, 0, 0))
    );
    // The copy is a reminder of its own, shown with the original.
    let mut both = vec![call.clone(), copy.clone()];
    both.sort();
    let mut shown: Vec<String> = show(&db, at(day(), 9, 0))
        .into_iter()
        .map(|r| r.task_id)
        .collect();
    shown.sort();
    assert_eq!(shown, both);
    assert!(due_ids(&db, at(day(), 9, 1)).is_empty());

    let snapshot = TaskSnapshot::take(&db, std::slice::from_ref(&call)).unwrap();
    db.set_task_reminder(&call, None).unwrap();
    snapshot.restore(&db, day()).unwrap();
    assert_eq!(
        db.get_task(&call).unwrap().unwrap().reminder,
        Some(time(9, 0, 0, 0))
    );
}

#[test]
fn a_reminder_shown_for_a_task_since_deleted_is_ignored() {
    let db = Database::open_in_memory().unwrap();
    let call = reminded(&db, 1, day(), time(9, 0, 0, 0));
    let (due, _) = db.reminders_due(at(day(), 9, 0), &zone()).unwrap();
    db.trash_task(&call).unwrap();
    db.empty_trash().unwrap();
    // Recording it after the task is gone changes nothing.
    db.mark_reminders_shown(&due, ms(at(day(), 9, 0))).unwrap();
}

#[test]
fn emptying_the_trash_removes_a_shown_reminders_record() {
    let db = Database::open_in_memory().unwrap();
    let call = reminded(&db, 1, day(), time(9, 0, 0, 0));
    show(&db, at(day(), 9, 0));
    db.trash_task(&call).unwrap();
    assert_eq!(db.empty_trash().unwrap(), 1);
    let left: i64 = db
        .with_raw_conn(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM reminder_deliveries", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn reminders_sync_but_what_each_device_showed_does_not() {
    let a = Database::open_in_memory().unwrap();
    a.enable_sync().unwrap();
    let b = Database::open_in_memory().unwrap();
    b.enable_sync().unwrap();
    let call = reminded(&a, 1, day(), time(9, 0, 0, 0));
    // A reminder set when the task is created is recorded too.
    let mut later = Task::new(id(2), "later", 2.0);
    later.start_date = Some(day());
    later.reminder = Some(time(10, 0, 0, 0));
    a.add_task(&mut later).unwrap();
    show(&a, at(day(), 9, 0));

    let engine = a.engine_snapshot().unwrap().unwrap();
    let reminder_of = |n: u128| {
        engine
            .tasks
            .iter()
            .find(|t| t.id == id(n))
            .unwrap()
            .reminder
    };
    assert_eq!(reminder_of(1), Some(time(9, 0, 0, 0)));
    assert_eq!(reminder_of(2), Some(time(10, 0, 0, 0)));

    for (_, bytes) in a.outgoing_updates(0).unwrap() {
        b.receive_update(&bytes).unwrap();
    }
    assert_eq!(
        b.get_task(&call).unwrap().unwrap().reminder,
        Some(time(9, 0, 0, 0))
    );
    // b shows its own.
    assert_eq!(due_ids(&b, at(day(), 9, 0)), std::slice::from_ref(&call));

    // Cleared on b, cleared on a.
    let sent = b.outgoing_updates(0).unwrap().len();
    b.set_task_reminder(&call, None).unwrap();
    let updates = b.outgoing_updates(0).unwrap();
    assert_eq!(updates.len(), sent + 1);
    a.receive_update(&updates.last().unwrap().1).unwrap();
    assert_eq!(a.get_task(&call).unwrap().unwrap().reminder, None);
}

#[test]
fn receiving_an_update_keeps_what_this_device_showed() {
    let a = Database::open_in_memory().unwrap();
    a.enable_sync().unwrap();
    let b = Database::open_in_memory().unwrap();
    b.enable_sync().unwrap();
    let call = reminded(&a, 1, day(), time(9, 0, 0, 0));
    let gone = reminded(&a, 2, day(), time(9, 0, 0, 0));
    let mut sent = 0;
    for (seq, bytes) in a.outgoing_updates(0).unwrap() {
        b.receive_update(&bytes).unwrap();
        sent = seq;
    }
    assert_eq!(show(&b, at(day(), 9, 0)).len(), 2);

    // Any update from a rebuilds b's rows from the engine: b's record of
    // what it showed survives it, for the tasks that still exist.
    a.rename_task(&call, "renamed").unwrap();
    a.trash_task(&gone).unwrap();
    a.empty_trash().unwrap();
    for (_, bytes) in a.outgoing_updates(sent).unwrap() {
        b.receive_update(&bytes).unwrap();
    }
    assert_eq!(b.get_task(&call).unwrap().unwrap().title, "renamed");
    assert!(b.get_task(&gone).unwrap().is_none());
    assert!(due_ids(&b, at(day(), 9, 30)).is_empty(), "not shown again");
    let kept: Vec<String> = b
        .with_raw_conn(|c| {
            let mut stmt = c.prepare("SELECT task_id FROM reminder_deliveries")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .unwrap();
    assert_eq!(
        kept,
        [call.clone()],
        "the deleted task's record went with it"
    );
}

#[test]
fn typed_times_are_understood_or_refused() {
    use matters_core::reminders::parse_typed_time;
    let t = |h, m| Some(time(h, m, 0, 0));
    for (text, want) in [
        ("9", t(9, 0)),
        ("9:30", t(9, 30)),
        ("09:30", t(9, 30)),
        ("21:30", t(21, 30)),
        ("0:00", t(0, 0)),
        ("23:59", t(23, 59)),
        ("9am", t(9, 0)),
        ("9 am", t(9, 0)),
        ("9:30pm", t(21, 30)),
        (" 9:30 PM ", t(21, 30)),
        ("12am", t(0, 0)),
        ("12:15am", t(0, 15)),
        ("12pm", t(12, 0)),
        ("1pm", t(13, 0)),
        ("noon", t(12, 0)),
        ("Midnight", t(0, 0)),
    ] {
        assert_eq!(parse_typed_time(text), want, "{text:?}");
    }
    for refused in [
        "", "24:00", "9:60", "9:5", "930", "0am", "13pm", "9 :30", "nine", "9:30:00", "-1", "+9",
        "9.30", "am", "9a", "１:00",
    ] {
        assert_eq!(parse_typed_time(refused), None, "{refused:?}");
    }
}

#[test]
fn a_reminder_on_a_task_without_a_date_schedules_it_for_today() {
    let db = Database::open_in_memory().unwrap();
    let mut inbox = Task::new(id(1), "undated", 1.0);
    db.add_task(&mut inbox).unwrap();
    let mut someday = Task::new(id(2), "someday", 2.0);
    someday.is_someday = true;
    db.add_task(&mut someday).unwrap();
    let mut evening = Task::new(id(3), "evening", 3.0);
    let later = day().tomorrow().unwrap();
    evening.start_date = Some(later);
    evening.time_of_day = Some("evening".into());
    db.add_task(&mut evening).unwrap();

    for n in 1..=3 {
        db.remind_task_at(&id(n), time(18, 45, 30, 0), day())
            .unwrap();
    }
    let task = |n| db.get_task(&id(n)).unwrap().unwrap();
    for n in [1, 2] {
        let t = task(n);
        assert_eq!(t.start_date, Some(day()), "task {n} is due today");
        assert!(!t.is_someday && !t.is_evening());
        assert_eq!(t.reminder, Some(time(18, 45, 0, 0)), "to the minute");
    }
    // A dated task keeps its date and This Evening.
    let t = task(3);
    assert_eq!(t.start_date, Some(later));
    assert!(t.is_evening());
    assert_eq!(due(&t), Some(later.at(18, 45, 0, 0)));

    assert!(db.remind_task_at(&id(99), time(9, 0, 0, 0), day()).is_err());
}

#[test]
fn reminding_records_the_schedule_and_the_time_in_the_engine() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    let mut inbox = Task::new(id(1), "undated", 1.0);
    db.add_task(&mut inbox).unwrap();
    db.remind_task_at(&id(1), time(8, 0, 0, 0), day()).unwrap();
    let engine = db.engine_snapshot().unwrap().unwrap();
    let t = engine.tasks.iter().find(|t| t.id == id(1)).unwrap();
    assert_eq!(t.start_date, Some(day()));
    assert_eq!(t.reminder, Some(time(8, 0, 0, 0)));
}
