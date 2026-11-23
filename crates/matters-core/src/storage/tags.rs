//! Tags (#57): creating, renaming, nesting and deleting them, and assigning
//! them to tasks, projects and areas. Matching a task by tag, with what it
//! inherits from its project and area, is a query (`crate::QueryEngine`).

use rusqlite::{params, Connection, OptionalExtension, Row};

use super::batch::Writer;
use super::Database;
use crate::domain::{Tag, Tagged, Task};
use crate::error::{MattersError, Result};

const TAG_COLUMNS: &str = "id, name, parent_id, sort_order, created_at, updated_at";

impl Database {
    /// Add a tag, last among its siblings, nested under `tag.parent_id` if
    /// set. Its name is cleaned like a task title ([`Task::clean_title`]);
    /// a blank name, a name already taken (ignoring case) or a missing
    /// parent is refused, and nothing is added.
    pub fn add_tag(&self, tag: &mut Tag) -> Result<()> {
        self.batch(|w| w.add_tag(tag))
    }

    pub fn get_tag(&self, id: &str) -> Result<Option<Tag>> {
        self.with_conn(|conn| match get_tag(conn, id) {
            Ok(tag) => Ok(Some(tag)),
            Err(MattersError::NotFound { .. }) => Ok(None),
            Err(err) => Err(err),
        })
    }

