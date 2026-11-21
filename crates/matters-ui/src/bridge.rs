//! QML-facing objects.
//!
//! * `AppController` owns navigation state, sidebar data and every command.
//!   After any change it bumps `revision`, which tells views to reload.
//! * `TaskListModel` is the task list for one view. It reconciles rows by
//!   task id so delegates (and their expansion, focus and edit state) survive
//!   reloads; see [`matters_core::list_diff`]. Today and Upcoming list
//!   calendar events among the tasks (#62, [`crate::calendar`]).
//! * `ChecklistModel` is the checklist of one open task, reconciled the same
//!   way by item id.
//!
//! View membership, "When" parsing and task placement all live in
//! `matters_core`; this layer only translates between QML and those types.

use core::pin::Pin;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QList, QModelIndex, QString, QVariant,
};
use matters_core::{
    local_today, Area, ChecklistItem, ChecklistProgress, ColorScheme, Database, DateOrder,
    DayTracker, DeadlineStatus, DeletedHeading, Heading, Home, ListRow, LogPolicy, Outcome,
    Project, QueryEngine, Schedule, Settings, SettingsFile, Status, Tag, Tagged, Task,
    TaskSnapshot, View, Visit,
};
use uuid::Uuid;

use matters_core::calendar::{
    event_days, event_section_key, events_by_day, merge_events, Calendar, DayEvent,
};
use matters_core::list_diff::{reconcile, ListSink};
use matters_core::repeat::RepeatRule;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qlist.h");
        type QList_i32 = cxx_qt_lib::QList<i32>;
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;
    }

    // `auto_cxx_name` exposes every Rust snake_case name to C++ and QML in
    // camelCase, the QML naming convention (`current_view` -> `currentView`).
    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, current_view)]
        #[qproperty(QString, current_title)]
        #[qproperty(QString, current_project_id)]
        /// The open area, in an area view (#15).
        #[qproperty(QString, current_area_id)]
        #[qproperty(f64, current_project_progress)]
        /// The open project's status (`open`, `completed` or `canceled`),
        /// in a project view; empty elsewhere (#55).
        #[qproperty(QString, current_project_status)]
        /// How many of the open project's tasks are still open.
        #[qproperty(i32, current_project_open_tasks)]
        /// The open project's notes, When (ISO start date, This Evening,
        /// Someday) and deadline (ISO date and `DeadlineStatus`), for its
        /// header (#56); empty outside a project.
        #[qproperty(QString, current_project_notes)]
        #[qproperty(QString, current_project_start_date)]
        #[qproperty(bool, current_project_evening)]
        #[qproperty(bool, current_project_someday)]
        #[qproperty(QString, current_project_deadline)]
        #[qproperty(QString, current_project_deadline_status)]
        #[qproperty(i32, inbox_count)]
        #[qproperty(i32, today_count)]
        #[qproperty(i32, upcoming_count)]
        #[qproperty(i32, anytime_count)]
        #[qproperty(i32, someday_count)]
        #[qproperty(i32, logbook_count)]
        #[qproperty(QString, projects_json)]
        /// Areas in the sidebar, in order.
        #[qproperty(QString, areas_json)]
        /// Every tag (#57), parents before their children, as JSON objects
        /// with `id`, `name`, `parent_id` and `depth`.
        #[qproperty(QString, tags_json)]
        /// The tags of the open project or area, as JSON `{id, name}`.
        #[qproperty(QString, current_tags_json)]
        /// Trashed areas, listed in the Trash view.
        #[qproperty(QString, trashed_areas_json)]
        /// Trashed projects, listed in the Trash view.
        #[qproperty(QString, trashed_projects_json)]
        #[qproperty(QString, search_results_json)]
        #[qproperty(QString, error_message)]
        #[qproperty(i32, revision)]
        /// Incremented on every navigation; a new value starts a new visit
        /// for the task list (`matters_core::Visit`).
        #[qproperty(i32, visit)]
        /// Whether the current view is arranged by hand
        /// (`View::is_reorderable`): reordering controls show only then.
        #[qproperty(bool, reorderable)]
        /// Whether tasks can be captured in the current view
        /// (`View::accepts_new_tasks`): not in the Logbook or the Trash.
        #[qproperty(bool, accepts_new_tasks)]
        /// Whether the desktop asks for reduced motion
        /// (`crate::motion::prefers_reduced_motion`); animations are skipped.
        #[qproperty(bool, reduce_motion)]
        /// When finished tasks move to the Logbook (#67), as a
        /// `LogPolicy` key; chosen with `choose_log_policy`.
        #[qproperty(QString, log_policy)]
        /// The user's calendars (#62), as JSON `[{id, name, color}]`.
        #[qproperty(QString, calendars_json)]
        /// The calendars chosen not to be shown, as a JSON array of ids.
        /// Apart from the calendars, so choosing one doesn't rebuild the
        /// list it is chosen in.
        #[qproperty(QString, hidden_calendars_json)]
        /// Why no calendar events can be shown, or empty.
        #[qproperty(QString, calendar_problem)]
        /// Whether Today and Upcoming show calendar events.
        #[qproperty(bool, show_events)]
        /// Light or dark (#72), as a `ColorScheme` key; chosen with
        /// `choose_color_scheme`.
        #[qproperty(QString, color_scheme)]
        /// The desktop's accent colour as `#rrggbb`, or empty when it has
        /// none (#76); from `crate::appearance`, updated as it changes.
        #[qproperty(QString, system_accent)]
        /// Whether the desktop asks for higher contrast (#76).
        #[qproperty(bool, high_contrast)]
        /// Whether the desktop's accent replaces Matters' own (#76);
        /// chosen with `choose_follow_accent`.
        #[qproperty(bool, follow_accent)]
        /// Sync (#42), from the app's sync session (`matters_sync_app`).
        /// Whether this device can sync at all: false without a keyring.
        #[qproperty(bool, sync_available)]
        /// `off`, `joining`, `recovering` or `on`.
        #[qproperty(QString, sync_phase)]
        /// `idle`, `syncing`, `up-to-date`, `retrying`, `waiting-for-relay`,
        /// `removed`, `storage-full`, `conflict` or `unsupported`.
        #[qproperty(QString, sync_health)]
        /// The reason behind `retrying` and `unsupported`.
        #[qproperty(QString, sync_detail)]
        /// Changes saved here that the relay has not acknowledged.
        #[qproperty(i32, sync_unsent)]
        #[qproperty(QString, sync_endpoint)]
        /// The invitation to show a new device, while adding one.
        #[qproperty(QString, sync_invitation)]
        /// The recovery kit to show once, as JSON `{endpoint, collection,
        /// words}`, or empty.
        #[qproperty(QString, sync_kit_json)]
        /// The relay operator's command for a new collection, or empty.
        #[qproperty(QString, sync_provision_command)]
        /// Member devices, as JSON `[{id, short, thisDevice, admin}]`.
        #[qproperty(QString, sync_devices_json)]
        #[qproperty(bool, sync_admin)]
        #[qproperty(QString, sync_error)]
        type AppController = super::AppControllerRust;

        /// Start a new collection syncing with the relay at `endpoint`.
        #[qinvokable]
        fn sync_create(self: Pin<&mut AppController>, endpoint: &QString);
        /// Join the collection an invitation from another device names.
        #[qinvokable]
        fn sync_join(self: Pin<&mut AppController>, endpoint: &QString, invitation: &QString);
        /// Recover the collection from its recovery kit.
        #[qinvokable]
        fn sync_recover(
            self: Pin<&mut AppController>,
            endpoint: &QString,
            collection: &QString,
            words: &QString,
        );
        /// Show an invitation for a new device.
        #[qinvokable]
        fn sync_invite(self: Pin<&mut AppController>);
        #[qinvokable]
        fn sync_cancel_invitation(self: Pin<&mut AppController>);
        /// The recovery kit is written down: stop showing it.
        #[qinvokable]
        fn sync_kit_kept(self: Pin<&mut AppController>);
        /// Remove the device whose ID is `id` (hex), as the admin.
        #[qinvokable]
        fn sync_remove_device(self: Pin<&mut AppController>, id: &QString);
        /// Stop syncing and keep everything here. Returns 0 when done, or
        /// how many changes have not reached the relay when that refused
        /// it; `discard_unsent` keeps them here only.
        #[qinvokable]
        fn sync_leave(self: Pin<&mut AppController>, discard_unsent: bool) -> i32;
        #[qinvokable]
        fn sync_now(self: Pin<&mut AppController>);

        /// Choose when finished tasks move to the Logbook (a `LogPolicy`
        /// key), and keep the choice (#67, #72). What is finished so far
        /// counts as logged.
        #[qinvokable]
        fn choose_log_policy(self: Pin<&mut AppController>, policy: &QString);
        /// Log Completed: every finished task still in its list moves to the
        /// Logbook (#67).
        #[qinvokable]
        fn log_completed(self: Pin<&mut AppController>);
        /// Read the calendars again (#62): at start, then every minute
        /// (`check_date`). In the app the read happens on a worker thread
        /// and the lists reload when it changed something.
        #[qinvokable]
        fn refresh_calendars(self: Pin<&mut AppController>);
        /// Show calendar events in Today and Upcoming, or not, and keep the
        /// choice.
        #[qinvokable]
        fn choose_show_events(self: Pin<&mut AppController>, shown: bool);
        /// Show or hide the events of calendar `id`, and keep the choice.
        #[qinvokable]
        fn show_calendar(self: Pin<&mut AppController>, id: &QString, shown: bool);
        /// Choose light, dark or the desktop's scheme (a `ColorScheme`
        /// key), and keep the choice (#72).
        #[qinvokable]
        fn choose_color_scheme(self: Pin<&mut AppController>, scheme: &QString);
        /// Use the desktop's accent colour, when it has one, or Matters'
        /// own, and keep the choice (#76).
        #[qinvokable]
        fn choose_follow_accent(self: Pin<&mut AppController>, follow: bool);

        #[qinvokable]
        fn select_view(self: Pin<&mut AppController>, view: &QString);
        #[qinvokable]
        fn select_project(self: Pin<&mut AppController>, project_id: &QString);
        /// Open an area: the tasks filed directly in it.
        #[qinvokable]
        fn select_area(self: Pin<&mut AppController>, area_id: &QString);
        /// Create an area at the end of the sidebar and open it; returns its id,
        /// or an empty string for a blank name.
        #[qinvokable]
        fn create_area(self: Pin<&mut AppController>, title: &QString) -> QString;
        /// Rename an area; false, changing nothing, for a blank name.
        #[qinvokable]
        fn rename_area(self: Pin<&mut AppController>, area_id: &QString, title: &QString) -> bool;
        /// Move an area, with its projects and tasks, to the Trash. If it is
        /// the open view, the Inbox is shown instead.
        #[qinvokable]
        fn trash_area(self: Pin<&mut AppController>, area_id: &QString);
        /// Put a trashed area back with its projects and tasks.
        #[qinvokable]
        fn restore_area(self: Pin<&mut AppController>, area_id: &QString);
        /// Rename a project; false, changing nothing, for a blank name.
        #[qinvokable]
        fn rename_project(
            self: Pin<&mut AppController>,
            project_id: &QString,
            title: &QString,
        ) -> bool;
        /// File a project in an area, or take it out of one (empty `area_id`).
        #[qinvokable]
        fn set_project_area(self: Pin<&mut AppController>, project_id: &QString, area_id: &QString);
        #[qinvokable]
        /// Capture a task in the current view; returns its id, or an empty
        /// string when nothing was created.
        fn add_task(self: Pin<&mut AppController>, title: &QString) -> QString;
        /// Capture a task right below `below_id` in the current view (Enter on
        /// a selected task); returns its id, or an empty string when nothing
        /// was created (a blank title, or a list not arranged by hand).
        #[qinvokable]
        fn add_task_below(
            self: Pin<&mut AppController>,
            title: &QString,
            below_id: &QString,
        ) -> QString;
        /// Tick an open task done; reopen a completed or canceled one.
        #[qinvokable]
        fn toggle_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Cancel a task: finished without being done (#65). A canceled
        /// task is reopened by `toggle_task`.
        #[qinvokable]
        fn cancel_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Copy a task just below itself, open, with its details and
        /// checklist (#65); returns the copy's id, or an empty string.
        #[qinvokable]
        fn duplicate_task(self: Pin<&mut AppController>, task_id: &QString) -> QString;
        /// Turn an open task into a project of its checklist and open the
        /// project (#65); returns its id, or an empty string.
        #[qinvokable]
        fn convert_task_to_project(self: Pin<&mut AppController>, task_id: &QString) -> QString;
        /// Move a task to the Trash (recoverable until the Trash is emptied).
        #[qinvokable]
        fn trash_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Put a trashed task back where it was.
        #[qinvokable]
        fn restore_task(self: Pin<&mut AppController>, task_id: &QString);
        /// Move a task `step` places up (negative) or down in the current
        /// view, if the view is arranged by hand (`View::is_reorderable`).
        #[qinvokable]
        fn move_task(self: Pin<&mut AppController>, task_id: &QString, step: i32);
        /// Move a task in the current view so it follows `after_id`, or comes
        /// first when `after_id` is empty (drag and drop).
        #[qinvokable]
        fn place_task(self: Pin<&mut AppController>, task_id: &QString, after_id: &QString);
        /// Put a Today task in the daytime tasks or This Evening (`evening`),
        /// after `after_id` within that section, or first when it is empty.
        /// Changing section switches This Evening (drag and drop in Today).
        #[qinvokable]
        fn place_today_task(
            self: Pin<&mut AppController>,
            task_id: &QString,
            evening: bool,
            after_id: &QString,
        );
        /// Move a project in the sidebar so it follows `after_id`, or comes
        /// first when `after_id` is empty.
        #[qinvokable]
        fn place_project(self: Pin<&mut AppController>, project_id: &QString, after_id: &QString);
        /// File a project in an area (empty: none) right after `after_id`
        /// among its projects, or first when that is empty (#164).
        #[qinvokable]
        fn place_project_in_area(
            self: Pin<&mut AppController>,
            project_id: &QString,
            area_id: &QString,
            after_id: &QString,
        );
        /// Move an area in the sidebar so it follows `after_id`, or comes
        /// first when that is empty.
        #[qinvokable]
        fn place_area(self: Pin<&mut AppController>, area_id: &QString, after_id: &QString);
        /// Permanently delete everything in the Trash.
        #[qinvokable]
        fn empty_trash(self: Pin<&mut AppController>);
        /// Move a project and its tasks to the Trash. If it is the open view,
        /// the Inbox is shown instead.
        #[qinvokable]
        fn trash_project(self: Pin<&mut AppController>, project_id: &QString);
        /// Put a trashed project back with its tasks.
        #[qinvokable]
        fn restore_project(self: Pin<&mut AppController>, project_id: &QString);
        /// Complete or cancel a project (`outcome`: `completed` or
        /// `canceled`), closing its open tasks as `open_tasks` says, the same
        /// two choices (#55). Returns false, changing nothing, when refused.
        #[qinvokable]
        fn finish_project(
            self: Pin<&mut AppController>,
            project_id: &QString,
            outcome: &QString,
            open_tasks: &QString,
        ) -> bool;
        /// Reopen a finished project, with the tasks finishing it closed.
        #[qinvokable]
        fn reopen_project(self: Pin<&mut AppController>, project_id: &QString);
        /// The project counterparts of `schedule_task`,
        /// `schedule_task_typed`, `set_task_deadline`,
        /// `set_task_deadline_typed` and `update_task_notes` (#56): the When
        /// popover and the header's notes edit a project with them.
        #[qinvokable]
        fn schedule_project(self: Pin<&mut AppController>, project_id: &QString, when: &QString);
        #[qinvokable]
        fn schedule_project_typed(
            self: Pin<&mut AppController>,
            project_id: &QString,
            text: &QString,
            month_first: bool,
        ) -> bool;
        #[qinvokable]
        fn set_project_deadline(
            self: Pin<&mut AppController>,
            project_id: &QString,
            date: &QString,
        ) -> bool;
        #[qinvokable]
        fn set_project_deadline_typed(
            self: Pin<&mut AppController>,
            project_id: &QString,
            text: &QString,
            month_first: bool,
        ) -> bool;
        #[qinvokable]
        fn update_project_notes(
            self: Pin<&mut AppController>,
            project_id: &QString,
            notes: &QString,
        );
        /// Tags (#57). `kind` names what is tagged: `task`, `project` or
        /// `area`. `create_tag` returns the new tag's id, or an empty
        /// string when the name is refused (blank or taken).
        #[qinvokable]
        fn create_tag(
            self: Pin<&mut AppController>,
            name: &QString,
            parent_id: &QString,
        ) -> QString;
        #[qinvokable]
        fn rename_tag(self: Pin<&mut AppController>, tag_id: &QString, name: &QString) -> bool;
        /// Nest a tag under another, or make it top-level (empty parent).
        #[qinvokable]
        fn set_tag_parent(
            self: Pin<&mut AppController>,
            tag_id: &QString,
            parent_id: &QString,
        ) -> bool;
        #[qinvokable]
        fn delete_tag(self: Pin<&mut AppController>, tag_id: &QString);
        #[qinvokable]
        fn add_tag(
            self: Pin<&mut AppController>,
            kind: &QString,
            item_id: &QString,
            tag_id: &QString,
        );
        #[qinvokable]
        fn remove_tag(
            self: Pin<&mut AppController>,
            kind: &QString,
            item_id: &QString,
            tag_id: &QString,
        );
        /// Give an item the tag named `name` (ignoring case), creating the
        /// tag when there is none. False when the name is blank.
        #[qinvokable]
        fn tag_by_name(
            self: Pin<&mut AppController>,
            kind: &QString,
            item_id: &QString,
            name: &QString,
        ) -> bool;
        #[qinvokable]
        fn schedule_task(self: Pin<&mut AppController>, task_id: &QString, when: &QString);
        #[qinvokable]
        fn update_task_notes(self: Pin<&mut AppController>, task_id: &QString, notes: &QString);
        /// What a typed When means (`Schedule::parse_typed`): `today`,
        /// `evening`, `someday`, an ISO date, or an empty string when it is
        /// refused. `month_first` follows the user's locale (`12/24`).
        #[qinvokable]
        fn preview_when(self: &AppController, text: &QString, month_first: bool) -> QString;
        /// Schedule a task from a typed When; false, changing nothing, when the
        /// text is refused.
        #[qinvokable]
        fn schedule_task_typed(
            self: Pin<&mut AppController>,
            task_id: &QString,
            text: &QString,
            month_first: bool,
        ) -> bool;
        /// What a typed deadline means (`Schedule::parse_typed_deadline`): an
        /// ISO date, or an empty string when it is refused.
        #[qinvokable]
        fn preview_deadline(self: &AppController, text: &QString, month_first: bool) -> QString;
        /// What a typed reminder time means (#60,
        /// `reminders::parse_typed_time`): `HH:MM`, or an empty string when
        /// it is refused.
        #[qinvokable]
        fn preview_reminder(self: &AppController, text: &QString) -> QString;
        /// Remind about a task at a typed time (#60); a task with no date is
        /// scheduled for today. False, changing nothing, when the time is
        /// refused.
        #[qinvokable]
        fn set_task_reminder_typed(
            self: Pin<&mut AppController>,
            task_id: &QString,
            text: &QString,
        ) -> bool;
        /// Clear a task's reminder.
        #[qinvokable]
        fn clear_task_reminder(self: Pin<&mut AppController>, task_id: &QString);
        /// Make a task repeat by a rule (#61), as JSON (`RepeatRule`), or
        /// stop it repeating (an empty string). False, changing nothing,
        /// when the rule can't be used.
        #[qinvokable]
        fn set_task_repeat(
            self: Pin<&mut AppController>,
            task_id: &QString,
            rule: &QString,
        ) -> bool;
        /// A rule (JSON) in words, or an empty string when it can't be used.
        #[qinvokable]
        fn describe_repeat(self: &AppController, rule: &QString) -> QString;
        /// Day `iso` (YYYY-MM-DD) as the user's locale writes it (#75):
        /// `style` is "day-month", "day-month-year", "weekday-day-month",
        /// "weekday-day-month-year", "month-year" or "weekday" (its short
        /// name). Empty for a day that isn't one.
        #[qinvokable]
        fn format_date(self: &AppController, iso: &QString, style: &QString) -> QString;
        /// Show the reminders now due (#60). Called every few seconds.
        #[qinvokable]
        fn check_reminders(self: Pin<&mut AppController>);
        /// Open was chosen on a reminder: show task `task_id`, or Today for
        /// a reminder of several (an empty id).
        #[qsignal]
        fn reminder_opened(self: Pin<&mut AppController>, task_id: QString);
        /// Set a task's deadline from typed text; false, changing nothing,
        /// when the text is refused.
        #[qinvokable]
        fn set_task_deadline_typed(
            self: Pin<&mut AppController>,
            task_id: &QString,
            text: &QString,
            month_first: bool,
        ) -> bool;
        /// Set a task's deadline to an ISO date, or clear it with an empty
        /// string; false when the date isn't valid.
        #[qinvokable]
        fn set_task_deadline(
            self: Pin<&mut AppController>,
            task_id: &QString,
            date: &QString,
        ) -> bool;
        /// Rename a task. Returns false, changing nothing, when the title is
        /// blank (`Database::rename_task`); the field then shows the old title.
        #[qinvokable]
        fn rename_task(self: Pin<&mut AppController>, task_id: &QString, title: &QString) -> bool;
        #[qinvokable]
        fn create_project(self: Pin<&mut AppController>, title: &QString);
        /// Navigate to the view that contains the task (`View::containing`).
        #[qinvokable]
        fn reveal_task(self: Pin<&mut AppController>, task_id: &QString);
        #[qinvokable]
        fn search(self: Pin<&mut AppController>, query: &QString);
        #[qinvokable]
        fn clear_error(self: Pin<&mut AppController>);
        /// Refresh if the local date changed since the views were built.
        /// Called periodically by the UI.
        #[qinvokable]
        fn check_date(self: Pin<&mut AppController>);

        /// Add an item to a task's checklist: right below `after_id`, or at
        /// the end when it is empty. Returns the new item's id, or an empty
        /// string when nothing was added (a blank title).
        #[qinvokable]
        fn add_checklist_item(
            self: Pin<&mut AppController>,
            task_id: &QString,
            title: &QString,
            after_id: &QString,
        ) -> QString;
        /// Rename a checklist item. Returns false, changing nothing, when the
        /// title is blank; the field then shows the old title.
        #[qinvokable]
        fn rename_checklist_item(
            self: Pin<&mut AppController>,
            item_id: &QString,
            title: &QString,
        ) -> bool;
        #[qinvokable]
        fn set_checklist_item_done(self: Pin<&mut AppController>, item_id: &QString, done: bool);
        /// Delete a checklist item; `restore_checklist_item` undoes the
        /// last deletion (#169).
        #[qinvokable]
        fn delete_checklist_item(self: Pin<&mut AppController>, item_id: &QString);
        #[qinvokable]
        fn restore_checklist_item(self: Pin<&mut AppController>);
        /// Remember tasks `ids_json` (a JSON array of ids) as they are, before
        /// an action on all of them (#64); returns how many were remembered.
        /// Replaces what was remembered before.
        #[qinvokable]
        fn remember_tasks(self: Pin<&mut AppController>, ids_json: &QString) -> i32;
        /// Undo the action on the tasks last remembered: put each back as it
        /// was (`matters_core::TaskSnapshot`). Once only.
        #[qinvokable]
        fn restore_remembered_tasks(self: Pin<&mut AppController>);
        /// Move a heading, with its tasks, to another project (#169).
        #[qinvokable]
        fn move_heading_to_project(
            self: Pin<&mut AppController>,
            heading_id: &QString,
            project_id: &QString,
        );
        /// Move a checklist item `step` places up (negative) or down.
        #[qinvokable]
        fn move_checklist_item(self: Pin<&mut AppController>, item_id: &QString, step: i32);
        /// Move a checklist item to follow `after_id` (first when empty).
        #[qinvokable]
        fn place_checklist_item(
            self: Pin<&mut AppController>,
            item_id: &QString,
            after_id: &QString,
        );

        /// Add a heading at the end of the current project. Returns its id,
        /// or an empty string when nothing was added (a blank title).
        #[qinvokable]
        fn add_heading(self: Pin<&mut AppController>, title: &QString) -> QString;
        /// Rename a heading. Returns false, changing nothing, when the title
        /// is blank.
        #[qinvokable]
        fn rename_heading(
            self: Pin<&mut AppController>,
            heading_id: &QString,
            title: &QString,
        ) -> bool;
        /// Delete a heading; its tasks stay in the project, above the
        /// headings. `restore_heading` undoes the last deletion.
        #[qinvokable]
        fn delete_heading(self: Pin<&mut AppController>, heading_id: &QString);
        #[qinvokable]
        fn restore_heading(self: Pin<&mut AppController>);
        /// Move a heading, with its tasks, `step` places up or down.
        #[qinvokable]
        fn move_heading(self: Pin<&mut AppController>, heading_id: &QString, step: i32);
        /// Put a task of the current project under `heading_id` (empty: above
        /// the headings), right after `after_id` there (first when empty).
        #[qinvokable]
        fn place_project_task(
            self: Pin<&mut AppController>,
            task_id: &QString,
            heading_id: &QString,
            after_id: &QString,
        );

        /// Where the Move popover can send `task_id` (SPEC §4), as JSON, in
        /// sidebar order: the Inbox; the projects without an area, each
        /// followed by its headings; then each area with its projects. Each
        /// entry has `kind` (`inbox`, `area`, `project` or `heading`), `id`,
        /// `title`, `context` (the project or area it sits in), `depth`
        /// (for indenting) and `current` (the task's home).
        #[qinvokable]
        fn move_destinations(self: &AppController, task_id: &QString) -> QString;
        /// File a task at the end of a destination from `move_destinations`
        /// (`kind` and `id`; the Inbox has no id). Returns false, changing
        /// nothing, when it is refused.
        #[qinvokable]
        fn move_task_to(
            self: Pin<&mut AppController>,
            task_id: &QString,
            kind: &QString,
            id: &QString,
        ) -> bool;
    }

    impl cxx_qt::Initialize for AppController {}

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        #[qproperty(QString, error_message)]
        /// The tags the view can be narrowed to (#58), its tasks' own and
        /// inherited tags with their parents, as JSON `[{id, name, depth}]`;
        /// worked out before any filter applies.
        #[qproperty(QString, present_tags_json)]
        type TaskListModel = super::TaskListModelRust;

        /// Show the tasks of `view` (a `View` key; `project_id` for projects)
        /// during navigation `visit` (`AppController.visit`), narrowed to
        /// the tag `tag` unless it is empty (#58).
        #[qinvokable]
        fn load(
            self: Pin<&mut TaskListModel>,
            view: &QString,
            project_id: &QString,
            visit: i32,
            tag: &QString,
        );
        /// Row of the task with this id, or -1.
        #[qinvokable]
        fn row_of(self: &TaskListModel, task_id: &QString) -> i32;
        /// Id of the task at `row`, or an empty string when out of range.
        #[qinvokable]
        fn id_at(self: &TaskListModel, row: i32) -> QString;
        /// Title of the task at `row`, or an empty string when out of range.
        #[qinvokable]
        fn title_at(self: &TaskListModel, row: i32) -> QString;
        /// Whether the task at `row` is scheduled for This Evening.
        #[qinvokable]
        fn evening_at(self: &TaskListModel, row: i32) -> bool;
        /// The reminder time of the task at `row` (#60), `HH:MM`, or an
        /// empty string when it has none.
        #[qinvokable]
        fn reminder_at(self: &TaskListModel, row: i32) -> QString;
        /// The repeat rule the task at `row` carries (#61), as JSON, or an
        /// empty string when it carries none.
        #[qinvokable]
        fn repeat_at(self: &TaskListModel, row: i32) -> QString;
        /// The status of the task at `row` (`open`, `completed` or
        /// `canceled`), or an empty string when out of range.
        #[qinvokable]
        fn status_at(self: &TaskListModel, row: i32) -> QString;
        /// The heading the row at `row` is under (a task) or is (an empty
        /// heading's row), or an empty string.
        #[qinvokable]
        fn heading_at(self: &TaskListModel, row: i32) -> QString;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[inherit]
        fn begin_insert_rows(
            self: Pin<&mut TaskListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_insert_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn begin_remove_rows(
            self: Pin<&mut TaskListModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_remove_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn begin_move_rows(
            self: Pin<&mut TaskListModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            destination_parent: &QModelIndex,
            destination_child: i32,
        ) -> bool;
        #[inherit]
        fn end_move_rows(self: Pin<&mut TaskListModel>);
        #[inherit]
        fn index(self: &TaskListModel, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;

        #[qsignal]
        #[inherit]
        fn data_changed(
            self: Pin<&mut TaskListModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[cxx_override]
        fn data(self: &TaskListModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn role_names(self: &TaskListModel) -> QHash_i32_QByteArray;
        #[cxx_override]
        fn row_count(self: &TaskListModel, parent: &QModelIndex) -> i32;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QAbstractListModel]
        type ChecklistModel = super::ChecklistModelRust;

        /// Show the checklist of task `task_id`. Rows are reconciled by item
        /// id, so a field being edited keeps its delegate across reloads.
        #[qinvokable]
        fn load(self: Pin<&mut ChecklistModel>, task_id: &QString);
        /// Id of the item at `row`, or an empty string when out of range.
        #[qinvokable]
        fn id_at(self: &ChecklistModel, row: i32) -> QString;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[inherit]
        fn begin_insert_rows(
            self: Pin<&mut ChecklistModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_insert_rows(self: Pin<&mut ChecklistModel>);
        #[inherit]
        fn begin_remove_rows(
            self: Pin<&mut ChecklistModel>,
            parent: &QModelIndex,
            first: i32,
            last: i32,
        );
        #[inherit]
        fn end_remove_rows(self: Pin<&mut ChecklistModel>);
        #[inherit]
        fn begin_move_rows(
            self: Pin<&mut ChecklistModel>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            destination_parent: &QModelIndex,
            destination_child: i32,
        ) -> bool;
        #[inherit]
        fn end_move_rows(self: Pin<&mut ChecklistModel>);
        #[inherit]
        fn index(self: &ChecklistModel, row: i32, column: i32, parent: &QModelIndex)
            -> QModelIndex;

        #[qsignal]
        #[inherit]
        fn data_changed(
            self: Pin<&mut ChecklistModel>,
            top_left: &QModelIndex,
            bottom_right: &QModelIndex,
            roles: &QList_i32,
        );

        #[cxx_override]
        fn data(self: &ChecklistModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        fn role_names(self: &ChecklistModel) -> QHash_i32_QByteArray;
        #[cxx_override]
        fn row_count(self: &ChecklistModel, parent: &QModelIndex) -> i32;
    }

    // Calendar reads (#62) and reminder notifications (#60) finish on
    // other threads and are handed back here.
    impl cxx_qt::Threading for AppController {}
}

// =============================================================================
// Shared database
// =============================================================================

/// QML instantiates both objects through `Default`, which can neither fail nor
/// take arguments, so `main` opens the database first and installs it here.
static DATABASE: OnceLock<Database> = OnceLock::new();

/// The app's sync session (#42), when this device can sync.
static SYNC: OnceLock<matters_sync_app::SyncApp> = OnceLock::new();
/// Who hears the session's changes: the controller, once it exists.
static SYNC_LISTENER: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

/// What the sync session calls on every change of its view, from any
/// thread: it reaches the controller on the Qt thread.
pub fn sync_changed_notice() {
    if let Some(listener) = SYNC_LISTENER.lock().unwrap().as_ref() {
        listener();
    }
}

/// Install the app's sync session, before any QML object is created.
pub fn install_sync(app: matters_sync_app::SyncApp) {
    if SYNC.set(app).is_err() {
        panic!("matters-ui: sync installed twice");
    }
}

/// Keep the relay this device syncs with in the settings, or forget it.
pub fn save_sync_endpoint(endpoint: Option<String>) {
    let mut state = settings();
    state.settings.sync.endpoint = endpoint;
    if let Err(err) = state.save() {
        eprintln!("matters: {}", not_saved(&err));
    }
}

/// The relay this device syncs with, as the settings keep it.
pub fn saved_sync_endpoint() -> Option<String> {
    settings().settings.sync.endpoint.clone()
}

pub fn install_database(db: Database) {
    if DATABASE.set(db).is_err() {
        panic!("matters-ui: database installed twice");
    }
}

fn database() -> Database {
    DATABASE
        .get()
        .cloned()
        .expect("matters-ui: QML object created before install_database()")
}

// =============================================================================
// Shared settings
// =============================================================================

/// This device's settings (#72) and where they are kept.
struct SettingsState {
    /// `None` when there is nowhere to keep them: they last until exit.
    file: Option<SettingsFile>,
    settings: Settings,
    /// Why the settings are the defaults, said once the window is up.
    problem: Option<SettingsTrouble>,
}

/// Why the settings are the defaults (#72), said in the user's language
/// (#75) when it is reported, not when it is found: that is before the
/// translations are installed.
#[derive(Clone)]
enum SettingsTrouble {
    /// The file couldn't be read.
    Unreadable(matters_core::SettingsProblem),
    /// There is nowhere to keep them, and why.
    NotKept(String),
}

impl SettingsTrouble {
    fn message(&self) -> String {
        match self {
            SettingsTrouble::Unreadable(problem) => crate::strings::SETTINGS_UNREADABLE
                .fill(&[&problem.path.display().to_string(), &problem.cause]),
            SettingsTrouble::NotKept(cause) => crate::strings::SETTINGS_NOT_KEPT.fill(&[cause]),
        }
    }
}

/// "Your settings couldn't be saved", in the user's language.
fn not_saved(err: &matters_core::MattersError) -> String {
    crate::strings::SETTINGS_NOT_SAVED.fill(&[&err.to_string()])
}

impl SettingsState {
    /// Keep the settings, if there is somewhere to.
    fn save(&self) -> matters_core::Result<()> {
        match &self.file {
            Some(file) => file.save(&self.settings),
            None => Ok(()),
        }
    }
}

static SETTINGS: OnceLock<Mutex<SettingsState>> = OnceLock::new();

/// Load the settings from `file` (`None`: nowhere to keep them, so the
/// defaults, unsaved, with `not_kept` saying why). Call before QML starts;
/// without it the defaults are used and nothing is saved.
pub fn install_settings(file: Option<SettingsFile>, not_kept: Option<String>) {
    let (settings, problem) = match &file {
        Some(file) => {
            let (settings, problem) = file.load();
            (settings, problem.map(SettingsTrouble::Unreadable))
        }
        None => (Settings::default(), not_kept.map(SettingsTrouble::NotKept)),
    };
    let state = SettingsState {
        file,
        settings,
        problem,
    };
    if SETTINGS.set(Mutex::new(state)).is_err() {
        panic!("matters-ui: settings installed twice");
    }
}

fn settings() -> std::sync::MutexGuard<'static, SettingsState> {
    SETTINGS
        .get_or_init(|| {
            Mutex::new(SettingsState {
                file: None,
                settings: Settings::default(),
                problem: None,
            })
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The time up to which finished tasks are logged today
/// (`Settings::logged_until`), and whether a visit keeps ticked tasks.
fn logging() -> (Option<i64>, bool) {
    let state = settings();
    (
        state.settings.logged_until(local_today()),
        state.settings.logbook.policy.keeps_ticked(),
    )
}

/// A calendar as the Preferences list it (#62).
#[derive(serde::Serialize)]
struct CalendarEntry {
    id: String,
    name: String,
    /// `#rrggbb`, or empty.
    color: String,
}

fn to_json<T: serde::Serialize>(value: &T) -> QString {
    QString::from(
        serde_json::to_string(value)
            .expect("domain types serialize")
            .as_str(),
    )
}

// =============================================================================
// AppController
// =============================================================================

pub struct AppControllerRust {
    current_view: QString,
    current_title: QString,
    current_project_id: QString,
    current_project_progress: f64,
    current_project_status: QString,
    current_project_open_tasks: i32,
    current_project_notes: QString,
    current_project_start_date: QString,
    current_project_evening: bool,
    current_project_someday: bool,
    current_project_deadline: QString,
    current_project_deadline_status: QString,
    inbox_count: i32,
    today_count: i32,
    upcoming_count: i32,
    anytime_count: i32,
    someday_count: i32,
    logbook_count: i32,
    projects_json: QString,
    areas_json: QString,
    tags_json: QString,
    current_tags_json: QString,
    trashed_areas_json: QString,
    current_area_id: QString,
    trashed_projects_json: QString,
    search_results_json: QString,
    error_message: QString,
    revision: i32,
    log_policy: QString,
    calendars_json: QString,
    hidden_calendars_json: QString,
    calendar_problem: QString,
    show_events: bool,
    color_scheme: QString,
    system_accent: QString,
    high_contrast: bool,
    follow_accent: bool,
    sync_available: bool,
    sync_phase: QString,
    sync_health: QString,
    sync_detail: QString,
    sync_unsent: i32,
    sync_endpoint: QString,
    sync_invitation: QString,
    sync_kit_json: QString,
    sync_provision_command: QString,
    sync_devices_json: QString,
    sync_admin: bool,
    sync_error: QString,
    visit: i32,
    reorderable: bool,
    accepts_new_tasks: bool,
    reduce_motion: bool,

    view: View,
    /// The date the sidebar and header were last built for.
    day: DayTracker,
    search_query: String,
    db: Database,
    /// The last heading deleted, for Undo (`restore_heading`).
    deleted_heading: Option<DeletedHeading>,
    /// The last checklist item deleted, for Undo (`restore_checklist_item`).
    deleted_checklist_item: Option<ChecklistItem>,
    /// The tasks as they were before the last action on several (#64).
    remembered: Option<TaskSnapshot>,
    /// Why reminders can't be shown, once said (#60): it isn't repeated
    /// on every check.
    reminder_problem: Option<String>,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        let view = View::Today;
        // One lock: a guard taken inside the struct expression would live
        // to its end, and a second `settings()` there would wait on it.
        let (log_policy, show_events, hidden, scheme, follow_accent) = {
            let state = settings();
            (
                state.settings.logbook.policy.key(),
                state.settings.calendar.show_events,
                to_json(&state.settings.calendar.hidden),
                state.settings.appearance.scheme.key(),
                state.settings.appearance.follow_accent,
            )
        };
        Self {
            current_view: QString::from(view.key()),
            current_title: QString::from(view.title()),
            current_project_id: QString::default(),
            current_project_progress: 0.0,
            current_project_status: QString::default(),
            current_project_open_tasks: 0,
            current_project_notes: QString::default(),
            current_project_start_date: QString::default(),
            current_project_evening: false,
            current_project_someday: false,
            current_project_deadline: QString::default(),
            current_project_deadline_status: QString::default(),
            inbox_count: 0,
            today_count: 0,
            upcoming_count: 0,
            anytime_count: 0,
            someday_count: 0,
            logbook_count: 0,
            projects_json: QString::from("[]"),
            areas_json: QString::from("[]"),
            tags_json: QString::from("[]"),
            current_tags_json: QString::from("[]"),
            trashed_areas_json: QString::from("[]"),
            current_area_id: QString::default(),
            trashed_projects_json: QString::from("[]"),
            search_results_json: QString::from("[]"),
            error_message: QString::default(),
            revision: 0,
            visit: 0,
            reorderable: view.is_reorderable(),
            accepts_new_tasks: view.accepts_new_tasks(),
            reduce_motion: crate::motion::prefers_reduced_motion(),
            log_policy: QString::from(log_policy),
            calendars_json: QString::from("[]"),
            hidden_calendars_json: hidden,
            calendar_problem: QString::default(),
            show_events,
            color_scheme: QString::from(scheme),
            system_accent: QString::default(),
            high_contrast: false,
            follow_accent,
            sync_available: SYNC.get().is_some(),
            sync_phase: QString::from("off"),
            sync_health: QString::from("idle"),
            sync_detail: QString::default(),
            sync_unsent: 0,
            sync_endpoint: QString::default(),
            sync_invitation: QString::default(),
            sync_kit_json: QString::default(),
            sync_provision_command: QString::default(),
            sync_devices_json: QString::from("[]"),
            sync_admin: false,
            sync_error: QString::default(),
            view,
            day: DayTracker::new(local_today()),
            search_query: String::new(),
            deleted_heading: None,
            deleted_checklist_item: None,
            remembered: None,
            reminder_problem: None,
            db: database(),
        }
    }
}

impl cxx_qt::Initialize for qobject::AppController {
    fn initialize(mut self: Pin<&mut Self>) {
        // Occurrences that came due while Matters was closed (#61).
        self.as_mut().advance_repeats();
        self.as_mut().changed();
        // The desktop's accent and contrast (#76): as they are now, then
        // each change, on this thread.
        let thread = self.qt_thread();
        crate::appearance::on_change(Box::new(move || {
            // Gone only when the window is.
            let _ = thread.queue(|controller| controller.appearance_changed());
        }));
        self.as_mut().appearance_changed();
        // Sync (#42): each change of the session's view, on this thread.
        let thread = self.qt_thread();
        *SYNC_LISTENER.lock().unwrap() = Some(Box::new(move || {
            let _ = thread.queue(|controller| controller.sync_changed());
        }));
        self.as_mut().sync_changed();
        let problem = settings().problem.clone();
        if let Some(problem) = problem {
            self.report(problem.message());
        }
    }
}

impl qobject::AppController {
    pub fn choose_log_policy(mut self: Pin<&mut Self>, policy: &QString) {
        let Some(policy) = LogPolicy::from_key(&String::from(policy)) else {
            return self.report(format!("unknown Logbook policy '{policy}'"));
        };
        let saved = {
            let mut state = settings();
            state
                .settings
                .set_log_policy(policy, matters_core::clock::now_millis());
            state.save()
        };
        self.as_mut().set_log_policy(QString::from(policy.key()));
        if let Err(err) = saved {
            self.as_mut().report(not_saved(&err));
        }
        self.changed();
    }

    pub fn refresh_calendars(self: Pin<&mut Self>) {
        if crate::calendar::in_background() {
            let thread = self.qt_thread();
            crate::calendar::start_worker(move || {
                // Gone only when the app is quitting.
                let _ = thread.queue(|controller| controller.calendars_changed());
            });
            crate::calendar::request_refresh();
        } else if crate::calendar::request_refresh() {
            self.calendars_changed();
        }
    }

    pub fn choose_show_events(mut self: Pin<&mut Self>, shown: bool) {
        let saved = {
            let mut state = settings();
            state.settings.calendar.show_events = shown;
            state.save()
        };
        self.as_mut().set_show_events(shown);
        if let Err(err) = saved {
            self.as_mut().report(not_saved(&err));
        }
        self.calendars_changed();
    }

    pub fn show_calendar(mut self: Pin<&mut Self>, id: &QString, shown: bool) {
        let (saved, hidden) = {
            let mut state = settings();
            state.settings.calendar.set_shown(&String::from(id), shown);
            (state.save(), to_json(&state.settings.calendar.hidden))
        };
        self.as_mut().set_hidden_calendars_json(hidden);
        if let Err(err) = saved {
            self.as_mut().report(not_saved(&err));
        }
        self.calendars_changed();
    }

    pub fn choose_color_scheme(mut self: Pin<&mut Self>, scheme: &QString) {
        let Some(scheme) = ColorScheme::from_key(&String::from(scheme)) else {
            return self.report(format!("unknown colour scheme '{scheme}'"));
        };
        let saved = {
            let mut state = settings();
            state.settings.appearance.scheme = scheme;
            state.save()
        };
        self.as_mut().set_color_scheme(QString::from(scheme.key()));
        if let Err(err) = saved {
            self.report(not_saved(&err));
        }
    }

    pub fn choose_follow_accent(mut self: Pin<&mut Self>, follow: bool) {
        let saved = {
            let mut state = settings();
            state.settings.appearance.follow_accent = follow;
            state.save()
        };
        self.as_mut().set_follow_accent(follow);
        if let Err(err) = saved {
            self.report(not_saved(&err));
        }
    }

    /// Show the sync session's view (#42), and reload the lists: what other
    /// devices changed has arrived. Delegates keep their focus and edits
    /// (`TaskListModel` reconciles by task id).
    fn sync_changed(mut self: Pin<&mut Self>) {
        let Some(sync) = SYNC.get() else {
            return;
        };
        use matters_sync_app::{Health, Phase};
        let v = sync.view();
        let phase = match v.phase {
            Phase::Off => "off",
            Phase::Joining => "joining",
            Phase::Recovering => "recovering",
            Phase::On => "on",
        };
        let (health, detail) = match &v.health {
            Health::Idle => ("idle", String::new()),
            Health::Syncing => ("syncing", String::new()),
            Health::UpToDate => ("up-to-date", String::new()),
            Health::Retrying(why) => ("retrying", why.clone()),
            Health::WaitingForRelay => ("waiting-for-relay", String::new()),
            Health::Removed => ("removed", String::new()),
            Health::StorageFull => ("storage-full", String::new()),
            Health::Conflict => ("conflict", String::new()),
            Health::Unsupported(why) => ("unsupported", why.clone()),
        };
        let kit = v.kit.as_ref().map_or(String::new(), |k| {
            serde_json::json!({
                "endpoint": k.endpoint,
                "collection": k.collection,
                "words": k.words,
            })
            .to_string()
        });
        let devices: Vec<serde_json::Value> = v
            .devices
            .iter()
            .map(|d| {
                serde_json::json!({
                    "id": d.id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    "short": d.short,
                    "thisDevice": d.this_device,
                    "admin": d.admin,
                })
            })
            .collect();
        let arrived = health == "up-to-date" && String::from(&self.sync_health) != "up-to-date";
        self.as_mut().set_sync_phase(QString::from(phase));
        self.as_mut().set_sync_health(QString::from(health));
        self.as_mut().set_sync_detail(QString::from(detail.as_str()));
        self.as_mut().set_sync_unsent(v.unsent as i32);
        self.as_mut()
            .set_sync_endpoint(QString::from(v.endpoint.as_deref().unwrap_or("")));
        self.as_mut()
            .set_sync_invitation(QString::from(v.invitation.as_deref().unwrap_or("")));
        self.as_mut().set_sync_kit_json(QString::from(kit.as_str()));
        self.as_mut().set_sync_provision_command(QString::from(
            v.provisioning.as_ref().map_or("", |p| p.command.as_str()),
        ));
        self.as_mut().set_sync_devices_json(QString::from(
            serde_json::Value::from(devices).to_string().as_str(),
        ));
        self.as_mut().set_sync_admin(v.admin);
        self.as_mut()
            .set_sync_error(QString::from(v.error.as_deref().unwrap_or("")));
        // Reload when a round finished: what other devices changed is here.
        if arrived {
            if let Err(err) = self.as_mut().refresh_sidebar_and_header() {
                self.as_mut().report(err.to_string());
            }
            let next = self.revision.wrapping_add(1);
            self.set_revision(next);
        }
    }

    pub fn sync_create(self: Pin<&mut Self>, endpoint: &QString) {
        if let Some(sync) = SYNC.get() {
            let _ = sync.create(&String::from(endpoint));
        }
        self.sync_changed();
    }

    pub fn sync_join(self: Pin<&mut Self>, endpoint: &QString, invitation: &QString) {
        if let Some(sync) = SYNC.get() {
            let _ = sync.join(&String::from(endpoint), &String::from(invitation));
        }
        self.sync_changed();
    }

    pub fn sync_recover(
        self: Pin<&mut Self>,
        endpoint: &QString,
        collection: &QString,
        words: &QString,
    ) {
        if let Some(sync) = SYNC.get() {
            let _ = sync.recover(
                &String::from(endpoint),
                &String::from(collection),
                &String::from(words),
            );
        }
        self.sync_changed();
    }

    pub fn sync_invite(self: Pin<&mut Self>) {
        if let Some(sync) = SYNC.get() {
            let _ = sync.invite();
        }
        self.sync_changed();
    }

    pub fn sync_cancel_invitation(self: Pin<&mut Self>) {
        if let Some(sync) = SYNC.get() {
            sync.cancel_invitation();
        }
        self.sync_changed();
    }

    pub fn sync_kit_kept(self: Pin<&mut Self>) {
        if let Some(sync) = SYNC.get() {
            sync.kit_kept();
        }
        self.sync_changed();
    }

    pub fn sync_remove_device(self: Pin<&mut Self>, id: &QString) {
        let hex = String::from(id);
        let bytes: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
            .collect();
        if let (Some(sync), Some(Ok(id))) = (
            SYNC.get(),
            bytes.map(<[u8; 48]>::try_from),
        ) {
            let _ = sync.remove_device(&id);
        }
        self.sync_changed();
    }

    pub fn sync_leave(mut self: Pin<&mut Self>, discard_unsent: bool) -> i32 {
        let Some(sync) = SYNC.get() else {
            return -1;
        };
        let result = match sync.leave(discard_unsent) {
            Ok(()) => 0,
            Err(matters_sync_app::LeaveRefused::Unsent(n)) => n as i32,
            Err(matters_sync_app::LeaveRefused::Failed(_)) => -1,
        };
        self.as_mut().sync_changed();
        result
    }

    pub fn sync_now(self: Pin<&mut Self>) {
        if let Some(sync) = SYNC.get() {
            sync.sync_now();
        }
    }

    /// Show the desktop's accent and contrast as they are now (#76).
    fn appearance_changed(mut self: Pin<&mut Self>) {
        let now = crate::appearance::current();
        let accent = now.accent.map(|rgb| rgb.hex()).unwrap_or_default();
        self.as_mut()
            .set_system_accent(QString::from(accent.as_str()));
        self.set_high_contrast(now.high_contrast);
    }

    /// Show what the calendars hold now, and reload the lists.
    fn calendars_changed(mut self: Pin<&mut Self>) {
        let read = crate::calendar::snapshot();
        let calendars: Vec<CalendarEntry> = read
            .calendars
            .iter()
            .map(|c| CalendarEntry {
                id: c.id.clone(),
                name: c.name.clone(),
                color: c.color.clone().unwrap_or_default(),
            })
            .collect();
        let listed = to_json(&calendars);
        // Unchanged, it is left alone: Preferences lists it.
        if listed != self.calendars_json {
            self.as_mut().set_calendars_json(listed);
        }
        self.as_mut()
            .set_calendar_problem(QString::from(read.problem.as_deref().unwrap_or_default()));
        self.changed();
    }

    pub fn log_completed(mut self: Pin<&mut Self>) {
        let saved = {
            let mut state = settings();
            state
                .settings
                .log_completed(matters_core::clock::now_millis());
            state.save()
        };
        if let Err(err) = saved {
            self.as_mut().report(not_saved(&err));
        }
        self.changed();
    }

    pub fn select_view(self: Pin<&mut Self>, view: &QString) {
        match View::from_key(&String::from(view), None) {
            Some(view) => self.set_view(view),
            None => self.report(format!("unknown view '{view}'")),
        }
    }

    pub fn select_area(self: Pin<&mut Self>, area_id: &QString) {
        match View::from_key("area", Some(&String::from(area_id))) {
            Some(view) => self.set_view(view),
            None => self.report("no area selected".to_string()),
        }
    }

    pub fn create_area(self: Pin<&mut Self>, title: &QString) -> QString {
        let Some(title) = Task::clean_title(&String::from(title)) else {
            return QString::default();
        };
        let mut area = Area::new(Uuid::new_v4().to_string(), title, 0.0);
        match self.db.add_area(&mut area) {
            Ok(()) => {
                self.set_view(View::Area(area.id.clone()));
                QString::from(area.id.as_str())
            }
            Err(err) => {
                self.finish(Err(err));
                QString::default()
            }
        }
    }

    pub fn rename_area(self: Pin<&mut Self>, area_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_area(&String::from(area_id), &title);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn trash_area(self: Pin<&mut Self>, area_id: &QString) {
        let id = String::from(area_id);
        let result = self.db.trash_area(&id);
        // The open area, or a project in it, is gone from the sidebar.
        let open_project_in_area = match self.view.project_id() {
            Some(project) => self
                .db
                .get_project(project)
                .ok()
                .flatten()
                .is_some_and(|p| p.area_id.as_deref() == Some(id.as_str())),
            None => false,
        };
        if result.is_ok() && (self.view.area_id() == Some(id.as_str()) || open_project_in_area) {
            return self.set_view(View::Inbox);
        }
        self.finish(result);
    }

    pub fn restore_area(self: Pin<&mut Self>, area_id: &QString) {
        let result = self.db.restore_area(&String::from(area_id));
        self.finish(result);
    }

    pub fn rename_project(self: Pin<&mut Self>, project_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_project(&String::from(project_id), &title);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_project_area(self: Pin<&mut Self>, project_id: &QString, area_id: &QString) {
        let area = String::from(area_id);
        let result = self.db.set_project_area(
            &String::from(project_id),
            (!area.is_empty()).then_some(area.as_str()),
        );
        self.finish(result);
    }

    pub fn select_project(self: Pin<&mut Self>, project_id: &QString) {
        match View::from_key("project", Some(&String::from(project_id))) {
            Some(view) => self.set_view(view),
            None => self.report("no project selected".to_string()),
        }
    }

    /// Capture a task in the current view (placement rules: `View::new_task`).
    pub fn add_task_below(self: Pin<&mut Self>, title: &QString, below_id: &QString) -> QString {
        let title = String::from(title);
        let Some(mut task) = self
            .view
            .new_task(Uuid::new_v4().to_string(), &title, local_today())
        else {
            return QString::default();
        };
        let result = matters_core::ordering::add_task_below(
            &self.db,
            &self.view,
            self.day.day(),
            &mut task,
            &String::from(below_id),
        );
        let id = if result.is_ok() {
            QString::from(task.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn add_task(self: Pin<&mut Self>, title: &QString) -> QString {
        let title = String::from(title);
        let Some(mut task) = self
            .view
            .new_task(Uuid::new_v4().to_string(), &title, local_today())
        else {
            // A blank title, or the Logbook or the Trash.
            return QString::default();
        };
        let result = self.db.add_task(&mut task);
        let id = if result.is_ok() {
            QString::from(task.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn move_task(self: Pin<&mut Self>, task_id: &QString, step: i32) {
        if !self.view.is_reorderable() {
            return;
        }
        let result = matters_core::ordering::move_task(
            &self.db,
            &self.view,
            self.day.day(),
            &String::from(task_id),
            step as isize,
        );
        self.finish(result);
    }

    pub fn place_task(self: Pin<&mut Self>, task_id: &QString, after_id: &QString) {
        if !self.view.is_reorderable() {
            return;
        }
        let after = String::from(after_id);
        let result = matters_core::ordering::place_task(
            &self.db,
            &self.view,
            self.day.day(),
            &String::from(task_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn place_today_task(
        self: Pin<&mut Self>,
        task_id: &QString,
        evening: bool,
        after_id: &QString,
    ) {
        if self.view != View::Today {
            return;
        }
        let after = String::from(after_id);
        let result = matters_core::ordering::place_in_today(
            &self.db,
            self.day.day(),
            &String::from(task_id),
            evening,
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn place_project(self: Pin<&mut Self>, project_id: &QString, after_id: &QString) {
        let after = String::from(after_id);
        let result = matters_core::ordering::place_project(
            &self.db,
            &String::from(project_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn move_destinations(&self, task_id: &QString) -> QString {
        let current = self
            .db
            .get_task(&String::from(task_id))
            .ok()
            .flatten()
            .and_then(|task| Home::of(&task).ok());
        match move_destinations(&self.db, current.as_ref()) {
            Ok(destinations) => to_json(&destinations),
            Err(err) => {
                eprintln!("matters: {err}");
                QString::from("[]")
            }
        }
    }

    pub fn move_task_to(
        self: Pin<&mut Self>,
        task_id: &QString,
        kind: &QString,
        id: &QString,
    ) -> bool {
        let id = String::from(id);
        let home = match String::from(kind).as_str() {
            "inbox" => Ok(Home::Inbox),
            "area" => Ok(Home::Area(id)),
            "project" => Ok(Home::Project {
                project: id,
                heading: None,
            }),
            "heading" => match self.db.get_heading(&id) {
                Ok(Some(heading)) => Ok(Home::Project {
                    project: heading.project_id,
                    heading: Some(heading.id),
                }),
                Ok(None) => Err(matters_core::MattersError::NotFound {
                    entity: "heading",
                    id,
                }),
                Err(err) => Err(err),
            },
            other => Err(matters_core::MattersError::Validation(format!(
                "'{other}' is not a destination"
            ))),
        };
        let result = home.and_then(|home| self.db.set_task_home(&String::from(task_id), &home));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn finish_project(
        self: Pin<&mut Self>,
        project_id: &QString,
        outcome: &QString,
        open_tasks: &QString,
    ) -> bool {
        let parse = |s: &QString| match String::from(s).as_str() {
            "completed" => Ok(Outcome::Completed),
            "canceled" => Ok(Outcome::Canceled),
            other => Err(matters_core::MattersError::Validation(format!(
                "'{other}' is not how a project finishes"
            ))),
        };
        let result = parse(outcome).and_then(|outcome| {
            let open_tasks = parse(open_tasks)?;
            self.db
                .finish_project(&String::from(project_id), outcome, open_tasks)
        });
        let ok = result.is_ok();
        self.finish_project_change(result);
        ok
    }

    pub fn reopen_project(self: Pin<&mut Self>, project_id: &QString) {
        let result = self.db.reopen_project(&String::from(project_id));
        self.finish_project_change(result);
    }

    /// Finishing or reopening a project changes its tasks all at once: the
    /// list starts a new visit, so they are shown as they now are rather
    /// than kept in place like a task ticked by hand (`Visit`).
    fn finish_project_change(mut self: Pin<&mut Self>, result: matters_core::Result<()>) {
        if result.is_ok() {
            let visit = self.visit.wrapping_add(1);
            self.as_mut().set_visit(visit);
        }
        self.finish(result);
    }

    pub fn place_project_in_area(
        self: Pin<&mut Self>,
        project_id: &QString,
        area_id: &QString,
        after_id: &QString,
    ) {
        let (area, after) = (String::from(area_id), String::from(after_id));
        let result = matters_core::ordering::place_project_in_area(
            &self.db,
            &String::from(project_id),
            (!area.is_empty()).then_some(area.as_str()),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn place_area(self: Pin<&mut Self>, area_id: &QString, after_id: &QString) {
        let after = String::from(after_id);
        let result = matters_core::ordering::place_area(
            &self.db,
            &String::from(area_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn toggle_task(self: Pin<&mut Self>, task_id: &QString) {
        let id = String::from(task_id);
        let db = &self.db;
        let result = db.get_task(&id).and_then(|task| match task {
            Some(task) if task.status == Status::Open => db.complete_task(&id),
            Some(_) => db.uncomplete_task(&id),
            None => Ok(()),
        });
        self.finish(result);
    }

    pub fn trash_task(self: Pin<&mut Self>, task_id: &QString) {
        let result = self.db.trash_task(&String::from(task_id));
        self.finish(result);
    }

    pub fn cancel_task(self: Pin<&mut Self>, task_id: &QString) {
        let result = self.db.cancel_task(&String::from(task_id));
        self.finish(result);
    }

    pub fn duplicate_task(self: Pin<&mut Self>, task_id: &QString) -> QString {
        match self.db.duplicate_task(&String::from(task_id)) {
            Ok(copy) => {
                self.finish(Ok(()));
                QString::from(copy.as_str())
            }
            Err(err) => {
                self.finish(Err(err));
                QString::default()
            }
        }
    }

    pub fn convert_task_to_project(self: Pin<&mut Self>, task_id: &QString) -> QString {
        match self.db.convert_task_to_project(&String::from(task_id)) {
            Ok(project) => {
                self.set_view(View::Project(project.clone()));
                QString::from(project.as_str())
            }
            Err(err) => {
                self.finish(Err(err));
                QString::default()
            }
        }
    }

    pub fn restore_task(self: Pin<&mut Self>, task_id: &QString) {
        let result = self.db.restore_task(&String::from(task_id));
        self.finish(result);
    }

    pub fn empty_trash(self: Pin<&mut Self>) {
        let result = self.db.empty_trash().map(drop);
        self.finish(result);
    }

    pub fn trash_project(self: Pin<&mut Self>, project_id: &QString) {
        let id = String::from(project_id);
        let result = self.db.trash_project(&id);
        if result.is_ok() && self.view.project_id() == Some(id.as_str()) {
            return self.set_view(View::Inbox);
        }
        self.finish(result);
    }

    pub fn restore_project(self: Pin<&mut Self>, project_id: &QString) {
        let result = self.db.restore_project(&String::from(project_id));
        self.finish(result);
    }

    /// `when` is a `Schedule::parse` string: today, evening, someday, none or
    /// YYYY-MM-DD.
    pub fn schedule_task(self: Pin<&mut Self>, task_id: &QString, when: &QString) {
        let Some(schedule) = Schedule::parse(&String::from(when)) else {
            return self.report(format!("'{when}' is not a schedule"));
        };
        let result = gone_task_is_fine(self.db.set_task_schedule(
            &String::from(task_id),
            &schedule,
            local_today(),
        ));
        self.finish(result);
    }

    fn typed_when(&self, text: &QString, month_first: bool) -> Option<Schedule> {
        let order = if month_first {
            DateOrder::MonthFirst
        } else {
            DateOrder::DayFirst
        };
        Schedule::parse_typed(&String::from(text), self.day.day(), order)
    }

    pub fn preview_when(&self, text: &QString, month_first: bool) -> QString {
        QString::from(match self.typed_when(text, month_first) {
            None => String::new(),
            Some(Schedule::Today { evening: false }) => "today".into(),
            Some(Schedule::Today { evening: true }) => "evening".into(),
            Some(Schedule::Someday) => "someday".into(),
            Some(Schedule::None) => "none".into(),
            Some(Schedule::Date(date)) => date.to_string(),
        })
    }

    pub fn schedule_task_typed(
        self: Pin<&mut Self>,
        task_id: &QString,
        text: &QString,
        month_first: bool,
    ) -> bool {
        let Some(schedule) = self.typed_when(text, month_first) else {
            return false;
        };
        let today = self.day.day();
        let result = gone_task_is_fine(self.db.set_task_schedule(
            &String::from(task_id),
            &schedule,
            today,
        ));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    fn date_order(month_first: bool) -> DateOrder {
        if month_first {
            DateOrder::MonthFirst
        } else {
            DateOrder::DayFirst
        }
    }

    pub fn preview_deadline(&self, text: &QString, month_first: bool) -> QString {
        let date = Schedule::parse_typed_deadline(
            &String::from(text),
            self.day.day(),
            Self::date_order(month_first),
        );
        QString::from(date.map(|d| d.to_string()).unwrap_or_default().as_str())
    }

    pub fn preview_reminder(&self, text: &QString) -> QString {
        let time = matters_core::reminders::parse_typed_time(&String::from(text));
        QString::from(
            time.map(matters_core::reminders::stored_time)
                .unwrap_or_default()
                .as_str(),
        )
    }

    pub fn set_task_reminder_typed(
        self: Pin<&mut Self>,
        task_id: &QString,
        text: &QString,
    ) -> bool {
        let Some(time) = matters_core::reminders::parse_typed_time(&String::from(text)) else {
            return false;
        };
        let result = gone_task_is_fine(self.db.remind_task_at(
            &String::from(task_id),
            time,
            local_today(),
        ));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_task_repeat(self: Pin<&mut Self>, task_id: &QString, rule: &QString) -> bool {
        let rule = String::from(rule);
        let rule = if rule.trim().is_empty() {
            None
        } else {
            match serde_json::from_str::<RepeatRule>(&rule) {
                Ok(rule) if rule.check().is_ok() => Some(rule),
                _ => return false,
            }
        };
        let result = gone_task_is_fine(self.db.set_task_repeat(
            &String::from(task_id),
            rule,
            local_today(),
        ));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn format_date(&self, iso: &QString, style: &QString) -> QString {
        use crate::locale_format::{self, Parts};
        let Ok(day) = String::from(iso).parse::<jiff::civil::Date>() else {
            return QString::default();
        };
        let text = match String::from(style).as_str() {
            "day-month" => locale_format::date("", day, Parts::DAY_MONTH),
            "day-month-year" => locale_format::date("", day, Parts::DAY_MONTH_YEAR),
            "weekday-day-month" => locale_format::date("", day, Parts::WEEKDAY_DAY_MONTH),
            "weekday-day-month-year" => locale_format::date("", day, Parts::WEEKDAY_DAY_MONTH_YEAR),
            "month-year" => locale_format::date("", day, Parts::MONTH_YEAR),
            "weekday" => locale_format::day_name("", day.weekday(), false),
            _ => String::new(),
        };
        QString::from(text.as_str())
    }

    pub fn describe_repeat(&self, rule: &QString) -> QString {
        let text = serde_json::from_str::<RepeatRule>(&String::from(rule))
            .ok()
            .filter(|r| r.check().is_ok())
            .map(|r| crate::repeat_words::describe(&r, ""))
            .unwrap_or_default();
        QString::from(text.as_str())
    }

    /// Create the repeating tasks' occurrences due today (#61); the lists
    /// reload if any were.
    fn advance_repeats(self: Pin<&mut Self>) {
        match self.db.advance_repeats(local_today()) {
            Ok(created) if created.is_empty() => {}
            Ok(_) => self.changed(),
            Err(err) => self.report(err.to_string()),
        }
    }

    pub fn clear_task_reminder(self: Pin<&mut Self>, task_id: &QString) {
        let result = gone_task_is_fine(self.db.set_task_reminder(&String::from(task_id), None));
        self.finish(result);
    }

    pub fn check_reminders(mut self: Pin<&mut Self>) {
        let Some(notifier) = crate::reminders::notifier() else {
            return;
        };
        let thread = self.qt_thread();
        let on_action: crate::notify::OnAction = std::sync::Arc::new(move |action| {
            // Gone only when the app is quitting.
            let _ = thread.queue(move |controller| controller.reminder_action(action));
        });
        let delivered = crate::reminders::deliver(
            &self.db,
            notifier,
            &on_action,
            jiff::Timestamp::now(),
            &jiff::tz::TimeZone::system(),
        );
        match delivered {
            Ok(_) => self.as_mut().rust_mut().reminder_problem = None,
            Err(err) => {
                // Said once, not on every check; they are tried again.
                if self.reminder_problem.as_deref() != Some(err.as_str()) {
                    self.as_mut().rust_mut().reminder_problem = Some(err.clone());
                    self.report(crate::strings::REMINDERS_NOT_SHOWN.fill(&[&err]));
                }
            }
        }
    }

    /// What the user chose on a reminder.
    fn reminder_action(mut self: Pin<&mut Self>, action: crate::notify::Action) {
        match action {
            crate::notify::Action::Open(id) => {
                self.as_mut().reminder_opened(QString::from(id.as_str()))
            }
            crate::notify::Action::Complete(id) => {
                let result = gone_task_is_fine(self.db.complete_task(&id));
                self.finish(result);
            }
        }
    }

    pub fn set_task_deadline_typed(
        self: Pin<&mut Self>,
        task_id: &QString,
        text: &QString,
        month_first: bool,
    ) -> bool {
        let order = Self::date_order(month_first);
        let Some(date) = Schedule::parse_typed_deadline(&String::from(text), self.day.day(), order)
        else {
            return false;
        };
        let result = gone_task_is_fine(
            self.db
                .set_task_deadline(&String::from(task_id), Some(date)),
        );
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_task_deadline(self: Pin<&mut Self>, task_id: &QString, date: &QString) -> bool {
        let Some(deadline) = deadline_from_iso(&String::from(date)) else {
            return false;
        };
        let deadline = match deadline {
            Schedule::Date(d) => Some(d),
            _ => None,
        };
        let result = gone_task_is_fine(self.db.set_task_deadline(&String::from(task_id), deadline));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn schedule_project(self: Pin<&mut Self>, project_id: &QString, when: &QString) {
        let Some(schedule) = Schedule::parse(&String::from(when)) else {
            return self.report(format!("'{when}' is not a schedule"));
        };
        let today = self.day.day();
        let result = self
            .db
            .set_project_schedule(&String::from(project_id), &schedule, today);
        self.finish(result);
    }

    pub fn schedule_project_typed(
        self: Pin<&mut Self>,
        project_id: &QString,
        text: &QString,
        month_first: bool,
    ) -> bool {
        let Some(schedule) = self.typed_when(text, month_first) else {
            return false;
        };
        let today = self.day.day();
        let result = self
            .db
            .set_project_schedule(&String::from(project_id), &schedule, today);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_project_deadline(
        self: Pin<&mut Self>,
        project_id: &QString,
        date: &QString,
    ) -> bool {
        let Some(deadline) = deadline_from_iso(&String::from(date)) else {
            return false;
        };
        let deadline = match deadline {
            Schedule::Date(d) => Some(d),
            _ => None,
        };
        let result = self
            .db
            .set_project_deadline(&String::from(project_id), deadline);
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_project_deadline_typed(
        self: Pin<&mut Self>,
        project_id: &QString,
        text: &QString,
        month_first: bool,
    ) -> bool {
        let order = Self::date_order(month_first);
        let Some(date) = Schedule::parse_typed_deadline(&String::from(text), self.day.day(), order)
        else {
            return false;
        };
        let result = self
            .db
            .set_project_deadline(&String::from(project_id), Some(date));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn update_project_notes(self: Pin<&mut Self>, project_id: &QString, notes: &QString) {
        let notes = String::from(notes);
        let result = self.db.set_project_notes(&String::from(project_id), &notes);
        self.finish(result);
    }

    pub fn create_tag(self: Pin<&mut Self>, name: &QString, parent_id: &QString) -> QString {
        let parent = String::from(parent_id);
        let mut tag = Tag::new(
            Uuid::new_v4().to_string(),
            String::from(name),
            (!parent.is_empty()).then_some(parent),
        );
        let result = self.db.add_tag(&mut tag);
        let id = if result.is_ok() {
            QString::from(tag.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn rename_tag(self: Pin<&mut Self>, tag_id: &QString, name: &QString) -> bool {
        let result = self
            .db
            .rename_tag(&String::from(tag_id), &String::from(name));
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn set_tag_parent(self: Pin<&mut Self>, tag_id: &QString, parent_id: &QString) -> bool {
        let parent = String::from(parent_id);
        let result = self.db.set_tag_parent(
            &String::from(tag_id),
            (!parent.is_empty()).then_some(parent.as_str()),
        );
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn delete_tag(self: Pin<&mut Self>, tag_id: &QString) {
        let result = self.db.delete_tag(&String::from(tag_id));
        self.finish(result);
    }

    pub fn add_tag(self: Pin<&mut Self>, kind: &QString, item_id: &QString, tag_id: &QString) {
        let id = String::from(item_id);
        let result = tagged(&String::from(kind), &id)
            .and_then(|item| self.db.add_tag_to(item, &String::from(tag_id)));
        self.finish(result);
    }

    pub fn remove_tag(self: Pin<&mut Self>, kind: &QString, item_id: &QString, tag_id: &QString) {
        let id = String::from(item_id);
        let result = tagged(&String::from(kind), &id)
            .and_then(|item| self.db.remove_tag_from(item, &String::from(tag_id)));
        self.finish(result);
    }

    pub fn tag_by_name(
        self: Pin<&mut Self>,
        kind: &QString,
        item_id: &QString,
        name: &QString,
    ) -> bool {
        let name = String::from(name);
        if Task::clean_title(&name).is_none() {
            return false;
        }
        let id = String::from(item_id);
        let db = &self.db;
        let result = tagged(&String::from(kind), &id).and_then(|item| {
            let tag = match db.tag_named(&name)? {
                Some(tag) => tag,
                None => {
                    let mut tag = Tag::new(Uuid::new_v4().to_string(), name.as_str(), None);
                    db.add_tag(&mut tag)?;
                    tag
                }
            };
            db.add_tag_to(item, &tag.id)
        });
        let ok = result.is_ok();
        self.finish(result);
        ok
    }

    pub fn rename_task(self: Pin<&mut Self>, task_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if matters_core::Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_task(&String::from(task_id), &title);
        let renamed = result.is_ok();
        self.finish(result);
        renamed
    }

    pub fn update_task_notes(self: Pin<&mut Self>, task_id: &QString, notes: &QString) {
        let notes = String::from(notes);
        let result = gone_task_is_fine(self.db.set_task_notes(&String::from(task_id), &notes));
        self.finish(result);
    }

    pub fn create_project(self: Pin<&mut Self>, title: &QString) {
        let title = String::from(title).trim().to_string();
        if title.is_empty() {
            return;
        }
        let mut project = Project::new(Uuid::new_v4().to_string(), title, 0.0);
        let result = self.db.add_project(&mut project);
        self.finish(result);
    }

    pub fn reveal_task(self: Pin<&mut Self>, task_id: &QString) {
        match self.db.get_task(&String::from(task_id)) {
            Ok(Some(task)) => self.set_view(View::containing(&task, local_today())),
            Ok(None) => self.report(crate::strings::TASK_GONE.text()),
            Err(err) => self.report(err.to_string()),
        }
    }

    pub fn search(mut self: Pin<&mut Self>, query: &QString) {
        self.as_mut().rust_mut().search_query = String::from(query).trim().to_string();
        self.run_search();
    }

    pub fn clear_error(self: Pin<&mut Self>) {
        self.set_error_message(QString::default());
    }

    pub fn check_date(mut self: Pin<&mut Self>) {
        if self.day.day() != local_today() {
            self.as_mut().changed();
        }
        self.as_mut().advance_repeats();
        self.refresh_calendars();
    }

    // ---- internals ----------------------------------------------------------

    fn set_view(mut self: Pin<&mut Self>, view: View) {
        self.as_mut().set_current_view(QString::from(view.key()));
        self.as_mut()
            .set_current_project_id(QString::from(view.project_id().unwrap_or_default()));
        self.as_mut()
            .set_current_area_id(QString::from(view.area_id().unwrap_or_default()));
        self.as_mut().set_reorderable(view.is_reorderable());
        self.as_mut()
            .set_accepts_new_tasks(view.accepts_new_tasks());
        self.as_mut().rust_mut().view = view;
        let visit = self.visit.wrapping_add(1);
        self.as_mut().set_visit(visit);
        self.changed();
    }

    /// Add a checklist item below `after_id` ("" for the end). Returns the
    /// new item's ID, or "" when nothing was added.
    pub fn add_checklist_item(
        self: Pin<&mut Self>,
        task_id: &QString,
        title: &QString,
        after_id: &QString,
    ) -> QString {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return QString::default();
        }
        let mut item = ChecklistItem::new(
            Uuid::new_v4().to_string(),
            String::from(task_id),
            title,
            0.0,
        );
        let after = String::from(after_id);
        let result = if after.is_empty() {
            self.db.add_checklist_item(&mut item)
        } else {
            matters_core::ordering::add_checklist_item_below(&self.db, &mut item, &after)
        };
        let id = if result.is_ok() {
            QString::from(item.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn rename_checklist_item(self: Pin<&mut Self>, item_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self
            .db
            .rename_checklist_item(&String::from(item_id), &title);
        let renamed = result.is_ok();
        self.finish(result);
        renamed
    }

    pub fn set_checklist_item_done(self: Pin<&mut Self>, item_id: &QString, done: bool) {
        let result = self
            .db
            .set_checklist_item_done(&String::from(item_id), done);
        self.finish(result);
    }

    pub fn remember_tasks(mut self: Pin<&mut Self>, ids_json: &QString) -> i32 {
        let ids: Vec<String> = match serde_json::from_str(&String::from(ids_json)) {
            Ok(ids) => ids,
            Err(err) => {
                self.report(format!("not a list of task ids: {err}"));
                return 0;
            }
        };
        match TaskSnapshot::take(&self.db, &ids) {
            Ok(snapshot) => {
                let count = i32::try_from(snapshot.len()).unwrap_or(i32::MAX);
                self.as_mut().rust_mut().remembered = Some(snapshot);
                count
            }
            Err(err) => {
                self.report(err.to_string());
                0
            }
        }
    }

    pub fn restore_remembered_tasks(mut self: Pin<&mut Self>) {
        let Some(snapshot) = self.as_mut().rust_mut().remembered.take() else {
            return;
        };
        let result = snapshot.restore(&self.db, local_today());
        self.finish(result);
    }

    pub fn restore_checklist_item(mut self: Pin<&mut Self>) {
        let Some(item) = self.as_mut().rust_mut().deleted_checklist_item.take() else {
            return;
        };
        let result = self.db.create_checklist_item(&item);
        self.finish(result);
    }

    pub fn move_heading_to_project(
        self: Pin<&mut Self>,
        heading_id: &QString,
        project_id: &QString,
    ) {
        let result = matters_core::ordering::move_heading_to_project(
            &self.db,
            &String::from(heading_id),
            &String::from(project_id),
        );
        self.finish(result);
    }

    pub fn delete_checklist_item(mut self: Pin<&mut Self>, item_id: &QString) {
        let id = String::from(item_id);
        if let Ok(Some(item)) = self.db.get_checklist_item(&id) {
            self.as_mut().rust_mut().deleted_checklist_item = Some(item);
        }
        let result = self.db.delete_checklist_item(&id);
        self.finish(result);
    }

    pub fn move_checklist_item(self: Pin<&mut Self>, item_id: &QString, step: i32) {
        let result = matters_core::ordering::move_checklist_item(
            &self.db,
            &String::from(item_id),
            step as isize,
        );
        self.finish(result);
    }

    pub fn place_checklist_item(self: Pin<&mut Self>, item_id: &QString, after_id: &QString) {
        let after = String::from(after_id);
        let result = matters_core::ordering::place_checklist_item(
            &self.db,
            &String::from(item_id),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    pub fn add_heading(self: Pin<&mut Self>, title: &QString) -> QString {
        let title = String::from(title);
        let Some(project_id) = self.view.project_id().map(str::to_string) else {
            return QString::default();
        };
        if Task::clean_title(&title).is_none() {
            return QString::default();
        }
        let mut heading = Heading::new(Uuid::new_v4().to_string(), project_id, title, 0.0);
        let result = self.db.add_heading(&mut heading);
        let id = if result.is_ok() {
            QString::from(heading.id.as_str())
        } else {
            QString::default()
        };
        self.finish(result);
        id
    }

    pub fn rename_heading(self: Pin<&mut Self>, heading_id: &QString, title: &QString) -> bool {
        let title = String::from(title);
        if Task::clean_title(&title).is_none() {
            return false;
        }
        let result = self.db.rename_heading(&String::from(heading_id), &title);
        let renamed = result.is_ok();
        self.finish(result);
        renamed
    }

    pub fn delete_heading(mut self: Pin<&mut Self>, heading_id: &QString) {
        let result = self
            .db
            .delete_heading(&String::from(heading_id))
            .map(|deleted| {
                self.as_mut().rust_mut().deleted_heading = Some(deleted);
            });
        self.finish(result);
    }

    pub fn restore_heading(mut self: Pin<&mut Self>) {
        let Some(deleted) = self.as_mut().rust_mut().deleted_heading.take() else {
            return;
        };
        let result = self.db.restore_heading(&deleted);
        self.finish(result);
    }

    pub fn move_heading(self: Pin<&mut Self>, heading_id: &QString, step: i32) {
        let result = matters_core::ordering::move_heading(
            &self.db,
            &String::from(heading_id),
            step as isize,
        );
        self.finish(result);
    }

    pub fn place_project_task(
        self: Pin<&mut Self>,
        task_id: &QString,
        heading_id: &QString,
        after_id: &QString,
    ) {
        let Some(project_id) = self.view.project_id().map(str::to_string) else {
            return;
        };
        let heading = String::from(heading_id);
        let after = String::from(after_id);
        let result = matters_core::ordering::place_in_heading(
            &self.db,
            &project_id,
            self.day.day(),
            &String::from(task_id),
            (!heading.is_empty()).then_some(heading.as_str()),
            (!after.is_empty()).then_some(after.as_str()),
        );
        self.finish(result);
    }

    /// Report a command's outcome and publish the new state.
    fn finish(mut self: Pin<&mut Self>, result: matters_core::Result<()>) {
        if let Err(err) = result {
            self.as_mut()
                .report(crate::strings::NOT_SAVED.fill(&[&err.to_string()]));
        }
        self.changed();
    }

    fn report(self: Pin<&mut Self>, message: String) {
        eprintln!("matters: {message}");
        self.set_error_message(QString::from(message.as_str()));
    }

    /// Re-read what the sidebar and header show, then bump `revision` so the
    /// task list reloads. Only aggregate counts are queried for the sidebar;
    /// the rows of the current view are loaded by `TaskListModel`.
    fn changed(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().day.advance(local_today());
        if let Err(err) = self.as_mut().refresh_sidebar_and_header() {
            self.as_mut().report(err.to_string());
        }
        if !self.search_query.is_empty() {
            self.as_mut().run_search();
        }
        let next = self.revision.wrapping_add(1);
        self.set_revision(next);
        // A local change: send it soon (#42).
        if let Some(sync) = SYNC.get() {
            sync.sync_now();
        }
    }

    fn refresh_sidebar_and_header(mut self: Pin<&mut Self>) -> matters_core::Result<()> {
        let (
            counts,
            projects,
            trashed_projects,
            areas,
            trashed_areas,
            title,
            progress,
            status,
            open_tasks,
            details,
        ) = {
            let engine = QueryEngine::new(&self.db).logged_until(logging().0);
            let counts = engine.counts(self.day.day())?;
            let projects = self.db.list_projects()?;
            let trashed_projects = self.db.list_trashed_projects()?;
            let areas = self.db.list_areas()?;
            let trashed_areas = self.db.list_trashed_areas()?;
            let mut status = "";
            let mut open_tasks = 0;
            let mut details = None;
            let (title, progress) = match &self.view {
                View::Project(id) => {
                    let project = self.db.get_project(id)?;
                    let progress = engine.project_progress(id)?;
                    status = project.as_ref().map_or("", |p| p.status.as_str());
                    open_tasks = progress.total_tasks - progress.completed_tasks;
                    details = project.clone();
                    (
                        project.map_or_else(|| "Project".into(), |p| p.title),
                        progress.ratio(),
                    )
                }
                View::Area(id) => (
                    self.db
                        .get_area(id)?
                        .map(|a| a.title)
                        .unwrap_or_else(|| "Area".into()),
                    0.0,
                ),
                other => (other.title().to_string(), 0.0),
            };
            (
                counts,
                projects,
                trashed_projects,
                areas,
                trashed_areas,
                title,
                progress,
                status,
                open_tasks,
                details,
            )
        };

        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        self.as_mut().set_inbox_count(count(counts.inbox));
        self.as_mut().set_today_count(count(counts.today));
        self.as_mut().set_upcoming_count(count(counts.upcoming));
        self.as_mut().set_anytime_count(count(counts.anytime));
        self.as_mut().set_someday_count(count(counts.someday));
        self.as_mut().set_logbook_count(count(counts.logbook));
        self.as_mut().set_projects_json(to_json(&projects));
        self.as_mut()
            .set_trashed_projects_json(to_json(&trashed_projects));
        self.as_mut().set_areas_json(to_json(&areas));
        self.as_mut()
            .set_trashed_areas_json(to_json(&trashed_areas));
        self.as_mut()
            .set_current_title(QString::from(title.as_str()));
        self.as_mut().set_current_project_progress(progress);
        let (all_tags, current_tags) = {
            let item = match &self.view {
                View::Project(id) => Some(Tagged::Project(id)),
                View::Area(id) => Some(Tagged::Area(id)),
                _ => None,
            };
            let current = match item {
                Some(item) => self.db.tags_of(item)?,
                None => Vec::new(),
            };
            (tag_entries(&self.db.list_tags()?), tag_chips(&current))
        };
        self.as_mut().set_tags_json(to_json(&all_tags));
        self.as_mut().set_current_tags_json(to_json(&current_tags));
        self.as_mut()
            .set_current_project_status(QString::from(status));
        self.as_mut()
            .set_current_project_open_tasks(count(open_tasks));
        let project = details.unwrap_or_default();
        let iso = |d: Option<String>| d.unwrap_or_default();
        self.as_mut()
            .set_current_project_notes(QString::from(project.notes.as_str()));
        self.as_mut().set_current_project_start_date(QString::from(
            iso(project.start_date.map(|d| d.to_string())).as_str(),
        ));
        self.as_mut()
            .set_current_project_evening(project.time_of_day.as_deref() == Some("evening"));
        self.as_mut()
            .set_current_project_someday(project.is_someday);
        self.as_mut().set_current_project_deadline(QString::from(
            iso(project.deadline.map(|d| d.to_string())).as_str(),
        ));
        let deadline_status = deadline_key(project.deadline_status(self.day.day()));
        self.as_mut()
            .set_current_project_deadline_status(QString::from(deadline_status));
        Ok(())
    }

    fn run_search(self: Pin<&mut Self>) {
        let result = if self.search_query.is_empty() {
            Ok(QString::from("[]"))
        } else {
            QueryEngine::new(&self.db)
                .quick_find(&self.search_query)
                .map(|found| to_json(&search_results(&found)))
        };
        match result {
            Ok(json) => self.set_search_results_json(json),
            Err(err) => self.report(err.to_string()),
        }
    }
}

/// One Quick Find result, as QML lists it: what it is (`area`, `project` or
/// `task`), its id and its title.
#[derive(serde::Serialize)]
struct SearchResult<'a> {
    kind: &'static str,
    id: &'a str,
    title: &'a str,
}

/// Quick Find's results in the order they are listed: areas, then projects,
/// then tasks (#26).
fn search_results(found: &matters_core::QuickFindResult) -> Vec<SearchResult<'_>> {
    let areas = found.areas.iter().map(|a| SearchResult {
        kind: "area",
        id: &a.id,
        title: &a.title,
    });
    let projects = found.projects.iter().map(|p| SearchResult {
        kind: "project",
        id: &p.id,
        title: &p.title,
    });
    let tasks = found.tasks.iter().map(|t| SearchResult {
        kind: "task",
        id: &t.id,
        title: &t.title,
    });
    areas.chain(projects).chain(tasks).collect()
}

/// A deadline as QML sets it: an ISO date, read the same way as a When
/// date (`Schedule::Date`), or an empty string to clear it
/// (`Schedule::None`). `None` when the text is neither.
fn deadline_from_iso(text: &str) -> Option<Schedule> {
    if text.is_empty() {
        return Some(Schedule::None);
    }
    match Schedule::parse(text) {
        Some(date @ Schedule::Date(_)) => Some(date),
        _ => None,
    }
}

/// What `kind` (`task`, `project` or `area`) and `id` name, for tagging.
fn tagged<'a>(kind: &str, id: &'a str) -> matters_core::Result<Tagged<'a>> {
    match kind {
        "task" => Ok(Tagged::Task(id)),
        "project" => Ok(Tagged::Project(id)),
        "area" => Ok(Tagged::Area(id)),
        other => Err(matters_core::MattersError::Validation(format!(
            "'{other}' can't be tagged"
        ))),
    }
}

/// A tag as the tag list and the Tags dialog show it.
#[derive(serde::Serialize)]
struct TagEntry {
    id: String,
    name: String,
    parent_id: Option<String>,
    /// How deeply it is nested: 0 at the top.
    depth: usize,
}

/// `tags` (parents before children, `Database::list_tags`) with depths.
fn tag_entries(tags: &[Tag]) -> Vec<TagEntry> {
    let mut depth: HashMap<&str, usize> = HashMap::new();
    tags.iter()
        .map(|t| {
            let d = t
                .parent_id
                .as_deref()
                .and_then(|p| depth.get(p))
                .map_or(0, |d| d + 1);
            depth.insert(&t.id, d);
            TagEntry {
                id: t.id.clone(),
                name: t.name.clone(),
                parent_id: t.parent_id.clone(),
                depth: d,
            }
        })
        .collect()
}

/// A tag as an item's chip: its id and name.
#[derive(serde::Serialize, PartialEq, Clone)]
struct TagChip {
    id: String,
    name: String,
}

fn tag_chips(tags: &[Tag]) -> Vec<TagChip> {
    tags.iter()
        .map(|t| TagChip {
            id: t.id.clone(),
            name: t.name.clone(),
        })
        .collect()
}

/// How QML names a `DeadlineStatus`: none, set, today or overdue.
fn deadline_key(status: DeadlineStatus) -> &'static str {
    match status {
        DeadlineStatus::None => "none",
        DeadlineStatus::Set => "set",
        DeadlineStatus::DueToday => "today",
        DeadlineStatus::Overdue => "overdue",
    }
}

/// One entry of the Move popover (`AppController::move_destinations`).
#[derive(serde::Serialize)]
struct MoveDestination {
    kind: &'static str,
    id: String,
    title: String,
    context: String,
    depth: u8,
    current: bool,
}

/// The Move popover's destinations, in sidebar order, with `current` (the
/// task's home) marked.
fn move_destinations(
    db: &Database,
    current: Option<&Home>,
) -> matters_core::Result<Vec<MoveDestination>> {
    let mut list = vec![MoveDestination {
        kind: "inbox",
        id: String::new(),
        title: "Inbox".into(),
        context: String::new(),
        depth: 0,
        current: current == Some(&Home::Inbox),
    }];
    let projects = db.list_projects()?;
    let add_project = |list: &mut Vec<MoveDestination>,
                       project: &Project,
                       context: &str,
                       depth: u8|
     -> matters_core::Result<()> {
        let here = |heading: Option<&str>| {
            current
                == Some(&Home::Project {
                    project: project.id.clone(),
                    heading: heading.map(str::to_string),
                })
        };
        list.push(MoveDestination {
            kind: "project",
            id: project.id.clone(),
            title: project.title.clone(),
            context: context.to_string(),
            depth,
            current: here(None),
        });
        for heading in db.list_headings_for_project(&project.id)? {
            list.push(MoveDestination {
                kind: "heading",
                current: here(Some(&heading.id)),
                id: heading.id,
                title: heading.title,
                context: project.title.clone(),
                depth: depth + 1,
            });
        }
        Ok(())
    };
    for project in projects.iter().filter(|p| p.area_id.is_none()) {
        add_project(&mut list, project, "", 0)?;
    }
    for area in db.list_areas()? {
        list.push(MoveDestination {
            kind: "area",
            current: current == Some(&Home::Area(area.id.clone())),
            id: area.id.clone(),
            title: area.title.clone(),
            context: String::new(),
            depth: 0,
        });
        for project in projects
            .iter()
            .filter(|p| p.area_id.as_deref() == Some(area.id.as_str()))
        {
            add_project(&mut list, project, &area.title, 1)?;
        }
    }
    Ok(list)
}

// =============================================================================
// TaskListModel
// =============================================================================

#[repr(i32)]
#[derive(Clone, Copy)]
enum Role {
    TaskId = 0x0100, // Qt::UserRole
    Title,
    Notes,
    Done,
    StartDate,
    Evening,
    /// ISO date, or empty.
    Deadline,
    /// `DeadlineStatus` on today: none, set, today or overdue.
    DeadlineStatus,
    /// The section the row sits under (`QueryEngine::section_keys`).
    Section,
    /// Whether the task is deferred to Someday.
    Someday,
    /// `task`; `heading` for a heading with no tasks to list (#22);
    /// `project`; or `event` (#62).
    Kind,
    /// The heading a task is under, or an empty heading's own id.
    HeadingId,
    /// Checklist items done, and in all (0 without a checklist).
    ChecklistDone,
    ChecklistTotal,
    /// `open`, `completed` or `canceled`.
    Status,
    /// The task's own tags, as JSON `[{id, name}]` (#57).
    Tags,
    /// The time an open, dated task reminds at, `HH:MM`, or empty (#60).
    Reminder,
    /// The repeat rule a task carries, in words, or empty (#61).
    Repeat,
    /// An event's local start and end, `YYYY-MM-DDTHH:MM:SS` (#62); the
    /// end is exclusive. Its day is `startDate`.
    EventStart,
    EventEnd,
    /// Whether an event lasts whole days.
    AllDay,
    /// An event's calendar: its colour (`#rrggbb`, or empty) and name.
    CalendarColor,
    CalendarName,
}

impl Role {
    const ALL: [(Role, &'static str); 23] = [
        (Role::TaskId, "taskId"),
        (Role::Title, "title"),
        (Role::Notes, "notes"),
        (Role::Done, "done"),
        (Role::StartDate, "startDate"),
        (Role::Evening, "evening"),
        (Role::Deadline, "deadline"),
        (Role::DeadlineStatus, "deadlineStatus"),
        (Role::Section, "section"),
        (Role::Someday, "someday"),
        (Role::ChecklistDone, "checklistDone"),
        (Role::ChecklistTotal, "checklistTotal"),
        (Role::Kind, "kind"),
        (Role::HeadingId, "headingId"),
        (Role::Status, "status"),
        (Role::Tags, "tags"),
        (Role::Reminder, "reminder"),
        (Role::Repeat, "repeat"),
        (Role::EventStart, "eventStart"),
        (Role::EventEnd, "eventEnd"),
        (Role::AllDay, "allDay"),
        (Role::CalendarColor, "calendarColor"),
        (Role::CalendarName, "calendarName"),
    ];

    fn from_i32(role: i32) -> Option<Role> {
        Role::ALL
            .iter()
            .map(|(r, _)| *r)
            .find(|r| *r as i32 == role)
    }
}

/// The key a row is reconciled by: a task's id, or `heading:<id>` for an
/// empty heading's row (task ids are UUIDs, so the two never meet).
fn row_key(row: &ListRow) -> String {
    match row {
        ListRow::Task(task) => task.id.clone(),
        ListRow::EmptyHeading(heading) => format!("heading:{}", heading.id),
        ListRow::Project(project) => format!("project:{}", project.id),
        ListRow::Event(listed) => listed.key(),
        ListRow::Preview(preview) => preview.key(),
    }
}

/// `rows` of `view` with the calendar events it lists (#62): those of the
/// calendars shown, from the latest read. Also the calendars of the events
/// listed, by id.
fn with_events(view: &View, rows: Vec<ListRow>) -> (Vec<ListRow>, HashMap<String, Calendar>) {
    let today = local_today();
    let Some((from, until)) = event_days(view, today) else {
        return (rows, HashMap::new());
    };
    let read = crate::calendar::snapshot();
    let events: Vec<_> = {
        let state = settings();
        read.events
            .into_iter()
            .filter(|e| state.settings.calendar.shows(&e.calendar_id))
            .collect()
    };
    let calendars = read
        .calendars
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();
    let rows = merge_events(view, rows, events_by_day(&events, from, until), today);
    (rows, calendars)
}

/// Upcoming's `rows` with the repeating series' future occurrences among
/// them (#61), over the days it lists events; other views are unchanged.
fn with_previews(
    db: &Database,
    view: &View,
    rows: Vec<ListRow>,
) -> matters_core::Result<Vec<ListRow>> {
    if *view != View::Upcoming {
        return Ok(rows);
    }
    let Some((from, until)) = event_days(view, local_today()) else {
        return Ok(rows);
    };
    Ok(matters_core::repeat::merge_previews(
        rows,
        db.repeat_previews(from, until)?,
    ))
}

fn row_task(row: &ListRow) -> Option<&Task> {
    match row {
        ListRow::Task(task) => Some(task),
        ListRow::EmptyHeading(_)
        | ListRow::Project(_)
        | ListRow::Event(_)
        | ListRow::Preview(_) => None,
    }
}

pub struct TaskListModelRust {
    error_message: QString,
    present_tags_json: QString,
    rows: Vec<ListRow>,
    /// Section key per row (`row_key`), #25.
    sections: HashMap<String, String>,
    /// Checklist progress per listed task id that has a checklist (#21).
    progress: HashMap<String, ChecklistProgress>,
    /// The tags given to each listed task that has any (#57).
    tags: HashMap<String, Vec<TagChip>>,
    /// The calendars of the listed events, by id (#62).
    calendars: HashMap<String, Calendar>,
    db: Database,
    /// Keeps tasks ticked during this visit in place (see `matters_core::Visit`).
    visit: Visit,
    /// Set while `load` is notifying views. A `load` requested from inside one
    /// of those notifications is queued here and run afterwards, so row
    /// mutations never interleave.
    loading: bool,
    queued: Option<(String, String, i32, String)>,
    /// The navigation the current rows belong to.
    visit_id: Option<i32>,
}

impl Default for TaskListModelRust {
    fn default() -> Self {
        Self {
            error_message: QString::default(),
            present_tags_json: QString::from("[]"),
            rows: Vec::new(),
            calendars: HashMap::new(),
            db: database(),
            visit: Visit::new(),
            loading: false,
            queued: None,
            visit_id: None,
            sections: HashMap::new(),
            progress: HashMap::new(),
            tags: HashMap::new(),
        }
    }
}

impl qobject::TaskListModel {
    pub fn load(
        mut self: Pin<&mut Self>,
        view: &QString,
        project_id: &QString,
        visit: i32,
        tag: &QString,
    ) {
        let mut request = (
            String::from(view),
            String::from(project_id),
            visit,
            String::from(tag),
        );
        if self.loading {
            self.as_mut().rust_mut().queued = Some(request);
            return;
        }
        self.as_mut().rust_mut().loading = true;
        loop {
            self.as_mut()
                .load_now(&request.0, &request.1, request.2, &request.3);
            match self.as_mut().rust_mut().queued.take() {
                Some(next) => request = next,
                None => break,
            }
        }
        self.as_mut().rust_mut().loading = false;
    }

    fn load_now(mut self: Pin<&mut Self>, view_key: &str, project_id: &str, visit: i32, tag: &str) {
        let Some(view) = View::from_key(view_key, Some(project_id)) else {
            let message = format!("unknown view '{view_key}'");
            return self.set_error_message(QString::from(message.as_str()));
        };
        let target = {
            let this = self.as_mut().rust_mut().get_mut();
            if this.visit_id != Some(visit) {
                this.visit_id = Some(visit);
                this.visit.restart();
            }
            let (logged_until, keep_ticked) = logging();
            this.visit.keep_ticked(keep_ticked);
            let db = &this.db;
            let engine = QueryEngine::new(db).logged_until(logged_until);
            engine
                .tasks_in(&view, local_today())
                .and_then(|members| {
                    let shown: Vec<Task> = this.rows.iter().filter_map(row_task).cloned().collect();
                    this.visit
                        .rows(&view, members, &shown, |id| db.get_task(id))
                })
                .and_then(|tasks| {
                    let keys = engine.section_keys(&view, &tasks)?;
                    let mut sections: HashMap<String, String> =
                        tasks.iter().map(|t| t.id.clone()).zip(keys).collect();
                    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
                    let progress = db.checklist_progress(&ids)?;
                    let tags: HashMap<String, Vec<TagChip>> = db
                        .tags_of_tasks(&ids)?
                        .into_iter()
                        .map(|(task, tags)| (task, tag_chips(&tags)))
                        .collect();
                    let present = tag_entries(&engine.tags_present(&tasks)?);
                    let rows = engine.list_rows(&view, tasks, local_today())?;
                    // Events have no tags: a list narrowed to one has none.
                    // Events and previews have no tags: a list narrowed to one
                    // has neither.
                    let (rows, calendars) = if tag.is_empty() {
                        let (rows, calendars) = with_events(&view, rows);
                        (with_previews(db, &view, rows)?, calendars)
                    } else {
                        (engine.filter_rows(rows, tag)?, HashMap::new())
                    };
                    for row in &rows {
                        let key = match row {
                            ListRow::EmptyHeading(h) => {
                                matters_core::heading_section_key(&h.id, &h.title)
                            }
                            ListRow::Project(p) => matters_core::project_section_key(&view, p),
                            ListRow::Event(e) => event_section_key(&view, e),
                            ListRow::Preview(p) => format!("date:{}", p.day),
                            ListRow::Task(_) => continue,
                        };
                        sections.insert(row_key(row), key);
                    }
                    Ok((rows, sections, progress, tags, present, calendars))
                })
        };
        match target {
            Ok((rows, sections, progress, tags, present, calendars)) => {
                self.as_mut().set_present_tags_json(to_json(&present));
                let this = self.as_mut().rust_mut().get_mut();
                this.calendars = calendars;
                let old_sections = std::mem::replace(&mut this.sections, sections);
                let old_progress = std::mem::replace(&mut this.progress, progress);
                let old_tags = std::mem::replace(&mut this.tags, tags);
                reconcile(&mut QtRows(self.as_mut()), rows, row_key);
                // A row whose task is unchanged can still move to another
                // section (the Logbook after midnight, say) or have its
                // checklist ticked: refresh those.
                let moved: Vec<usize> = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        let key = row_key(row);
                        old_sections.get(&key) != self.sections.get(&key)
                            || old_progress.get(&key) != self.progress.get(&key)
                            || old_tags.get(&key) != self.tags.get(&key)
                    })
                    .map(|(i, _)| i)
                    .collect();
                for row in moved {
                    QtRows(self.as_mut()).changed(row);
                }
                self.set_error_message(QString::default());
            }
            Err(err) => {
                eprintln!("matters: {err}");
                self.set_error_message(QString::from(err.to_string().as_str()));
            }
        }
    }

    pub fn row_of(&self, task_id: &QString) -> i32 {
        let id = String::from(task_id);
        self.rows
            .iter()
            .position(|row| row_task(row).is_some_and(|t| t.id == id))
            .map_or(-1, qt_row)
    }

    pub fn id_at(&self, row: i32) -> QString {
        self.task_at(row)
            .map_or_else(QString::default, |task| QString::from(task.id.as_str()))
    }

    pub fn title_at(&self, row: i32) -> QString {
        self.task_at(row)
            .map_or_else(QString::default, |task| QString::from(task.title.as_str()))
    }

    pub fn evening_at(&self, row: i32) -> bool {
        self.task_at(row).is_some_and(Task::is_evening)
    }

    pub fn repeat_at(&self, row: i32) -> QString {
        let rule = self.task_at(row).and_then(|task| task.repeat.as_ref());
        QString::from(
            matters_core::repeat::stored_rule(rule)
                .unwrap_or_default()
                .as_str(),
        )
    }

    pub fn reminder_at(&self, row: i32) -> QString {
        let time = self.task_at(row).and_then(|task| task.reminder);
        QString::from(
            time.map(matters_core::reminders::stored_time)
                .unwrap_or_default()
                .as_str(),
        )
    }

    pub fn status_at(&self, row: i32) -> QString {
        self.task_at(row)
            .map_or_else(QString::default, |task| QString::from(task.status.as_str()))
    }

    pub fn heading_at(&self, row: i32) -> QString {
        let heading = match usize::try_from(row).ok().and_then(|r| self.rows.get(r)) {
            Some(ListRow::Task(task)) => task.heading_id.as_deref(),
            Some(ListRow::EmptyHeading(heading)) => Some(heading.id.as_str()),
            Some(ListRow::Project(_) | ListRow::Event(_) | ListRow::Preview(_)) | None => None,
        };
        QString::from(heading.unwrap_or_default())
    }

    fn task_at(&self, row: i32) -> Option<&Task> {
        usize::try_from(row)
            .ok()
            .and_then(|r| self.rows.get(r))
            .and_then(row_task)
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = usize::try_from(index.row()).ok();
        let (Some(row), Some(role)) = (row.and_then(|r| self.rows.get(r)), Role::from_i32(role))
        else {
            return QVariant::default();
        };
        let task = match row {
            ListRow::Task(task) => task,
            ListRow::EmptyHeading(heading) => return self.heading_data(row, heading, role),
            ListRow::Project(project) => return self.project_data(row, project, role),
            ListRow::Event(listed) => return self.event_data(row, listed, role),
            ListRow::Preview(preview) => return self.preview_data(row, preview, role),
        };
        match role {
            Role::Kind => QVariant::from(&QString::from("task")),
            Role::HeadingId => QVariant::from(&QString::from(
                task.heading_id.as_deref().unwrap_or_default(),
            )),
            Role::TaskId => QVariant::from(&QString::from(task.id.as_str())),
            Role::Title => QVariant::from(&QString::from(task.title.as_str())),
            Role::Notes => QVariant::from(&QString::from(task.notes.as_str())),
            Role::Done => QVariant::from(&(task.status == Status::Completed)),
            Role::StartDate => QVariant::from(&QString::from(
                task.start_date
                    .map(|d| d.to_string())
                    .unwrap_or_default()
                    .as_str(),
            )),
            Role::Evening => QVariant::from(&task.is_evening()),
            Role::Deadline => QVariant::from(&QString::from(
                task.deadline
                    .map(|d| d.to_string())
                    .unwrap_or_default()
                    .as_str(),
            )),
            Role::Someday => QVariant::from(&task.is_someday),
            Role::Status => QVariant::from(&QString::from(task.status.as_str())),
            Role::Tags => match self.tags.get(&task.id) {
                Some(tags) => QVariant::from(&to_json(tags)),
                None => QVariant::from(&QString::from("[]")),
            },
            Role::Section => QVariant::from(&QString::from(
                self.sections
                    .get(&task.id)
                    .map(String::as_str)
                    .unwrap_or_default(),
            )),
            Role::ChecklistDone => {
                let done = self.progress.get(&task.id).map_or(0, |p| p.done);
                QVariant::from(&i32::try_from(done).unwrap_or(i32::MAX))
            }
            Role::ChecklistTotal => {
                let total = self.progress.get(&task.id).map_or(0, |p| p.total);
                QVariant::from(&i32::try_from(total).unwrap_or(i32::MAX))
            }
            Role::DeadlineStatus => QVariant::from(&QString::from(deadline_key(
                task.deadline_status(local_today()),
            ))),
            Role::Reminder => QVariant::from(&QString::from(
                matters_core::reminders::due(task)
                    .map(|due| matters_core::reminders::stored_time(due.time()))
                    .unwrap_or_default()
                    .as_str(),
            )),
            Role::Repeat => QVariant::from(&QString::from(
                task.repeat
                    .as_ref()
                    .map(|r| crate::repeat_words::describe(r, ""))
                    .unwrap_or_default()
                    .as_str(),
            )),
            Role::EventStart | Role::EventEnd | Role::CalendarColor | Role::CalendarName => {
                QVariant::from(&QString::default())
            }
            Role::AllDay => QVariant::from(&false),
        }
    }

    /// A calendar event's row (#62): its kind, key (as `taskId`), title,
    /// day (as `startDate`), times, calendar and section; the task roles
    /// are empty.
    fn event_data(&self, row: &ListRow, listed: &DayEvent, role: Role) -> QVariant {
        let event = &listed.event;
        let calendar = self.calendars.get(&event.calendar_id);
        let text = |s: &str| QVariant::from(&QString::from(s));
        match role {
            Role::Kind => text("event"),
            Role::TaskId => text(&listed.key()),
            Role::Title => text(&event.title),
            Role::StartDate => text(&listed.day.to_string()),
            Role::EventStart => text(&event.start.to_string()),
            Role::EventEnd => text(&event.end.to_string()),
            Role::AllDay => QVariant::from(&event.all_day),
            Role::CalendarColor => text(
                calendar
                    .and_then(|c| c.color.as_deref())
                    .unwrap_or_default(),
            ),
            Role::CalendarName => text(calendar.map(|c| c.name.as_str()).unwrap_or_default()),
            Role::Section => text(
                self.sections
                    .get(&row_key(row))
                    .map(String::as_str)
                    .unwrap_or_default(),
            ),
            Role::Done | Role::Evening | Role::Someday => QVariant::from(&false),
            Role::ChecklistDone | Role::ChecklistTotal => QVariant::from(&0_i32),
            Role::Notes
            | Role::HeadingId
            | Role::Deadline
            | Role::DeadlineStatus
            | Role::Status
            | Role::Reminder
            | Role::Repeat => QVariant::from(&QString::default()),
            Role::Tags => text("[]"),
        }
    }

    /// A repeating task's future occurrence (#61): kind `preview`, its key
    /// (as `taskId`), title, day (as `startDate`), This Evening, rule and
    /// section; the other roles are empty.
    fn preview_data(
        &self,
        row: &ListRow,
        preview: &matters_core::repeat::Preview,
        role: Role,
    ) -> QVariant {
        let task = &preview.task;
        let text = |s: &str| QVariant::from(&QString::from(s));
        match role {
            Role::Kind => text("preview"),
            Role::TaskId => text(&preview.key()),
            Role::Title => text(&task.title),
            Role::StartDate => text(&preview.day.to_string()),
            Role::Evening => QVariant::from(&task.is_evening()),
            Role::Repeat => text(
                &task
                    .repeat
                    .as_ref()
                    .map(|r| crate::repeat_words::describe(r, ""))
                    .unwrap_or_default(),
            ),
            Role::Section => text(
                self.sections
                    .get(&row_key(row))
                    .map(String::as_str)
                    .unwrap_or_default(),
            ),
            Role::Done | Role::Someday | Role::AllDay => QVariant::from(&false),
            Role::ChecklistDone | Role::ChecklistTotal => QVariant::from(&0_i32),
            Role::Notes
            | Role::HeadingId
            | Role::Deadline
            | Role::DeadlineStatus
            | Role::Status
            | Role::Reminder
            | Role::EventStart
            | Role::EventEnd
            | Role::CalendarColor
            | Role::CalendarName => QVariant::from(&QString::default()),
            Role::Tags => text("[]"),
        }
    }

    /// A project's row (the Logbook, Today, Upcoming, Someday): its kind, id
    /// (as `taskId`), title, status, When, deadline and section; the
    /// task-only roles are empty.
    fn project_data(&self, row: &ListRow, project: &Project, role: Role) -> QVariant {
        let date =
            |d: Option<String>| QVariant::from(&QString::from(d.unwrap_or_default().as_str()));
        match role {
            Role::Kind => QVariant::from(&QString::from("project")),
            Role::TaskId => QVariant::from(&QString::from(project.id.as_str())),
            Role::Title => QVariant::from(&QString::from(project.title.as_str())),
            Role::Done => QVariant::from(&(project.status == Status::Completed)),
            Role::Status => QVariant::from(&QString::from(project.status.as_str())),
            Role::StartDate => date(project.start_date.map(|d| d.to_string())),
            Role::Evening => QVariant::from(&(project.time_of_day.as_deref() == Some("evening"))),
            Role::Someday => QVariant::from(&project.is_someday),
            Role::Deadline => date(project.deadline.map(|d| d.to_string())),
            Role::DeadlineStatus => QVariant::from(&QString::from(deadline_key(
                project.deadline_status(local_today()),
            ))),
            Role::Section => QVariant::from(&QString::from(
                self.sections
                    .get(&row_key(row))
                    .map(String::as_str)
                    .unwrap_or_default(),
            )),
            Role::ChecklistDone | Role::ChecklistTotal => QVariant::from(&0_i32),
            Role::Notes
            | Role::HeadingId
            | Role::Reminder
            | Role::Repeat
            | Role::EventStart
            | Role::EventEnd
            | Role::CalendarColor
            | Role::CalendarName => QVariant::from(&QString::default()),
            Role::AllDay => QVariant::from(&false),
            Role::Tags => QVariant::from(&QString::from("[]")),
        }
    }

    /// An empty heading's row: its kind, id, title and section; the task
    /// roles are empty.
    fn heading_data(&self, row: &ListRow, heading: &Heading, role: Role) -> QVariant {
        match role {
            Role::Kind => QVariant::from(&QString::from("heading")),
            Role::HeadingId => QVariant::from(&QString::from(heading.id.as_str())),
            Role::Title => QVariant::from(&QString::from(heading.title.as_str())),
            Role::Section => QVariant::from(&QString::from(
                self.sections
                    .get(&row_key(row))
                    .map(String::as_str)
                    .unwrap_or_default(),
            )),
            Role::Done | Role::Evening | Role::Someday => QVariant::from(&false),
            Role::ChecklistDone | Role::ChecklistTotal => QVariant::from(&0_i32),
            Role::TaskId
            | Role::Notes
            | Role::StartDate
            | Role::Deadline
            | Role::DeadlineStatus
            | Role::Status
            | Role::Reminder
            | Role::Repeat
            | Role::EventStart
            | Role::EventEnd
            | Role::CalendarColor
            | Role::CalendarName => QVariant::from(&QString::default()),
            Role::AllDay => QVariant::from(&false),
            Role::Tags => QVariant::from(&QString::from("[]")),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (role, name) in Role::ALL {
            roles.insert(role as i32, QByteArray::from(name));
        }
        roles
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0 // flat list: only the root has children
        } else {
            i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
        }
    }
}

fn qt_row(row: usize) -> i32 {
    i32::try_from(row).expect("row index fits in i32")
}

/// Declares a [`ListSink`] adapter for a flat Qt list model, so
/// [`reconcile`] drives the `begin*`/`end*` notifications around each
/// mutation. One definition serves every model.
macro_rules! qt_list_sink {
    ($(#[$doc:meta])* $sink:ident, $model:ty, $row:ty) => {
        $(#[$doc])*
        struct $sink<'a>(Pin<&'a mut $model>);

        impl ListSink<$row> for $sink<'_> {
            fn rows(&mut self) -> &mut Vec<$row> {
                &mut self.0.as_mut().rust_mut().get_mut().rows
            }
            fn begin_remove(&mut self, row: usize) {
                let row = qt_row(row);
                self.0
                    .as_mut()
                    .begin_remove_rows(&QModelIndex::default(), row, row);
            }
            fn end_remove(&mut self) {
                self.0.as_mut().end_remove_rows();
            }
            fn begin_insert(&mut self, row: usize) {
                let row = qt_row(row);
                self.0
                    .as_mut()
                    .begin_insert_rows(&QModelIndex::default(), row, row);
            }
            fn end_insert(&mut self) {
                self.0.as_mut().end_insert_rows();
            }
            fn begin_move(&mut self, from: usize, to: usize) {
                let root = QModelIndex::default();
                let from = qt_row(from);
                let moved = self
                    .0
                    .as_mut()
                    .begin_move_rows(&root, from, from, &root, qt_row(to));
                assert!(moved, "beginMoveRows rejected a move from {from} to {to}");
            }
            fn end_move(&mut self) {
                self.0.as_mut().end_move_rows();
            }
            fn changed(&mut self, row: usize) {
                let index = self.0.index(qt_row(row), 0, &QModelIndex::default());
                self.0
                    .as_mut()
                    .data_changed(&index, &index, &QList::<i32>::default());
            }
        }
    };
}

qt_list_sink!(
    /// The task list model as a [`ListSink`].
    QtRows,
    qobject::TaskListModel,
    ListRow
);

// =============================================================================
// ChecklistModel
// =============================================================================

#[repr(i32)]
#[derive(Clone, Copy)]
enum ItemRole {
    ItemId = 0x0100, // Qt::UserRole
    Title,
    Done,
}

impl ItemRole {
    const ALL: [(ItemRole, &'static str); 3] = [
        (ItemRole::ItemId, "itemId"),
        (ItemRole::Title, "title"),
        (ItemRole::Done, "done"),
    ];

    fn from_i32(role: i32) -> Option<ItemRole> {
        ItemRole::ALL
            .iter()
            .map(|(r, _)| *r)
            .find(|r| *r as i32 == role)
    }
}

pub struct ChecklistModelRust {
    rows: Vec<ChecklistItem>,
    db: Database,
}

impl Default for ChecklistModelRust {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            db: database(),
        }
    }
}

impl qobject::ChecklistModel {
    pub fn load(mut self: Pin<&mut Self>, task_id: &QString) {
        match self.db.list_checklist_items(&String::from(task_id)) {
            Ok(items) => reconcile(&mut ChecklistRows(self.as_mut()), items, |i| i.id.clone()),
            Err(err) => eprintln!("matters: {err}"),
        }
    }

    pub fn id_at(&self, row: i32) -> QString {
        usize::try_from(row)
            .ok()
            .and_then(|r| self.rows.get(r))
            .map_or_else(QString::default, |item| QString::from(item.id.as_str()))
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let row = usize::try_from(index.row()).ok();
        let (Some(item), Some(role)) =
            (row.and_then(|r| self.rows.get(r)), ItemRole::from_i32(role))
        else {
            return QVariant::default();
        };
        match role {
            ItemRole::ItemId => QVariant::from(&QString::from(item.id.as_str())),
            ItemRole::Title => QVariant::from(&QString::from(item.title.as_str())),
            ItemRole::Done => QVariant::from(&item.is_completed),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (role, name) in ItemRole::ALL {
            roles.insert(role as i32, QByteArray::from(name));
        }
        roles
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
        }
    }
}

qt_list_sink!(
    /// The checklist model as a [`ListSink`].
    ChecklistRows,
    qobject::ChecklistModel,
    ChecklistItem
);

/// A task edit that finds the task already gone (deleted while its editor
/// was open, say) changes nothing, as before field-by-field writes.
fn gone_task_is_fine(result: matters_core::Result<()>) -> matters_core::Result<()> {
    match result {
        Err(matters_core::MattersError::NotFound { entity: "task", .. }) => Ok(()),
        other => other,
    }
}
