//! Desktop notifications for reminders (#60; `notify-rust` through the
//! freedesktop notification service, decided there).
//!
//! A [`Notifier`] shows a [`Notice`] and reports what the user does with
//! it. The app uses [`DesktopNotifier`]; tests use [`RecordingNotifier`].

use std::sync::{Arc, Mutex, PoisonError};

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The task it is about, or empty for a summary of several.
    pub task_id: String,
    pub summary: String,
    pub body: String,
}

/// What the user did with a notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open its task (or, for a summary, Today).
    Open(String),
    /// Complete its task.
    Complete(String),
}

/// Called, on any thread, with what the user did.
pub type OnAction = Arc<dyn Fn(Action) + Send + Sync>;

pub trait Notifier: Send + Sync {
    /// Show `notice`; `on_action` hears what the user does with it. An
    /// error means it wasn't shown.
    fn show(&self, notice: &Notice, on_action: OnAction) -> Result<(), String>;
}

/// The freedesktop notification service, over D-Bus.
pub struct DesktopNotifier;

/// The action a notification's button or body sends, as an [`Action`].
/// Closing it is no action.
pub fn action_for(task_id: &str, key: &str) -> Option<Action> {
    match key {
        "default" | "open" => Some(Action::Open(task_id.to_string())),
        "complete" if !task_id.is_empty() => Some(Action::Complete(task_id.to_string())),
        _ => None,
    }
}

impl Notifier for DesktopNotifier {
    fn show(&self, notice: &Notice, on_action: OnAction) -> Result<(), String> {
        let mut notification = notify_rust::Notification::new();
        notification
            .appname("Matters")
            .summary(&notice.summary)
            .body(&notice.body)
            .icon("appointment-soon")
            // Clicking the notification opens the task, as Open does.
            .action("default", "Open")
            .action("open", "Open")
            // A reminder stays until it is dealt with.
            .timeout(notify_rust::Timeout::Never);
        if !notice.task_id.is_empty() {
            notification.action("complete", "Complete");
        }
        let handle = notification.show().map_err(|err| err.to_string())?;
        let task_id = notice.task_id.clone();
        // Waiting for the user blocks, so on a thread of its own, which
        // ends when the notification is dismissed or acted on.
        std::thread::Builder::new()
            .name("matters-reminder".into())
            .spawn(move || {
                handle.wait_for_action(|key| {
                    if let Some(action) = action_for(&task_id, key) {
                        on_action(action);
                    }
                });
            })
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

/// Keeps the notices it is asked to show, for tests; fails while
/// [`RecordingNotifier::fail`] is set.
#[derive(Clone, Default)]
pub struct RecordingNotifier {
    pub shown: Arc<Mutex<Vec<Notice>>>,
    pub failing: Arc<Mutex<bool>>,
}

impl RecordingNotifier {
    pub fn shown(&self) -> Vec<Notice> {
        self.shown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn fail(&self, failing: bool) {
        *self.failing.lock().unwrap_or_else(PoisonError::into_inner) = failing;
    }
}

impl Notifier for RecordingNotifier {
    fn show(&self, notice: &Notice, _on_action: OnAction) -> Result<(), String> {
        if *self.failing.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err("no notification service".into());
        }
        self.shown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(notice.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_to_actions() {
        assert_eq!(action_for("t", "default"), Some(Action::Open("t".into())));
        assert_eq!(action_for("t", "open"), Some(Action::Open("t".into())));
        assert_eq!(
            action_for("t", "complete"),
            Some(Action::Complete("t".into()))
        );
        // A summary has nothing to complete; closing does nothing.
        assert_eq!(action_for("", "complete"), None);
        assert_eq!(action_for("", "default"), Some(Action::Open(String::new())));
        assert_eq!(action_for("t", "__closed"), None);
    }
}