    /// Every tag, parents before their children and siblings in their
    /// order: a depth-first walk of the tree, as a tag list shows it.
    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        self.with_conn(tags_in)
    }

    /// Rename a tag; the name is cleaned and checked as for
    /// [`Self::add_tag`] (it may change only in case).
    pub fn rename_tag(&self, id: &str, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                get_tag(tx, id)?;
                check_name_free(tx, &name, Some(id))?;
                tx.execute(
                    "UPDATE tags SET name = ?1, updated_at = ?2 WHERE id = ?3",
                    params![name, now, id],
                )?;
                Ok(())
            },
            |_, ()| {
                Ok(vec![crate::crdt::Command::RenameTag {
                    id: id.into(),
                    name: name.clone(),
                }])
            },
        )
    }

    /// Nest a tag under `parent`, or make it top-level (`None`). A tag can't
    /// be nested under itself or any tag nested under it.
    pub fn set_tag_parent(&self, id: &str, parent: Option<&str>) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                get_tag(tx, id)?;
                if let Some(parent) = parent {
                    get_tag(tx, parent)?;
                    if descendants_and_self(tx, id)?.iter().any(|d| d == parent) {
                        return Err(MattersError::Validation(format!(
                            "tag '{id}' can't be nested under itself or a tag inside it"
                        )));
                    }
                }
                tx.execute(
                    "UPDATE tags SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
                    params![parent, now, id],
                )?;
                Ok(())
            },
            |tx, ()| super::sync::tags_nested(tx, &[id.to_string()]),
        )
    }

    /// Delete a tag. Every task, project and area that had it keeps
    /// everything else; only the tag goes. Tags nested under it move up to
    /// its parent.
    pub fn delete_tag(&self, id: &str) -> Result<()> {
        let now = crate::clock::now_millis();
        self.write(
            now,
            |tx| {
                let tag = get_tag(tx, id)?;
                let children = {
                    let mut stmt = tx.prepare("SELECT id FROM tags WHERE parent_id = ?1")?;
                    let ids = stmt
                        .query_map(params![id], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?;
                    ids
                };
                tx.execute(
                    "UPDATE tags SET parent_id = ?1, updated_at = ?2 WHERE parent_id = ?3",
                    params![tag.parent_id, now, id],
                )?;
                for (table, _) in [
                    Tagged::Task("").table(),
                    Tagged::Project("").table(),
                    Tagged::Area("").table(),
                ] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE tag_id = ?1"),
                        params![id],
                    )?;
                }
                tx.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
                Ok(children)
            },
            |tx, children| {
                let mut commands = vec![crate::crdt::Command::DeleteTag { id: id.into() }];
                commands.extend(super::sync::tags_nested(tx, children)?);
                Ok(commands)
            },
        )
        .map(|_| ())
    }

    /// Give `item` the tag `tag_id`; giving it a tag it has changes nothing.
    /// The item and the tag must exist.
    pub fn add_tag_to(&self, item: Tagged<'_>, tag_id: &str) -> Result<()> {
        self.batch(|w| w.add_tag_to(item, tag_id))
    }

    /// Take the tag `tag_id` off `item`; removing a tag it hasn't got
    /// changes nothing.
    pub fn remove_tag_from(&self, item: Tagged<'_>, tag_id: &str) -> Result<()> {
        self.batch(|w| w.remove_tag_from(item, tag_id))
    }

    /// The tags given to each of `task_ids` itself, in tag-list order, for
    /// a list's rows; tasks without tags are left out. One query per 500
    /// tasks.
    pub fn tags_of_tasks(
        &self,
        task_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<Tag>>> {
        let order: std::collections::HashMap<String, (usize, Tag)> = self
            .list_tags()?
            .into_iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), (i, t)))
            .collect();
        let mut found: std::collections::HashMap<String, Vec<(usize, Tag)>> =
            std::collections::HashMap::new();
        self.with_conn(|conn| {
            for chunk in task_ids.chunks(500) {
                let marks = vec!["?"; chunk.len()].join(",");
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT task_id, tag_id FROM task_tags WHERE task_id IN ({marks})"
                ))?;
                let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (task, tag) = row?;
                    if let Some(entry) = order.get(&tag) {
                        found.entry(task).or_default().push(entry.clone());
                    }
                }
            }
            Ok(())
        })?;
        Ok(found
            .into_iter()
            .map(|(task, mut tags)| {
                tags.sort_by_key(|(i, _)| *i);
                (task, tags.into_iter().map(|(_, t)| t).collect())
            })
            .collect())
    }

    /// The tag named `name`, ignoring case, if there is one.
    pub fn tag_named(&self, name: &str) -> Result<Option<Tag>> {
        let Some(name) = Task::clean_title(name) else {
            return Ok(None);
        };
        self.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {TAG_COLUMNS} FROM tags WHERE name = ?1 COLLATE NOCASE"),
                    params![name],
                    map_tag_row,
                )
                .optional()?)
        })
    }

    /// The tags given to `item` itself, in tag-list order (not what a task
    /// inherits from its project and area).
    pub fn tags_of(&self, item: Tagged<'_>) -> Result<Vec<Tag>> {
        let (table, column) = item.table();
        let (_, _, item_id) = item.item();
        let mine: Vec<String> = self.with_conn(|conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT tag_id FROM {table} WHERE {column} = ?1"))?;
            let rows = stmt.query_map(params![item_id], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
        Ok(self
            .list_tags()?
            .into_iter()
            .filter(|t| mine.contains(&t.id))
            .collect())
    }
}

impl Writer<'_> {
    /// [`Database::add_tag`], in this batch.
    pub fn add_tag(&mut self, tag: &mut Tag) -> Result<()> {
        self.op(|w| {
            tag.name = clean_name(&tag.name)?;
            let at = tag.updated_at;
            w.write(
                at,
                |tx| {
                    check_name_free(tx, &tag.name, None)?;
                    if let Some(parent) = &tag.parent_id {
                        get_tag(tx, parent)?;
                    }
                    tag.sort_order = tx.query_row(
                        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM tags",
                        [],
                        |r| r.get(0),
                    )?;
                    tx.execute(
                        &format!(
                            "INSERT INTO tags ({TAG_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                        ),
                        params![
                            tag.id,
                            tag.name,
                            tag.parent_id,
                            tag.sort_order,
                            tag.created_at,
                            tag.updated_at
                        ],
                    )?;
                    Ok(tag.clone())
                },
                |tx, tag| {
                    Ok(vec![crate::crdt::Command::CreateTag {
                        id: tag.id.clone(),
                        name: tag.name.clone(),
                        parent: tag.parent_id.clone(),
                        at: super::sync::tag_anchor(tx, &tag.id)?,
                        created_at: tag.created_at,
                    }])
                },
            )
            .map(|_| ())
        })
    }

    /// [`Database::add_tag_to`], in this batch.
    pub fn add_tag_to(&mut self, item: Tagged<'_>, tag_id: &str) -> Result<()> {
        self.op(|w| {
            let (_, _, item_id) = item.item();
            w.write(
                crate::clock::now_millis(),
                |tx| {
                    check_item(tx, item)?;
                    get_tag(tx, tag_id)?;
                    let (table, column) = item.table();
                    Ok(tx.execute(
                        &format!(
                            "INSERT OR IGNORE INTO {table} ({column}, tag_id) VALUES (?1, ?2)"
                        ),
                        params![item_id, tag_id],
                    )? > 0)
                },
                |_, added| Ok(super::sync::link(*added, item_id, tag_id, true)),
            )
            .map(|_| ())
        })
    }

    /// [`Database::remove_tag_from`], in this batch.
    pub fn remove_tag_from(&mut self, item: Tagged<'_>, tag_id: &str) -> Result<()> {
        self.op(|w| {
            let (table, column) = item.table();
            let (_, _, item_id) = item.item();
            w.write(
                crate::clock::now_millis(),
                |tx| {
                    Ok(tx.execute(
                        &format!("DELETE FROM {table} WHERE {column} = ?1 AND tag_id = ?2"),
                        params![item_id, tag_id],
                    )? > 0)
                },
                |_, removed| Ok(super::sync::link(*removed, item_id, tag_id, false)),
            )
            .map(|_| ())
        })
    }
}

/// Every tag in `conn`, in [`Database::list_tags`] order.
pub(crate) fn tags_in(conn: &Connection) -> Result<Vec<Tag>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TAG_COLUMNS} FROM tags ORDER BY sort_order ASC, name COLLATE NOCASE ASC, id ASC"
    ))?;
    let rows = stmt.query_map([], map_tag_row)?;
    let all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ordered = Vec::with_capacity(all.len());
    fn walk(all: &[Tag], parent: Option<&str>, out: &mut Vec<Tag>) {
        for tag in all.iter().filter(|t| t.parent_id.as_deref() == parent) {
            out.push(tag.clone());
            walk(all, Some(&tag.id), out);
        }
    }
    walk(&all, None, &mut ordered);
    Ok(ordered)
}

