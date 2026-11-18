//! Manual order of tasks, projects and areas (#53).
//!
//! Each list the user arranges by hand keeps a numeric key per item:
//!
//! * a task's home list (the Inbox, its area or its project) uses
//!   `tasks.sort_order`;
//! * Today keeps its own order, `tasks.today_order`, so planning the day never
//!   rearranges a project, and the reverse. This Evening is ordered within
//!   itself; moving a task past its boundary moves the task into the other
//!   section, switching This Evening on or off (decided on #53);
//! * the sidebar orders projects and areas by their `sort_order`.
//!
//! A moved item takes the midpoint of its new neighbours' keys. When the
//! list has tied keys (a list never arranged before, whose ties are broken by
//! another order), or the neighbours are too close for a midpoint, the whole
//! list is renumbered in its new displayed order, so from then on its order
//! depends only on its own keys. Only these operations (and appending new
//! items, `Database::add_*`) change order keys; saving an item never does.

use chrono::NaiveDate;
use rusqlite::params;

use crate::domain::{Task, View};
use crate::error::{MattersError, Result};
use crate::query::QueryEngine;
use crate::storage::Database;

/// Smallest gap between neighbouring keys that still takes a midpoint.
const MIN_GAP: f64 = 1e-6;

/// Move a task within `view` (Inbox, Today or a project) so it follows
/// `after`, or comes first when `after` is `None`. In Today, both tasks must
/// be in the same section (daytime or This Evening).
pub fn place_task(
    db: &Database,
    view: &View,
    today: NaiveDate,
    id: &str,
    after: Option<&str>,
) -> Result<()> {
    let (column, list) = section_of(db, view, today, id)?;
    if let Some(after) = after {
        if !list.iter().any(|t| t.id == after) {
            return Err(invalid(format!(
                "'{after}' is not next to '{id}' in this list"
            )));
        }
    }
    let keyed: Vec<(String, f64)> = list
        .iter()
        .map(|t| (t.id.clone(), key(t, column)))
        .collect();
    write(db, "tasks", column, &place(&keyed, id, after))
}

/// Move a task `step` places up (negative) or down (positive) in `view`,
/// stopping at the ends of the list. In Today, a task moved past the This
/// Evening boundary goes into the other section ([`place_in_today`]): one
/// step down from the last daytime task is the top of This Evening.
pub fn move_task(
    db: &Database,
    view: &View,
    today: NaiveDate,
    id: &str,
    step: isize,
) -> Result<()> {
    if *view == View::Today {
        return move_in_today(db, today, id, step);
    }
    let (_, list) = section_of(db, view, today, id)?;
    let from = list
        .iter()
        .position(|t| t.id == id)
        .expect("section_of includes the task");
    let to = from.saturating_add_signed(step).min(list.len() - 1);
    if to == from {
        return Ok(());
    }
    let rest: Vec<&Task> = list.iter().filter(|t| t.id != id).collect();
    let after = to.checked_sub(1).map(|i| rest[i].id.as_str());
    place_task(db, view, today, id, after)
}

fn move_in_today(db: &Database, today: NaiveDate, id: &str, step: isize) -> Result<()> {
    let list = QueryEngine::new(db).tasks_in(&View::Today, today)?;
    let Some(task) = list.iter().find(|t| t.id == id) else {
        return Err(invalid(format!("task '{id}' is not in Today")));
    };
    let evening = task.is_evening();
    let section: Vec<&str> = list
        .iter()
        .filter(|t| t.is_evening() == evening)
        .map(|t| t.id.as_str())
        .collect();
    let other: Vec<&str> = list
        .iter()
        .filter(|t| t.is_evening() != evening)
        .map(|t| t.id.as_str())
        .collect();
    let from = section
        .iter()
        .position(|t| *t == id)
        .expect("the task is in its own section") as isize;
    let target = from + step;
    let len = section.len() as isize;

    if (0..len).contains(&target) {
        if target == from {
            return Ok(());
        }
        let rest: Vec<&str> = section.iter().copied().filter(|t| *t != id).collect();
        let after = (target as usize).checked_sub(1).map(|i| rest[i]);
        return place_task(db, &View::Today, today, id, after);
    }
    if target >= len && !evening {
        // Down past the last daytime task: into This Evening, `k` places
        // below its top.
        let k = ((target - len) as usize).min(other.len());
        return place_in_today(db, today, id, true, k.checked_sub(1).map(|i| other[i]));
    }
    if target < 0 && evening {
        // Up past the first evening task: into the daytime tasks, `k` places
        // above their end.
        let k = (-target - 1) as usize;
        let at = other.len().saturating_sub(k);
        return place_in_today(db, today, id, false, at.checked_sub(1).map(|i| other[i]));
    }
    Ok(()) // already at the top of Today or the bottom of This Evening
}

