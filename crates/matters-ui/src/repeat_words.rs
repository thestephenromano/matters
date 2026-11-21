//! How a task repeats, in words (#61, #75): "Every 2 weeks on Mon, Thu",
//! "Every month on the last Friday", "Every 3 days after completion, until
//! Dec 1, 2026". Built from whole phrases in [`crate::strings`], so each
//! language can order them, with the locale's day names and dates.

use jiff::civil::{Date, Weekday};
use matters_core::repeat::{Frequency, RepeatRule};

use crate::locale_format::{self, Parts};
use crate::strings as s;

fn weekday(n: i8) -> Option<Weekday> {
    Weekday::from_monday_one_offset(n).ok()
}

/// `rule` in words, in `locale` ("" for the system's).
pub fn describe(rule: &RepeatRule, locale: &str) -> String {
    let every = i64::from(rule.every);
    let mut text = match (&rule.frequency, every) {
        (Frequency::Daily, 1) => s::EVERY_DAY.text(),
        (Frequency::Daily, n) => s::EVERY_N_DAYS.count(n),
        (Frequency::Weekly { .. }, 1) => s::EVERY_WEEK.text(),
        (Frequency::Weekly { .. }, n) => s::EVERY_N_WEEKS.count(n),
        (Frequency::MonthlyOnDay { .. } | Frequency::MonthlyOnWeekday { .. }, 1) => {
            s::EVERY_MONTH.text()
        }
        (Frequency::MonthlyOnDay { .. } | Frequency::MonthlyOnWeekday { .. }, n) => {
            s::EVERY_N_MONTHS.count(n)
        }
        (Frequency::Yearly { .. }, 1) => s::EVERY_YEAR.text(),
        (Frequency::Yearly { .. }, n) => s::EVERY_N_YEARS.count(n),
    };
    if rule.after_completion {
        text = s::AFTER_COMPLETION.fill(&[&text]);
    } else {
        match &rule.frequency {
            Frequency::Daily => {}
            Frequency::Weekly { days } => {
                let mut days = days.clone();
                days.sort();
                days.dedup();
                let names: Vec<String> = days
                    .iter()
                    .filter_map(|d| weekday(*d))
                    .map(|d| locale_format::day_name(locale, d, false))
                    .collect();
                text = s::ON_DAYS.fill(&[&text, &names.join(&s::LIST_SEPARATOR.text())]);
            }
            Frequency::MonthlyOnDay { day: -1 } => text = s::ON_LAST_DAY.fill(&[&text]),
            Frequency::MonthlyOnDay { day } => {
                text = s::ON_DAY_OF_MONTH.fill(&[&text, &day.to_string()])
            }
            Frequency::MonthlyOnWeekday { nth, weekday: w } => {
                let which = match nth {
                    1 => s::ON_FIRST,
                    2 => s::ON_SECOND,
                    3 => s::ON_THIRD,
                    4 => s::ON_FOURTH,
                    5 => s::ON_FIFTH,
                    _ => s::ON_LAST,
                };
                let day = weekday(*w)
                    .map(|d| locale_format::day_name(locale, d, true))
                    .unwrap_or_default();
                text = which.fill(&[&text, &day]);
            }
            Frequency::Yearly { month, day } => {
                // A leap year, so 29 February can be written.
                if let Ok(date) = Date::new(2000, *month, *day) {
                    text = s::ON_DATE
                        .fill(&[&text, &locale_format::date(locale, date, Parts::DAY_MONTH)]);
                }
            }
        }
    }
    if let Some(until) = rule.until {
        text = s::UNTIL.fill(&[
            &text,
            &locale_format::date(locale, until, Parts::DAY_MONTH_YEAR),
        ]);
    }
    text = match rule.left {
        Some(0) => s::ENDING_NOW.fill(&[&text]),
        Some(1) => s::ONCE_MORE.fill(&[&text]),
        Some(n) => s::MORE_TIMES.count_fill(i64::from(n), &[&text]),
        None => text,
    };
    if rule.paused {
        text = s::PAUSED.fill(&[&text]);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn rule(every: u16, frequency: Frequency) -> RepeatRule {
        RepeatRule {
            every,
            frequency,
            after_completion: false,
            start: date(2026, 11, 21),
            until: None,
            left: None,
            paused: false,
        }
    }

    // In American English, untranslated: the phrases as declared.
    fn says(r: RepeatRule) -> String {
        describe(&r, "en_US")
    }

    #[test]
    fn rules_are_described_in_words() {
        assert_eq!(says(rule(1, Frequency::Daily)), "Every day");
        assert_eq!(says(rule(3, Frequency::Daily)), "Every 3 days");
        assert_eq!(
            says(rule(
                2,
                Frequency::Weekly {
                    days: vec![4, 1, 4]
                }
            )),
            "Every 2 weeks on Mon, Thu"
        );
        assert_eq!(
            says(rule(
                1,
                Frequency::Weekly {
                    days: vec![7, 6, 5, 3, 2]
                }
            )),
            "Every week on Tue, Wed, Fri, Sat, Sun"
        );
        assert_eq!(
            says(rule(1, Frequency::MonthlyOnDay { day: 21 })),
            "Every month on day 21"
        );
        assert_eq!(
            says(rule(1, Frequency::MonthlyOnDay { day: -1 })),
            "Every month on the last day"
        );
        for (nth, word) in [
            (1, "first"),
            (2, "second"),
            (3, "third"),
            (4, "fourth"),
            (5, "fifth"),
            (-1, "last"),
        ] {
            assert_eq!(
                says(rule(2, Frequency::MonthlyOnWeekday { nth, weekday: 6 })),
                format!("Every 2 months on the {word} Saturday")
            );
        }
        assert_eq!(
            says(rule(1, Frequency::Yearly { month: 2, day: 29 })),
            "Every year on Feb 29"
        );
        assert_eq!(
            says(rule(2, Frequency::Yearly { month: 7, day: 4 })),
            "Every 2 years on Jul 4"
        );
        assert_eq!(
            says(RepeatRule {
                after_completion: true,
                until: Some(date(2026, 12, 1)),
                ..rule(3, Frequency::Daily)
            }),
            "Every 3 days after completion, until Dec 1, 2026"
        );
        // After completion, the days don't matter.
        assert_eq!(
            says(RepeatRule {
                after_completion: true,
                ..rule(1, Frequency::Weekly { days: vec![1] })
            }),
            "Every week after completion"
        );
        for (left, end) in [
            (0, ", ending with this one"),
            (1, ", once more"),
            (4, ", 4 more times"),
        ] {
            assert_eq!(
                says(RepeatRule {
                    left: Some(left),
                    ..rule(1, Frequency::Daily)
                }),
                format!("Every day{end}")
            );
        }
        assert_eq!(
            says(RepeatRule {
                paused: true,
                ..rule(1, Frequency::Daily)
            }),
            "Every day (paused)"
        );
    }

    #[test]
    fn the_names_and_dates_are_the_locales() {
        assert_eq!(
            describe(&rule(1, Frequency::Weekly { days: vec![1, 3] }), "fr_FR"),
            "Every week on lun., mer."
        );
        assert_eq!(
            describe(&rule(1, Frequency::Yearly { month: 12, day: 24 }), "de_DE"),
            "Every year on 24. Dez."
        );
    }
}
