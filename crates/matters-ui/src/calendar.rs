//! Calendar events for Today and Upcoming (#62), from a
//! [`CalendarSource`]: Evolution Data Server in the app ([`crate::eds`]), a
//! fake in tests.
//!
//! Sources may block (connecting to a calendar service can take seconds),
//! so the app reads them on a worker thread ([`start_worker`]) and keeps the
//! latest result here ([`snapshot`]); list loads only read that. Tests
//! install a source without the worker and refresh synchronously.

use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use jiff::civil::Date;
use matters_core::calendar::{event_days, Calendar, CalendarSource, Event};
use matters_core::View;

/// What was last read from the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub calendars: Vec<Calendar>,
    /// Events overlapping the days read (`window`).
    pub events: Vec<Event>,
    /// Why there are no events to show, when the source failed.
    pub problem: Option<String>,
    /// The days read, `from` up to `until`, exclusive.
    pub window: Option<(Date, Date)>,
}

struct Service {
    source: Mutex<Box<dyn CalendarSource>>,
    snapshot: Mutex<Snapshot>,
    /// Read on a worker thread rather than when asked.
    background: bool,
    /// Set when a refresh is wanted; the worker waits on `wake`.
    pending: Mutex<bool>,
    wake: Condvar,
    worker: OnceLock<()>,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Read events from `source`. With `background`, reads happen on a worker
/// thread ([`start_worker`]); otherwise [`request_refresh`] reads at once.
/// Call before QML starts; without it no events are shown and none are
/// asked for.
pub fn install_calendar_source(source: Box<dyn CalendarSource>, background: bool) {
    let service = Service {
        source: Mutex::new(source),
        snapshot: Mutex::new(Snapshot::default()),
        background,
        pending: Mutex::new(false),
        wake: Condvar::new(),
        worker: OnceLock::new(),
    };
    if SERVICE.set(service).is_err() {
        panic!("matters-ui: calendar source installed twice");
    }
}

/// The days read around local day `today`: everything Today and Upcoming
/// list.
pub fn window(today: Date) -> Option<(Date, Date)> {
    let (from, _) = event_days(&View::Today, today)?;
    let (_, until) = event_days(&View::Upcoming, today)?;
    Some((from, until))
}

/// Read the calendars and the events around `today` now, blocking; true
/// when the result differs from the last.
fn refresh_now(service: &Service, today: Date) -> bool {
    let next = {
        let mut source = lock(&service.source);
        let window = window(today);
        let read = source.calendars().and_then(|calendars| {
            let events = match window {
                Some((from, until)) => source.events(from, until)?,
                None => Vec::new(),
            };
            Ok((calendars, events))
        });
        match read {
            Ok((calendars, events)) => Snapshot {
                calendars,
                events,
                problem: None,
                window,
            },
            Err(reason) => Snapshot {
                problem: Some(reason.0),
                window,
                ..Snapshot::default()
            },
        }
    };
    let mut snapshot = lock(&service.snapshot);
    if *snapshot == next {
        return false;
    }
    *snapshot = next;
    true
}

/// The latest result. Empty when no source is installed or none has been
/// read yet.
pub fn snapshot() -> Snapshot {
    SERVICE
        .get()
        .map(|service| lock(&service.snapshot).clone())
        .unwrap_or_default()
}

/// Whether reads happen on a worker thread.
pub fn in_background() -> bool {
    SERVICE.get().is_some_and(|service| service.background)
}

/// Start the worker that reads the source whenever [`request_refresh`]
/// asks, calling `changed` (on the worker thread) after a read that changed
/// the result. Only the first call starts it.
pub fn start_worker(changed: impl Fn() + Send + 'static) {
    let Some(service) = SERVICE.get() else {
        return;
    };
    service.worker.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("matters-calendar".into())
            .spawn(move || loop {
                {
                    let mut pending = lock(&service.pending);
                    while !*pending {
                        pending = service
                            .wake
                            .wait(pending)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    *pending = false;
                }
                if refresh_now(service, matters_core::local_today()) {
                    changed();
                }
            });
        if let Err(err) = spawned {
            eprintln!("matters: calendar events are off: {err}");
        }
    });
}

/// Ask for a fresh read. In the background, the worker reads when it is
/// free and this returns false; otherwise the read happens now and this
/// says whether the result changed.
pub fn request_refresh() -> bool {
    let Some(service) = SERVICE.get() else {
        return false;
    };
    if service.background {
        *lock(&service.pending) = true;
        service.wake.notify_one();
        false
    } else {
        refresh_now(service, matters_core::local_today())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use matters_core::calendar::FakeCalendarSource;

    fn service(source: FakeCalendarSource) -> Service {
        Service {
            source: Mutex::new(Box::new(source)),
            snapshot: Mutex::new(Snapshot::default()),
            background: false,
            pending: Mutex::new(false),
            wake: Condvar::new(),
            worker: OnceLock::new(),
        }
    }

    #[test]
    fn the_window_covers_today_and_upcoming() {
        assert_eq!(
            window(date(2026, 11, 21)),
            Some((date(2026, 11, 21), date(2026, 12, 6)))
        );
    }

    #[test]
    fn a_refresh_reports_only_changes() {
        let calendar = Calendar {
            id: "work".into(),
            name: "Work".into(),
            color: Some("#62a0ea".into()),
        };
        let service = service(FakeCalendarSource {
            calendars: vec![calendar.clone()],
            events: vec![Event {
                id: "s".into(),
                calendar_id: "work".into(),
                title: "Standup".into(),
                start: "2026-11-21T09:00".parse().unwrap(),
                end: "2026-11-21T09:15".parse().unwrap(),
                all_day: false,
            }],
            unavailable: None,
        });
        let today = date(2026, 11, 21);
        assert!(refresh_now(&service, today));
        assert!(!refresh_now(&service, today), "nothing changed");
        let read = lock(&service.snapshot).clone();
        assert_eq!(read.calendars, [calendar]);
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.problem, None);
        // The next day's window no longer holds the event.
        assert!(refresh_now(&service, date(2026, 11, 22)));
        assert!(lock(&service.snapshot).events.is_empty());
    }

    #[test]
    fn an_unavailable_source_leaves_its_reason() {
        let service = service(FakeCalendarSource::unavailable(
            "Evolution Data Server isn't installed.",
        ));
        assert!(refresh_now(&service, date(2026, 11, 21)));
        let read = lock(&service.snapshot).clone();
        assert_eq!(
            read.problem.as_deref(),
            Some("Evolution Data Server isn't installed.")
        );
        assert!(read.calendars.is_empty() && read.events.is_empty());
    }
}
