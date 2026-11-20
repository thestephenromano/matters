//! Showing reminders as they come due (#60). The controller calls
//! [`deliver`] every few seconds; the rules for when a reminder is due, and
//! that it is shown once, are `matters_core::reminders`.
//!
//! A frequent check against the wall clock, rather than one timer set for
//! the next reminder, is what keeps reminders on time after a suspend or a
//! change of clock or time zone: the next check after any of them sees what
//! is due.

use std::sync::OnceLock;

use jiff::tz::TimeZone;
use jiff::Timestamp;
use matters_core::reminders::Reminder;
use matters_core::Database;

use crate::notify::{Notice, Notifier, OnAction};

/// More reminders than this at once are shown as one summary.
pub const MOST_SHOWN_APART: usize = 3;

static NOTIFIER: OnceLock<Box<dyn Notifier>> = OnceLock::new();

/// Show reminders with `notifier`. Call before QML starts; without it
/// reminders are recorded as due but never shown.
pub fn install_notifier(notifier: Box<dyn Notifier>) {
    if NOTIFIER.set(notifier).is_err() {
        panic!("matters-ui: notifier installed twice");
    }
}

pub(crate) fn notifier() -> Option<&'static dyn Notifier> {
    NOTIFIER.get().map(Box::as_ref)
}

/// When a reminder was for, as its notice says: the time, with the day if
/// it isn't `now`'s.
fn when(reminder: &Reminder, now: Timestamp, zone: &TimeZone) -> String {
    let time = reminder.due.time().strftime("%H:%M").to_string();
    if reminder.due.date() == now.to_zoned(zone.clone()).date() {
        time
    } else {
        format!("{} at {time}", reminder.due.date().strftime("%a %-d %b"))
    }
}

/// The notices for reminders `due` at `now`: one each, saying which were
/// missed; or, past [`MOST_SHOWN_APART`], one for them all.
pub fn notices(due: &[Reminder], now: Timestamp, zone: &TimeZone) -> Vec<Notice> {
    if due.len() > MOST_SHOWN_APART {
        let titles: Vec<&str> = due
            .iter()
            .take(MOST_SHOWN_APART)
            .map(|r| r.title.as_str())
            .collect();
        return vec![Notice {
            task_id: String::new(),
            summary: format!("{} reminders", due.len()),
            body: format!(
                "{} and {} more",
                titles.join(", "),
                due.len() - MOST_SHOWN_APART
            ),
        }];
    }
    due.iter()
        .map(|r| Notice {
            task_id: r.task_id.clone(),
            summary: r.title.clone(),
            body: if r.is_late(now) {
                format!("Missed reminder, for {}", when(r, now, zone))
            } else {
                format!("Reminder for {}", when(r, now, zone))
            },
        })
        .collect()
}

