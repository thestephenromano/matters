//! Calendar events shown beside tasks (#62): read-only context from the
//! user's calendars, above Today's tasks and in Upcoming's days. Events are
//! never stored in the Matters database.
//!
//! A frontend reads them through a [`CalendarSource`] (Evolution Data Server
//! on Linux, decided on #62); [`FakeCalendarSource`] stands in for it in
//! tests. This module decides which days an event is listed on
//! ([`Event::days`]) and where its rows go among a view's rows
//! ([`merge_events`]).

use std::collections::BTreeMap;

use jiff::civil::{Date, DateTime};
use jiff::ToSpan;
use serde::{Deserialize, Serialize};

use crate::domain::View;
use crate::query::ListRow;

/// How many days ahead of today Upcoming lists events: tomorrow through
/// two weeks from today. Later days list a task's events only through the
/// task's own date, so the window bounds how much is read from the source.
pub const UPCOMING_EVENT_DAYS: i64 = 14;

/// One of the user's calendars.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    /// The source's stable identifier.
    pub id: String,
    pub name: String,
    /// The calendar's colour as `#rrggbb`, if it has one.
    pub color: Option<String>,
}

/// One occurrence of an event, in local time (a repeating event gives one
/// per occurrence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Identifies the occurrence among all events: the source's event id
    /// with the occurrence's start.
    pub id: String,
    pub calendar_id: String,
    pub title: String,
    /// Local start. All-day events start at midnight.
    pub start: DateTime,
    /// Local end, exclusive: an all-day event on the 3rd ends at midnight
    /// on the 4th.
    pub end: DateTime,
    pub all_day: bool,
}

impl Event {
    /// The days the event is listed on: every day it covers. It ends
    /// exclusively, so an event ending at midnight isn't listed on the day
    /// it ends; an event that ends no later than it starts (zero length or
    /// malformed) is listed on the day it starts.
    pub fn days(&self) -> impl Iterator<Item = Date> {
        let first = self.start.date();
        let last = if self.end.time() == jiff::civil::Time::midnight() {
            self.end.date().yesterday().unwrap_or(first)
        } else {
            self.end.date()
        };
        // Never before the first day, however it ends.
        let last = last.max(first);
        first.series(1.day()).take_while(move |day| *day <= last)
    }
}

/// An event as listed on one of its days.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayEvent {
    pub day: Date,
    pub event: Event,
}

impl DayEvent {
    /// Unique among a list's rows: an event spanning days has a row on
    /// each.
    pub fn key(&self) -> String {
        format!("event:{}:{}", self.day, self.event.id)
    }

    /// Whether the event started before this day (it continues into it).
    pub fn continues(&self) -> bool {
        self.event.start.date() < self.day
    }
}

/// Why no events can be shown, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where events come from. Calls may block (on a calendar service), so a
/// frontend calls them off its UI thread.
pub trait CalendarSource: Send {
    /// The calendars that can be shown.
    fn calendars(&mut self) -> Result<Vec<Calendar>, Unavailable>;

    /// The occurrences of every calendar's events that overlap the local
    /// days `from` up to `until` (exclusive).
    fn events(&mut self, from: Date, until: Date) -> Result<Vec<Event>, Unavailable>;
}

/// A calendar source holding fixed calendars and events, or failing with a
/// fixed reason: for tests and for running without a calendar service.
#[derive(Debug, Clone, Default)]
pub struct FakeCalendarSource {
    pub calendars: Vec<Calendar>,
    pub events: Vec<Event>,
    /// When set, every call fails with this reason.
    pub unavailable: Option<String>,
}

impl FakeCalendarSource {
    /// A source that is never available, for `reason`.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            unavailable: Some(reason.into()),
            ..Self::default()
        }
    }

    fn check(&self) -> Result<(), Unavailable> {
        match &self.unavailable {
            Some(reason) => Err(Unavailable(reason.clone())),
            None => Ok(()),
        }
    }
}

