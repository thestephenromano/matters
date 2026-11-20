//! Reading the merged document back as application rows. Every replica
//! holding the same history produces the same snapshot, whatever order its
//! updates arrived in. States only concurrency produces are repaired here,
//! deterministically, and listed in [`Snapshot::repairs`].

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use loro::{LoroDoc, LoroTree, ToJson, TreeID, TreeParentId};
use serde_json::Value;

use super::{engine, invalid, Kind, HOME, LINKS, PURGED, TAGS, TODAY};
use crate::domain::{Area, ChecklistItem, Heading, Project, Status, Tag, Task};
use crate::error::Result;

/// What a tag is given to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LinkKind {
    Task,
    Project,
    Area,
}

impl LinkKind {
    pub(super) fn of(kind: Kind) -> Option<Self> {
        match kind {
            Kind::Task => Some(LinkKind::Task),
            Kind::Project => Some(LinkKind::Project),
            Kind::Area => Some(LinkKind::Area),
            _ => None,
        }
    }
}

/// Entity UUIDs are unique across kinds, so an assignment needs only the
/// item and the tag.
pub(super) fn link_key(item: &str, tag: &str) -> String {
    format!("{item}\t{tag}")
}

/// The application's rows for the merged state. Order keys are ranks:
/// only their relative order is meaningful.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Snapshot {
    pub areas: Vec<Area>,
    pub projects: Vec<Project>,
    pub headings: Vec<Heading>,
    pub tasks: Vec<Task>,
    pub checklist_items: Vec<ChecklistItem>,
    pub tags: Vec<Tag>,
    pub links: BTreeSet<(LinkKind, String, String)>,
    /// Closed tasks that finishing their project closed.
    pub closed_by_project: BTreeSet<String>,
    /// Permanently deleted entity and tag IDs.
    pub tombstones: BTreeSet<String>,
    /// Deterministic repairs of states only concurrent edits produce.
    pub repairs: BTreeSet<String>,
    /// Headings and checklist items left without a legal parent by a
    /// concurrent permanent deletion: kept in the document, not in the rows.
    pub recovery: BTreeSet<String>,
}

/// Among nodes created concurrently under one UUID, the one with the
/// smallest tree ID keeps it.
pub(super) fn identity_owners<'a>(
    nodes: impl IntoIterator<Item = (TreeID, &'a str)>,
) -> BTreeMap<&'a str, TreeID> {
    let mut owners: BTreeMap<&str, TreeID> = BTreeMap::new();
    for (node, id) in nodes {
        owners
            .entry(id)
            .and_modify(|o| *o = (*o).min(node))
            .or_insert(node);
    }
    owners
}

/// The derived identity of a conflict copy: a UUIDv8 carrying the node's
/// tree ID (64-bit peer, 32-bit counter) in bytes the version and variant
/// bits leave alone. Unique per node, equal on every replica, and never a
/// valid created ID.
pub(super) fn conflict_copy_id(node: TreeID) -> String {
    let peer = node.peer.to_be_bytes();
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&peer[..6]);
    bytes[9..11].copy_from_slice(&peer[6..]);
    bytes[12..].copy_from_slice(&(node.counter as u32).to_be_bytes());
    uuid::Uuid::new_v8(bytes).to_string()
}

/// Depth-first, in sibling order.
fn ordered(tree: &LoroTree) -> Vec<TreeID> {
    let mut stack = tree.children(TreeParentId::Root).unwrap_or_default();
    stack.reverse();
    let mut out = Vec::new();
    while let Some(node) = stack.pop() {
        out.push(node);
        let mut children = tree.children(node).unwrap_or_default();
        children.reverse();
        stack.extend(children);
    }
    out
}

fn text(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("missing {key}")))
}

/// A register that is written only once set (the reminder, #60), and is
/// missing from documents written before it existed: missing means unset.
fn optional_json<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<Option<T>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => json(value, key),
    }
}

fn json<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<T> {
    serde_json::from_str(&text(value, key)?).map_err(engine)
}

fn int(value: &Value, key: &str) -> i64 {
    value[key].as_i64().unwrap_or(0)
}

struct Node {
    id: String,
    kind: Kind,
    value: Value,
}

