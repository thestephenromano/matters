//! The strings Rust shows (#75), declared for translation: `lupdate` reads
//! this file (`translations/update.sh`), so each literal reaches the
//! catalog. Keep it to these declarations: lupdate reads it as C++.

use crate::i18n::{Text, QT_TRANSLATE_NOOP, QT_TRANSLATE_NOOP3, QT_TRANSLATE_N_NOOP};

// Settings (#72).
pub const SETTINGS_NOT_SAVED: Text = QT_TRANSLATE_NOOP("Matters", "Your settings couldn't be saved: %1");
pub const SETTINGS_UNREADABLE: Text = QT_TRANSLATE_NOOP("Matters", "Your settings in %1 couldn't be read (%2), so the defaults are used.");
pub const SETTINGS_NOT_KEPT: Text = QT_TRANSLATE_NOOP("Matters", "Your settings can't be kept: %1");

// Changes that couldn't be made.
pub const NOT_SAVED: Text = QT_TRANSLATE_NOOP("Matters", "The change couldn't be saved: %1");
pub const TASK_GONE: Text = QT_TRANSLATE_NOOP("Matters", "That task no longer exists.");

// Reminders (#60).
pub const REMINDERS_NOT_SHOWN: Text = QT_TRANSLATE_NOOP("Matters", "Reminders can't be shown: %1");
pub const REMINDER_FOR: Text = QT_TRANSLATE_NOOP("Matters", "Reminder for %1");
pub const REMINDER_MISSED: Text = QT_TRANSLATE_NOOP("Matters", "Missed reminder, for %1");
pub const REMINDER_DAY_AT: Text = QT_TRANSLATE_NOOP3("Matters", "%1 at %2", "a day, then a time");
pub const REMINDERS_MANY: Text = QT_TRANSLATE_N_NOOP("Matters", "%n reminders");
pub const REMINDERS_AND_MORE: Text = QT_TRANSLATE_N_NOOP("Matters", "%1 and %n more");
pub const REMINDER_OPEN: Text = QT_TRANSLATE_NOOP("Matters", "Open");
pub const REMINDER_COMPLETE: Text = QT_TRANSLATE_NOOP("Matters", "Complete");

// Calendar events (#62).
pub const EDS_MISSING: Text = QT_TRANSLATE_NOOP("Matters", "Calendar events come from Evolution Data Server, which isn't installed.");
pub const EDS_TOO_OLD: Text = QT_TRANSLATE_NOOP("Matters", "This version of Evolution Data Server can't be used (%1).");
pub const EDS_UNREACHABLE: Text = QT_TRANSLATE_NOOP("Matters", "Evolution Data Server couldn't be reached: %1.");
pub const EDS_DAYS: Text = QT_TRANSLATE_NOOP("Matters", "The calendar's days can't be read: %1.");

// How a task repeats (#61), built from these phrases.
pub const EVERY_DAY: Text = QT_TRANSLATE_NOOP("Matters", "Every day");
pub const EVERY_N_DAYS: Text = QT_TRANSLATE_N_NOOP("Matters", "Every %n days");
pub const EVERY_WEEK: Text = QT_TRANSLATE_NOOP("Matters", "Every week");
pub const EVERY_N_WEEKS: Text = QT_TRANSLATE_N_NOOP("Matters", "Every %n weeks");
pub const EVERY_MONTH: Text = QT_TRANSLATE_NOOP("Matters", "Every month");
pub const EVERY_N_MONTHS: Text = QT_TRANSLATE_N_NOOP("Matters", "Every %n months");
pub const EVERY_YEAR: Text = QT_TRANSLATE_NOOP("Matters", "Every year");
pub const EVERY_N_YEARS: Text = QT_TRANSLATE_N_NOOP("Matters", "Every %n years");
pub const AFTER_COMPLETION: Text = QT_TRANSLATE_NOOP("Matters", "%1 after completion");
pub const ON_DAYS: Text = QT_TRANSLATE_NOOP3("Matters", "%1 on %2", "how often, then the weekdays");
pub const ON_DATE: Text = QT_TRANSLATE_NOOP3("Matters", "%1 on %2", "every year, then a day and month");
pub const ON_DAY_OF_MONTH: Text = QT_TRANSLATE_NOOP("Matters", "%1 on day %2");
pub const ON_LAST_DAY: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the last day");
pub const ON_FIRST: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the first %2");
pub const ON_SECOND: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the second %2");
pub const ON_THIRD: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the third %2");
pub const ON_FOURTH: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the fourth %2");
pub const ON_FIFTH: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the fifth %2");
pub const ON_LAST: Text = QT_TRANSLATE_NOOP("Matters", "%1 on the last %2");
pub const UNTIL: Text = QT_TRANSLATE_NOOP("Matters", "%1, until %2");
pub const ENDING_NOW: Text = QT_TRANSLATE_NOOP("Matters", "%1, ending with this one");
pub const ONCE_MORE: Text = QT_TRANSLATE_NOOP("Matters", "%1, once more");
pub const MORE_TIMES: Text = QT_TRANSLATE_N_NOOP("Matters", "%1, %n more times");
pub const PAUSED: Text = QT_TRANSLATE_NOOP("Matters", "%1 (paused)");

// Lists of names: "Mon, Wed".
pub const LIST_SEPARATOR: Text = QT_TRANSLATE_NOOP3("Matters", ", ", "between the items of a list");
