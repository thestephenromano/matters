//! Repeating tasks (#61): repeat rules and the dates they produce.
//!
//! A rule repeats a task every `every` days, weeks, months or years, either
//! on a fixed schedule from its start date or a set time after each
//! occurrence is completed. On a fixed schedule:
//!
//! * weekly rules name their days, and weeks start on Monday;
//! * monthly rules name a day of the month (or the last day), or a weekday
//!   of the month (the third Saturday, the last Friday);
//! * yearly rules name a month and day.
//!
//! The dates are those on or after the start that match, and a month
//! without the day (the 31st, the fifth Monday, 29 February) is skipped,
//! as RFC 5545 (iCalendar) has it. The tests check the dates against an
//! independent implementation of RFC 5545, python-dateutil, through vectors
//! it generated (`tests/fixtures/repeat_vectors.json`). After completion,
//! the next date is the completion day plus the interval, with month and
//! year steps clamped to the month's last day.
//!
//! Dates are calendar dates, never instants, so time zones and daylight
//! saving never move an occurrence.
//!
//! How a rule turns into tasks (the newest occurrence carries it, as
//! decided on #61) is `crate::storage::repeats`.

use jiff::civil::{Date, Weekday};
use jiff::ToSpan;
use serde::{Deserialize, Serialize};

/// What a rule repeats on, within its unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frequency {
    /// Every `every` days.
    Daily,
    /// On these days of every `every` weeks: 1 is Monday, 7 Sunday.
    Weekly { days: Vec<i8> },
    /// On this day of every `every` months, 1 to 31, or -1 for the last.
    MonthlyOnDay { day: i8 },
    /// On the `nth` (1 to 5, or -1 for the last) `weekday` (1 is Monday) of
    /// every `every` months.
    MonthlyOnWeekday { nth: i8, weekday: i8 },
    /// On this month (1 to 12) and day of every `every` years.
    Yearly { month: i8, day: i8 },
}

/// A repeat rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatRule {
    /// The interval, in the frequency's unit: at least 1.
    pub every: u16,
    pub frequency: Frequency,
    /// Repeat a set time after each occurrence is completed, rather than
    /// on a schedule. The frequency then gives only the unit.
    #[serde(default)]
    pub after_completion: bool,
    /// The first day of the schedule: the series' first occurrence.
    pub start: Date,
    /// No occurrence after this day.
    #[serde(default)]
    pub until: Option<Date>,
    /// How many occurrences are still to come after the current one;
    /// `None` for no limit.
    #[serde(default)]
    pub left: Option<u32>,
    /// Paused: no new occurrences until resumed.
    #[serde(default)]
    pub paused: bool,
}

/// More periods than any rule has: the calendar's days, from year 1 to
/// 9999, are fewer.
const MOST_PERIODS: u32 = 4_000_000;

fn weekday(n: i8) -> Option<Weekday> {
    Weekday::from_monday_one_offset(n).ok()
}

impl RepeatRule {
    /// Whether the rule can be used, or why not.
    pub fn check(&self) -> Result<(), String> {
        if self.every == 0 || self.every > 999 {
            return Err("repeats every 1 to 999".into());
        }
        match &self.frequency {
            Frequency::Daily => {}
            Frequency::Weekly { days } => {
                if days.is_empty() {
                    return Err("a weekly repeat needs at least one day".into());
                }
                if days.iter().any(|d| weekday(*d).is_none()) {
                    return Err("days are 1 (Monday) to 7 (Sunday)".into());
                }
            }
            Frequency::MonthlyOnDay { day } => {
                if !(*day == -1 || (1..=31).contains(day)) {
                    return Err("a day of the month is 1 to 31, or the last".into());
                }
            }
            Frequency::MonthlyOnWeekday { nth, weekday: w } => {
                if !(*nth == -1 || (1..=5).contains(nth)) || weekday(*w).is_none() {
                    return Err(
                        "a weekday of the month is the 1st to 5th or last, Monday to Sunday".into(),
                    );
                }
            }
            Frequency::Yearly { month, day } => {
                // 29 February is allowed: it repeats in leap years.
                if Date::new(2000, *month, *day).is_err() {
                    return Err("not a day of the year".into());
                }
            }
        }
        if self.until.is_some_and(|until| until < self.start) {
            return Err("it ends before it starts".into());
        }
        Ok(())
    }

