//! Calendar events beside tasks (#62): the days an event is listed on, their
//! order, the days each view lists, and where event rows go among a view's
//! rows. Every date is pinned.

use jiff::civil::{date, Date, DateTime};
use matters_core::calendar::{
    event_days, event_section_key, events_by_day, merge_events, CalendarSource, DayEvent, Event,
    FakeCalendarSource, Unavailable, UPCOMING_EVENT_DAYS,
};
use matters_core::{ListRow, Project, Task, View};

fn at(text: &str) -> DateTime {
    text.parse().unwrap()
}

fn timed(id: &str, title: &str, start: &str, end: &str) -> Event {
    Event {
        id: id.into(),
        calendar_id: "work".into(),
        title: title.into(),
        start: at(start),
        end: at(end),
        all_day: false,
    }
}

/// An all-day event on the days `first` through `last`.
fn all_day(id: &str, title: &str, first: Date, last: Date) -> Event {
    Event {
        id: id.into(),
        calendar_id: "home".into(),
        title: title.into(),
        start: first.at(0, 0, 0, 0),
        end: last.tomorrow().unwrap().at(0, 0, 0, 0),
        all_day: true,
    }
}

fn days(event: &Event) -> Vec<Date> {
    event.days().collect()
}

#[test]
fn a_timed_event_is_listed_on_each_day_it_covers() {
    let standup = timed("s", "Standup", "2026-11-21T09:00", "2026-11-21T09:15");
    assert_eq!(days(&standup), [date(2026, 11, 21)]);

    // Overnight, it is listed on both days.
    let flight = timed("f", "Flight", "2026-11-21T22:00", "2026-11-22T02:00");
    assert_eq!(days(&flight), [date(2026, 11, 21), date(2026, 11, 22)]);

    // Ending at midnight, it doesn't reach the next day.
    let late = timed("l", "Late", "2026-11-21T20:00", "2026-11-22T00:00");
    assert_eq!(days(&late), [date(2026, 11, 21)]);
}

#[test]
fn an_all_day_event_ends_before_the_day_after_its_last() {
    let one = all_day("o", "Holiday", date(2026, 11, 21), date(2026, 11, 21));
    assert_eq!(days(&one), [date(2026, 11, 21)]);

    let three = all_day("t", "Trip", date(2026, 11, 21), date(2026, 11, 23));
    assert_eq!(
        days(&three),
        [date(2026, 11, 21), date(2026, 11, 22), date(2026, 11, 23)]
    );
}

#[test]
fn events_spanning_month_year_and_leap_day_boundaries() {
    let new_year = all_day("n", "Break", date(2026, 12, 30), date(2027, 1, 2));
    assert_eq!(
        days(&new_year),
        [
            date(2026, 12, 30),
            date(2026, 12, 31),
            date(2027, 1, 1),
            date(2027, 1, 2)
        ]
    );
    let leap = timed("p", "Conference", "2028-02-28T10:00", "2028-03-01T12:00");
    assert_eq!(
        days(&leap),
        [date(2028, 2, 28), date(2028, 2, 29), date(2028, 3, 1)]
    );
    let common = timed("c", "Conference", "2027-02-28T10:00", "2027-03-01T12:00");
    assert_eq!(days(&common), [date(2027, 2, 28), date(2027, 3, 1)]);
}

#[test]
fn an_event_without_length_is_listed_on_the_day_it_starts() {
    let reminder = timed("z", "Moment", "2026-11-21T09:00", "2026-11-21T09:00");
    assert_eq!(days(&reminder), [date(2026, 11, 21)]);
    let backwards = timed("b", "Broken", "2026-11-21T09:00", "2026-11-20T09:00");
    assert_eq!(days(&backwards), [date(2026, 11, 21)]);
    let midnight = timed("m", "Midnight", "2026-11-21T00:00", "2026-11-21T00:00");
    assert_eq!(days(&midnight), [date(2026, 11, 21)]);
}

#[test]
fn each_day_lists_all_day_events_first_then_by_start_then_title() {
    let day = date(2026, 11, 21);
    let events = [
        timed("b", "Lunch", "2026-11-21T12:00", "2026-11-21T13:00"),
        timed("a", "Standup", "2026-11-21T09:00", "2026-11-21T09:15"),
        timed("d", "Coffee", "2026-11-21T09:00", "2026-11-21T09:30"),
        all_day("c", "Holiday", day, day),
        // Started yesterday: it continues into the day, so it comes first
        // with the all-day events.
        timed("e", "Night shift", "2026-11-20T22:00", "2026-11-21T06:00"),
        // Starts with the all-day event, and before it by title, yet comes
        // after it.
        timed("f", "Alarm", "2026-11-21T00:00", "2026-11-21T00:30"),
    ];
    let by_day = events_by_day(&events, day, day.tomorrow().unwrap());
    let titles: Vec<&str> = by_day[&day]
        .iter()
        .map(|l| l.event.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Night shift",
            "Holiday",
            "Alarm",
            "Coffee",
            "Standup",
            "Lunch"
        ]
    );
    assert!(by_day[&day][0].continues());
    assert!(!by_day[&day][2].continues());
}