pub(super) fn project(doc: &LoroDoc) -> Result<Snapshot> {
    let mut out = Snapshot::default();
    let purged = doc.get_map(PURGED).get_deep_value().to_json_value();
    for (id, flag) in purged
        .as_object()
        .ok_or_else(|| invalid("invalid tombstones"))?
    {
        if flag != &Value::Bool(true) {
            return Err(invalid("a tombstone is permanent"));
        }
        out.tombstones.insert(id.clone());
    }

    // Identities, resolving concurrent creations under one UUID.
    let tree = doc.get_tree(HOME);
    let mut raw = Vec::new();
    for node in tree.nodes() {
        let value = tree
            .get_meta(node)
            .map_err(engine)?
            .get_deep_value()
            .to_json_value();
        let id = text(&value, "id")?;
        let kind: Kind = json(&value, "kind")?;
        raw.push((node, id, kind, value));
    }
    let owners = identity_owners(raw.iter().map(|(n, id, ..)| (*n, id.as_str())));
    let mut nodes: BTreeMap<TreeID, Node> = BTreeMap::new();
    for (node, id, kind, value) in &raw {
        let id = if owners[id.as_str()] == *node {
            id.clone()
        } else {
            let copy = conflict_copy_id(*node);
            out.repairs.insert(format!("identity-copy:{copy}"));
            copy
        };
        nodes.insert(
            *node,
            Node {
                id,
                kind: *kind,
                value: value.clone(),
            },
        );
    }

    // A node hosts children when it survives: not permanently deleted and,
    // for headings and checklist items, not removed.
    let tombstones = out.tombstones.clone();
    let hosts = |node: &TreeID| -> bool {
        let n = &nodes[node];
        !tombstones.contains(&n.id) && !n.value["removed"].as_bool().unwrap_or(false)
    };

    // Projected parent of each surviving node: the nearest hosting ancestor
    // its kind may live in. Parents come before children in this walk.
    let mut parent_of: BTreeMap<TreeID, Option<TreeID>> = BTreeMap::new();
    let mut ranks: BTreeMap<Option<TreeID>, f64> = BTreeMap::new();
    let mut rank_of: BTreeMap<TreeID, f64> = BTreeMap::new();
    for node in ordered(&tree) {
        let n = &nodes[&node];
        if tombstones.contains(&n.id) {
            continue;
        }
        let mut ancestor = match tree.parent(node) {
            Some(TreeParentId::Node(p)) => Some(p),
            Some(TreeParentId::Root) => None,
            _ => return Err(invalid("unexpected engine node")),
        };
        let mut selected = None;
        while let Some(p) = ancestor {
            if hosts(&p) && n.kind.permits(Some(nodes[&p].kind)) {
                selected = Some(p);
                break;
            }
            ancestor = match tree.parent(p) {
                Some(TreeParentId::Node(next)) => Some(next),
                _ => None,
            };
        }
        if selected.is_none() && !n.kind.permits(None) {
            out.recovery.insert(n.id.clone());
            continue;
        }
        if n.value["removed"].as_bool().unwrap_or(false) {
            // Deleted heading or checklist item: its children still project.
            continue;
        }
        parent_of.insert(node, selected);
        let rank = ranks.entry(selected).or_default();
        rank_of.insert(node, *rank);
        *rank += 1.0;
    }

    let id_of = |node: Option<TreeID>| node.map(|p| nodes[&p].id.clone());
    let today = today_ranks(doc, &nodes, &parent_of, &mut out.repairs)?;
    for (node, parent) in &parent_of {
        let n = &nodes[node];
        let v = &n.value;
        let sort_order = rank_of[node];
        let (created_at, updated_at) = (int(v, "created_at"), int(v, "updated_at"));
        let parent_kind = parent.map(|p| nodes[&p].kind);
        match n.kind {
            Kind::Area => out.areas.push(Area {
                id: n.id.clone(),
                title: text(v, "title")?,
                sort_order,
                created_at,
                updated_at,
                trashed_at: json(v, "trashed_at")?,
            }),
            Kind::Project => {
                let (start_date, evening, someday): (Option<Date>, bool, bool) =
                    json(v, "schedule")?;
                let (status, completed_at, _): (Status, Option<i64>, bool) = json(v, "status")?;
                out.projects.push(Project {
                    id: n.id.clone(),
                    area_id: id_of(*parent),
                    title: text(v, "title")?,
                    notes: text(v, "notes")?,
                    start_date,
                    time_of_day: evening.then(|| "evening".to_string()),
                    is_someday: someday,
                    deadline: json(v, "deadline")?,
                    status,
                    completed_at,
                    trashed_at: json(v, "trashed_at")?,
                    sort_order,
                    created_at,
                    updated_at,
                });
            }
            Kind::Heading => out.headings.push(Heading {
                id: n.id.clone(),
                project_id: id_of(*parent).ok_or_else(|| invalid("heading without project"))?,
                title: text(v, "title")?,
                sort_order,
                created_at,
                updated_at,
            }),
            Kind::Task => {
                let (start_date, evening, someday): (Option<Date>, bool, bool) =
                    json(v, "schedule")?;
                let (status, completed_at, closed): (Status, Option<i64>, bool) =
                    json(v, "status")?;
                if closed {
                    out.closed_by_project.insert(n.id.clone());
                }
                let (area_id, project_id, heading_id) = match parent_kind {
                    None => (None, None, None),
                    Some(Kind::Area) => (id_of(*parent), None, None),
                    Some(Kind::Project) => (None, id_of(*parent), None),
                    Some(Kind::Heading) => {
                        let heading = parent.expect("a heading parent");
                        (None, id_of(parent_of[&heading]), id_of(Some(heading)))
                    }
                    Some(_) => return Err(invalid("task under a checklist item")),
                };
                let occurrence: Option<(String, Date)> = optional_json(v, "occurrence")?;
                out.tasks.push(Task {
                    id: n.id.clone(),
                    area_id,
                    project_id,
                    heading_id,
                    title: text(v, "title")?,
                    notes: text(v, "notes")?,
                    start_date,
                    time_of_day: evening.then(|| "evening".to_string()),
                    is_someday: someday,
                    deadline: json(v, "deadline")?,
                    reminder: optional_json(v, "reminder")?.flatten(),
                    repeat: optional_json(v, "repeat")?.flatten(),
                    series: occurrence.as_ref().map(|(series, _)| series.clone()),
                    occurrence: occurrence.map(|(_, date)| date),
                    status,
                    completed_at,
                    trashed_at: json(v, "trashed_at")?,
                    sort_order,
                    today_order: today.get(&n.id).copied().unwrap_or(0.0),
                    created_at,
                    updated_at,
                });
            }
            Kind::ChecklistItem => out.checklist_items.push(ChecklistItem {
                id: n.id.clone(),
                task_id: id_of(*parent).ok_or_else(|| invalid("checklist item without task"))?,
                title: text(v, "title")?,
                is_completed: v["done"].as_bool().unwrap_or(false),
                sort_order,
                created_at,
                updated_at,
            }),
        }
    }
    let surviving: BTreeMap<String, Kind> = parent_of
        .keys()
        .map(|n| (nodes[n].id.clone(), nodes[n].kind))
        .collect();
    let merged = project_tags(doc, &out.tombstones, &mut out.tags, &mut out.repairs)?;

    let links = doc.get_map(LINKS).get_deep_value().to_json_value();
    for (key, present) in links.as_object().ok_or_else(|| invalid("invalid links"))? {
        if present != &Value::Bool(true) {
            continue;
        }
        let Some((item, tag)) = key.split_once('\t') else {
            return Err(invalid("invalid link"));
        };
        let Some(tag) = merged.get(tag) else { continue };
        if let Some(kind) = surviving.get(item).and_then(|k| LinkKind::of(*k)) {
            out.links.insert((kind, item.to_owned(), tag.clone()));
        }
    }
    Ok(out)
}

