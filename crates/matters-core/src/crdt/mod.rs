//! The engine document behind sync ([ADR 0001], #35): areas, projects,
//! headings, tasks and checklist items in one ordered Loro tree, Today's
//! order in a movable list, tags in a second tree, and permanent deletion
//! as tombstones. [`Document::apply`] turns typed commands into engine
//! operations; [`Document::project`] reads the merged state back as the
//! application's rows.
//!
//! Not yet wired into [`crate::Database`]: the next slice of #35 records
//! each typed write here in the same SQLite transaction.
//!
//! [ADR 0001]: https://github.com/thestephenromano/matters/blob/main/docs/decisions/0001-crdt-adoption.md

mod projection;

use std::collections::BTreeMap;

use jiff::civil::Date;
use loro::{
    Container, ExportMode, LoroDoc, LoroMap, LoroText, LoroTree, TreeID, TreeParentId,
    ValueOrContainer, VersionVector,
};
use serde::{Deserialize, Serialize};

use crate::domain::Status;
use crate::error::{MattersError, Result};

pub use projection::{LinkKind, Snapshot};

/// What an entity in the home tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Area,
    Project,
    Heading,
    Task,
    ChecklistItem,
}

impl Kind {
    /// The parents SPEC §2 allows: areas at the top; projects at the top
    /// or in an area; headings in a project; tasks anywhere but under a
    /// checklist; checklist items in a task.
    pub fn permits(self, parent: Option<Kind>) -> bool {
        matches!(
            (self, parent),
            (Kind::Area, None)
                | (Kind::Project, None | Some(Kind::Area))
                | (Kind::Heading, Some(Kind::Project))
                | (
                    Kind::Task,
                    None | Some(Kind::Area | Kind::Project | Kind::Heading)
                )
                | (Kind::ChecklistItem, Some(Kind::Task))
        )
    }

    fn has_notes(self) -> bool {
        matches!(self, Kind::Project | Kind::Task)
    }
}

/// Where an entity goes among its new siblings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    First,
    /// Right after this sibling.
    After(String),
    Last,
}

/// One intent, as the typed `Database` operations express them. A batch of
/// commands is one engine commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Create {
        id: String,
        kind: Kind,
        parent: Option<String>,
        at: Anchor,
        title: String,
        created_at: i64,
    },
    /// File an entity under `parent` (`None`: the top level) at `at`;
    /// also how siblings are reordered.
    Move {
        id: String,
        parent: Option<String>,
        at: Anchor,
    },
    Rename {
        id: String,
        title: String,
    },
    /// Replace the notes; the engine records the smallest text edit.
    SetNotes {
        id: String,
        notes: String,
    },
    /// The When: start date, evening and Someday change together.
    SetSchedule {
        id: String,
        start_date: Option<Date>,
        evening: bool,
        someday: bool,
    },
    SetDeadline {
        id: String,
        deadline: Option<Date>,
    },
    /// Status, finish time and the "closed by its project" mark change
    /// together, so they can never disagree.
    SetStatus {
        id: String,
        status: Status,
        completed_at: Option<i64>,
        closed_by_project: bool,
    },
    SetTrashed {
        id: String,
        trashed_at: Option<i64>,
    },
    /// Delete a heading or checklist item, or undo that.
    SetRemoved {
        id: String,
        removed: bool,
    },
    SetDone {
        id: String,
        done: bool,
    },
    PlaceInToday {
        id: String,
        at: Anchor,
    },
    /// Empty Trash: tombstone exactly the entities the issuing replica saw.
    Purge {
        ids: Vec<String>,
    },
    CreateTag {
        id: String,
        name: String,
        parent: Option<String>,
        at: Anchor,
        created_at: i64,
    },
    RenameTag {
        id: String,
        name: String,
    },
    MoveTag {
        id: String,
        parent: Option<String>,
        at: Anchor,
    },
    /// Delete a tag; tags nested under it move up to its parent.
    DeleteTag {
        id: String,
    },
    /// Give `item` (a task, project or area) the tag, or take it off.
    Link {
        item: String,
        tag: String,
        present: bool,
    },
}

