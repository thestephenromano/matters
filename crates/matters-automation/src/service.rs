//! The interface object: `io.github.thestephenromano.Matters.Automation1`.

use std::sync::Arc;

use jiff::civil::Date;
use matters_core::calendar::Event;
use matters_core::{Database, QueryEngine, Task, View};
use zbus::interface;

use crate::clients::{verifier_of_presented, Client, Clients, Permission};
use crate::error::{Error, Result};
use crate::items::{self, Extras, Item};

/// How many finished tasks the Logbook view returns, newest first.
pub const LOGBOOK_LIMIT: usize = 500;

/// Today's calendar events as the window shows them (the app applies the
/// user's calendar choices), or none without a calendar service.
pub type TodayEvents = Box<dyn Fn(Date) -> Vec<Event> + Send + Sync>;

/// The automation interface, over the app's own database.
pub struct Automation {
    db: Database,
    clients: Arc<dyn Clients>,
    today: Box<dyn Fn() -> Date + Send + Sync>,
    events: Option<TodayEvents>,
}

impl Automation {
    pub fn new(db: Database, clients: Arc<dyn Clients>) -> Self {
        Automation {
            db,
            clients,
            today: Box::new(matters_core::local_today),
            events: None,
        }
    }

    /// Use another clock: the views depend on today's date.
    pub fn with_today(mut self, today: impl Fn() -> Date + Send + Sync + 'static) -> Self {
        self.today = Box::new(today);
        self
    }

    pub fn with_events(mut self, events: TodayEvents) -> Self {
        self.events = Some(events);
        self
    }

    /// The client presenting `token`, if it may do `needs`.
    fn authorise(&self, token: &str, needs: Permission) -> Result<Client> {
        let unknown = || Error::UnknownToken("no client has this token".into());
        let verifier = verifier_of_presented(token).ok_or_else(unknown)?;
        let client = self.clients.by_verifier(&verifier).ok_or_else(unknown)?;
        if client.revoked {
            return Err(Error::RevokedToken(format!(
                "{} was revoked; set it up again in Matters' preferences",
                client.name
            )));
        }
        if !client.permissions.allows(needs) {
            return Err(Error::PermissionDenied(format!(
                "{} does not have the {} permission",
                client.name,
                needs.name()
            )));
        }
        Ok(client)
    }

    fn tasks(&self, tasks: &[(Option<String>, &Task)]) -> Result<Vec<Item>> {
        let ids: Vec<&str> = tasks.iter().map(|(_, t)| t.id.as_str()).collect();
        let extras = Extras {
            tags: self.db.tags_of_tasks(&ids)?,
            checklists: self.db.checklist_progress(&ids)?,
        };
        Ok(tasks
            .iter()
            .map(|(section, t)| items::task(t, section.clone(), &extras))
            .collect())
    }

