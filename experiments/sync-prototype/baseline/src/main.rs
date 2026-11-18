//! Optional same-host baseline. Each line measures the real app save path;
//! the prototype's test/build graph has no dependency on this executable.
use matters_core::{domain::Task, storage::Database};
use std::io::{BufRead, Write};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("database path required")?;
    let count: usize = std::env::args()
        .nth(2)
        .ok_or("task count required")?
        .parse()?;
    let database = Database::open(path)?;
    database.with_conn(|c| Ok(c.execute_batch("PRAGMA synchronous=FULL;")?))?;
    for n in 0..count {
        let mut task = Task::new(
            uuid::Uuid::from_u128(n as u128 + 1).to_string(),
            format!("task {n}"),
            n as f64,
        );
        task.notes = "x".repeat(1024);
        database.create_task(&task)?;
    }
    let mut output = std::io::stdout().lock();
    writeln!(output, "{{\"ready\":true}}")?;
    output.flush()?;
    for line in std::io::stdin().lock().lines() {
        let (id, title): (String, String) = serde_json::from_str(&line?)?;
        let start = Instant::now();
        let mut task = database.get_task(&id)?.ok_or("missing task")?;
        task.title = title;
        database.update_task(&task)?;
        let milliseconds = start.elapsed().as_secs_f64() * 1000.0;
        writeln!(output, "{milliseconds}")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../ci/sqlite_runtime.rs"]
mod sqlite_runtime;

#[test]
fn linked_sqlite_matches_approved_release() {
    let db = Database::open_in_memory().unwrap();
    db.with_conn(|conn| {
        let (version, source): (String, String) =
            conn.query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        sqlite_runtime::assert_runtime(&version, &source);
        Ok(())
    })
    .unwrap();
}
