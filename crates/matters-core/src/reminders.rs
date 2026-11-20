//! Reminders (#60): a task can be given a time of day, and is due for a
//! reminder at that time on its start date, in the system's time zone.
//!
//! A reminder belongs to its task's date: moving the task to another day
//! moves the reminder with it, and a task with no date, deferred to Someday,
//! finished or in the Trash has none due. Completing or trashing a task
//! therefore cancels its reminder.
//!
//! Each device records the reminders it has shown, by task and local date
//! and time (`reminder_deliveries`, never synced), so each is shown once:
//! after a restart, a suspend or a change of clock or time zone alike. A
//! reminder whose time passed while Matters was closed is still due when it
//! starts, and [`Reminder::is_late`] tells the two apart.
//!
//! The frontend asks for [`Database::reminders_due`] often (every few
//! seconds), shows what is due, then records it with
//! [`Database::mark_reminders_shown`]. Nothing here reads the clock: every
//! function takes the instant and the time zone, so tests pin both.

use jiff::civil::{DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use rusqlite::params;

use crate::domain::{Status, Task};
use crate::error::Result;
use crate::storage::db::{Database, TASK_COLUMNS};
use crate::storage::trash::LIVE_TASK;

/// A reminder shown more than this long after its time is late: it was
/// missed while Matters was closed or the computer was asleep.
pub const LATE_AFTER: SignedDuration = SignedDuration::from_secs(60);

/// A time of day as stored: `HH:MM`.
pub fn stored_time(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

/// A stored time of day (`HH:MM`); `None` for anything else.
pub fn parse_stored_time(text: &str) -> Option<Time> {
    let (hour, minute) = text.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    Time::new(hour.parse().ok()?, minute.parse().ok()?, 0, 0).ok()
}

/// A time of day as typed: `9`, `9:30`, `09:30`, `21:30`, `9am`, `9:30 pm`,
/// `noon` or `midnight`. Anything else is refused, never guessed. A
/// 12-hour time needs its hour from 1 to 12.
pub fn parse_typed_time(text: &str) -> Option<Time> {
    let text = text.trim().to_lowercase();
    match text.as_str() {
        "noon" => return Time::new(12, 0, 0, 0).ok(),
        "midnight" => return Some(Time::midnight()),
        _ => {}
    }
    let (clock, half) = if let Some(rest) = text.strip_suffix("am") {
        (rest.trim_end(), Some(false))
    } else if let Some(rest) = text.strip_suffix("pm") {
        (rest.trim_end(), Some(true))
    } else {
        (text.as_str(), None)
    };
    let (hour, minute) = match clock.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h, m),
        Some(_) => return None,
        None => (clock, "00"),
    };
    let digits = |s: &str| !s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(hour) || !digits(minute) {
        return None;
    }
    let (mut hour, minute): (i8, i8) = (hour.parse().ok()?, minute.parse().ok()?);
    if let Some(pm) = half {
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    Time::new(hour, minute, 0, 0).ok()
}

/// `time` without its seconds: reminders are set to the minute.
pub fn to_minute(time: Time) -> Time {
    time.with()
        .second(0)
        .subsec_nanosecond(0)
        .build()
        .unwrap_or(time)
}

/// The local date and time `task` is due for a reminder, if it is: it has
/// a reminder and a start date, is open, and is neither deferred to Someday
/// nor in the Trash by itself. (A task in a trashed project or area is
/// excluded by [`Database::reminders_due`].)
pub fn due(task: &Task) -> Option<DateTime> {
    if task.status != Status::Open || task.is_someday || task.trashed_at.is_some() {
        return None;
    }
    Some(task.start_date?.to_datetime(to_minute(task.reminder?)))
}

/// A reminder due for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub task_id: String,
    pub title: String,
    /// The local date and time it is for.
    pub due: DateTime,
    /// That time as an instant, in the time zone asked for. A time the
    /// clocks skip (a spring-forward gap) is the instant just after the gap;
    /// a time that happens twice (falling back) is the first.
    pub due_at: Timestamp,
}

impl Reminder {
    /// Whether, shown at `now`, it is late: its time passed more than
    /// [`LATE_AFTER`] ago.
    pub fn is_late(&self, now: Timestamp) -> bool {
        now.duration_since(self.due_at) > LATE_AFTER
    }
}

/// The key a shown reminder is recorded under: the local date and time it
/// was for, so a change of time zone doesn't make it due again.
fn due_key(due: DateTime) -> String {
    format!("{}T{}", due.date(), stored_time(due.time()))
}

impl Database {
    /// The reminders due by `now` in time zone `zone` that this device
    /// hasn't shown, earliest first; and when the next one not yet due is.
    pub fn reminders_due(
        &self,
        now: Timestamp,
        zone: &TimeZone,
    ) -> Result<(Vec<Reminder>, Option<Timestamp>)> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks
             WHERE reminder_time IS NOT NULL AND start_date IS NOT NULL AND {LIVE_TASK}
               AND NOT EXISTS (
                   SELECT 1 FROM reminder_deliveries d
                   WHERE d.task_id = tasks.id
                     AND d.due = tasks.start_date || 'T' || tasks.reminder_time)"
        );
        let tasks = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], Database::map_task_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        let mut pending: Vec<Reminder> = tasks
            .into_iter()
            .filter_map(|task| {
                let due = due(&task)?;
                let due_at = zone.to_ambiguous_zoned(due).compatible().ok()?.timestamp();
                Some(Reminder {
                    task_id: task.id,
                    title: task.title,
                    due,
                    due_at,
                })
            })
            .collect();
        pending.sort_by(|a, b| {
            a.due_at
                .cmp(&b.due_at)
                .then_with(|| a.task_id.cmp(&b.task_id))
        });
        let split = pending.partition_point(|r| r.due_at <= now);
        let next = pending.get(split).map(|r| r.due_at);
        pending.truncate(split);
        Ok((pending, next))
    }

    /// Record that `reminders` were shown at `now` (milliseconds), so they
    /// aren't due again on this device. Never synced: another device shows
    /// its own.
    pub fn mark_reminders_shown(&self, reminders: &[Reminder], now: i64) -> Result<()> {
        self.write(
            now,
            |tx| {
                for reminder in reminders {
                    tx.execute(
                        "INSERT OR IGNORE INTO reminder_deliveries (task_id, due, delivered_at)
                         SELECT id, ?2, ?3 FROM tasks WHERE id = ?1",
                        params![reminder.task_id, due_key(reminder.due), now],
                    )?;
                }
                Ok(())
            },
            |_, ()| Ok(Vec::new()),
        )
    }
}