    fn view(&self, name: &str) -> Result<Vec<Item>> {
        let today = (self.today)();
        let q = QueryEngine::new(&self.db);
        let plain = |tasks: Vec<Task>| -> Result<Vec<Item>> {
            self.tasks(&tasks.iter().map(|t| (None, t)).collect::<Vec<_>>())
        };
        let view = match name {
            "inbox" => View::Inbox,
            "today" => View::Today,
            "upcoming" => View::Upcoming,
            "anytime" => View::Anytime,
            "someday" => View::Someday,
            "logbook" => View::Logbook,
            "trash" => View::Trash,
            other => match other.split_once(':') {
                Some(("project", id)) if !id.is_empty() => View::Project(id.into()),
                Some(("area", id)) if !id.is_empty() => View::Area(id.into()),
                _ => {
                    return Err(Error::InvalidArgument(format!(
                        "{other:?} is not a view; see docs/AUTOMATION.md"
                    )))
                }
            },
        };
        match view {
            View::Inbox => plain(q.inbox()?),
            View::Today => {
                let t = q.today(today)?;
                let mut out: Vec<Item> = self
                    .events
                    .as_ref()
                    .map(|events| events(today).iter().map(items::event).collect())
                    .unwrap_or_default();
                let rows: Vec<_> = t
                    .daytime
                    .iter()
                    .map(|t| (None, t))
                    .chain(t.evening.iter().map(|t| (Some("evening".into()), t)))
                    .collect();
                out.extend(self.tasks(&rows)?);
                Ok(out)
            }
            View::Upcoming => {
                let groups = q.upcoming(today)?;
                let rows: Vec<_> = groups
                    .iter()
                    .flat_map(|g| g.tasks.iter().map(|t| (Some(g.date.to_string()), t)))
                    .collect();
                self.tasks(&rows)
            }
            View::Anytime => plain(q.anytime(today)?),
            View::Someday => plain(q.someday_tasks()?),
            View::Logbook => plain(q.logbook(LOGBOOK_LIMIT)?),
            View::Trash => plain(q.trash()?),
            View::Project(id) => {
                self.db.get_project(&id)?.ok_or_else(|| not_found(&id))?;
                plain(q.project_tasks(&id)?)
            }
            View::Area(id) => {
                self.db.get_area(&id)?.ok_or_else(|| not_found(&id))?;
                plain(q.tasks_in(&View::Area(id), today)?)
            }
        }
    }
}

fn not_found(id: &str) -> Error {
    Error::NotFound(format!("no item has the ID {id:?}"))
}

/// A view's revision: it changes whenever anything the view returns
/// changes, and only then.
pub(crate) fn revision(items: &[Item]) -> String {
    let digest = libcrux_sha2::sha256(format!("{items:?}").as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

#[interface(interface = "io.github.thestephenromano.Matters.Automation1")]
impl Automation {
    /// A view's items and its revision. `view` is `inbox`, `today`,
    /// `upcoming`, `anytime`, `someday`, `logbook`, `trash`,
    /// `project:<id>` or `area:<id>`. Needs Read.
    fn get_view(&self, token: &str, view: &str) -> Result<(String, Vec<Item>)> {
        self.authorise(token, Permission::Read)?;
        let items = self.view(view)?;
        Ok((revision(&items), items))
    }

    /// Tasks, projects and areas whose title matches `query`, as Quick
    /// Find shows them. Needs Read.
    fn find(&self, token: &str, query: &str) -> Result<Vec<Item>> {
        self.authorise(token, Permission::Read)?;
        let found = QueryEngine::new(&self.db).quick_find(query)?;
        let mut out = self.tasks(&found.tasks.iter().map(|t| (None, t)).collect::<Vec<_>>())?;
        out.extend(found.projects.iter().map(items::project));
        out.extend(found.areas.iter().map(items::area));
        Ok(out)
    }

    /// One task, project or area by its ID. Needs Read.
    fn get(&self, token: &str, id: &str) -> Result<Item> {
        self.authorise(token, Permission::Read)?;
        if let Some(t) = self.db.get_task(id)? {
            return Ok(self.tasks(&[(None, &t)])?.remove(0));
        }
        if let Some(p) = self.db.get_project(id)? {
            return Ok(items::project(&p));
        }
        if let Some(a) = self.db.get_area(id)? {
            return Ok(items::area(&a));
        }
        Err(not_found(id))
    }

    /// Every open project that is not trashed, in sidebar order. Needs Read.
    fn list_projects(&self, token: &str) -> Result<Vec<Item>> {
        self.authorise(token, Permission::Read)?;
        Ok(self
            .db
            .list_projects()?
            .iter()
            .map(items::project)
            .collect())
    }

    /// Every area that is not trashed, in sidebar order. Needs Read.
    fn list_areas(&self, token: &str) -> Result<Vec<Item>> {
        self.authorise(token, Permission::Read)?;
        Ok(self.db.list_areas()?.iter().map(items::area).collect())
    }
}