/// Add a captured task (from `View::new_task`) right below `below` in
/// `view` (Enter on a selected task, SPEC §4). In Today the new task joins
/// `below`'s section, so a task added below an evening task is scheduled for
/// This Evening too. `below` must be listed in `view`, which must be arranged
/// by hand; nothing is created otherwise.
pub fn add_task_below(
    db: &Database,
    view: &View,
    today: NaiveDate,
    task: &mut Task,
    below: &str,
) -> Result<()> {
    if !view.is_reorderable() {
        return Err(invalid(format!("{} is not arranged by hand", view.title())));
    }
    let list = QueryEngine::new(db).tasks_in(view, today)?;
    let Some(anchor) = list.iter().find(|t| t.id == below) else {
        return Err(invalid(format!(
            "task '{below}' is not in {}",
            view.title()
        )));
    };
    let evening = anchor.is_evening();
    db.add_task(task)?;
    if *view == View::Today {
        place_in_today(db, today, &task.id, evening, Some(below))
    } else {
        place_task(db, view, today, &task.id, Some(below))
    }
}

/// Put a Today task in the daytime section or This Evening, right after
/// `after` within that section (first when `None`). Changing section changes
/// only This Evening: the task's date is kept, so an overdue task stays
/// overdue. A task listed in Today only because of a due deadline has no
/// start date; moving it into This Evening schedules it for `today`.
pub fn place_in_today(
    db: &Database,
    today: NaiveDate,
    id: &str,
    evening: bool,
    after: Option<&str>,
) -> Result<()> {
    let list = QueryEngine::new(db).tasks_in(&View::Today, today)?;
    let Some(task) = list.iter().find(|t| t.id == id) else {
        return Err(invalid(format!("task '{id}' is not in Today")));
    };
    if let Some(after) = after {
        if !list
            .iter()
            .any(|t| t.id == after && t.is_evening() == evening && t.id != id)
        {
            return Err(invalid(format!(
                "'{after}' is not in the section '{id}' is moving to"
            )));
        }
    }
    if task.is_evening() != evening {
        let mut task = task.clone();
        task.time_of_day = evening.then(|| "evening".to_string());
        if evening && task.start_date.is_none() {
            task.start_date = Some(today);
        }
        db.update_task(&task)?;
    }
    place_task(db, &View::Today, today, id, after)
}

/// Move a project in the sidebar so it follows `after` (or comes first).
pub fn place_project(db: &Database, id: &str, after: Option<&str>) -> Result<()> {
    let list: Vec<(String, f64)> = db
        .list_projects()?
        .into_iter()
        .map(|p| (p.id, p.sort_order))
        .collect();
    check_member(&list, id, after)?;
    write(db, "projects", "sort_order", &place(&list, id, after))
}

/// Move an area in the sidebar so it follows `after` (or comes first).
pub fn place_area(db: &Database, id: &str, after: Option<&str>) -> Result<()> {
    let list: Vec<(String, f64)> = db
        .list_areas()?
        .into_iter()
        .map(|a| (a.id, a.sort_order))
        .collect();
    check_member(&list, id, after)?;
    write(db, "areas", "sort_order", &place(&list, id, after))
}