fn map_tag_row(row: &Row) -> rusqlite::Result<Tag> {
    Ok(Tag {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        sort_order: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn clean_name(raw: &str) -> Result<String> {
    Task::clean_title(raw).ok_or_else(|| MattersError::Validation("a tag needs a name".into()))
}

fn get_tag(conn: &Connection, id: &str) -> Result<Tag> {
    conn.query_row(
        &format!("SELECT {TAG_COLUMNS} FROM tags WHERE id = ?1"),
        params![id],
        map_tag_row,
    )
    .optional()?
    .ok_or_else(|| MattersError::NotFound {
        entity: "tag",
        id: id.to_string(),
    })
}

/// Refuse `name` when another tag (not `except`) has it, ignoring case.
fn check_name_free(conn: &Connection, name: &str, except: Option<&str>) -> Result<()> {
    let taken: Option<String> = conn
        .query_row(
            "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE AND id IS NOT ?2",
            params![name, except],
            |r| r.get(0),
        )
        .optional()?;
    match taken {
        Some(_) => Err(MattersError::Validation(format!(
            "there is already a tag named '{name}'"
        ))),
        None => Ok(()),
    }
}

fn check_item(conn: &Connection, item: Tagged<'_>) -> Result<()> {
    let (table, entity, id) = item.item();
    let found = conn
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE id = ?1"),
            params![id],
            |_| Ok(()),
        )
        .optional()?;
    found.ok_or_else(|| MattersError::NotFound {
        entity,
        id: id.to_string(),
    })
}

/// `id` and every tag nested under it, at any depth.
fn descendants_and_self(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
            SELECT ?1 UNION SELECT t.id FROM tags t JOIN sub ON t.parent_id = sub.id
         )
         SELECT id FROM sub",
    )?;
    let rows = stmt.query_map(params![id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