    /// The scheduled dates, from the start on, in order, up to `until` or
    /// the end of the calendar (9999); `left` is not applied (it counts
    /// from the current occurrence, which the dates don't know). A rule
    /// that matches no day (the 31st of every twelfth month from April)
    /// has none, found by reaching the calendar's end.
    pub fn dates(&self) -> impl Iterator<Item = Date> + '_ {
        self.dates_within(MOST_PERIODS)
    }

    /// [`Self::dates`] from at most the first `most` periods (days, weeks,
    /// months or years of the rule's interval). The scan stops there even
    /// if the calendar goes on, so it always ends.
    pub fn dates_within(&self, most: u32) -> impl Iterator<Item = Date> + '_ {
        let mut period = 0u32;
        let mut pending: std::vec::IntoIter<Date> = Vec::new().into_iter();
        std::iter::from_fn(move || loop {
            if let Some(date) = pending.next() {
                if self.until.is_some_and(|until| date > until) {
                    return None;
                }
                return Some(date);
            }
            if period >= most {
                return None;
            }
            let (first_day, candidates) = self.period(period)?;
            if self.until.is_some_and(|until| first_day > until) {
                return None;
            }
            period = period.checked_add(1)?;
            let found: Vec<Date> = candidates
                .into_iter()
                .filter(|d| *d >= self.start)
                .collect();
            pending = found.into_iter();
        })
    }

    /// The first day of period `k` (k intervals after the start's period)
    /// and the dates in it that match, in order; `None` past the calendar.
    fn period(&self, k: u32) -> Option<(Date, Vec<Date>)> {
        let step = i64::from(k) * i64::from(self.every);
        match &self.frequency {
            Frequency::Daily => {
                let day = self.start.checked_add(step.days()).ok()?;
                Some((day, vec![day]))
            }
            Frequency::Weekly { days } => {
                let monday = self
                    .start
                    .checked_sub(i64::from(self.start.weekday().to_monday_zero_offset()).days())
                    .ok()?;
                let first = monday.checked_add((step * 7).days()).ok()?;
                let mut dates: Vec<Date> = days
                    .iter()
                    .filter_map(|d| weekday(*d))
                    .filter_map(|w| {
                        first
                            .checked_add(i64::from(w.to_monday_zero_offset()).days())
                            .ok()
                    })
                    .collect();
                dates.sort();
                dates.dedup();
                Some((first, dates))
            }
            Frequency::MonthlyOnDay { day } => {
                let first = self
                    .start
                    .first_of_month()
                    .checked_add(step.months())
                    .ok()?;
                let date = if *day == -1 {
                    Some(first.last_of_month())
                } else {
                    Date::new(first.year(), first.month(), *day).ok()
                };
                Some((first, date.into_iter().collect()))
            }
            Frequency::MonthlyOnWeekday { nth, weekday: w } => {
                let first = self
                    .start
                    .first_of_month()
                    .checked_add(step.months())
                    .ok()?;
                let date = weekday(*w).and_then(|w| first.nth_weekday_of_month(*nth, w).ok());
                Some((first, date.into_iter().collect()))
            }
            Frequency::Yearly { month, day } => {
                let year = i64::from(self.start.year()).checked_add(step)?;
                let year = i16::try_from(year).ok()?;
                let first = Date::new(year, 1, 1).ok()?;
                let date = Date::new(year, *month, *day).ok();
                Some((first, date.into_iter().collect()))
            }
        }
    }

    /// The first scheduled date after `day`.
    pub fn next_after(&self, day: Date) -> Option<Date> {
        self.dates().find(|d| *d > day)
    }

    /// The latest scheduled date after `after` and no later than `today`:
    /// the occurrence due now, skipping any that were missed.
    pub fn latest_due(&self, after: Date, today: Date) -> Option<Date> {
        self.dates()
            .skip_while(|d| *d <= after)
            .take_while(|d| *d <= today)
            .last()
    }

    /// After completion: the next date, `every` units after `completed`,
    /// unless that is past `until`.
    pub fn after_completed(&self, completed: Date) -> Option<Date> {
        let every = i64::from(self.every);
        let span = match self.frequency {
            Frequency::Daily => every.days(),
            Frequency::Weekly { .. } => (every * 7).days(),
            Frequency::MonthlyOnDay { .. } | Frequency::MonthlyOnWeekday { .. } => every.months(),
            Frequency::Yearly { .. } => every.years(),
        };
        let next = completed.checked_add(span).ok()?;
        (!self.until.is_some_and(|until| next > until)).then_some(next)
    }

    /// The rule in words, as the When popover and a row's tooltip say it:
    /// "Every 2 weeks on Mon, Thu", "Every month on the last Friday",
    /// "Every 3 days after completion, until 1 Dec 2026".
    pub fn describe(&self) -> String {
        let unit = match self.frequency {
            Frequency::Daily => "day",
            Frequency::Weekly { .. } => "week",
            Frequency::MonthlyOnDay { .. } | Frequency::MonthlyOnWeekday { .. } => "month",
            Frequency::Yearly { .. } => "year",
        };
        let mut text = if self.every == 1 {
            format!("Every {unit}")
        } else {
            format!("Every {} {unit}s", self.every)
        };
        if self.after_completion {
            text.push_str(" after completion");
        } else {
            match &self.frequency {
                Frequency::Daily => {}
                Frequency::Weekly { days } => {
                    let mut days = days.clone();
                    days.sort();
                    days.dedup();
                    let names: Vec<&str> = days
                        .iter()
                        .filter_map(|d| weekday(*d))
                        .map(short_day)
                        .collect();
                    text.push_str(&format!(" on {}", names.join(", ")));
                }
                Frequency::MonthlyOnDay { day: -1 } => text.push_str(" on the last day"),
                Frequency::MonthlyOnDay { day } => {
                    text.push_str(&format!(" on the {}", ordinal(i32::from(*day))));
                }
                Frequency::MonthlyOnWeekday { nth, weekday: w } => {
                    let which = if *nth == -1 {
                        "last".to_string()
                    } else {
                        ordinal(i32::from(*nth))
                    };
                    let day = weekday(*w).map(long_day).unwrap_or("day");
                    text.push_str(&format!(" on the {which} {day}"));
                }
                Frequency::Yearly { month, day } => {
                    if let Ok(date) = Date::new(2000, *month, *day) {
                        text.push_str(&format!(" on {}", date.strftime("%-d %b")));
                    }
                }
            }
        }
        if let Some(until) = self.until {
            text.push_str(&format!(", until {}", until.strftime("%-d %b %Y")));
        }
        match self.left {
            Some(0) => text.push_str(", ending with this one"),
            Some(1) => text.push_str(", once more"),
            Some(n) => text.push_str(&format!(", {n} more times")),
            None => {}
        }
        if self.paused {
            text.push_str(" (paused)");
        }
        text
    }

    /// The rule the next occurrence carries: one fewer left, or `None` when
    /// none are left (the series ends with the current one).
    pub fn passed_on(&self) -> Option<RepeatRule> {
        let left = match self.left {
            Some(0) => return None,
            Some(n) => Some(n - 1),
            None => None,
        };
        Some(RepeatRule {
            left,
            ..self.clone()
        })
    }
}

