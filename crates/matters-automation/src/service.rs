//! The interface object: `io.github.thestephenromano.Matters.Automation1`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use jiff::civil::Date;
use matters_core::calendar::Event;
use matters_core::{Database, MattersError, QueryEngine, Task, View};
use zbus::interface;
use zbus::OwnedVariant;

use crate::changes::{Change, Run, MAX_CHANGES, MAX_REQUEST_ID};
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
    activity: Option<Box<dyn Fn() + Send + Sync>>,
    changed: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Automation {
    pub fn new(db: Database, clients: Arc<dyn Clients>) -> Self {
        Automation {
            db,
            clients,
            today: Box::new(matters_core::local_today),
            events: None,
            activity: None,
            changed: None,
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

    /// Called at the start of every method call, refused or not: the app
    /// uses it to stay running while it is being used.
    pub fn with_activity(mut self, activity: impl Fn() + Send + Sync + 'static) -> Self {
        self.activity = Some(Box::new(activity));
        self
    }

    /// Called after `Apply` changed something: the app reloads its lists,
    /// as when sync receives changes.
    pub fn with_changed(mut self, changed: impl Fn() + Send + Sync + 'static) -> Self {
        self.changed = Some(Box::new(changed));
        self
    }

    /// The client presenting `token`, if it may do `needs`.
    fn authorise(&self, token: &str, needs: Permission) -> Result<Client> {
        let client = self.client(token)?;
        self.allows(&client, needs)?;
        Ok(client)
    }

    /// The client presenting `token`, known and not revoked.
    fn client(&self, token: &str) -> Result<Client> {
        if let Some(activity) = &self.activity {
            activity();
        }
        let unknown = || Error::UnknownToken("no client has this token".into());
        let verifier = verifier_of_presented(token).ok_or_else(unknown)?;
        let client = self.clients.by_verifier(&verifier).ok_or_else(unknown)?;
        if client.revoked {
            return Err(Error::RevokedToken(format!(
                "{} was revoked; set it up again in Matters' preferences",
                client.name
            )));
        }
        Ok(client)
    }

    fn tasks(&self, q: &QueryEngine<'_>, tasks: &[(Option<String>, &Task)]) -> Result<Vec<Item>> {
        let ids: Vec<&str> = tasks.iter().map(|(_, t)| t.id.as_str()).collect();
        let extras = Extras {
            tags: q.tags_of_tasks(&ids)?,
            checklists: q.checklist_progress(&ids)?,
        };
        Ok(tasks
            .iter()
            .map(|(section, t)| items::task(t, section.clone(), &extras))
            .collect())
    }

    /// View `name`'s items, as `q` reads the data: through the database,
    /// or inside a batch, to check its revision there.
    fn view(&self, q: &QueryEngine<'_>, name: &str) -> Result<Vec<Item>> {
        let today = (self.today)();
        let plain = |tasks: Vec<Task>| -> Result<Vec<Item>> {
            self.tasks(q, &tasks.iter().map(|t| (None, t)).collect::<Vec<_>>())
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
                out.extend(self.tasks(q, &rows)?);
                Ok(out)
            }
            View::Upcoming => {
                let groups = q.upcoming(today)?;
                let rows: Vec<_> = groups
                    .iter()
                    .flat_map(|g| g.tasks.iter().map(|t| (Some(g.date.to_string()), t)))
                    .collect();
                self.tasks(q, &rows)
            }
            View::Anytime => plain(q.anytime(today)?),
            View::Someday => plain(q.someday_tasks()?),
            View::Logbook => plain(q.logbook(LOGBOOK_LIMIT)?),
            View::Trash => plain(q.trash()?),
            View::Project(id) => {
                q.project(&id)?.ok_or_else(|| not_found(&id))?;
                plain(q.project_tasks(&id)?)
            }
            View::Area(id) => {
                q.area(&id)?.ok_or_else(|| not_found(&id))?;
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
        let items = self.view(&QueryEngine::new(&self.db), view)?;
        Ok((revision(&items), items))
    }

    /// Tasks, projects and areas whose title matches `query`, as Quick
    /// Find shows them. Needs Read.
    fn find(&self, token: &str, query: &str) -> Result<Vec<Item>> {
        self.authorise(token, Permission::Read)?;
        let q = QueryEngine::new(&self.db);
        let found = q.quick_find(query)?;
        let mut out = self.tasks(
            &q,
            &found.tasks.iter().map(|t| (None, t)).collect::<Vec<_>>(),
        )?;
        out.extend(found.projects.iter().map(items::project));
        out.extend(found.areas.iter().map(items::area));
        Ok(out)
    }

    /// One task, project or area by its ID. Needs Read.
    fn get(&self, token: &str, id: &str) -> Result<Item> {
        self.authorise(token, Permission::Read)?;
        if let Some(t) = self.db.get_task(id)? {
            return Ok(self
                .tasks(&QueryEngine::new(&self.db), &[(None, &t)])?
                .remove(0));
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

    /// Make `changes` as one batch: all of them, in order, or none.
    /// `options` may name a `view` and the `revision` the changes were
    /// built from, and a `request` ID that makes a retry safe. Returns one
    /// dictionary per change, with the `id` of anything it created.
    /// Each change needs its own permission (docs/AUTOMATION.md).
    fn apply(
        &self,
        token: &str,
        changes: Vec<HashMap<String, OwnedVariant>>,
        options: HashMap<String, OwnedVariant>,
    ) -> Result<Vec<HashMap<String, OwnedVariant>>> {
        let client = self.client(token)?;
        let ids = self.apply_as(&client, changes, options)?;
        Ok(ids
            .into_iter()
            .map(|id| {
                id.map(|id| {
                    HashMap::from([("id".to_owned(), OwnedVariant::from(zbus::Str::from(id)))])
                })
                .unwrap_or_default()
            })
            .collect())
    }
}

/// `Apply`'s options, checked.
#[derive(Debug)]
struct Options {
    /// The view and the revision it had when the changes were built.
    revision: Option<(String, String)>,
    request: Option<String>,
}

impl Options {
    fn parse(mut map: HashMap<String, OwnedVariant>) -> Result<Options> {
        let mut string = |key: &str| -> Result<Option<String>> {
            map.remove(key)
                .map(|v| {
                    String::try_from(v).map_err(|_| {
                        Error::InvalidArgument(format!("option {key:?} must be a string (s)"))
                    })
                })
                .transpose()
        };
        let view = string("view")?;
        let revision = string("revision")?;
        let request = string("request")?;
        if let Some(key) = map.keys().min() {
            return Err(Error::InvalidArgument(format!("unknown option {key:?}")));
        }
        let revision = match (view, revision) {
            (Some(view), Some(revision)) => Some((view, revision)),
            (None, None) => None,
            _ => {
                return Err(Error::InvalidArgument(
                    "options \"view\" and \"revision\" go together".into(),
                ))
            }
        };
        if let Some(request) = &request {
            if request.is_empty() || request.len() > MAX_REQUEST_ID {
                return Err(Error::InvalidArgument(format!(
                    "a request ID is 1 to {MAX_REQUEST_ID} bytes"
                )));
            }
        }
        Ok(Options { revision, request })
    }
}

impl Automation {
    /// [`Automation::apply`] for `client`, already known.
    fn apply_as(
        &self,
        client: &Client,
        changes: Vec<HashMap<String, OwnedVariant>>,
        options: HashMap<String, OwnedVariant>,
    ) -> Result<Vec<Option<String>>> {
        let options = Options::parse(options)?;
        if options.revision.is_some() {
            // Checking a revision reads the view.
            self.allows(client, Permission::Read)?;
        }
        if changes.is_empty() || changes.len() > MAX_CHANGES {
            return Err(Error::InvalidArgument(format!(
                "a batch has 1 to {MAX_CHANGES} changes; send more as several batches"
            )));
        }
        // Everything is parsed and allowed before anything changes.
        let numbered = |i: usize| move |e: Error| e.in_change(i);
        let changes: Vec<Change> = changes
            .into_iter()
            .enumerate()
            .map(|(i, c)| Change::parse(c).map_err(numbered(i)))
            .collect::<Result<_>>()?;
        for (i, change) in changes.iter().enumerate() {
            self.allows(client, change.needs()).map_err(numbered(i))?;
        }
        let areas = self.allows(client, Permission::Areas).is_ok();
        // `run` holds the database's lock for the batch: everything in it
        // reads through `w`, never `self.db`, which would deadlock (the
        // lock is not re-entrant).
        // The batch's error, kept here: `matters_core` carries only its own.
        let mut refused: Option<Error> = None;
        let mut run =
            |w: &mut matters_core::Writer<'_>| -> matters_core::Result<Vec<Option<String>>> {
                let mut keep = |e: Error| {
                    refused = Some(e);
                    MattersError::Validation("refused".into())
                };
                if let Some((view, revision)) = &options.revision {
                    let now = self.view(&w.query(), view).map_err(&mut keep)?;
                    if &crate::service::revision(&now) != revision {
                        return Err(keep(Error::StaleRevision(format!(
                            "{view} has changed since revision {revision}; read it again"
                        ))));
                    }
                }
                let mut state = Run {
                    today: (self.today)(),
                    refs: BTreeMap::new(),
                    areas,
                };
                let mut created = Vec::with_capacity(changes.len());
                for (i, change) in changes.iter().enumerate() {
                    let made = state.apply(w, change).map_err(|e| keep(e.in_change(i)))?;
                    created.push(made);
                }
                Ok(created)
            };
        let done = match &options.request {
            None => self.db.batch(&mut run),
            Some(request) => {
                // Scoped by client, so two clients never share an ID.
                let key = format!("{}/{request}", client.id);
                let digest =
                    libcrux_sha2::sha256(format!("{changes:?}{:?}", options.revision).as_bytes());
                self.db
                    .batch_once(&key, &digest, |w| run(w).map(|ids| encode(&ids)))
                    .map(|bytes| decode(&bytes))
            }
        };
        match done {
            Ok(ids) => {
                if let Some(changed) = &self.changed {
                    changed();
                }
                Ok(ids)
            }
            Err(e) => Err(refused.take().unwrap_or_else(|| e.into())),
        }
    }

    /// [`Self::authorise`]'s permission check, for a client already known,
    /// with the fixed limits: the Links client never trashes or changes
    /// areas, whatever it was allowed.
    fn allows(&self, client: &Client, needs: Permission) -> Result<()> {
        let fixed = client.id == crate::clients::LINKS_CLIENT
            && matches!(needs, Permission::Trash | Permission::Areas);
        if fixed || !client.permissions.allows(needs) {
            return Err(Error::PermissionDenied(format!(
                "{} does not have the {} permission",
                client.name,
                needs.name()
            )));
        }
        Ok(())
    }
}

/// The created IDs, one line each (empty for none), as a request keeps
/// them for a retry.
fn encode(ids: &[Option<String>]) -> Vec<u8> {
    ids.iter()
        .map(|id| id.as_deref().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn decode(bytes: &[u8]) -> Vec<Option<String>> {
    String::from_utf8_lossy(bytes)
        .split('\n')
        .map(|id| (!id.is_empty()).then(|| id.to_owned()))
        .collect()
}