const HOME: &str = "home";
const TAGS: &str = "tags";
const LINKS: &str = "links";
const TODAY: &str = "today";
const PURGED: &str = "purged";

pub(crate) fn invalid(message: impl Into<String>) -> MattersError {
    MattersError::Validation(message.into())
}

pub(crate) fn engine(error: impl std::fmt::Display) -> MattersError {
    MattersError::Engine(error.to_string())
}

/// Entity UUIDs in canonical text form; version 8 is reserved for the
/// derived identities of conflict copies ([`projection`]).
fn check_uuid(id: &str) -> Result<()> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| invalid(format!("'{id}' is not a UUID")))?;
    if parsed.get_version_num() == 8 || parsed.to_string() != id {
        return Err(invalid(format!("'{id}' is not an entity UUID")));
    }
    Ok(())
}

fn put<T: Serialize>(map: &LoroMap, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value).map_err(engine)?;
    map.insert(key, json).map_err(engine)?;
    Ok(())
}

fn scalar_string(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(ValueOrContainer::Value(loro::LoroValue::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

/// The document, with an index from entity and tag UUIDs to tree nodes.
pub struct Document {
    doc: LoroDoc,
    entities: BTreeMap<String, (TreeID, Kind)>,
    tags: BTreeMap<String, TreeID>,
}

impl Document {
    /// An empty document whose edits carry `peer` as their engine actor.
    pub fn new(peer: u64) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).map_err(engine)?;
        Ok(Self {
            doc,
            entities: BTreeMap::new(),
            tags: BTreeMap::new(),
        })
    }

    /// A document from exported bytes (a snapshot or updates), editing as
    /// `peer`.
    pub fn import(bytes: &[u8], peer: u64) -> Result<Self> {
        let mut document = Self::new(peer)?;
        document.merge(bytes)?;
        Ok(document)
    }

    /// A document rebuilt from stored updates, in order, editing as
    /// `peer`.
    pub fn from_updates<'a>(
        updates: impl IntoIterator<Item = &'a [u8]>,
        peer: u64,
    ) -> Result<Self> {
        let mut document = Self::new(peer)?;
        for bytes in updates {
            document.doc.import(bytes).map_err(engine)?;
        }
        document.reindex()?;
        Ok(document)
    }

    /// Import updates or a snapshot from another replica, then rebuild the
    /// index.
    pub fn merge(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes).map_err(engine)?;
        self.reindex()
    }

    fn reindex(&mut self) -> Result<()> {
        self.entities.clear();
        let tree = self.doc.get_tree(HOME);
        let mut raw = Vec::new();
        for node in tree.nodes() {
            let meta = tree.get_meta(node).map_err(engine)?;
            let id = scalar_string(&meta, "id").ok_or_else(|| invalid("node without id"))?;
            let kind: Kind = serde_json::from_str(
                &scalar_string(&meta, "kind").ok_or_else(|| invalid("node without kind"))?,
            )
            .map_err(engine)?;
            raw.push((node, id, kind));
        }
        let owners: BTreeMap<String, TreeID> =
            projection::identity_owners(raw.iter().map(|(n, id, _)| (*n, id.as_str())))
                .into_iter()
                .map(|(id, node)| (id.to_owned(), node))
                .collect();
        for (node, id, kind) in raw {
            let key = if owners[&id] == node {
                id
            } else {
                projection::conflict_copy_id(node)
            };
            self.entities.insert(key, (node, kind));
        }
        self.tags.clear();
        let tags = self.doc.get_tree(TAGS);
        for node in tags.nodes() {
            let meta = tags.get_meta(node).map_err(engine)?;
            if let Some(id) = scalar_string(&meta, "id") {
                self.tags.entry(id).or_insert(node);
            }
        }
        Ok(())
    }

    /// The engine actor this document's edits carry.
    pub fn peer(&self) -> u64 {
        self.doc.peer_id()
    }

    /// The version this document has seen.
    pub fn version(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    /// Everything the document holds, for a new replica or a checkpoint.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.doc.export(ExportMode::Snapshot).map_err(engine)
    }

    /// The updates made after `since`, for another replica.
    pub fn updates_since(&self, since: &VersionVector) -> Result<Vec<u8>> {
        self.doc.export(ExportMode::updates(since)).map_err(engine)
    }

    /// The merged state as application rows.
    pub fn project(&self) -> Result<Snapshot> {
        projection::project(&self.doc)
    }

    fn purged(&self, id: &str) -> bool {
        self.doc.get_map(PURGED).get(id).is_some()
    }

    fn entity(&self, id: &str) -> Result<(TreeID, Kind)> {
        if self.purged(id) {
            return Err(invalid(format!("'{id}' was permanently deleted")));
        }
        self.entities
            .get(id)
            .copied()
            .ok_or_else(|| MattersError::NotFound {
                entity: "entity",
                id: id.to_string(),
            })
    }

    fn tag(&self, id: &str) -> Result<TreeID> {
        if self.purged(id) {
            return Err(invalid(format!("tag '{id}' was deleted")));
        }
        self.tags
            .get(id)
            .copied()
            .ok_or_else(|| MattersError::NotFound {
                entity: "tag",
                id: id.to_string(),
            })
    }

    fn meta(&self, node: TreeID) -> Result<LoroMap> {
        self.doc.get_tree(HOME).get_meta(node).map_err(engine)
    }

    /// Apply `commands` as one engine commit at time `at` (milliseconds),
    /// which becomes each touched entity's `updated_at`. Commands are
    /// checked as they apply; a refused command leaves the earlier ones of
    /// the batch applied but uncommitted, so callers discard the document on
    /// error and reload it from storage.
    pub fn apply(&mut self, commands: &[Command], at: i64) -> Result<()> {
        for command in commands {
            self.apply_one(command, at)?;
        }
        self.doc.commit();
        Ok(())
    }

    fn apply_one(&mut self, command: &Command, at: i64) -> Result<()> {
        let home = self.doc.get_tree(HOME);
        match command {
            Command::Create {
                id,
                kind,
                parent,
                at: anchor,
                title,
                created_at,
            } => {
                check_uuid(id)?;
                if self.entities.contains_key(id) || self.tags.contains_key(id) || self.purged(id) {
                    return Err(invalid(format!("'{id}' already exists")));
                }
                let parent = self.parent(*kind, parent.as_deref())?;
                let node = home
                    .create_at(parent, home.children_num(parent).unwrap_or(0))
                    .map_err(engine)?;
                self.entities.insert(id.clone(), (node, *kind));
                let meta = self.meta(node)?;
                meta.insert("id", id.as_str()).map_err(engine)?;
                put(&meta, "kind", kind)?;
                meta.insert("title", title.as_str()).map_err(engine)?;
                if kind.has_notes() {
                    meta.insert_container("notes", LoroText::new())
                        .map_err(engine)?;
                    put(&meta, "schedule", &(None::<Date>, false, false))?;
                    put(&meta, "deadline", &None::<Date>)?;
                    put(&meta, "status", &(Status::Open, None::<i64>, false))?;
                }
                put(&meta, "trashed_at", &None::<i64>)?;
                meta.insert("removed", false).map_err(engine)?;
                meta.insert("done", false).map_err(engine)?;
                meta.insert("created_at", *created_at).map_err(engine)?;
                meta.insert("updated_at", at).map_err(engine)?;
                self.place(&home, node, parent, anchor, false)?;
                if *kind == Kind::Task {
                    let today = self.doc.get_movable_list(TODAY);
                    today.insert(today.len(), id.as_str()).map_err(engine)?;
                }
                return Ok(());
            }
            Command::Move {
                id,
                parent,
                at: anchor,
            } => {
                let (node, kind) = self.entity(id)?;
                let parent = self.parent(kind, parent.as_deref())?;
                if Some(node) == parent {
                    return Err(invalid("an entity can't contain itself"));
                }
                self.place(&home, node, parent, anchor, false)?;
                self.touch(node, at)?;
                return Ok(());
            }
            Command::PlaceInToday { id, at: anchor } => {
                let (_, kind) = self.entity(id)?;
                if kind != Kind::Task {
                    return Err(invalid("only tasks are in Today"));
                }
                return self.place_in_today(id, anchor);
            }
            Command::Purge { ids } => {
                let purged = self.doc.get_map(PURGED);
                for id in ids {
                    self.entity(id)?;
                    purged.insert(id, true).map_err(engine)?;
                }
                return Ok(());
            }
            Command::CreateTag {
                id,
                name,
                parent,
                at: anchor,
                created_at,
            } => {
                check_uuid(id)?;
                if self.tags.contains_key(id) || self.entities.contains_key(id) || self.purged(id) {
                    return Err(invalid(format!("'{id}' already exists")));
                }
                let tags = self.doc.get_tree(TAGS);
                let parent = parent.as_deref().map(|p| self.tag(p)).transpose()?;
                let node = tags
                    .create_at(parent, tags.children_num(parent).unwrap_or(0))
                    .map_err(engine)?;
                self.tags.insert(id.clone(), node);
                let meta = tags.get_meta(node).map_err(engine)?;
                meta.insert("id", id.as_str()).map_err(engine)?;
                meta.insert("name", name.as_str()).map_err(engine)?;
                meta.insert("created_at", *created_at).map_err(engine)?;
                meta.insert("updated_at", at).map_err(engine)?;
                self.place(&tags, node, parent, anchor, true)?;
                return Ok(());
            }
            Command::RenameTag { id, name } => {
                let node = self.tag(id)?;
                let meta = self.doc.get_tree(TAGS).get_meta(node).map_err(engine)?;
                meta.insert("name", name.as_str()).map_err(engine)?;
                meta.insert("updated_at", at).map_err(engine)?;
                return Ok(());
            }
            Command::MoveTag {
                id,
                parent,
                at: anchor,
            } => {
                let node = self.tag(id)?;
                let tags = self.doc.get_tree(TAGS);
                let parent = parent.as_deref().map(|p| self.tag(p)).transpose()?;
                self.place(&tags, node, parent, anchor, true)?;
                return Ok(());
            }
            Command::DeleteTag { id } => {
                self.tag(id)?;
                self.doc.get_map(PURGED).insert(id, true).map_err(engine)?;
                return Ok(());
            }
            Command::Link { item, tag, present } => {
                let (_, kind) = self.entity(item)?;
                if LinkKind::of(kind).is_none() {
                    return Err(invalid("only tasks, projects and areas take tags"));
                }
                self.tag(tag)?;
                self.doc
                    .get_map(LINKS)
                    .insert(&projection::link_key(item, tag), *present)
                    .map_err(engine)?;
                return Ok(());
            }
            _ => {}
        }
        // Field registers on one entity.
        let (id, allowed): (&String, fn(Kind) -> bool) = match command {
            Command::Rename { id, .. } => (id, |_| true),
            Command::SetNotes { id, .. }
            | Command::SetSchedule { id, .. }
            | Command::SetDeadline { id, .. }
            | Command::SetStatus { id, .. } => (id, Kind::has_notes),
            Command::SetTrashed { id, .. } => {
                (id, |k| matches!(k, Kind::Area | Kind::Project | Kind::Task))
            }
            Command::SetRemoved { id, .. } => {
                (id, |k| matches!(k, Kind::Heading | Kind::ChecklistItem))
            }
            Command::SetDone { id, .. } => (id, |k| k == Kind::ChecklistItem),
            _ => unreachable!("handled above"),
        };
        let (node, kind) = self.entity(id)?;
        if !allowed(kind) {
            return Err(invalid(format!("not a command for a {kind:?}")));
        }
        let meta = self.meta(node)?;
        match command {
            Command::Rename { title, .. } => {
                meta.insert("title", title.as_str()).map_err(engine)?;
            }
            Command::SetNotes { notes, .. } => match meta.get("notes") {
                Some(ValueOrContainer::Container(Container::Text(text))) => {
                    text.update(notes, Default::default()).map_err(engine)?;
                }
                _ => return Err(invalid("missing notes")),
            },
            Command::SetSchedule {
                start_date,
                evening,
                someday,
                ..
            } => put(&meta, "schedule", &(start_date, evening, someday))?,
            Command::SetDeadline { deadline, .. } => put(&meta, "deadline", deadline)?,
            Command::SetStatus {
                status,
                completed_at,
                closed_by_project,
                ..
            } => {
                let completed_at = if *status == Status::Open {
                    None
                } else {
                    *completed_at
                };
                let closed = *closed_by_project && *status != Status::Open;
                put(&meta, "status", &(status, completed_at, closed))?;
            }
            Command::SetTrashed { trashed_at, .. } => put(&meta, "trashed_at", trashed_at)?,
            Command::SetRemoved { removed, .. } => {
                meta.insert("removed", *removed).map_err(engine)?;
            }
            Command::SetDone { done, .. } => {
                meta.insert("done", *done).map_err(engine)?;
            }
            _ => unreachable!("handled above"),
        }
        meta.insert("updated_at", at).map_err(engine)?;
        Ok(())
    }

    fn touch(&self, node: TreeID, at: i64) -> Result<()> {
        self.meta(node)?.insert("updated_at", at).map_err(engine)?;
        Ok(())
    }

    /// The parent node for an entity of `kind` under `parent`, checked
    /// against SPEC §2.
    fn parent(&self, kind: Kind, parent: Option<&str>) -> Result<Option<TreeID>> {
        let parent = parent.map(|p| self.entity(p)).transpose()?;
        if !kind.permits(parent.map(|(_, k)| k)) {
            return Err(invalid(format!("a {kind:?} can't go there")));
        }
        Ok(parent.map(|(node, _)| node))
    }

    /// Put `node` under `parent` at `anchor`. An anchor must be a sibling
    /// under that parent, in the same tree.
    fn place(
        &self,
        tree: &LoroTree,
        node: TreeID,
        parent: Option<TreeID>,
        anchor: &Anchor,
        is_tags: bool,
    ) -> Result<()> {
        let parent_id: TreeParentId = parent.into();
        match anchor {
            Anchor::First => tree.mov_to(node, parent, 0),
            Anchor::Last => {
                let n = tree.children_num(parent_id).unwrap_or(0);
                let already = tree.parent(node) == Some(parent_id);
                tree.mov_to(node, parent, if already { n.saturating_sub(1) } else { n })
            }
            Anchor::After(sibling) => {
                let sibling = if is_tags {
                    self.tag(sibling)?
                } else {
                    self.entity(sibling)?.0
                };
                if sibling == node {
                    return Err(invalid("an entity can't follow itself"));
                }
                if tree.parent(sibling) != Some(parent_id) {
                    return Err(invalid("the anchor is not a sibling there"));
                }
                tree.mov_after(node, sibling)
            }
        }
        .map_err(engine)
    }

    fn place_in_today(&self, id: &str, anchor: &Anchor) -> Result<()> {
        let list = self.doc.get_movable_list(TODAY);
        let raw: Vec<String> = list
            .get_value()
            .into_list()
            .map_err(|_| invalid("invalid Today order"))?
            .iter()
            .map(|v| v.as_string().map(|s| s.to_string()).unwrap_or_default())
            .collect();
        let from = raw
            .iter()
            .position(|v| v == id)
            .ok_or_else(|| invalid("not in Today's order"))?;
        let rest: Vec<&String> = raw.iter().filter(|v| v.as_str() != id).collect();
        let to = match anchor {
            Anchor::First => 0,
            Anchor::Last => rest.len(),
            Anchor::After(sibling) => {
                rest.iter()
                    .position(|v| v.as_str() == sibling)
                    .ok_or_else(|| invalid("the anchor is not in Today's order"))?
                    + 1
            }
        };
        if from != to {
            list.mov(from, to).map_err(engine)?;
        }
        Ok(())
    }
}
