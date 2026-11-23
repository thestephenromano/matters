//! Changes as `Apply` takes them (#203): each one a dictionary naming its
//! operation (`op`) and that operation's arguments.
//!
//! Every change is parsed, and its permission checked, before anything is
//! changed. A whole batch then runs in one `matters_core` batch: one
//! transaction, one sync record, or nothing at all.

use std::collections::{BTreeMap, HashMap};

use jiff::civil::Date;
use matters_core::{
    Area, ChecklistItem, DateOrder, Heading, Home, Outcome, Project, Schedule, Tagged, Task, View,
    Writer,
};
use zbus::OwnedVariant;

use crate::clients::Permission;
use crate::error::{Error, Result};

/// The most changes one `Apply` may carry. The batch holds the database
/// for its whole run, and its record must stay well inside the 1 MiB a
/// record holds; larger work is sent as several batches.
pub const MAX_CHANGES: usize = 200;

/// The longest request ID a caller may give.
pub const MAX_REQUEST_ID: usize = 128;

/// An item named in a change: by its ID, or by the `ref` an earlier change
/// in the same batch gave the item it created.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    Id(String),
    Ref(String),
}

impl Target {
    fn parse(text: String) -> Target {
        match text.strip_prefix("ref:") {
            Some(name) => Target::Ref(name.to_owned()),
            None => Target::Id(text),
        }
    }
}

/// Where `move` files an item.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Destination {
    Inbox,
    /// No area, for a project.
    None,
    Area(Target),
    Project(Target, Option<Target>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Op {
    CreateTask {
        title: String,
        notes: Option<String>,
        when: Option<String>,
        deadline: Option<String>,
        home: Destination,
        tags: Vec<String>,
    },
    CreateProject {
        title: String,
        notes: Option<String>,
        when: Option<String>,
        deadline: Option<String>,
        area: Option<Target>,
    },
    CreateHeading {
        project: Target,
        title: String,
    },
    AddChecklistItem {
        task: Target,
        title: String,
    },
    CreateArea {
        title: String,
    },
    Update {
        id: Target,
        title: Option<String>,
        notes: Option<String>,
        when: Option<String>,
        deadline: Option<String>,
    },
    RenameArea {
        id: Target,
        title: String,
    },
    Finish {
        id: Target,
        outcome: Outcome,
        open_tasks: Option<Outcome>,
    },
    Reopen {
        id: Target,
    },
    Move {
        id: Target,
        to: Destination,
    },
    Place {
        id: Target,
        view: String,
        after: Option<Target>,
    },
    Tag {
        id: Target,
        tag: String,
        on: bool,
    },
    Check {
        id: Target,
        done: bool,
    },
    Trash {
        id: Target,
        restore: bool,
    },
}

/// One parsed change: its operation, and the `ref` it names what it
/// creates by, if any.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Change {
    pub op: Op,
    pub reference: Option<String>,
}

/// A change's dictionary, taken key by key: a key left over is refused,
/// so a misspelt argument is never silently ignored.
struct Fields(HashMap<String, OwnedVariant>);

impl Fields {
    fn string(&mut self, key: &str) -> Result<Option<String>> {
        match self.0.remove(key) {
            None => Ok(None),
            Some(v) => String::try_from(v)
                .map(Some)
                .map_err(|_| invalid(format!("{key:?} must be a string (s)"))),
        }
    }

    fn required(&mut self, key: &str) -> Result<String> {
        self.string(key)?
            .ok_or_else(|| invalid(format!("{key:?} is required")))
    }

    fn boolean(&mut self, key: &str) -> Result<Option<bool>> {
        match self.0.remove(key) {
            None => Ok(None),
            Some(v) => bool::try_from(v)
                .map(Some)
                .map_err(|_| invalid(format!("{key:?} must be a boolean (b)"))),
        }
    }

    fn strings(&mut self, key: &str) -> Result<Vec<String>> {
        match self.0.remove(key) {
            None => Ok(Vec::new()),
            Some(v) => Vec::<String>::try_from(v)
                .map_err(|_| invalid(format!("{key:?} must be a list of strings (as)"))),
        }
    }

    fn target(&mut self, key: &str) -> Result<Option<Target>> {
        Ok(self.string(key)?.map(Target::parse))
    }

    fn required_target(&mut self, key: &str) -> Result<Target> {
        Ok(Target::parse(self.required(key)?))
    }

