//! Reading the merged document back as application rows. Every replica
//! holding the same history produces the same snapshot, whatever order its
//! updates arrived in. States only concurrency produces are repaired here,
//! deterministically, and listed in [`Snapshot::repairs`].

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use loro::{LoroDoc, LoroTree, ToJson, TreeID, TreeParentId};
use serde_json::Value;

use super::{engine, invalid, Kind, HOME, PURGED, TODAY};
use crate::domain::{Area, ChecklistItem, Heading, Project, Status, Task};
use crate::error::Result;

/// The application's rows for the merged state. Order keys are ranks:
/// only their relative order is meaningful.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Snapshot {
    pub areas: Vec<Area>,
    pub projects: Vec<Project>,
    pub headings: Vec<Heading>,
    pub tasks: Vec<Task>,
    pub checklist_items: Vec<ChecklistItem>,
    /// Closed tasks that finishing their project closed.
    pub closed_by_project: BTreeSet<String>,
    /// Permanently deleted entity IDs.
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