/// The tasks `id` is ordered among in `view`, and the key column for it.
fn section_of(
    db: &Database,
    view: &View,
    today: NaiveDate,
    id: &str,
) -> Result<(&'static str, Vec<Task>)> {
    if !view.is_reorderable() {
        return Err(invalid(format!("{} is not arranged by hand", view.title())));
    }
    let mut list = QueryEngine::new(db).tasks_in(view, today)?;
    let Some(task) = list.iter().find(|t| t.id == id) else {
        return Err(invalid(format!("task '{id}' is not in {}", view.title())));
    };
    if *view != View::Today {
        return Ok(("sort_order", list));
    }
    let evening = task.is_evening();
    list.retain(|t| t.is_evening() == evening);
    Ok(("today_order", list))
}

fn key(task: &Task, column: &str) -> f64 {
    if column == "today_order" {
        task.today_order
    } else {
        task.sort_order
    }
}

fn check_member(list: &[(String, f64)], id: &str, after: Option<&str>) -> Result<()> {
    for wanted in std::iter::once(id).chain(after) {
        if !list.iter().any(|(i, _)| i == wanted) {
            return Err(invalid(format!("'{wanted}' is not in this list")));
        }
    }
    Ok(())
}

fn invalid(message: String) -> MattersError {
    MattersError::Validation(message)
}

/// The key changes that put `id` right after `after` (first when `None`) in
/// `list`, which is in displayed order. Usually one change; a renumbering of
/// the whole list when the neighbours leave no room.
fn place(list: &[(String, f64)], id: &str, after: Option<&str>) -> Vec<(String, f64)> {
    let mut rest: Vec<(String, f64)> = list.iter().filter(|(i, _)| i != id).cloned().collect();
    let at = match after {
        None => 0,
        Some(a) if a == id => return Vec::new(),
        Some(a) => rest
            .iter()
            .position(|(i, _)| i == a)
            .map_or(rest.len(), |p| p + 1),
    };
    let prev = at.checked_sub(1).map(|i| rest[i].1);
    let next = rest.get(at).map(|(_, k)| *k);
    let tied = rest.windows(2).any(|w| w[1].1 - w[0].1 <= MIN_GAP);
    let key = match (prev, next) {
        _ if tied => None,
        (None, None) => Some(1.0),
        (Some(p), None) => Some(p + 1.0),
        (None, Some(n)) => Some(n - 1.0),
        (Some(p), Some(n)) if n - p > MIN_GAP => Some(p + (n - p) / 2.0),
        _ => None,
    };
    match key {
        Some(key) => vec![(id.to_string(), key)],
        None => {
            // Renumber in the new displayed order.
            rest.insert(at, (id.to_string(), 0.0));
            rest.iter()
                .enumerate()
                .map(|(i, (item, _))| (item.clone(), (i + 1) as f64))
                .filter(|(item, new)| {
                    list.iter().find(|(i, _)| i == item).map(|(_, old)| old) != Some(new)
                })
                .collect()
        }
    }
}

/// Apply key changes in one transaction. `table` and `column` are fixed names.
fn write(db: &Database, table: &str, column: &str, changes: &[(String, f64)]) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    db.with_conn_mut(|conn| {
        let tx = conn.transaction()?;
        {
            let mut stmt =
                tx.prepare(&format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2"))?;
            for (id, key) in changes {
                stmt.execute(params![key, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::place;

    fn list(keys: &[(&str, f64)]) -> Vec<(String, f64)> {
        keys.iter().map(|(i, k)| (i.to_string(), *k)).collect()
    }

    #[test]
    fn takes_the_midpoint_or_extends_past_an_end() {
        let l = list(&[("a", 1.0), ("b", 2.0), ("c", 3.0)]);
        assert_eq!(place(&l, "c", Some("a")), list(&[("c", 1.5)]));
        assert_eq!(
            place(&l, "a", None),
            list(&[("a", 1.0)]),
            "already first: key kept"
        );
        assert_eq!(place(&l, "b", None), list(&[("b", 0.0)]));
        assert_eq!(place(&l, "a", Some("c")), list(&[("a", 4.0)]));
    }

    #[test]
    fn renumbers_when_neighbours_are_equal() {
        let l = list(&[("a", 0.0), ("b", 0.0), ("c", 0.0)]);
        // c between a and b: new order a, c, b.
        assert_eq!(
            place(&l, "c", Some("a")),
            list(&[("a", 1.0), ("c", 2.0), ("b", 3.0)])
        );
    }
}
