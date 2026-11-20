//! Repeat rules (#61): the dates a rule produces, checked against
//! python-dateutil's RFC 5545 implementation (`fixtures/repeat_vectors.json`,
//! made by `fixtures/repeat_vectors.py`), and the rules dateutil can't
//! express: repeating after completion, how many are left, and the
//! occurrence ids.

use jiff::civil::{date, Date};
use matters_core::repeat::{occurrence_id, Frequency, RepeatRule};

#[derive(serde::Deserialize)]
struct Vectors {
    generator: String,
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    rule: RepeatRule,
    dates: Vec<Date>,
}

#[test]
fn scheduled_dates_match_dateutil() {
    let vectors: Vectors =
        serde_json::from_str(include_str!("fixtures/repeat_vectors.json")).unwrap();
    assert!(vectors.generator.starts_with("python-dateutil"));
    assert_eq!(vectors.cases.len(), 30);
    for case in &vectors.cases {
        case.rule
            .check()
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        let got: Vec<Date> = if case.rule.until.is_some() {
            case.rule.dates().collect()
        } else {
            case.rule.dates().take(case.dates.len()).collect()
        };
        assert_eq!(got, case.dates, "{}", case.name);
    }
}

fn rule(every: u16, frequency: Frequency, start: Date) -> RepeatRule {
    RepeatRule {
        every,
        frequency,
        after_completion: false,
        start,
        until: None,
        left: None,
        paused: false,
    }
}

#[test]
fn rules_that_cant_be_used_are_refused() {
    let start = date(2026, 11, 21);
    let bad = [
        rule(0, Frequency::Daily, start),
        rule(1000, Frequency::Daily, start),
        rule(1, Frequency::Weekly { days: vec![] }, start),
        rule(1, Frequency::Weekly { days: vec![0] }, start),
        rule(1, Frequency::Weekly { days: vec![8] }, start),
        rule(1, Frequency::MonthlyOnDay { day: 0 }, start),
        rule(1, Frequency::MonthlyOnDay { day: 32 }, start),
        rule(1, Frequency::MonthlyOnDay { day: -2 }, start),
        rule(1, Frequency::MonthlyOnWeekday { nth: 0, weekday: 1 }, start),
        rule(1, Frequency::MonthlyOnWeekday { nth: 6, weekday: 1 }, start),
        rule(1, Frequency::MonthlyOnWeekday { nth: 1, weekday: 8 }, start),
        rule(1, Frequency::Yearly { month: 2, day: 30 }, start),
        rule(1, Frequency::Yearly { month: 13, day: 1 }, start),
        RepeatRule {
            until: Some(date(2026, 11, 20)),
            ..rule(1, Frequency::Daily, start)
        },
    ];
    for r in bad {
        assert!(r.check().is_err(), "{r:?}");
    }
    for good in [
        rule(1, Frequency::Daily, start),
        rule(999, Frequency::Daily, start),
        rule(1, Frequency::Weekly { days: vec![1, 7] }, start),
        rule(1, Frequency::MonthlyOnDay { day: -1 }, start),
        rule(1, Frequency::MonthlyOnDay { day: 31 }, start),
        rule(
            1,
            Frequency::MonthlyOnWeekday {
                nth: -1,
                weekday: 7,
            },
            start,
        ),
        rule(1, Frequency::MonthlyOnWeekday { nth: 5, weekday: 1 }, start),
        rule(1, Frequency::Yearly { month: 2, day: 29 }, start),
        RepeatRule {
            until: Some(start),
            ..rule(1, Frequency::Daily, start)
        },
    ] {
        assert_eq!(good.check(), Ok(()), "{good:?}");
    }
}

#[test]
fn daylight_saving_never_moves_an_occurrence() {
    // Occurrences are calendar dates: the days the clocks change (8 March
    // and 1 November 2026 in New York, 29 March and 25 October in Europe)
    // are days like any other.
    let daily = rule(1, Frequency::Daily, date(2026, 3, 6));
    let march: Vec<Date> = daily.dates().take(4).collect();
    assert_eq!(
        march,
        [
            date(2026, 3, 6),
            date(2026, 3, 7),
            date(2026, 3, 8),
            date(2026, 3, 9)
        ]
    );
    let sundays = rule(1, Frequency::Weekly { days: vec![7] }, date(2026, 10, 18));
    let autumn: Vec<Date> = sundays.dates().take(3).collect();
    assert_eq!(
        autumn,
        [date(2026, 10, 18), date(2026, 10, 25), date(2026, 11, 1)]
    );
    let last_sunday = rule(
        1,
        Frequency::MonthlyOnWeekday {
            nth: -1,
            weekday: 7,
        },
        date(2026, 3, 1),
    );
    assert_eq!(
        last_sunday.dates().take(2).collect::<Vec<_>>(),
        [date(2026, 3, 29), date(2026, 4, 26)]
    );
}