#[test]
fn events_alike_in_start_and_title_are_ordered_by_id() {
    let day = date(2026, 11, 21);
    let events = [
        timed("y", "Standup", "2026-11-21T09:00", "2026-11-21T09:15"),
        timed("x", "Standup", "2026-11-21T09:00", "2026-11-21T09:15"),
    ];
    let by_day = events_by_day(&events, day, day.tomorrow().unwrap());
    let ids: Vec<&str> = by_day[&day].iter().map(|l| l.event.id.as_str()).collect();
    assert_eq!(ids, ["x", "y"]);
}

#[test]
fn events_by_day_keeps_only_the_days_asked_for() {
    let trip = all_day("t", "Trip", date(2026, 11, 20), date(2026, 11, 24));
    let by_day = events_by_day(&[trip], date(2026, 11, 22), date(2026, 11, 24));
    assert_eq!(
        by_day.keys().copied().collect::<Vec<_>>(),
        [date(2026, 11, 22), date(2026, 11, 23)]
    );
    // An event spanning days has a distinct row key on each.
    let keys: Vec<String> = by_day.values().flatten().map(DayEvent::key).collect();
    assert_eq!(keys, ["event:2026-11-22:t", "event:2026-11-23:t"]);
}

#[test]
fn today_lists_todays_events_and_upcoming_the_next_two_weeks() {
    let today = date(2026, 12, 25);
    assert_eq!(
        event_days(&View::Today, today),
        Some((today, date(2026, 12, 26)))
    );
    assert_eq!(UPCOMING_EVENT_DAYS, 14);
    // Tomorrow through 14 days from today, across the year's end.
    assert_eq!(
        event_days(&View::Upcoming, today),
        Some((date(2026, 12, 26), date(2027, 1, 9)))
    );
    for view in [
        View::Inbox,
        View::Anytime,
        View::Someday,
        View::Logbook,
        View::Trash,
    ] {
        assert_eq!(event_days(&view, today), None, "{view:?}");
    }
}

fn task(id: &str, start: Option<Date>) -> ListRow {
    ListRow::Task(Task {
        start_date: start,
        ..Task::new(id, id, 0.0)
    })
}

fn project(id: &str, start: Option<Date>) -> ListRow {
    ListRow::Project(Project {
        id: id.into(),
        title: id.into(),
        start_date: start,
        ..Project::default()
    })
}

fn names(rows: &[ListRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id.clone(),
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
            ListRow::Event(e) => format!("({} {})", e.event.id, e.day),
            ListRow::Preview(p) => format!("~{}", p.key()),
        })
        .collect()
}

#[test]
fn today_lists_its_events_above_its_projects_and_tasks() {
    let today = date(2026, 11, 21);
    let events = [
        timed("standup", "Standup", "2026-11-21T09:00", "2026-11-21T09:15"),
        timed("later", "Tomorrow", "2026-11-22T09:00", "2026-11-22T10:00"),
    ];
    let (from, until) = event_days(&View::Today, today).unwrap();
    let rows = vec![project("due", None), task("a", Some(today))];
    let merged = merge_events(
        &View::Today,
        rows,
        events_by_day(&events, from, until),
        today,
    );
    assert_eq!(names(&merged), ["(standup 2026-11-21)", "<due>", "a"]);
}

#[test]
fn upcoming_lists_events_at_the_head_of_their_day_and_days_with_only_events() {
    let today = date(2026, 11, 21);
    let d = |n: i8| date(2026, 11, 21 + n);
    let events = [
        timed("e1", "Dentist", "2026-11-22T15:00", "2026-11-22T16:00"),
        timed("e2", "Party", "2026-11-24T20:00", "2026-11-24T23:00"),
        all_day("e3", "Trip", d(5), d(6)),
        // Past the last row.
        timed("e4", "Review", "2026-11-30T10:00", "2026-11-30T11:00"),
    ];
    let rows = vec![
        task("a", Some(d(1))),
        project("p", Some(d(3))),
        task("b", Some(d(3))),
        task("c", Some(d(6))),
    ];
    let (from, until) = event_days(&View::Upcoming, today).unwrap();
    let merged = merge_events(
        &View::Upcoming,
        rows,
        events_by_day(&events, from, until),
        today,
    );
    assert_eq!(
        names(&merged),
        [
            "(e1 2026-11-22)",
            "a",
            "(e2 2026-11-24)",
            "<p>",
            "b",
            "(e3 2026-11-26)",
            "(e3 2026-11-27)",
            "c",
            "(e4 2026-11-30)",
        ]
    );
    // Each event sits under its day's heading.
    let sections: Vec<String> = merged
        .iter()
        .filter_map(|row| match row {
            ListRow::Event(e) => Some(event_section_key(&View::Upcoming, e)),
            _ => None,
        })
        .collect();
    assert_eq!(
        sections,
        [
            "date:2026-11-22",
            "date:2026-11-24",
            "date:2026-11-26",
            "date:2026-11-27",
            "date:2026-11-30"
        ]
    );
}

