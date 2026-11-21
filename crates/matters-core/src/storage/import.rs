//! Bringing an existing Matters database into sync (#41): one CRDT lineage
//! from the rows already there.
//!
//! `import_rows` turns every row into the engine commands its creation
//! would have recorded:
//! - parents first, each list in its own order;
//! - Today's order in a second pass, once every task exists;
//! - each entity's commands applied at that entity's own `updated_at`.
//!
//! The updates go into the journal and are sealed in batches under the
//! record cap, all in the caller's transaction. `verify` then rebuilds the
//! rows from the engine in that same transaction and requires them to
//! match the originals: every column, with list order compared as order.
//! Any difference aborts the migration, and the transaction never commits,
//! so the original rows remain exactly as they were.

use rusqlite::{params, Connection, Transaction};

use super::db::{Database, AREA_COLUMNS, PROJECT_COLUMNS, TASK_COLUMNS};
use super::sealing::{seal_update, Sealing};
use crate::crdt::{Command, Document};
use crate::error::{MattersError, Result};

/// Engine updates are grouped into records of at most this many bytes,
/// well under the record cap.
const BATCH_BYTES: usize = 256 * 1024;

fn rows<T>(
    tx: &Transaction<'_>,
    sql: &str,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut stmt = tx.prepare(sql)?;
    let out = stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

/// Every entity's creation, parents first: `(updated_at, commands)`.
fn creations(tx: &Transaction<'_>) -> Result<Vec<(i64, Vec<Command>)>> {
    use super::sync::{
        create_area, create_checklist_item, create_heading, create_project, create_task, tag_anchor,
    };
    let mut out = Vec::new();
    for area in rows(
        tx,
        &format!("SELECT {AREA_COLUMNS} FROM areas ORDER BY sort_order, id"),
        Database::map_area_row,
    )? {
        out.push((area.updated_at, create_area(tx, &area)?));
    }
    for project in rows(
        tx,
        &format!("SELECT {PROJECT_COLUMNS} FROM projects ORDER BY sort_order, id"),
        Database::map_project_row,
    )? {
        out.push((project.updated_at, create_project(tx, &project)?));
    }
    for heading in rows(
        tx,
        "SELECT id, project_id, title, sort_order, created_at, updated_at
         FROM headings ORDER BY sort_order, id",
        Database::map_heading_row,
    )? {
        out.push((heading.updated_at, create_heading(tx, &heading)?));
    }
    let tasks = rows(
        tx,
        &format!("SELECT {TASK_COLUMNS}, closed_by_project FROM tasks ORDER BY sort_order, id"),
        |r| {
            Ok((
                Database::map_task_row(r)?,
                r.get::<_, bool>("closed_by_project")?,
            ))
        },
    )?;
    for (task, closed_by_project) in &tasks {
        let mut commands: Vec<Command> = create_task(tx, task)?
            .into_iter()
            // Today's order is placed once every task exists.
            .filter(|c| !matches!(c, Command::PlaceInToday { .. }))
            .collect();
        if *closed_by_project {
            commands.push(Command::SetStatus {
                id: task.id.clone(),
                status: task.status,
                completed_at: task.completed_at,
                closed_by_project: true,
            });
        }
        out.push((task.updated_at, commands));
    }
    for item in rows(
        tx,
        "SELECT id, task_id, title, is_completed, sort_order, created_at, updated_at
         FROM checklist_items ORDER BY sort_order, id",
        Database::map_checklist_row,
    )? {
        out.push((item.updated_at, create_checklist_item(tx, &item)?));
    }
    // Tags, each after its parent: by depth, then in sibling order.
    let tags: Vec<(String, String, Option<String>, i64, i64)> = rows(
        tx,
        "WITH RECURSIVE depth(id, d) AS (
             SELECT id, 0 FROM tags WHERE parent_id IS NULL
             UNION ALL SELECT t.id, depth.d + 1 FROM tags t JOIN depth ON t.parent_id = depth.id)
         SELECT t.id, t.name, t.parent_id, t.created_at, t.updated_at
         FROM tags t JOIN depth ON depth.id = t.id ORDER BY depth.d, t.sort_order, t.id",
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    for (id, name, parent, created_at, updated_at) in tags {
        let at = tag_anchor(tx, &id)?;
        out.push((
            updated_at,
            vec![Command::CreateTag {
                id,
                name,
                parent,
                at,
                created_at,
            }],
        ));
    }
    for table in ["task_tags", "project_tags", "area_tags"] {
        let item = &table[..table.len() - 5];
        let links: Vec<(String, String)> = rows(
            tx,
            &format!("SELECT {item}_id, tag_id FROM {table} ORDER BY {item}_id, tag_id"),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        for (item, tag) in links {
            out.push((
                0,
                vec![Command::Link {
                    item,
                    tag,
                    present: true,
                }],
            ));
        }
    }
    // Today's order, every task now existing: each after its predecessor.
    let ordered: Vec<(String, i64)> = rows(
        tx,
        "SELECT id, updated_at FROM tasks ORDER BY today_order, id",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    for (id, updated_at) in ordered {
        // Tasks not in Today place nothing; `import_rows` skips them.
        let placed: Vec<Command> =
            super::sync::reorder(tx, "tasks", "today_order", std::slice::from_ref(&id))?
                .into_iter()
                .filter(|c| matches!(c, Command::PlaceInToday { .. }))
                .collect();
        out.push((updated_at, placed));
    }
    Ok(out)
}

/// Apply every row's creation to `doc`, journal the updates and seal them
/// as `sealing`, in batches, all in `tx`.
pub(super) fn import_rows(
    tx: &Transaction<'_>,
    doc: &mut Document,
    sealing: &Sealing,
) -> Result<()> {
    let mut since = doc.version();
    let mut pending = 0usize;
    let flush = |doc: &Document, since: &loro::VersionVector| -> Result<()> {
        let update = doc.updates_since(since)?;
        tx.execute(
            "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 1)",
            params![update],
        )?;
        seal_update(tx, sealing, tx.last_insert_rowid(), &update)?;
        Ok(())
    };
    for (at, commands) in creations(tx)? {
        if commands.is_empty() {
            continue;
        }
        doc.apply(&commands, at)?;
        pending += commands.len();
        // A cheap size check every few commands keeps batches bounded.
        if pending >= 64 && doc.updates_since(&since)?.len() >= BATCH_BYTES {
            flush(doc, &since)?;
            since = doc.version();
            pending = 0;
        }
    }
    if pending > 0 {
        flush(doc, &since)?;
    }
    Ok(())
}

/// One table's rows as text, list keys replaced by order within the list.
fn dump(conn: &Connection, table: &str, order: &str, group: &[&str]) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))?;
    let names: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let mut out = Vec::new();
    let mut ranks: std::collections::BTreeMap<String, usize> = Default::default();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let mut fields = Vec::new();
        let mut key = String::new();
        for (i, name) in names.iter().enumerate() {
            let value: rusqlite::types::Value = row.get(i)?;
            if group.contains(&name.as_str()) {
                key.push_str(&format!("{value:?}|"));
            }
            if name == "sort_order" || name == "today_order" {
                continue;
            }
            fields.push(format!("{name}={value:?}"));
        }
        let rank = ranks.entry(key).or_default();
        fields.push(format!("rank={rank}"));
        *rank += 1;
        out.push(fields.join(" "));
    }
    out.sort();
    Ok(out)
}