#[test]
fn the_scan_stops_after_its_periods() {
    let daily = rule(1, Frequency::Daily, date(2026, 11, 21));
    assert_eq!(
        daily.dates_within(2).collect::<Vec<_>>(),
        [date(2026, 11, 21), date(2026, 11, 22)]
    );
    assert_eq!(daily.dates_within(0).next(), None);
    // The full scan reaches the calendar's end.
    let late = rule(1, Frequency::Daily, date(9999, 12, 30));
    assert_eq!(
        late.dates().collect::<Vec<_>>(),
        [date(9999, 12, 30), date(9999, 12, 31)]
    );
}

#[test]
fn a_rule_that_never_matches_has_no_dates() {
    // The 31st of every twelfth month from April: April has no 31st.
    let never = rule(12, Frequency::MonthlyOnDay { day: 31 }, date(2026, 4, 1));
    assert_eq!(never.dates().next(), None);
    assert_eq!(never.next_after(date(2026, 4, 1)), None);
}

#[test]
fn the_next_date_and_the_one_due_now() {
    let weekly = rule(
        1,
        Frequency::Weekly { days: vec![1, 4] },
        date(2026, 11, 16),
    );
    // Monday 16th, Thursday 19th, Monday 23rd...
    assert_eq!(
        weekly.next_after(date(2026, 11, 16)),
        Some(date(2026, 11, 19))
    );
    assert_eq!(
        weekly.next_after(date(2026, 11, 20)),
        Some(date(2026, 11, 23))
    );
    assert_eq!(
        weekly.next_after(date(2026, 11, 1)),
        Some(date(2026, 11, 16))
    );
    // Away for two weeks: only the latest missed date is due, not each.
    assert_eq!(
        weekly.latest_due(date(2026, 11, 16), date(2026, 12, 2)),
        Some(date(2026, 11, 30))
    );
    assert_eq!(
        weekly.latest_due(date(2026, 11, 16), date(2026, 11, 18)),
        None,
        "nothing due yet"
    );
    assert_eq!(
        weekly.latest_due(date(2026, 11, 16), date(2026, 11, 19)),
        Some(date(2026, 11, 19)),
        "due on its day"
    );
}

#[test]
fn after_completion_counts_from_the_day_it_was_done() {
    let start = date(2026, 1, 31);
    let after = |frequency, every| RepeatRule {
        after_completion: true,
        ..rule(every, frequency, start)
    };
    let done = date(2026, 1, 31);
    assert_eq!(
        after(Frequency::Daily, 3).after_completed(done),
        Some(date(2026, 2, 3))
    );
    assert_eq!(
        after(Frequency::Weekly { days: vec![6] }, 2).after_completed(done),
        Some(date(2026, 2, 14))
    );
    // A month after 31 January is the end of February.
    assert_eq!(
        after(Frequency::MonthlyOnDay { day: 31 }, 1).after_completed(done),
        Some(date(2026, 2, 28))
    );
    assert_eq!(
        after(Frequency::MonthlyOnWeekday { nth: 1, weekday: 1 }, 2).after_completed(done),
        Some(date(2026, 3, 31))
    );
    // A year after 29 February is 28 February.
    assert_eq!(
        after(Frequency::Yearly { month: 2, day: 29 }, 1).after_completed(date(2028, 2, 29)),
        Some(date(2029, 2, 28))
    );
    let ending = RepeatRule {
        until: Some(date(2026, 2, 2)),
        ..after(Frequency::Daily, 3)
    };
    assert_eq!(ending.after_completed(done), None, "past its end");
    let on_the_end = RepeatRule {
        until: Some(date(2026, 2, 3)),
        ..after(Frequency::Daily, 3)
    };
    assert_eq!(
        on_the_end.after_completed(done),
        Some(date(2026, 2, 3)),
        "its last day is still in"
    );
}

#[test]
fn each_occurrence_passes_on_one_fewer() {
    let r = RepeatRule {
        left: Some(2),
        ..rule(1, Frequency::Daily, date(2026, 11, 21))
    };
    let next = r.passed_on().unwrap();
    assert_eq!(next.left, Some(1));
    assert_eq!(next.passed_on().unwrap().left, Some(0));
    assert_eq!(next.passed_on().unwrap().passed_on(), None, "the last");
    let endless = rule(1, Frequency::Daily, date(2026, 11, 21));
    assert_eq!(endless.passed_on(), Some(endless.clone()));
}

#[test]
fn occurrence_ids_are_fixed_distinct_and_never_created_ids() {
    let series = "6f1c2c3e-2b4d-4c5e-9f60-718293a4b5c6";
    let a = occurrence_id(series, date(2026, 11, 21)).unwrap();
    assert_eq!(
        a,
        occurrence_id(series, date(2026, 11, 21)).unwrap(),
        "fixed"
    );
    // Pinned (computed independently in Python): its time is midnight UTC on
    // 21 November 2026 (0x01a1fb671800 milliseconds), the rest from the
    // series id.
    assert_eq!(a, "01a1fb67-1800-7c5e-9f60-1e9ebf9a9e8b");
    assert_ne!(a, occurrence_id(series, date(2026, 11, 22)).unwrap());
    assert_ne!(
        a,
        occurrence_id("7f1c2c3e-2b4d-4c5e-9f60-718293a4b5c6", date(2026, 11, 21)).unwrap()
    );
    let id = uuid::Uuid::parse_str(&a).unwrap();
    assert_eq!(
        id.get_version_num(),
        7,
        "neither a created v4 nor a conflict copy's v8"
    );
    assert_ne!(a, series);
    // Every day of a decade has its own.
    let mut ids = std::collections::BTreeSet::new();
    let mut day = date(2026, 1, 1);
    while day < date(2036, 1, 1) {
        assert!(ids.insert(occurrence_id(series, day).unwrap()));
        day = day.tomorrow().unwrap();
    }
    assert_eq!(occurrence_id("not a uuid", date(2026, 11, 21)), None);
}