    fn done(self) -> Result<()> {
        let mut left: Vec<&String> = self.0.keys().collect();
        left.sort();
        match left.first() {
            None => Ok(()),
            Some(_) => Err(invalid(format!("unknown keys {left:?}"))),
        }
    }
}

fn invalid(message: String) -> Error {
    Error::InvalidChange(message)
}

fn outcome(text: &str) -> Result<Outcome> {
    match text {
        "complete" => Ok(Outcome::Completed),
        "cancel" => Ok(Outcome::Canceled),
        other => Err(invalid(format!(
            "\"open_tasks\" is \"complete\" or \"cancel\", not {other:?}"
        ))),
    }
}

/// `inbox`, `none`, `area:<id>` or `project:<id>`, with `heading_id` for a
/// heading of that project.
fn destination(to: &str, heading: Option<Target>) -> Result<Destination> {
    let to = match to {
        "inbox" => Destination::Inbox,
        "none" => Destination::None,
        _ => match to.split_once(':') {
            Some(("area", id)) if !id.is_empty() => Destination::Area(Target::parse(id.into())),
            Some(("project", id)) if !id.is_empty() => {
                return Ok(Destination::Project(Target::parse(id.into()), heading))
            }
            _ => {
                return Err(invalid(format!(
                    "\"to\" is \"inbox\", \"none\", \"area:<id>\" or \"project:<id>\", not {to:?}"
                )))
            }
        },
    };
    match heading {
        Some(_) => Err(invalid("\"heading_id\" goes with \"project:<id>\"".into())),
        None => Ok(to),
    }
}

impl Change {
    /// Parse one change's dictionary.
    pub fn parse(map: HashMap<String, OwnedVariant>) -> Result<Change> {
        let mut f = Fields(map);
        let op_name = f.required("op")?;
        let reference = f.string("ref")?;
        let op = match op_name.as_str() {
            "create_task" => {
                let area = f.target("area_id")?;
                let project = f.target("project_id")?;
                let heading = f.target("heading_id")?;
                let home = match (area, project) {
                    (None, None) => destination("inbox", heading)?,
                    (Some(area), None) => {
                        destination("inbox", heading)?;
                        Destination::Area(area)
                    }
                    (None, Some(project)) => Destination::Project(project, heading),
                    (Some(_), Some(_)) => {
                        return Err(invalid(
                            "a task is in an area or a project, not both".into(),
                        ))
                    }
                };
                Op::CreateTask {
                    title: f.required("title")?,
                    notes: f.string("notes")?,
                    when: f.string("when")?,
                    deadline: f.string("deadline")?,
                    home,
                    tags: f.strings("tags")?,
                }
            }
            "create_project" => Op::CreateProject {
                title: f.required("title")?,
                notes: f.string("notes")?,
                when: f.string("when")?,
                deadline: f.string("deadline")?,
                area: f.target("area_id")?,
            },
            "create_heading" => Op::CreateHeading {
                project: f.required_target("project_id")?,
                title: f.required("title")?,
            },
            "add_checklist_item" => Op::AddChecklistItem {
                task: f.required_target("task_id")?,
                title: f.required("title")?,
            },
            "create_area" => Op::CreateArea {
                title: f.required("title")?,
            },
            "update" => Op::Update {
                id: f.required_target("id")?,
                title: f.string("title")?,
                notes: f.string("notes")?,
                when: f.string("when")?,
                deadline: f.string("deadline")?,
            },
            "rename_area" => Op::RenameArea {
                id: f.required_target("id")?,
                title: f.required("title")?,
            },
            "complete" | "cancel" => Op::Finish {
                id: f.required_target("id")?,
                outcome: outcome(&op_name)?,
                open_tasks: f.string("open_tasks")?.map(|o| outcome(&o)).transpose()?,
            },
            "reopen" => Op::Reopen {
                id: f.required_target("id")?,
            },
            "move" => {
                let id = f.required_target("id")?;
                let heading = f.target("heading_id")?;
                Op::Move {
                    id,
                    to: destination(&f.required("to")?, heading)?,
                }
            }
            "place" => Op::Place {
                id: f.required_target("id")?,
                view: f.required("view")?,
                after: f.target("after")?,
            },
            "tag" | "untag" => Op::Tag {
                id: f.required_target("id")?,
                tag: f.required("tag")?,
                on: op_name == "tag",
            },
            "check" => Op::Check {
                id: f.required_target("id")?,
                done: f.boolean("done")?.unwrap_or(true),
            },
            "trash" | "restore" => Op::Trash {
                id: f.required_target("id")?,
                restore: op_name == "restore",
            },
            other => return Err(invalid(format!("{other:?} is not an operation"))),
        };
        let creates = matches!(
            op,
            Op::CreateTask { .. }
                | Op::CreateProject { .. }
                | Op::CreateHeading { .. }
                | Op::AddChecklistItem { .. }
                | Op::CreateArea { .. }
        );
        if reference.is_some() && !creates {
            return Err(invalid(format!(
                "{op_name:?} creates nothing to give a ref"
            )));
        }
        f.done()?;
        Ok(Change { op, reference })
    }

