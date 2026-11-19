//! Typed "When" dates (#59): what each phrase means, pinned against a fixed
//! day so the behaviour of the `interim` crate underneath can't drift
//! unnoticed.

use jiff::civil::Date;
use matters_core::{DateOrder, Schedule};

/// Wednesday 18 November 2026.
fn today() -> Date {
    jiff::civil::date(2026, 11, 18)
}

fn date(m: i8, d: i8) -> Option<Schedule> {
    Some(Schedule::Date(jiff::civil::date(2026, m, d)))
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
    let jan5 = Some(Schedule::Date(jiff::civil::date(2027, 1, 5)));
    for input in ["jan 5", "January 5", "5 jan", "1/5"] {
        assert_eq!(parse(input), jan5, "{input:?}");
    }
    // Today's own month and day is today, not next year; so are other
    // phrases that name it.
    for input in [
        "nov 18",
        "18 november",
        "11/18",
        "2026-11-18",
        "0 days",
        "in 0 days",
        "wed",
    ] {
        assert_eq!(
            parse(input),
            Some(Schedule::Today { evening: false }),
            "{input:?}"
        );
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
        // Times typed as midnight, which `interim` gives like a bare date.
        "12am",
        "0:00",
        "00:00",
        "tomorrow 12am",
        "12 am",
        "noon",
        "in banana",
        "in week", // `in` only before a number
        // Phrases that name no day, which `interim` reads as today (#134).
        "week",
        "weeks",
        "month",
        "year",
        "day",
        "days",
        "hour",
        "minute",
        "second",
        "sec",
        "d",
        "w",
        "m",
        "y",
        "mo",
        "now",
        "this week",
        "this month",
        "this year",
        "this day",
        "weekend",
        "this weekend",
        "morning",
        "midnight",
    ] {
        assert_eq!(parse(input), None, "{input:?} should be refused");
    }
}