#[test]
fn rules_keep_their_stored_form() {
    let r = RepeatRule {
        after_completion: true,
        until: Some(date(2027, 1, 1)),
        left: Some(3),
        paused: true,
        ..rule(
            2,
            Frequency::MonthlyOnWeekday {
                nth: -1,
                weekday: 5,
            },
            date(2026, 11, 27),
        )
    };
    let text = serde_json::to_string(&r).unwrap();
    assert_eq!(
        text,
        r#"{"every":2,"frequency":{"kind":"monthly_on_weekday","nth":-1,"weekday":5},"after_completion":true,"start":"2026-11-27","until":"2027-01-01","left":3,"paused":true}"#
    );
    assert_eq!(serde_json::from_str::<RepeatRule>(&text).unwrap(), r);
    // Older or shorter forms take the defaults.
    let short: RepeatRule =
        serde_json::from_str(r#"{"every":1,"frequency":{"kind":"daily"},"start":"2026-11-21"}"#)
            .unwrap();
    assert_eq!(short, rule(1, Frequency::Daily, date(2026, 11, 21)));
}

#[test]
fn rules_are_described_in_words() {
    let start = date(2026, 11, 21);
    let says = |r: RepeatRule| r.describe();
    assert_eq!(says(rule(1, Frequency::Daily, start)), "Every day");
    assert_eq!(says(rule(3, Frequency::Daily, start)), "Every 3 days");
    assert_eq!(
        says(rule(
            2,
            Frequency::Weekly {
                days: vec![4, 1, 4]
            },
            start
        )),
        "Every 2 weeks on Mon, Thu"
    );
    assert_eq!(
        says(rule(
            1,
            Frequency::Weekly {
                days: vec![7, 6, 5, 3, 2]
            },
            start
        )),
        "Every week on Tue, Wed, Fri, Sat, Sun"
    );
    for (day, text) in [
        (1, "1st"),
        (2, "2nd"),
        (3, "3rd"),
        (4, "4th"),
        (11, "11th"),
        (12, "12th"),
        (13, "13th"),
        (21, "21st"),
        (22, "22nd"),
        (23, "23rd"),
        (31, "31st"),
    ] {
        assert_eq!(
            says(rule(1, Frequency::MonthlyOnDay { day }, start)),
            format!("Every month on the {text}")
        );
    }
    assert_eq!(
        says(rule(1, Frequency::MonthlyOnDay { day: -1 }, start)),
        "Every month on the last day"
    );
    assert_eq!(
        says(rule(
            1,
            Frequency::MonthlyOnWeekday { nth: 3, weekday: 6 },
            start
        )),
        "Every month on the 3rd Saturday"
    );
    assert_eq!(
        says(rule(
            6,
            Frequency::MonthlyOnWeekday {
                nth: -1,
                weekday: 5
            },
            start
        )),
        "Every 6 months on the last Friday"
    );
    for (w, name) in [
        (1, "Monday"),
        (2, "Tuesday"),
        (3, "Wednesday"),
        (4, "Thursday"),
        (7, "Sunday"),
    ] {
        assert_eq!(
            says(rule(
                1,
                Frequency::MonthlyOnWeekday { nth: 1, weekday: w },
                start
            )),
            format!("Every month on the 1st {name}")
        );
    }
    assert_eq!(
        says(rule(1, Frequency::Yearly { month: 2, day: 29 }, start)),
        "Every year on 29 Feb"
    );
    assert_eq!(
        says(RepeatRule {
            after_completion: true,
            until: Some(date(2026, 12, 1)),
            ..rule(3, Frequency::Daily, start)
        }),
        "Every 3 days after completion, until 1 Dec 2026"
    );
    assert_eq!(
        says(RepeatRule {
            after_completion: true,
            ..rule(1, Frequency::Weekly { days: vec![1] }, start)
        }),
        "Every week after completion",
        "after completion, the days don't matter"
    );
    for (left, end) in [
        (0, ", ending with this one"),
        (1, ", once more"),
        (4, ", 4 more times"),
    ] {
        assert_eq!(
            says(RepeatRule {
                left: Some(left),
                ..rule(1, Frequency::Daily, start)
            }),
            format!("Every day{end}")
        );
    }
    assert_eq!(
        says(RepeatRule {
            paused: true,
            ..rule(2, Frequency::Yearly { month: 11, day: 21 }, start)
        }),
        "Every 2 years on 21 Nov (paused)"
    );
}
