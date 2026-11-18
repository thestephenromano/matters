//! The user's local calendar date, and detection of when it changes.
//!
//! View membership (Today, Upcoming, Anytime) depends on "today", so a
//! frontend that stays open must re-query when the date changes: at midnight,
//! after resuming from sleep on a later day, or when the clock or time zone
//! changes. Frontends poll [`DayTracker::advance`] on a short interval rather
//! than arming a single midnight timer, which would miss suspend and clock
//! changes.

use jiff::civil::Date;

/// Today's date in the local time zone.
pub fn local_today() -> Date {
    jiff::Zoned::now().date()
}

/// Now, in milliseconds since the Unix epoch (how timestamps are stored).
pub fn now_millis() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

/// The local calendar day of a stored timestamp (milliseconds since the Unix
/// epoch), or `None` if it is out of range.
pub fn local_day_of_millis(ms: i64) -> Option<Date> {
    let at = jiff::Timestamp::from_millisecond(ms).ok()?;
    Some(at.to_zoned(jiff::tz::TimeZone::system()).date())
}

/// Parse a date as Matters stores and exchanges it: ISO 8601 `YYYY-MM-DD`.
pub fn parse_iso_date(s: &str) -> Option<Date> {
    s.parse().ok()
}

/// Remembers the date the views were last built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayTracker {
    day: Date,
}

impl DayTracker {
    pub fn new(day: Date) -> Self {
        Self { day }
    }

    /// The date the views were last built for.
    pub fn day(&self) -> Date {
        self.day
    }

    /// Record `now` and report whether it differs from the last recorded date.
    /// Any difference counts, including the clock moving backwards, because
    /// view membership depends on the exact date.
    pub fn advance(&mut self, now: Date) -> bool {
        let changed = now != self.day;
        self.day = now;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: i8) -> Date {
        jiff::civil::date(2026, 11, day)
    }

    #[test]
    fn reports_only_actual_date_changes() {
        let mut tracker = DayTracker::new(d(17));
        assert!(!tracker.advance(d(17)), "same day: no refresh");
        assert!(tracker.advance(d(18)), "midnight passed");
        assert_eq!(tracker.day(), d(18));
        assert!(!tracker.advance(d(18)), "reported once per change");
        assert!(tracker.advance(d(21)), "resumed days later");
        assert!(tracker.advance(d(20)), "clock moved back");
    }
}