impl CalendarSource for FakeCalendarSource {
    fn calendars(&mut self) -> Result<Vec<Calendar>, Unavailable> {
        self.check()?;
        Ok(self.calendars.clone())
    }

    fn events(&mut self, from: Date, until: Date) -> Result<Vec<Event>, Unavailable> {
        self.check()?;
        Ok(self
            .events
            .iter()
            .filter(|e| e.days().any(|day| from <= day && day < until))
            .cloned()
            .collect())
    }
}

/// `events` listed by day, for the days `from` up to `until` (exclusive).
/// Each day lists its all-day events first, then the others by start;
/// events starting together are ordered by title, then id, so the order
/// never depends on the source's.
pub fn events_by_day(events: &[Event], from: Date, until: Date) -> BTreeMap<Date, Vec<DayEvent>> {
    let mut days: BTreeMap<Date, Vec<DayEvent>> = BTreeMap::new();
    for event in events {
        for day in event.days().filter(|day| from <= *day && *day < until) {
            days.entry(day).or_default().push(DayEvent {
                day,
                event: event.clone(),
            });
        }
    }
    for listed in days.values_mut() {
        listed.sort_by(|a, b| {
            let rank = |e: &DayEvent| (!e.event.all_day && !e.continues(), e.event.start);
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.event.title.cmp(&b.event.title))
                .then_with(|| a.event.id.cmp(&b.event.id))
        });
    }
    days
}

/// The days of `view` that list events, on local day `today`: Today lists
/// today's; Upcoming, those from tomorrow through [`UPCOMING_EVENT_DAYS`]
/// days from today. Other views list none.
pub fn event_days(view: &View, today: Date) -> Option<(Date, Date)> {
    let tomorrow = today.tomorrow().ok()?;
    match view {
        View::Today => Some((today, tomorrow)),
        View::Upcoming => Some((
            tomorrow,
            today
                .checked_add(UPCOMING_EVENT_DAYS.days())
                .ok()?
                .tomorrow()
                .ok()?,
        )),
        _ => None,
    }
}

/// `rows` of `view` with the events of `by_day` among them, on local day
/// `today`. Today lists its events above everything else. Upcoming lists
/// each day's events at the head of that day, ahead of the day's projects
/// and tasks; a day with events but nothing else gets its own place in date
/// order. Other views are unchanged.
pub fn merge_events(
    view: &View,
    rows: Vec<ListRow>,
    mut by_day: BTreeMap<Date, Vec<DayEvent>>,
    today: Date,
) -> Vec<ListRow> {
    match view {
        View::Today => {
            let mut merged: Vec<ListRow> = by_day
                .remove(&today)
                .unwrap_or_default()
                .into_iter()
                .map(ListRow::Event)
                .collect();
            merged.extend(rows);
            merged
        }
        View::Upcoming => {
            let mut days = by_day.into_iter().peekable();
            let mut merged = Vec::with_capacity(rows.len());
            for row in rows {
                if let Some(date) = row_date(&row) {
                    while let Some((_, listed)) = days.next_if(|(day, _)| *day <= date) {
                        merged.extend(listed.into_iter().map(ListRow::Event));
                    }
                }
                merged.push(row);
            }
            merged.extend(days.flat_map(|(_, listed)| listed).map(ListRow::Event));
            merged
        }
        _ => rows,
    }
}

/// The day an Upcoming row is listed under.
fn row_date(row: &ListRow) -> Option<Date> {
    match row {
        ListRow::Task(task) => task.start_date,
        ListRow::Project(project) => project.start_date,
        ListRow::Event(listed) => Some(listed.day),
        ListRow::EmptyHeading(_) => None,
    }
}

/// The section an event's row sits in, in `view`: its day in Upcoming;
/// Today's first, unnamed section.
pub fn event_section_key(view: &View, listed: &DayEvent) -> String {
    match view {
        View::Upcoming => format!("date:{}", listed.day),
        _ => String::new(),
    }
}
