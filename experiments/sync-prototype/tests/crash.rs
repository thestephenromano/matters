use matters_sync_prototype::{Command, Fault, Kind, Replica, Result};

fn id() -> String {
    uuid::Uuid::from_u128(1).to_string()
}

#[test]
#[ignore = "invoked only as a child by abrupt_exit_recovers_sqlite"]
fn process_crash_worker() -> Result<()> {
    let path = std::env::var("MATTERS_CRASH_DB")?;
    let fault = match std::env::var("MATTERS_CRASH_POINT")?.as_str() {
        "before" => Fault::ExitBeforeCommit,
        "after" => Fault::ExitAfterCommit,
        _ => panic!("unknown crash point"),
    };
    let mut replica = Replica::open(path)?;
    replica.execute_with_fault(
        Command::Rename {
            id: id(),
            title: "durable after commit".into(),
        },
        fault,
    )?;
    panic!("fault must exit the subprocess")
}

#[test]
fn abrupt_exit_recovers_sqlite() -> Result<()> {
    for (point, code, expected_title, expected_records) in [
        ("before", 91, "original", 1),
        ("after", 92, "durable after commit", 2),
    ] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("crash.db");
        let mut replica = Replica::open(&path)?;
        replica.execute(Command::Create {
            id: id(),
            kind: Kind::Task,
            parent: None,
            title: "original".into(),
        })?;
        drop(replica);
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "process_crash_worker", "--ignored"])
            .env("MATTERS_CRASH_DB", &path)
            .env("MATTERS_CRASH_POINT", point)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let replica = Replica::open(&path)?;
        assert_eq!(replica.projection()?.entities[&id()].title, expected_title);
        assert_eq!(replica.outbox()?.len(), expected_records);
        assert_eq!(replica.journal_len()?, expected_records);
    }
    Ok(())
}

#[test]
fn actual_sql_write_failure_leaves_memory_and_disk_unchanged() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sql-failure.db");
    let mut replica = Replica::open(&path)?;
    replica.execute(Command::Create {
        id: id(),
        kind: Kind::Task,
        parent: None,
        title: "original".into(),
    })?;
    let before = replica.projection()?;
    let frontier = replica.frontier();
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch("CREATE TRIGGER deny_write BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT,'test write failure'); END;")?;
    assert!(replica
        .execute(Command::Rename {
            id: id(),
            title: "must roll back".into()
        })
        .is_err());
    assert_eq!(replica.frontier(), frontier);
    assert_eq!(replica.projection()?, before);
    assert_eq!(replica.outbox()?.len(), 1);
    connection.execute_batch("DROP TRIGGER deny_write;")?;
    replica.execute(Command::Rename {
        id: id(),
        title: "after failure".into(),
    })?;
    drop(replica);
    assert_eq!(
        Replica::open(path)?.projection()?.entities[&id()].title,
        "after failure"
    );
    Ok(())
}

#[test]
fn refuses_an_existing_application_database() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("app.db");
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch(
        "CREATE TABLE tasks(id TEXT PRIMARY KEY); INSERT INTO tasks VALUES ('keep');",
    )?;
    assert!(Replica::open(&path).is_err());
    let tables: usize = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(tables, 1);
    Ok(())
}