/// Today's order as ranks for surviving tasks. Duplicates and non-task
/// references come only from concurrent creations under one UUID: the
/// first occurrence in the list's order wins.
fn today_ranks(
    doc: &LoroDoc,
    nodes: &BTreeMap<TreeID, Node>,
    parent_of: &BTreeMap<TreeID, Option<TreeID>>,
    repairs: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, f64>> {
    let tasks: BTreeSet<&str> = parent_of
        .keys()
        .filter(|n| nodes[n].kind == Kind::Task)
        .map(|n| nodes[n].id.as_str())
        .collect();
    let others: BTreeSet<&str> = parent_of
        .keys()
        .filter(|n| nodes[n].kind != Kind::Task)
        .map(|n| nodes[n].id.as_str())
        .collect();
    let list = doc.get_movable_list(TODAY).get_deep_value().to_json_value();
    let mut ranks = BTreeMap::new();
    for id in list
        .as_array()
        .ok_or_else(|| invalid("invalid Today order"))?
    {
        let id = id
            .as_str()
            .ok_or_else(|| invalid("invalid Today reference"))?;
        if ranks.contains_key(id) {
            repairs.insert(format!("today-duplicate:{id}"));
        } else if tasks.contains(id) {
            let rank = ranks.len() as f64;
            ranks.insert(id.to_owned(), rank);
        } else if others.contains(id) {
            repairs.insert(format!("today-non-task:{id}"));
        }
    }
    Ok(ranks)
}

/// Surviving tags, with deleted parents lifted to their nearest surviving
/// ancestor. Names are unique ignoring case: among concurrently created
/// tags sharing a name, the smallest UUID keeps it and the others merge
/// into it. Returns where each surviving tag ID now points.
fn project_tags(
    doc: &LoroDoc,
    tombstones: &BTreeSet<String>,
    tags: &mut Vec<Tag>,
    repairs: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, String>> {
    let tree = doc.get_tree(TAGS);
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for node in ordered(&tree) {
        let value = tree
            .get_meta(node)
            .map_err(engine)?
            .get_deep_value()
            .to_json_value();
        let id = text(&value, "id")?;
        if tombstones.contains(&id) || !seen.insert(id.clone()) {
            continue;
        }
        let mut ancestor = tree.parent(node);
        let mut parent = None;
        while let Some(TreeParentId::Node(p)) = ancestor {
            let pid = text(
                &tree
                    .get_meta(p)
                    .map_err(engine)?
                    .get_deep_value()
                    .to_json_value(),
                "id",
            )?;
            if !tombstones.contains(&pid) {
                parent = Some(pid);
                break;
            }
            ancestor = tree.parent(p);
        }
        found.push((id, parent, value));
    }
    // Visiting IDs in order, the first to claim a name keeps it.
    let mut by_id: Vec<&(String, Option<String>, Value)> = found.iter().collect();
    by_id.sort_by(|x, y| x.0.cmp(&y.0));
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for (id, _, value) in by_id {
        owner
            .entry(text(value, "name")?.to_lowercase())
            .or_insert_with(|| id.clone());
    }
    let mut merged = BTreeMap::new();
    for (id, _, value) in &found {
        let keeper = owner[&text(value, "name")?.to_lowercase()].clone();
        if &keeper != id {
            repairs.insert(format!("tag-merge:{id}"));
        }
        merged.insert(id.clone(), keeper);
    }
    let mut ranks: BTreeMap<Option<String>, f64> = BTreeMap::new();
    for (id, parent, value) in found {
        if merged[&id] != id {
            continue;
        }
        let parent = parent.map(|p| merged[&p].clone()).filter(|p| p != &id);
        let rank = ranks.entry(parent.clone()).or_default();
        tags.push(Tag {
            id,
            name: text(&value, "name")?,
            parent_id: parent,
            sort_order: *rank,
            created_at: int(&value, "created_at"),
            updated_at: int(&value, "updated_at"),
        });
        *rank += 1.0;
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_copy_ids_are_fixed_v8_uuids() {
        // Bytes 0-5 and 9-10 hold the peer, 12-15 the counter; 6 and 8
        // carry the version and variant.
        let node = TreeID::new(0x0123_4567_89ab_cdef, 0x1234_5678);
        assert_eq!(
            conflict_copy_id(node),
            "01234567-89ab-8000-80cd-ef0012345678"
        );
        let ids: BTreeSet<_> = [(1, 0), (1, 1), (2, 0), (u64::MAX, i32::MAX), (0, -1)]
            .into_iter()
            .map(|(peer, counter)| conflict_copy_id(TreeID::new(peer, counter)))
            .collect();
        assert_eq!(ids.len(), 5);
        for id in ids {
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 8);
        }
    }
}