fn short_day(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

fn long_day(day: Weekday) -> &'static str {
    match day {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

/// 1st, 2nd, 3rd, 4th, 11th, 21st, 22nd...
fn ordinal(n: i32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// A future occurrence of a repeating task, as Upcoming previews it: the
/// day, and the newest occurrence it will be a copy of.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub day: Date,
    pub task: crate::domain::Task,
}

impl Preview {
    /// Unique among a list's rows.
    pub fn key(&self) -> String {
        format!("repeat:{}:{}", self.task.id, self.day)
    }

    /// The days from `from` up to `until` (exclusive) on which `task`'s
    /// series will have an occurrence it hasn't yet: fixed-schedule, not
    /// paused, after the task's own date, as many as are left.
    pub fn of(task: &crate::domain::Task, from: Date, until: Date) -> Vec<Preview> {
        let Some(rule) = task
            .repeat
            .as_ref()
            .filter(|r| !r.after_completion && !r.paused)
        else {
            return Vec::new();
        };
        let Some(after) = task.occurrence.or(task.start_date) else {
            return Vec::new();
        };
        let left = rule.left.map_or(usize::MAX, |n| n as usize);
        rule.dates()
            .skip_while(|d| *d <= after)
            .take(left)
            .take_while(|d| *d < until)
            .filter(|d| *d >= from)
            .map(|day| Preview {
                day,
                task: task.clone(),
            })
            .collect()
    }
}

/// Upcoming's `rows` with `previews` among them: each at the end of its
/// day, after that day's events, projects and tasks; a day with nothing
/// else gets its own place in date order. Previews of one day are ordered
/// by title, then task.
pub fn merge_previews(
    rows: Vec<crate::query::ListRow>,
    mut previews: Vec<Preview>,
) -> Vec<crate::query::ListRow> {
    use crate::query::ListRow;
    previews.sort_by(|a, b| {
        (a.day, &a.task.title, &a.task.id).cmp(&(b.day, &b.task.title, &b.task.id))
    });
    let mut previews = previews.into_iter().peekable();
    let mut merged = Vec::with_capacity(rows.len());
    for row in rows {
        let date = match &row {
            ListRow::Task(task) => task.start_date,
            ListRow::Project(project) => project.start_date,
            ListRow::Event(listed) => Some(listed.day),
            ListRow::Preview(preview) => Some(preview.day),
            ListRow::EmptyHeading(_) => None,
        };
        if let Some(date) = date {
            while let Some(preview) = previews.next_if(|p| p.day < date) {
                merged.push(ListRow::Preview(preview));
            }
        }
        merged.push(row);
    }
    merged.extend(previews.map(ListRow::Preview));
    merged
}

/// A rule as stored in `tasks.repeat_rule`.
pub fn stored_rule(rule: Option<&RepeatRule>) -> Option<String> {
    rule.map(|r| serde_json::to_string(r).expect("a rule serializes"))
}

/// The id of the occurrence of series `series` on `date`: equal on every
/// device and distinct per date. A UUIDv7 whose time is the date's start
/// (midnight UTC) and whose other bits come from the series id, so it is never
/// a created (v4) id, nor a conflict copy's (v8, reserved by the engine).
pub fn occurrence_id(series: &str, date: Date) -> Option<String> {
    let series = uuid::Uuid::parse_str(series).ok()?;
    let days = date.since(Date::new(1970, 1, 1).ok()?).ok()?.get_days();
    let millis = u64::try_from(i64::from(days) * 86_400_000).ok()?;
    let mut bytes = *series.as_bytes();
    // The series' first six bytes, which the time replaces, still count.
    for i in 0..6 {
        bytes[i + 10] ^= bytes[i];
    }
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    let id = uuid::Builder::from_bytes(bytes)
        .with_version(uuid::Version::SortRand)
        .with_variant(uuid::Variant::RFC4122)
        .into_uuid();
    Some(id.to_string())
}
