//! Typed "When" dates (#59): what each phrase means, pinned against a fixed
//! day so the behaviour of the `interim` crate underneath can't drift
//! unnoticed.

use chrono::NaiveDate;
use matters_core::{DateOrder, Schedule};

/// Wednesday 18 November 2026.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 18).unwrap()
}

fn date(m: u32, d: u32) -> Option<Schedule> {
    Some(Schedule::Date(NaiveDate::from_ymd_opt(2026, m, d).unwrap()))
}

fn parse(input: &str) -> Option<Schedule> {
    Schedule::parse_typed(input, today(), DateOrder::MonthFirst)
}

#[test]
fn matters_own_words() {
    assert_eq!(parse("today"), Some(Schedule::Today { evening: false }));
    for evening in ["tonight", "evening", "this evening", "  This   Evening "] {
        assert_eq!(
            parse(evening),
            Some(Schedule::Today { evening: true }),
            "{evening:?}"
        );
    }
    assert_eq!(parse("Someday"), Some(Schedule::Someday));
}

#[test]
fn relative_days() {
    let cases = [
        ("tomorrow", date(11, 19)),
        ("fri", date(11, 20)),
        ("Friday", date(11, 20)),
        ("next friday", date(11, 20)), // the coming Friday, as interim reads it
        ("monday", date(11, 23)),
        ("in 3 days", date(11, 21)),
        ("3 days", date(11, 21)),
        ("in 2 weeks", date(12, 2)),
        ("next week", date(11, 25)),
        ("next month", date(12, 18)),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input), expected, "{input:?}");
    }
    // Today's weekday names today.
    assert_eq!(parse("wednesday"), Some(Schedule::Today { evening: false }));
}

#[test]
fn calendar_dates() {
    for input in ["dec 24", "24 dec", "December 24", "2026-12-24"] {
        assert_eq!(parse(input), date(12, 24), "{input:?}");
    }
    // A month and day already past this year means next year.
    let jan5 = Some(Schedule::Date(NaiveDate::from_ymd_opt(2027, 1, 5).unwrap()));
    for input in ["jan 5", "January 5", "5 jan", "1/5"] {
        assert_eq!(parse(input), jan5, "{input:?}");
    }
}

#[test]
fn numeric_dates_follow_the_locale_order() {
    assert_eq!(
        Schedule::parse_typed("12/24", today(), DateOrder::MonthFirst),
        date(12, 24)
    );
    assert_eq!(
        Schedule::parse_typed("24/12", today(), DateOrder::DayFirst),
        date(12, 24)
    );
    assert_eq!(
        Schedule::parse_typed("12/24/2026", today(), DateOrder::MonthFirst),
        date(12, 24)
    );
}

#[test]
fn anything_unclear_is_refused() {
    for input in [
        "",
        "  ",
        "banana",
        "next tuesday-ish",
        "yesterday",
        "last friday",
        "3 days ago",
        "2026-11-10",
        "tomorrow 5pm",
        "noon",
        "in banana",
    ] {
        assert_eq!(parse(input), None, "{input:?} should be refused");
    }
}
