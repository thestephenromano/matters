//! Items as the interface returns them: D-Bus dictionaries (`a{sv}`).
//!
//! Every item, whatever its kind, is one dictionary with a `kind` key. A key
//! is present only when it has a value, and new keys may be added within
//! this interface version: callers ignore keys they do not know. Dates are
//! ISO 8601 (`2026-11-22`), times `HH:MM`, timestamps milliseconds since the
//! Unix epoch.

use std::collections::HashMap;

use matters_core::calendar::Event;
use matters_core::{Area, ChecklistProgress, Project, Status, Tag, Task};
use zbus::{DeserializeDict, SerializeDict, Type};

#[derive(Debug, Clone, Default, PartialEq, Eq, SerializeDict, DeserializeDict, Type)]
#[zbus(signature = "dict")]
pub struct Item {
    /// `task`, `project`, `area` or `event`.
    pub kind: String,
    pub id: String,
    pub title: String,
    /// Where the item sits in the view: `evening` for This Evening in
    /// Today, the day (`2026-11-23`) in Upcoming, `events` for Today's
    /// calendar events.
    pub section: Option<String>,
    pub notes: Option<String>,
    /// `open`, `completed` or `canceled`.
    pub status: Option<String>,
    pub start_date: Option<String>,
    pub evening: Option<bool>,
    pub someday: Option<bool>,
    pub deadline: Option<String>,
    pub reminder: Option<String>,
    pub repeats: Option<bool>,
    pub area_id: Option<String>,
    pub project_id: Option<String>,
    pub heading_id: Option<String>,
    pub completed_at: Option<i64>,
    pub trashed: Option<bool>,
    /// Tag names, in the tag list's order.
    pub tags: Option<Vec<String>>,
    pub checklist_done: Option<u32>,
    pub checklist_total: Option<u32>,
    /// Calendar events: local start and end (`2026-11-22T09:30`), and
    /// whether the event lasts all day.
    pub start: Option<String>,
    pub end: Option<String>,
    pub all_day: Option<bool>,
    pub calendar_id: Option<String>,
}

fn status(s: Status) -> String {
    s.as_str().to_owned()
}

/// What the interface adds to a task beyond its own row.
#[derive(Default)]
pub(crate) struct Extras {
    pub tags: HashMap<String, Vec<Tag>>,
    pub checklists: HashMap<String, ChecklistProgress>,
}

pub(crate) fn task(t: &Task, section: Option<String>, extras: &Extras) -> Item {
    let checklist = extras.checklists.get(&t.id);
    Item {
        kind: "task".into(),
        id: t.id.clone(),
        title: t.title.clone(),
        section,
        notes: (!t.notes.is_empty()).then(|| t.notes.clone()),
        status: Some(status(t.status)),
        start_date: t.start_date.map(|d| d.to_string()),
        evening: Some(t.time_of_day.as_deref() == Some("evening")),
        someday: Some(t.is_someday),
        deadline: t.deadline.map(|d| d.to_string()),
        reminder: t.reminder.map(|r| r.strftime("%H:%M").to_string()),
        repeats: Some(t.repeat.is_some()),
        area_id: t.area_id.clone(),
        project_id: t.project_id.clone(),
        heading_id: t.heading_id.clone(),
        completed_at: t.completed_at,
        trashed: Some(t.trashed_at.is_some()),
        tags: extras
            .tags
            .get(&t.id)
            .map(|tags| tags.iter().map(|t| t.name.clone()).collect()),
        checklist_done: checklist.map(|c| c.done as u32),
        checklist_total: checklist.map(|c| c.total as u32),
        ..Item::default()
    }
}

pub(crate) fn project(p: &Project) -> Item {
    Item {
        kind: "project".into(),
        id: p.id.clone(),
        title: p.title.clone(),
        notes: (!p.notes.is_empty()).then(|| p.notes.clone()),
        status: Some(status(p.status)),
        start_date: p.start_date.map(|d| d.to_string()),
        evening: Some(p.time_of_day.as_deref() == Some("evening")),
        someday: Some(p.is_someday),
        deadline: p.deadline.map(|d| d.to_string()),
        area_id: p.area_id.clone(),
        completed_at: p.completed_at,
        trashed: Some(p.trashed_at.is_some()),
        ..Item::default()
    }
}

pub(crate) fn area(a: &Area) -> Item {
    Item {
        kind: "area".into(),
        id: a.id.clone(),
        title: a.title.clone(),
        trashed: Some(a.trashed_at.is_some()),
        ..Item::default()
    }
}

pub(crate) fn event(e: &Event) -> Item {
    let minute = |dt: &jiff::civil::DateTime| dt.strftime("%Y-%m-%dT%H:%M").to_string();
    Item {
        kind: "event".into(),
        id: e.id.clone(),
        title: e.title.clone(),
        section: Some("events".into()),
        start: Some(minute(&e.start)),
        end: Some(minute(&e.end)),
        all_day: Some(e.all_day),
        calendar_id: Some(e.calendar_id.clone()),
        ..Item::default()
    }
}