/// Every synced table, as `dump` gives it.
fn snapshot(conn: &Connection) -> Result<Vec<(&'static str, Vec<String>)>> {
    let tables: [(&str, &str, &[&str]); 9] = [
        ("areas", "sort_order, id", &[]),
        ("projects", "area_id, sort_order, id", &["area_id"]),
        ("headings", "project_id, sort_order, id", &["project_id"]),
        (
            "tasks",
            "area_id, project_id, heading_id, sort_order, id",
            &["area_id", "project_id", "heading_id"],
        ),
        ("checklist_items", "task_id, sort_order, id", &["task_id"]),
        ("tags", "parent_id, sort_order, id", &["parent_id"]),
        ("task_tags", "task_id, tag_id", &[]),
        ("project_tags", "project_id, tag_id", &[]),
        ("area_tags", "area_id, tag_id", &[]),
    ];
    tables
        .iter()
        .map(|(t, order, group)| Ok((*t, dump(conn, t, order, group)?)))
        .collect()
}

/// Rebuild the rows from the engine in `tx` and require them to equal
/// `before`. Returns the first difference, table by table.
pub(super) fn verify(
    tx: &Transaction<'_>,
    doc: &Document,
    before: &[(&'static str, Vec<String>)],
) -> Result<()> {
    super::sync::rebuild_rows(tx, &doc.project()?)?;
    let after = snapshot(tx)?;
    for ((table, old), (_, new)) in before.iter().zip(&after) {
        if old != new {
            let first = old
                .iter()
                .zip(new)
                .find(|(a, b)| a != b)
                .map(|(a, b)| format!("before: {a}\n after: {b}"))
                .unwrap_or_else(|| format!("{} rows before, {} after", old.len(), new.len()));
            return Err(MattersError::Validation(format!(
                "migration changed {table}; nothing was converted:\n{first}"
            )));
        }
    }
    Ok(())
}

/// The synced tables as they are now, for `verify`.
pub(super) fn before(conn: &Connection) -> Result<Vec<(&'static str, Vec<String>)>> {
    snapshot(conn)
}

impl Database {
    /// Rebuild the rows from the engine and require them to equal the rows
    /// as they are: the check a migration ends with, for tests that change
    /// rows behind the engine's back.
    #[cfg(feature = "test-support")]
    pub fn verify_rows_match_engine(&self) -> Result<()> {
        self.with_sealing(|conn, engine, _| {
            let doc = engine
                .as_ref()
                .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
            let tx = conn.transaction()?;
            let rows = before(&tx)?;
            // Never kept: the rows stay as the test left them.
            verify(&tx, doc, &rows)
        })
    }
}

/// What [`Database::import_missing_from`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Entities added from the set-aside data, as this device's writes.
    pub added: usize,
    /// Entities skipped because the collection already holds their ID: a
    /// copy of the same database, which must not fork its identities.
    pub skipped: usize,
}

impl Database {
    /// Before this device joins a collection, set its own data aside: a
    /// consistent copy beside the database, then the synced tables emptied,
    /// in one transaction. Returns the copy's path, for
    /// [`Self::import_missing_from`] once joined. Nothing is lost: the copy
    /// holds every row.
    pub fn set_aside_local_data(&self) -> Result<std::path::PathBuf> {
        self.with_sealing(|conn, engine, _| {
            if engine.is_some() {
                return Err(MattersError::Validation("sync is already on".into()));
            }
            let path = conn
                .path()
                .filter(|p| !p.is_empty())
                .ok_or_else(|| MattersError::Validation("an in-memory database".into()))?;
            let copy = format!("{path}.local-{}.db", crate::clock::now_millis());
            conn.execute("VACUUM INTO ?1", params![copy])?;
            let tx = conn.transaction()?;
            for table in [
                "task_tags",
                "project_tags",
                "area_tags",
                "checklist_items",
                "tasks",
                "headings",
                "projects",
                "areas",
                "tags",
            ] {
                tx.execute(&format!("DELETE FROM {table}"), [])?;
            }
            tx.commit()?;
            Ok(std::path::PathBuf::from(copy))
        })
    }

    /// Bring this device's set-aside data into the collection it joined:
    /// every entity of `backup` whose ID the collection does not hold, as
    /// ordinary recorded writes, parents first. Entities it already holds
    /// are skipped, never duplicated.
    pub fn import_missing_from(&self, backup: &std::path::Path) -> Result<ImportReport> {
        let source = Database::open(backup)?;
        let mut report = ImportReport::default();
        let mut count = |added: bool| {
            if added {
                report.added += 1;
            } else {
                report.skipped += 1;
            }
        };
        let read = |sql: String| {
            source.with_conn(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(ids)
            })
        };
        for id in read("SELECT id FROM areas ORDER BY sort_order, id".into())? {
            let area = source.get_area(&id)?.ok_or_else(|| gone(&id))?;
            let added = self.get_area(&id)?.is_none();
            if added {
                self.create_area(&area)?;
            }
            count(added);
        }
        for id in read("SELECT id FROM projects ORDER BY sort_order, id".into())? {
            let project = source.get_project(&id)?.ok_or_else(|| gone(&id))?;
            let added = self.get_project(&id)?.is_none();
            if added {
                self.create_project(&project)?;
            }
            count(added);
        }
        for id in read("SELECT id FROM headings ORDER BY sort_order, id".into())? {
            let heading = source.get_heading(&id)?.ok_or_else(|| gone(&id))?;
            let added = self.get_heading(&id)?.is_none();
            if added {
                self.create_heading(&heading)?;
            }
            count(added);
        }
        for id in read("SELECT id FROM tasks ORDER BY sort_order, id".into())? {
            let task = source.get_task(&id)?.ok_or_else(|| gone(&id))?;
            let added = self.get_task(&id)?.is_none();
            if added {
                self.create_task(&task)?;
                for item in source.list_checklist_items(&id)? {
                    self.create_checklist_item(&item)?;
                }
            }
            count(added);
        }
        for mut tag in source.list_tags()? {
            let added = self.get_tag(&tag.id)?.is_none();
            if added {
                self.add_tag(&mut tag)?;
            }
            count(added);
        }
        for (table, column) in [
            ("task_tags", "task"),
            ("project_tags", "project"),
            ("area_tags", "area"),
        ] {
            let pairs = source.with_conn(move |c| {
                let mut stmt = c.prepare(&format!("SELECT {column}_id, tag_id FROM {table}"))?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })?;
            for (item, tag) in pairs {
                let tagged = match column {
                    "task" => crate::domain::Tagged::Task(&item),
                    "project" => crate::domain::Tagged::Project(&item),
                    _ => crate::domain::Tagged::Area(&item),
                };
                // Adding a link the collection has is a no-op.
                self.add_tag_to(tagged, &tag)?;
            }
        }
        Ok(report)
    }
}

fn gone(id: &str) -> MattersError {
    MattersError::NotFound {
        entity: "entity",
        id: id.to_owned(),
    }
}