    /// The permission this change needs, besides any the item it names
    /// turns out to need (an area's trash needs Areas too).
    pub fn needs(&self) -> Permission {
        match self.op {
            Op::CreateTask { .. }
            | Op::CreateProject { .. }
            | Op::CreateHeading { .. }
            | Op::AddChecklistItem { .. } => Permission::Create,
            Op::CreateArea { .. } | Op::RenameArea { .. } => Permission::Areas,
            Op::Trash { .. } => Permission::Trash,
            Op::Update { .. }
            | Op::Finish { .. }
            | Op::Reopen { .. }
            | Op::Move { .. }
            | Op::Place { .. }
            | Op::Tag { .. }
            | Op::Check { .. } => Permission::Organise,
        }
    }
}

/// What a batch knows while it runs.
pub(crate) struct Run {
    pub today: Date,
    /// What each `ref` named so far stands for.
    pub refs: BTreeMap<String, String>,
    /// May the client change areas: trashing one needs Areas too.
    pub areas: bool,
}

/// What a typed When or deadline means. A numeric date that month-first
/// and day-first read as different days (`3/4`) is refused, since a
/// program calling the interface has no locale the app could trust; one
/// only one order can read (`30/12`), or both read alike, is accepted.
fn typed<T: PartialEq>(
    text: &str,
    what: &str,
    parse: impl Fn(DateOrder) -> Option<T>,
) -> Result<T> {
    match (parse(DateOrder::MonthFirst), parse(DateOrder::DayFirst)) {
        (Some(a), Some(b)) if a != b => Err(invalid(format!(
            "{what} {text:?} reads differently month first and day first; use YYYY-MM-DD"
        ))),
        (Some(a), _) | (None, Some(a)) => Ok(a),
        (None, None) => Err(invalid(format!("{what} {text:?} is not one Matters reads"))),
    }
}