#[test]
fn todays_events_sit_in_its_first_unnamed_section() {
    let listed = DayEvent {
        day: date(2026, 11, 21),
        event: timed("s", "Standup", "2026-11-21T09:00", "2026-11-21T09:15"),
    };
    assert_eq!(event_section_key(&View::Today, &listed), "");
}

#[test]
fn other_views_never_list_events() {
    let today = date(2026, 11, 21);
    let events = [timed(
        "s",
        "Standup",
        "2026-11-21T09:00",
        "2026-11-21T09:15",
    )];
    let by_day = events_by_day(&events, today, today.tomorrow().unwrap());
    for view in [View::Inbox, View::Anytime, View::Someday, View::Logbook] {
        let merged = merge_events(&view, vec![task("a", None)], by_day.clone(), today);
        assert_eq!(names(&merged), ["a"], "{view:?}");
    }
}

#[test]
fn the_fake_source_returns_events_overlapping_the_days_asked_for() {
    let mut source = FakeCalendarSource {
        events: vec![
            timed("in", "In", "2026-11-21T09:00", "2026-11-21T10:00"),
            timed("before", "Before", "2026-11-19T09:00", "2026-11-19T10:00"),
            all_day("spans", "Spans", date(2026, 11, 19), date(2026, 11, 22)),
            timed("after", "After", "2026-11-23T00:00", "2026-11-23T01:00"),
        ],
        ..FakeCalendarSource::default()
    };
    let ids: Vec<String> = source
        .events(date(2026, 11, 21), date(2026, 11, 23))
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids, ["in", "spans"]);
}

#[test]
fn the_fake_source_lists_its_calendars() {
    let work = matters_core::calendar::Calendar {
        id: "work".into(),
        name: "Work".into(),
        color: Some("#3584e4".into()),
    };
    let mut source = FakeCalendarSource {
        calendars: vec![work.clone()],
        ..FakeCalendarSource::default()
    };
    assert_eq!(source.calendars(), Ok(vec![work]));
}

#[test]
fn an_unavailable_source_says_why() {
    let mut source = FakeCalendarSource::unavailable("No calendar service is running.");
    let reason = Unavailable("No calendar service is running.".into());
    assert_eq!(source.calendars(), Err(reason.clone()));
    assert_eq!(
        source.events(date(2026, 11, 21), date(2026, 11, 22)),
        Err(reason.clone())
    );
    assert_eq!(reason.to_string(), "No calendar service is running.");
}

#[test]
fn every_calendar_is_shown_until_hidden() {
    use matters_core::CalendarSettings;
    let mut settings = CalendarSettings::default();
    assert!(settings.show_events);
    assert!(settings.shows("work") && settings.shows("home"));

    settings.set_shown("work", false);
    assert!(!settings.shows("work") && settings.is_hidden("work"));
    assert!(settings.shows("home"));
    // Hiding twice records it once; showing undoes it.
    settings.set_shown("work", false);
    assert_eq!(settings.hidden, ["work"]);
    settings.set_shown("work", true);
    assert!(settings.shows("work") && settings.hidden.is_empty());

    // Turning events off hides every calendar, but remembers which were
    // chosen.
    settings.set_shown("home", false);
    settings.show_events = false;
    assert!(!settings.shows("work") && !settings.shows("home"));
    assert!(!settings.is_hidden("work") && settings.is_hidden("home"));
}

#[test]
fn calendar_choices_survive_a_save_and_old_files_show_events() {
    use matters_core::{Settings, SettingsFile};
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    let mut settings = Settings::default();
    settings.calendar.set_shown("birthdays", false);
    file.save(&settings).unwrap();
    assert_eq!(file.load(), (settings, None));

    // A file from before #62 has no calendar table: events are shown.
    std::fs::write(file.path(), "[logbook]\npolicy = \"daily\"\n").unwrap();
    let (loaded, problem) = file.load();
    assert_eq!(problem, None);
    assert!(loaded.calendar.show_events && loaded.calendar.hidden.is_empty());
}
