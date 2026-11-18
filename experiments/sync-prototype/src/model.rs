use std::collections::{BTreeMap, BTreeSet};

use loro::{Container, LoroDoc, LoroMap, LoroText, ToJson, TreeID, TreeParentId, ValueOrContainer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{invalid, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Area,
    Project,
    Heading,
    Task,
    Checklist,
}

impl Kind {
    fn permits(self, parent: Option<Self>) -> bool {
        matches!(
            (self, parent),
            (Self::Area, None)
                | (Self::Project, None | Some(Self::Area))
                | (Self::Heading, Some(Self::Project))
                | (
                    Self::Task,
                    None | Some(Self::Area | Self::Project | Self::Heading)
                )
                | (Self::Checklist, Some(Self::Task))
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Schedule {
    #[default]
    Unscheduled,
    Someday,
    Date {
        date: String,
        evening: bool,
    },
}

#[derive(Debug, Clone)]
pub enum Command {
    Create {
        id: String,
        kind: Kind,
        parent: Option<String>,
        title: String,
    },
    Rename {
        id: String,
        title: String,
    },
    Deadline {
        id: String,
        date: Option<String>,
    },
    Schedule {
        id: String,
        schedule: Schedule,
    },
    Complete {
        id: String,
        at: Option<i64>,
    },
    InsertNotes {
        id: String,
        index: usize,
        text: String,
    },
    Move {
        id: String,
        parent: Option<String>,
        index: usize,
    },
    MoveToday {
        id: String,
        index: usize,
    },
    Trash {
        id: String,
        trashed: bool,
    },
    PurgeTrash,
    Delete {
        id: String,
    },
    Check {
        id: String,
        checked: bool,
    },
    Tag {
        id: String,
        name: String,
    },
    TagMembership {
        task: String,
        tag: String,
        present: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub kind: Kind,
    pub parent: Option<String>,
    pub rank: usize,
    pub title: String,
    pub notes: String,
    pub deadline: Option<String>,
    pub schedule: Schedule,
    pub completed_at: Option<i64>,
    pub checked: bool,
    pub explicit_trash: bool,
    pub effective_trash: bool,
    /// A surviving heading/checklist without a legal parent needs explicit recovery.
    pub recovery_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Projection {
    pub entities: BTreeMap<String, Entity>,
    /// Stable ordering of all task references; Today membership remains a query concern.
    pub today_order: Vec<String>,
    pub tombstones: BTreeSet<String>,
    pub tags: BTreeMap<String, String>,
    pub memberships: BTreeSet<(String, String)>,
}

fn fields(doc: &LoroDoc, node: TreeID) -> Result<Value> {
    Ok(doc
        .get_tree("home")
        .get_meta(node)?
        .get_deep_value()
        .to_json_value())
}

fn string(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("missing {key}")))
}

fn scalar<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<T> {
    Ok(serde_json::from_str(&string(value, key)?)?)
}

fn put<T: Serialize>(map: &LoroMap, key: &str, value: &T) -> Result<()> {
    map.insert(key, serde_json::to_string(value)?)?;
    Ok(())
}

fn lookup(doc: &LoroDoc, id: &str) -> Result<TreeID> {
    for node in doc.get_tree("home").nodes() {
        if fields(doc, node)?["id"].as_str() == Some(id) {
            return Ok(node);
        }
    }
    Err(invalid(format!("unknown entity {id}")))
}

fn live(doc: &LoroDoc, id: &str) -> Result<TreeID> {
    if doc.get_map("purged").get(id).is_some() {
        return Err(invalid("permanently deleted entity"));
    }
    lookup(doc, id)
}

fn ordered_nodes(doc: &LoroDoc) -> Vec<TreeID> {
    let tree = doc.get_tree("home");
    let mut stack = tree.children(None).unwrap_or_default();
    stack.reverse();
    let mut nodes = Vec::new();
    while let Some(node) = stack.pop() {
        nodes.push(node);
        let mut children = tree.children(node).unwrap_or_default();
        children.reverse();
        stack.extend(children);
    }
    nodes
}

pub(crate) fn project(doc: &LoroDoc) -> Result<Projection> {
    let tree = doc.get_tree("home");
    let mut result = Projection::default();
    let purged = doc.get_map("purged").get_deep_value().to_json_value();
    for (id, value) in purged
        .as_object()
        .ok_or_else(|| invalid("invalid tombstones"))?
    {
        if value != &Value::Bool(true) {
            return Err(invalid("tombstone must be true"));
        }
        result.tombstones.insert(id.clone());
    }
    let mut nodes = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for node in tree.nodes() {
        let value = fields(doc, node)?;
        let id = string(&value, "id")?;
        uuid::Uuid::parse_str(&id)?;
        if !ids.insert(id.clone()) {
            return Err(invalid("duplicate entity UUID"));
        }
        let kind: Kind = scalar(&value, "kind")?;
        nodes.insert(node, (id, kind, value));
    }
    let mut ranks: BTreeMap<Option<String>, usize> = BTreeMap::new();
    for node in ordered_nodes(doc) {
        let (id, kind, value) = &nodes[&node];
        let parent = match tree.parent(node) {
            Some(TreeParentId::Root) => None,
            Some(TreeParentId::Node(p)) => Some(p),
            _ => return Err(invalid("unexpected hidden engine node")),
        };
        if !kind.permits(parent.map(|p| nodes[&p].1)) {
            return Err(invalid("invalid typed ancestry"));
        }
        if result.tombstones.contains(id) {
            continue;
        }
        // Find the nearest surviving, type-compatible ancestor after permanent deletion.
        let mut ancestor = parent;
        let mut selected = None;
        let mut effective_trash = value["trash"].as_bool().unwrap_or(false);
        while let Some(p) = ancestor {
            let (pid, pkind, pvalue) = &nodes[&p];
            if !result.tombstones.contains(pid) {
                if selected.is_none() && kind.permits(Some(*pkind)) {
                    selected = Some(pid.clone());
                }
                effective_trash |= pvalue["trash"].as_bool().unwrap_or(false);
            }
            ancestor = match tree.parent(p) {
                Some(TreeParentId::Node(next)) => Some(next),
                _ => None,
            };
        }
        let rank = ranks.entry(selected.clone()).or_default();
        let entity = Entity {
            id: id.clone(),
            kind: *kind,
            parent: selected.clone(),
            rank: *rank,
            title: string(value, "title")?,
            notes: string(value, "notes")?,
            deadline: scalar(value, "deadline")?,
            schedule: scalar(value, "schedule")?,
            completed_at: scalar(value, "completion")?,
            checked: value["checked"].as_bool().unwrap_or(false),
            explicit_trash: value["trash"].as_bool().unwrap_or(false),
            effective_trash,
            recovery_required: selected.is_none() && !kind.permits(None),
        };
        *rank += 1;
        result.entities.insert(id.clone(), entity);
    }
    let today = doc
        .get_movable_list("today")
        .get_deep_value()
        .to_json_value();
    let mut seen = BTreeSet::new();
    for id in today
        .as_array()
        .ok_or_else(|| invalid("invalid Today order"))?
    {
        let id = id
            .as_str()
            .ok_or_else(|| invalid("invalid Today reference"))?;
        if !seen.insert(id.to_owned()) {
            return Err(invalid("duplicate Today entry"));
        }
        if let Some(entity) = result.entities.get(id) {
            if entity.kind != Kind::Task {
                return Err(invalid("Today entry is not a task"));
            }
            result.today_order.push(id.to_owned());
        }
    }
    let tags = doc.get_map("tags").get_deep_value().to_json_value();
    let mut canonical: BTreeMap<String, String> = BTreeMap::new();
    for (id, name) in tags.as_object().ok_or_else(|| invalid("invalid tags"))? {
        uuid::Uuid::parse_str(id)?;
        let name = name.as_str().ok_or_else(|| invalid("invalid tag name"))?;
        canonical
            .entry(name.to_owned())
            .and_modify(|old| {
                if id < old {
                    *old = id.clone();
                }
            })
            .or_insert(id.clone());
    }
    result.tags = canonical
        .iter()
        .map(|(name, id)| (id.clone(), name.clone()))
        .collect();
    let members = doc.get_map("memberships").get_deep_value().to_json_value();
    for (key, present) in members
        .as_object()
        .ok_or_else(|| invalid("invalid memberships"))?
    {
        let (task, tag): (String, String) = serde_json::from_str(key)?;
        if present == &Value::Bool(true)
            && result
                .entities
                .get(&task)
                .is_some_and(|e| e.kind == Kind::Task)
        {
            let name = tags[&tag].as_str().ok_or_else(|| invalid("unknown tag"))?;
            result.memberships.insert((task, canonical[name].clone()));
        }
    }
    Ok(result)
}

pub(crate) fn apply(doc: &LoroDoc, command: Command) -> Result<()> {
    let tree = doc.get_tree("home");
    // Zero jitter makes fixture traces reproducible. Loro resolves equal positions by identity.
    tree.enable_fractional_index(0);
    match command {
        Command::Create {
            id,
            kind,
            parent,
            title,
        } => {
            uuid::Uuid::parse_str(&id)?;
            if lookup(doc, &id).is_ok() || doc.get_map("purged").get(&id).is_some() {
                return Err(invalid("UUID already exists"));
            }
            let parent = parent.as_deref().map(|p| live(doc, p)).transpose()?;
            let parent_kind = parent
                .map(|p| scalar::<Kind>(&fields(doc, p)?, "kind"))
                .transpose()?;
            if !kind.permits(parent_kind) {
                return Err(invalid("invalid parent kind"));
            }
            let node = tree.create_at(parent, tree.children_num(parent).unwrap_or(0))?;
            let map = tree.get_meta(node)?;
            map.insert("id", id.as_str())?;
            put(&map, "kind", &kind)?;
            map.insert("title", title)?;
            map.insert_container("notes", LoroText::new())?;
            put(&map, "deadline", &Option::<String>::None)?;
            put(&map, "schedule", &Schedule::Unscheduled)?;
            put(&map, "completion", &Option::<i64>::None)?;
            map.insert("trash", false)?;
            map.insert("checked", false)?;
            if kind == Kind::Task {
                let today = doc.get_movable_list("today");
                today.insert(today.len(), id)?;
            }
        }
        Command::Move { id, parent, index } => {
            let node = live(doc, &id)?;
            let parent = parent.as_deref().map(|p| live(doc, p)).transpose()?;
            let kind: Kind = scalar(&fields(doc, node)?, "kind")?;
            let pk = parent
                .map(|p| scalar::<Kind>(&fields(doc, p)?, "kind"))
                .transpose()?;
            if !kind.permits(pk) {
                return Err(invalid("invalid parent kind"));
            }
            tree.mov_to(node, parent, index)?;
        }
        Command::MoveToday { id, index } => {
            live(doc, &id)?;
            let list = doc.get_movable_list("today");
            let values = list.get_deep_value().to_json_value();
            let from = values
                .as_array()
                .and_then(|v| v.iter().position(|x| x.as_str() == Some(id.as_str())))
                .ok_or_else(|| invalid("not a Today reference"))?;
            list.mov(from, index)?;
        }
        Command::PurgeTrash => {
            for (id, entity) in project(doc)?.entities {
                if entity.effective_trash {
                    doc.get_map("purged").insert(&id, true)?;
                }
            }
        }
        Command::Delete { id } => {
            let kind: Kind = scalar(&fields(doc, live(doc, &id)?)?, "kind")?;
            if !matches!(kind, Kind::Heading | Kind::Checklist) {
                return Err(invalid("use trash before purge"));
            }
            doc.get_map("purged").insert(&id, true)?;
        }
        Command::Tag { id, name } => {
            uuid::Uuid::parse_str(&id)?;
            doc.get_map("tags").insert(&id, name)?;
        }
        Command::TagMembership { task, tag, present } => {
            let kind: Kind = scalar(&fields(doc, live(doc, &task)?)?, "kind")?;
            if kind != Kind::Task || doc.get_map("tags").get(&tag).is_none() {
                return Err(invalid("invalid tag membership"));
            }
            doc.get_map("memberships")
                .insert(&serde_json::to_string(&(task, tag))?, present)?;
        }
        command => {
            let id = match &command {
                Command::Rename { id, .. }
                | Command::Deadline { id, .. }
                | Command::Schedule { id, .. }
                | Command::Complete { id, .. }
                | Command::InsertNotes { id, .. }
                | Command::Trash { id, .. }
                | Command::Check { id, .. } => id,
                _ => unreachable!(),
            };
            let node = live(doc, id)?;
            let kind: Kind = scalar(&fields(doc, node)?, "kind")?;
            let allowed = match &command {
                Command::Rename { .. } => true,
                Command::Trash { .. } => matches!(kind, Kind::Area | Kind::Project | Kind::Task),
                Command::Check { .. } => kind == Kind::Checklist,
                _ => matches!(kind, Kind::Project | Kind::Task),
            };
            if !allowed {
                return Err(invalid("command is not valid for this entity kind"));
            }
            let map = tree.get_meta(node)?;
            match command {
                Command::Rename { title, .. } => map.insert("title", title)?,
                Command::Deadline { date, .. } => put(&map, "deadline", &date)?,
                Command::Schedule { schedule, .. } => put(&map, "schedule", &schedule)?,
                Command::Complete { at, .. } => put(&map, "completion", &at)?,
                Command::Trash { trashed, .. } => map.insert("trash", trashed)?,
                Command::Check { checked, .. } => map.insert("checked", checked)?,
                Command::InsertNotes { index, text, .. } => match map.get("notes") {
                    Some(ValueOrContainer::Container(Container::Text(notes))) => {
                        notes.insert(index, &text)?
                    }
                    _ => return Err(invalid("missing notes container")),
                },
                _ => unreachable!(),
            }
        }
    }
    doc.commit();
    Ok(())
}