impl Run {
    fn id(&self, target: &Target) -> Result<String> {
        match target {
            Target::Id(id) => Ok(id.clone()),
            Target::Ref(name) => self
                .refs
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("no earlier change has ref {name:?}"))),
        }
    }

    fn when(&self, text: &str) -> Result<Schedule> {
        match text.trim().to_lowercase().as_str() {
            "anytime" | "none" => Ok(Schedule::None),
            _ => typed(text, "When", |order| {
                Schedule::parse_typed(text, self.today, order)
            }),
        }
    }

    /// A deadline, or `None` for `""` or `none` (no deadline).
    fn deadline(&self, text: &str) -> Result<Option<Date>> {
        match text.trim().to_lowercase().as_str() {
            "" | "none" => Ok(None),
            _ => typed(text, "deadline", |order| {
                Schedule::parse_typed_deadline(text, self.today, order)
            })
            .map(Some),
        }
    }

    fn home(&self, to: &Destination) -> Result<Home> {
        Ok(match to {
            Destination::Inbox | Destination::None => Home::Inbox,
            Destination::Area(area) => Home::Area(self.id(area)?),
            Destination::Project(project, heading) => Home::Project {
                project: self.id(project)?,
                heading: heading.as_ref().map(|h| self.id(h)).transpose()?,
            },
        })
    }

    /// What kind of item `id` is, as the batch sees it now.
    fn kind(&self, w: &Writer<'_>, id: &str) -> Result<Kind> {
        let q = w.query();
        if q.task(id)?.is_some() {
            return Ok(Kind::Task);
        }
        if q.project(id)?.is_some() {
            return Ok(Kind::Project);
        }
        if q.area(id)?.is_some() {
            return Ok(Kind::Area);
        }
        Err(Error::NotFound(format!("no item has the ID {id:?}")))
    }

    fn not(&self, kind: Kind, op: &str, id: &str) -> Error {
        invalid(format!("{op} does not apply to {} {id:?}", kind.name()))
    }

    fn tag_id(&self, w: &Writer<'_>, name: &str) -> Result<String> {
        w.query().tag_named(name)?.map(|t| t.id).ok_or_else(|| {
            invalid(format!(
                "there is no tag {name:?}; tags are made in Matters"
            ))
        })
    }

    /// Make one change in batch `w`. Returns the ID of what it created.
    pub fn apply(&mut self, w: &mut Writer<'_>, change: &Change) -> Result<Option<String>> {
        let created = match &change.op {
            Op::CreateTask {
                title,
                notes,
                when,
                deadline,
                home,
                tags,
            } => {
                let id = new_id();
                let mut task = Task::new(&id, title.as_str(), 0.0);
                self.home(home)?.apply(&mut task);
                task.notes = notes.clone().unwrap_or_default();
                w.add_task(&mut task)?;
                if let Some(when) = when {
                    w.set_task_schedule(&id, &self.when(when)?, self.today)?;
                }
                if let Some(deadline) = deadline {
                    w.set_task_deadline(&id, self.deadline(deadline)?)?;
                }
                for tag in tags {
                    let tag = self.tag_id(w, tag)?;
                    w.add_tag_to(Tagged::Task(&id), &tag)?;
                }
                Some(id)
            }
            Op::CreateProject {
                title,
                notes,
                when,
                deadline,
                area,
            } => {
                let id = new_id();
                let mut project = Project::new(&id, title.as_str(), 0.0);
                project.area_id = area.as_ref().map(|a| self.id(a)).transpose()?;
                project.notes = notes.clone().unwrap_or_default();
                w.add_project(&mut project)?;
                if let Some(when) = when {
                    w.set_project_schedule(&id, &self.when(when)?, self.today)?;
                }
                if let Some(deadline) = deadline {
                    w.set_project_deadline(&id, self.deadline(deadline)?)?;
                }
                Some(id)
            }
            Op::CreateHeading { project, title } => {
                let id = new_id();
                w.add_heading(&mut Heading::new(
                    &id,
                    self.id(project)?,
                    title.as_str(),
                    0.0,
                ))?;
                Some(id)
            }
            Op::AddChecklistItem { task, title } => {
                let id = new_id();
                w.add_checklist_item(&mut ChecklistItem::new(
                    &id,
                    self.id(task)?,
                    title.as_str(),
                    0.0,
                ))?;
                Some(id)
            }
            Op::CreateArea { title } => {
                let id = new_id();
                w.add_area(&mut Area::new(&id, title.as_str(), 0.0))?;
                Some(id)
            }
            Op::Update {
                id,
                title,
                notes,
                when,
                deadline,
            } => {
                let id = self.id(id)?;
                match self.kind(w, &id)? {
                    Kind::Task => {
                        if let Some(title) = title {
                            w.rename_task(&id, title)?;
                        }
                        if let Some(notes) = notes {
                            w.set_task_notes(&id, notes)?;
                        }
                        if let Some(when) = when {
                            w.set_task_schedule(&id, &self.when(when)?, self.today)?;
                        }
                        if let Some(deadline) = deadline {
                            w.set_task_deadline(&id, self.deadline(deadline)?)?;
                        }
                    }
                    Kind::Project => {
                        if let Some(title) = title {
                            w.rename_project(&id, title)?;
                        }
                        if let Some(notes) = notes {
                            w.set_project_notes(&id, notes)?;
                        }
                        if let Some(when) = when {
                            w.set_project_schedule(&id, &self.when(when)?, self.today)?;
                        }
                        if let Some(deadline) = deadline {
                            w.set_project_deadline(&id, self.deadline(deadline)?)?;
                        }
                    }
                    kind => return Err(self.not(kind, "update", &id)),
                }
                None
            }
            Op::RenameArea { id, title } => {
                let id = self.id(id)?;
                w.rename_area(&id, title)?;
                None
            }
            Op::Finish {
                id,
                outcome,
                open_tasks,
            } => {
                let id = self.id(id)?;
                match (self.kind(w, &id)?, outcome, open_tasks) {
                    (Kind::Task, Outcome::Completed, None) => {
                        w.complete_task_on(&id, self.today)?
                    }
                    (Kind::Task, Outcome::Canceled, None) => w.cancel_task(&id)?,
                    (Kind::Task, _, Some(_)) => {
                        return Err(invalid("\"open_tasks\" is for projects".into()))
                    }
                    (Kind::Project, outcome, Some(open_tasks)) => {
                        w.finish_project(&id, *outcome, *open_tasks)?
                    }
                    (Kind::Project, _, None) => {
                        return Err(invalid(
                            "finishing a project needs \"open_tasks\": \"complete\" or \"cancel\""
                                .into(),
                        ))
                    }
                    (kind, _, _) => return Err(self.not(kind, "finishing", &id)),
                }
                None
            }
            Op::Reopen { id } => {
                let id = self.id(id)?;
                match self.kind(w, &id)? {
                    Kind::Task => w.uncomplete_task(&id)?,
                    Kind::Project => w.reopen_project(&id)?,
                    kind => return Err(self.not(kind, "reopen", &id)),
                }
                None
            }
            Op::Move { id, to } => {
                let id = self.id(id)?;
                match (self.kind(w, &id)?, to) {
                    (Kind::Task, Destination::None) => {
                        return Err(invalid("a task moves to \"inbox\", not \"none\"".into()))
                    }
                    (Kind::Task, to) => w.set_task_home(&id, &self.home(to)?)?,
                    (Kind::Project, Destination::None) => w.set_project_area(&id, None)?,
                    (Kind::Project, Destination::Area(area)) => {
                        w.set_project_area(&id, Some(&self.id(area)?))?
                    }
                    (Kind::Project, _) => {
                        return Err(invalid(
                            "a project moves to \"area:<id>\" or \"none\"".into(),
                        ))
                    }
                    (kind, _) => return Err(self.not(kind, "move", &id)),
                }
                None
            }
            Op::Place { id, view, after } => {
                let id = self.id(id)?;
                let after = after.as_ref().map(|a| self.id(a)).transpose()?;
                let view = match view.as_str() {
                    "inbox" => View::Inbox,
                    "today" => View::Today,
                    other => match other.split_once(':') {
                        Some(("project", p)) if !p.is_empty() => View::Project(self.id(&Target::parse(p.into()))?),
                        Some(("area", a)) if !a.is_empty() => View::Area(self.id(&Target::parse(a.into()))?),
                        _ => {
                            return Err(invalid(format!(
                                "tasks are placed in \"inbox\", \"today\", \"project:<id>\" or \"area:<id>\", not {other:?}"
                            )))
                        }
                    },
                };
                w.place_task(&view, self.today, &id, after.as_deref())?;
                None
            }
            Op::Tag { id, tag, on } => {
                let id = self.id(id)?;
                let tag = self.tag_id(w, tag)?;
                let item = match self.kind(w, &id)? {
                    Kind::Task => Tagged::Task(&id),
                    Kind::Project => Tagged::Project(&id),
                    Kind::Area => Tagged::Area(&id),
                };
                match on {
                    true => w.add_tag_to(item, &tag)?,
                    false => w.remove_tag_from(item, &tag)?,
                }
                None
            }
            Op::Check { id, done } => {
                w.set_checklist_item_done(&self.id(id)?, *done)?;
                None
            }
            Op::Trash { id, restore } => {
                let id = self.id(id)?;
                match (self.kind(w, &id)?, restore) {
                    (Kind::Task, false) => w.trash_task(&id)?,
                    (Kind::Task, true) => w.restore_task(&id)?,
                    (Kind::Project, false) => w.trash_project(&id)?,
                    (Kind::Project, true) => w.restore_project(&id)?,
                    (Kind::Area, _) if !self.areas => {
                        return Err(Error::PermissionDenied(
                            "trashing or restoring an area needs the Areas permission too".into(),
                        ))
                    }
                    (Kind::Area, false) => w.trash_area(&id)?,
                    (Kind::Area, true) => w.restore_area(&id)?,
                }
                None
            }
        };
        if let (Some(name), Some(id)) = (&change.reference, &created) {
            if self.refs.insert(name.clone(), id.clone()).is_some() {
                return Err(invalid(format!("ref {name:?} is given twice")));
            }
        }
        Ok(created)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Task,
    Project,
    Area,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Task => "task",
            Kind::Project => "project",
            Kind::Area => "area",
        }
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
