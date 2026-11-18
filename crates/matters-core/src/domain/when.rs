use interim::Dialect;
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

/// How numeric dates typed by the user are read: `12/24` is month first
/// (US), `24/12` day first. Frontends choose it from the user's locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateOrder {
    MonthFirst,
    DayFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeOfDay {
    Anytime,
    Evening,
}

impl TimeOfDay {
    pub fn as_db_str(&self) -> Option<&'static str> {
        match self {
            TimeOfDay::Anytime => None,
            TimeOfDay::Evening => Some("evening"),
        }
    }

    pub fn from_db_str(s: Option<&str>) -> Self {
        match s {
            Some("evening") => TimeOfDay::Evening,
            _ => TimeOfDay::Anytime,
        }
    }
}

/// The "When" representation:
/// A task can be scheduled for Today (daytime or evening), on a specific calendar date,
/// marked for Someday, or unscheduled (Inbox / Anytime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Schedule {
    Today { evening: bool },
    Date(Date),
    Someday,
    None,
}

impl Schedule {
    /// Parse the textual "When" a frontend sends: `today`, `evening`,
    /// `someday`, `none`, or an ISO `YYYY-MM-DD` date. Anything else is `None`
    /// (not a schedule), so callers cannot misread an unknown word as a date.
    pub fn parse(s: &str) -> Option<Schedule> {
        match s.trim() {
            "today" => Some(Schedule::Today { evening: false }),
            "evening" => Some(Schedule::Today { evening: true }),
            "someday" => Some(Schedule::Someday),
            "none" => Some(Schedule::None),
            other => crate::clock::parse_iso_date(other).map(Schedule::Date),
        }
    }

    /// Parse a "When" the user typed (#59): Matters' own words (`today`,
    /// `tonight`, `evening`, `this evening`, `someday`), or a date phrase such
    /// as `tomorrow`, `fri`, `in 3 days`, `dec 24` or `12/24`, read with the
    /// `interim` crate relative to `today`. `order` says how numeric dates
    /// like `12/24` are read.
    ///
    /// Anything that isn't clearly a day is refused (`None`), never guessed:
    /// unknown words, dates before `today`, and phrases with a time of day
    /// (times belong to reminders, #60). A date phrase meaning `today` is
    /// Today. Case and extra spaces don't matter.
    ///
    /// On top of `interim`: `in 3 days` is read as `3 days`, and a month and
    /// day without a year (`jan 5`, `1/5`) that has passed this year means
    /// next year. A weekday names the coming one, or today on that weekday.
    pub fn parse_typed(input: &str, today: Date, order: DateOrder) -> Option<Schedule> {
        let text = input
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        match text.as_str() {
            "" => return None,
            "today" => return Some(Schedule::Today { evening: false }),
            "tonight" | "evening" | "this evening" => {
                return Some(Schedule::Today { evening: true })
            }
            "someday" => return Some(Schedule::Someday),
            _ => {}
        }
        // `interim` reads "3 days" but not "in 3 days".
        let text = match text.strip_prefix("in ") {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest.to_string(),
            _ => text,
        };
        let midnight = today.to_zoned(TimeZone::UTC).ok()?;
        let dialect = match order {
            DateOrder::MonthFirst => Dialect::Us,
            DateOrder::DayFirst => Dialect::Uk,
        };
        let parsed = interim::parse_date_string(&text, midnight, dialect).ok()?;
        if parsed.time() != Time::midnight() {
            return None; // a time of day
        }
        let mut date = parsed.date();
        if date < today && names_a_day_of_the_year(&text) {
            // "jan 5" in November. A 29 February with no such day next
            // year is refused, not moved.
            date = date.with().year(date.year() + 1).build().ok()?;
        }
        match date.cmp(&today) {
            std::cmp::Ordering::Less => None,
            std::cmp::Ordering::Equal => Some(Schedule::Today { evening: false }),
            std::cmp::Ordering::Greater => Some(Schedule::Date(date)),
        }
    }

    /// Parse a typed deadline (#20): the same phrases as
    /// [`Schedule::parse_typed`], but only actual days. `today` and date
    /// phrases are accepted; This Evening and Someday aren't deadlines, and
    /// past days, times and anything unclear are refused.
    pub fn parse_typed_deadline(input: &str, today: Date, order: DateOrder) -> Option<Date> {
        match Schedule::parse_typed(input, today, order)? {
            Schedule::Today { evening: false } => Some(today),
            Schedule::Date(date) => Some(date),
            Schedule::Today { evening: true } | Schedule::Someday | Schedule::None => None,
        }
    }

    pub fn is_someday(&self) -> bool {
        matches!(self, Schedule::Someday)
    }

    pub fn to_db_fields(&self, today: Date) -> (Option<Date>, Option<&'static str>, bool) {
        match self {
            Schedule::Today { evening } => (
                Some(today),
                if *evening { Some("evening") } else { None },
                false,
            ),
            Schedule::Date(d) => (Some(*d), None, false),
            Schedule::Someday => (None, None, true),
            Schedule::None => (None, None, false),
        }
    }
}

/// Whether `text` names a day of the year without a year: it contains a
/// month name (`jan 5`, `5 january`) or is a numeric `month/day` pair. Such a
/// day is the next one to come, unlike relative phrases (`yesterday`,
/// `last friday`), which keep their meaning.
fn names_a_day_of_the_year(text: &str) -> bool {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let month_name = tokens.iter().any(|t| {
        let word = t.trim_matches(|c: char| !c.is_alphabetic());
        word.len() >= 3 && MONTHS.iter().any(|m| m.starts_with(word))
    });
    let numeric_pair = tokens.len() == 1 && {
        let parts: Vec<&str> = tokens[0].split(['/', '.', '-']).collect();
        parts.len() == 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
    };
    month_name || numeric_pair
}