/// Show what is due at `now` in `zone` and record it as shown. Returns how
/// many reminders were shown. If the notifier fails, nothing is recorded,
/// so the next check tries again, and the error says why.
pub fn deliver(
    db: &Database,
    notifier: &dyn Notifier,
    on_action: &OnAction,
    now: Timestamp,
    zone: &TimeZone,
) -> Result<usize, String> {
    let (due, _) = db.reminders_due(now, zone).map_err(|err| err.to_string())?;
    if due.is_empty() {
        return Ok(0);
    }
    for notice in notices(&due, now, zone) {
        notifier.show(&notice, on_action.clone())?;
    }
    db.mark_reminders_shown(&due, now.as_millisecond())
        .map_err(|err| err.to_string())?;
    Ok(due.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::RecordingNotifier;
    use jiff::civil::{date, time};
    use jiff::tz::Offset;
    use matters_core::Task;
    use std::sync::Arc;

    fn zone() -> TimeZone {
        TimeZone::fixed(Offset::constant(1))
    }

    fn at(h: i8, m: i8) -> Timestamp {
        date(2026, 11, 21)
            .at(h, m, 0, 0)
            .to_zoned(zone())
            .unwrap()
            .timestamp()
    }

    fn reminded(db: &Database, n: u128, hour: i8) -> String {
        let mut task = Task::new(
            uuid::Uuid::from_u128(n).to_string(),
            format!("task {n}"),
            0.0,
        );
        task.start_date = Some(date(2026, 11, 21));
        db.add_task(&mut task).unwrap();
        db.set_task_reminder(&task.id, Some(time(hour, 0, 0, 0)))
            .unwrap();
        task.id
    }

    fn nothing() -> OnAction {
        Arc::new(|_| {})
    }

    #[test]
    fn a_due_reminder_is_shown_once() {
        let db = Database::open_in_memory().unwrap();
        let id = reminded(&db, 1, 9);
        let notifier = RecordingNotifier::default();
        assert_eq!(
            deliver(&db, &notifier, &nothing(), at(8, 59), &zone()),
            Ok(0)
        );
        assert_eq!(
            deliver(&db, &notifier, &nothing(), at(9, 0), &zone()),
            Ok(1)
        );
        assert_eq!(
            deliver(&db, &notifier, &nothing(), at(9, 0), &zone()),
            Ok(0)
        );
        assert_eq!(
            notifier.shown(),
            [Notice {
                task_id: id,
                summary: "task 1".into(),
                body: "Reminder for 09:00".into(),
            }]
        );
    }

    #[test]
    fn a_reminder_that_fails_to_show_is_tried_again() {
        let db = Database::open_in_memory().unwrap();
        reminded(&db, 1, 9);
        let notifier = RecordingNotifier::default();
        notifier.fail(true);
        assert!(deliver(&db, &notifier, &nothing(), at(9, 0), &zone()).is_err());
        notifier.fail(false);
        assert_eq!(
            deliver(&db, &notifier, &nothing(), at(9, 5), &zone()),
            Ok(1)
        );
        assert_eq!(notifier.shown()[0].body, "Missed reminder, for 09:00");
    }

    #[test]
    fn missed_reminders_say_when_they_were_for() {
        let db = Database::open_in_memory().unwrap();
        reminded(&db, 1, 9);
        let mut yesterday = Task::new(uuid::Uuid::from_u128(2).to_string(), "old", 0.0);
        yesterday.start_date = Some(date(2026, 11, 20));
        db.add_task(&mut yesterday).unwrap();
        db.set_task_reminder(&yesterday.id, Some(time(18, 30, 0, 0)))
            .unwrap();
        let notifier = RecordingNotifier::default();
        deliver(&db, &notifier, &nothing(), at(12, 0), &zone()).unwrap();
        let bodies: Vec<String> = notifier.shown().into_iter().map(|n| n.body).collect();
        assert_eq!(
            bodies,
            [
                "Missed reminder, for Fri 20 Nov at 18:30",
                "Missed reminder, for 09:00"
            ]
        );
    }

    #[test]
    fn many_at_once_are_one_summary() {
        let db = Database::open_in_memory().unwrap();
        for n in 1..=5 {
            reminded(&db, n, 9);
        }
        let notifier = RecordingNotifier::default();
        assert_eq!(
            deliver(&db, &notifier, &nothing(), at(9, 0), &zone()),
            Ok(5)
        );
        assert_eq!(
            notifier.shown(),
            [Notice {
                task_id: String::new(),
                summary: "5 reminders".into(),
                body: "task 1, task 2, task 3 and 2 more".into(),
            }]
        );
    }

    #[test]
    fn up_to_three_are_shown_apart() {
        let db = Database::open_in_memory().unwrap();
        for n in 1..=3 {
            reminded(&db, n, 9);
        }
        let notifier = RecordingNotifier::default();
        deliver(&db, &notifier, &nothing(), at(9, 0), &zone()).unwrap();
        assert_eq!(notifier.shown().len(), 3);
        assert!(notifier.shown().iter().all(|n| !n.task_id.is_empty()));
    }
}
