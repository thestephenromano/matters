//! Tags (#163, part of #57): created, renamed, nested and deleted; given to
//! tasks, projects and areas; a task matches the tags of its project and
//! area, and a tag matches the tags nested under it.

use matters_core::storage::schema::{
    MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5, MIGRATION_V6, MIGRATION_V7, SCHEMA_V1,
};
use matters_core::{Area, Database, Home, MattersError, Project, QueryEngine, Tag, Tagged, Task};

fn tag(db: &Database, id: &str, name: &str, parent: Option<&str>) {
    let mut t = Tag::new(id, name, parent.map(str::to_string));
    db.add_tag(&mut t).unwrap();
}

fn names(db: &Database) -> Vec<(String, Option<String>)> {
    db.list_tags()
        .unwrap()
        .into_iter()
        .map(|t| (t.name, t.parent_id))
        .collect()
}

fn validation<T: std::fmt::Debug>(result: matters_core::Result<T>) -> bool {
    matches!(result, Err(MattersError::Validation(_)))
}

#[test]
fn tags_are_created_renamed_and_listed_as_a_tree() {
    let db = Database::open_in_memory().unwrap();
    tag(&db, "home", "  Home ", None);
    tag(&db, "errands", "Errands", None);
    tag(&db, "hardware", "Hardware", Some("errands"));
    tag(&db, "garden", "Garden", Some("home"));

    assert_eq!(
        names(&db),
        [
            ("Home".into(), None),
            ("Garden".into(), Some("home".into())),
            ("Errands".into(), None),
            ("Hardware".into(), Some("errands".into())),
        ],
        "names cleaned; each tag followed by those nested under it"
    );

    db.rename_tag("home", "House").unwrap();
    db.rename_tag("errands", "ERRANDS").unwrap();
    assert_eq!(db.get_tag("home").unwrap().unwrap().name, "House");
    assert_eq!(
        db.get_tag("errands").unwrap().unwrap().name,
        "ERRANDS",
        "a change of case is fine"
    );
}

#[test]
fn a_blank_or_taken_name_or_a_missing_parent_is_refused() {
    let db = Database::open_in_memory().unwrap();
    tag(&db, "home", "Home", None);

    assert!(validation(db.add_tag(&mut Tag::new("x", "   ", None))));
    assert!(
        validation(db.add_tag(&mut Tag::new("x", "home", None))),
        "ignoring case"
    );
    assert!(matches!(
        db.add_tag(&mut Tag::new("x", "Orphan", Some("missing".into()))),
        Err(MattersError::NotFound { entity: "tag", .. })
    ));
    assert!(db.get_tag("x").unwrap().is_none());

    tag(&db, "work", "Work", None);
    assert!(validation(db.rename_tag("work", "HOME")));
    assert!(validation(db.rename_tag("work", "")));
    assert!(matches!(
        db.rename_tag("missing", "New"),
        Err(MattersError::NotFound { entity: "tag", .. })
    ));
    assert_eq!(db.get_tag("work").unwrap().unwrap().name, "Work");
}

#[test]
fn a_tag_is_nested_and_unnested_but_never_under_itself() {
    let db = Database::open_in_memory().unwrap();
    tag(&db, "a", "A", None);
    tag(&db, "b", "B", Some("a"));
    tag(&db, "c", "C", Some("b"));
    tag(&db, "d", "D", None);

    for parent in ["a", "b", "c"] {
        assert!(validation(db.set_tag_parent("a", Some(parent))), "{parent}");
    }
    assert!(matches!(
        db.set_tag_parent("a", Some("missing")),
        Err(MattersError::NotFound { entity: "tag", .. })
    ));
    assert_eq!(db.get_tag("a").unwrap().unwrap().parent_id, None);

    db.set_tag_parent("c", Some("d")).unwrap();
    assert_eq!(
        db.get_tag("c").unwrap().unwrap().parent_id.as_deref(),
        Some("d")
    );
    db.set_tag_parent("b", None).unwrap();
    assert_eq!(db.get_tag("b").unwrap().unwrap().parent_id, None);
}

/// Area `home` with a task and project `p`; `p` with a task; a loose task.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("home", "Home", 0.0)).unwrap();
    let mut p = Project::new("p", "Garden", 0.0);
    p.area_id = Some("home".into());
    db.create_project(&p).unwrap();
    db.create_project(&Project::new("q", "Shed", 1.0)).unwrap();
    for (id, home) in [
        ("in-area", Home::Area("home".into())),
        (
            "in-p",
            Home::Project {
                project: "p".into(),
                heading: None,
            },
        ),
        (
            "in-q",
            Home::Project {
                project: "q".into(),
                heading: None,
            },
        ),
        ("loose", Home::Inbox),
    ] {
        let mut t = Task::new(id, id, 0.0);
        home.apply(&mut t);
        db.add_task(&mut t).unwrap();
    }
    db
}

fn has(db: &Database, task: &str, tag: &str) -> bool {
    QueryEngine::new(db).task_has_tag(task, tag).unwrap()
}

#[test]
fn tags_are_given_to_tasks_projects_and_areas_and_taken_off() {
    let db = fixture();
    tag(&db, "t", "T", None);
    for item in [
        Tagged::Task("loose"),
        Tagged::Project("p"),
        Tagged::Area("home"),
    ] {
        db.add_tag_to(item, "t").unwrap();
        db.add_tag_to(item, "t").unwrap(); // twice changes nothing
        assert_eq!(db.tags_of(item).unwrap().len(), 1, "{item:?}");
        db.remove_tag_from(item, "t").unwrap();
        db.remove_tag_from(item, "t").unwrap();
        assert!(db.tags_of(item).unwrap().is_empty(), "{item:?}");
    }

    for item in [
        Tagged::Task("missing"),
        Tagged::Project("missing"),
        Tagged::Area("missing"),
    ] {
        assert!(
            matches!(db.add_tag_to(item, "t"), Err(MattersError::NotFound { .. })),
            "{item:?}"
        );
    }
    assert!(matches!(
        db.add_tag_to(Tagged::Task("loose"), "missing"),
        Err(MattersError::NotFound { entity: "tag", .. })
    ));
}

#[test]
fn a_task_matches_its_own_tags_and_those_of_its_project_and_area() {
    let db = fixture();
    tag(&db, "mine", "Mine", None);
    tag(&db, "proj", "Proj", None);
    tag(&db, "area", "Area", None);
    db.add_tag_to(Tagged::Task("loose"), "mine").unwrap();
    db.add_tag_to(Tagged::Project("p"), "proj").unwrap();
    db.add_tag_to(Tagged::Area("home"), "area").unwrap();

    let cases = [
        ("loose", "mine", true),
        ("loose", "proj", false),
        ("in-p", "proj", true),
        ("in-p", "area", true), // through its project's area
        ("in-area", "area", true),
        ("in-area", "proj", false),
        ("in-q", "proj", false),
        ("in-q", "area", false),
    ];
    for (task, tag, expected) in cases {
        assert_eq!(has(&db, task, tag), expected, "{task} has {tag}");
    }
    assert!(!has(&db, "missing", "mine"));
}

#[test]
fn a_tag_matches_the_tags_nested_under_it() {
    let db = fixture();
    tag(&db, "errands", "Errands", None);
    tag(&db, "hardware", "Hardware", Some("errands"));
    tag(&db, "screws", "Screws", Some("hardware"));
    db.add_tag_to(Tagged::Task("loose"), "screws").unwrap();
    db.add_tag_to(Tagged::Project("q"), "hardware").unwrap();

    for wanted in ["errands", "hardware", "screws"] {
        assert!(has(&db, "loose", wanted), "{wanted}");
    }
    assert!(has(&db, "in-q", "errands"));
    assert!(
        !has(&db, "in-q", "screws"),
        "a parent's task doesn't match a child"
    );
}

#[test]
fn deleting_a_tag_keeps_its_items_and_lifts_its_children() {
    let db = fixture();
    tag(&db, "a", "A", None);
    tag(&db, "b", "B", Some("a"));
    tag(&db, "c", "C", Some("b"));
    db.add_tag_to(Tagged::Task("loose"), "b").unwrap();
    db.add_tag_to(Tagged::Project("p"), "b").unwrap();
    db.add_tag_to(Tagged::Area("home"), "b").unwrap();
    db.add_tag_to(Tagged::Task("loose"), "c").unwrap();

    db.delete_tag("b").unwrap();

    assert!(db.get_tag("b").unwrap().is_none());
    assert!(db.get_task("loose").unwrap().is_some());
    assert!(db.get_project("p").unwrap().is_some());
    assert!(db.get_area("home").unwrap().is_some());
    assert_eq!(
        db.get_tag("c").unwrap().unwrap().parent_id.as_deref(),
        Some("a"),
        "moved up to its parent"
    );
    let left: Vec<String> = db
        .tags_of(Tagged::Task("loose"))
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(left, ["c"]);
    assert!(db.tags_of(Tagged::Project("p")).unwrap().is_empty());
    assert!(has(&db, "loose", "a"), "c is under a now");
    assert!(matches!(
        db.delete_tag("b"),
        Err(MattersError::NotFound { entity: "tag", .. })
    ));
}

#[test]
fn a_v7_database_is_migrated_and_takes_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        for migration in [
            MIGRATION_V2,
            MIGRATION_V3,
            MIGRATION_V4,
            MIGRATION_V5,
            MIGRATION_V6,
            MIGRATION_V7,
        ] {
            conn.execute_batch(migration).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO schema_migrations (version, applied_at)
             VALUES (1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0), (7, 0);
             INSERT INTO tags (id, name) VALUES ('old', 'Old');
             INSERT INTO tasks (id, title, status, sort_order, created_at, updated_at)
             VALUES ('t', 'Task', 'open', 0, 0, 0);
             INSERT INTO task_tags (task_id, tag_id) VALUES ('t', 'old');",
        )
        .unwrap();
    }
    for _ in 0..2 {
        let db = Database::open(&path).unwrap();
        let old = db.get_tag("old").unwrap().unwrap();
        assert_eq!((old.name.as_str(), old.parent_id), ("Old", None));
        assert_eq!(db.tags_of(Tagged::Task("t")).unwrap().len(), 1, "kept");
    }
    let db = Database::open(&path).unwrap();
    tag(&db, "new", "New", Some("old"));
    assert!(validation(db.add_tag(&mut Tag::new("x", "OLD", None))));
    db.create_project(&Project::new("p", "P", 0.0)).unwrap();
    db.add_tag_to(Tagged::Project("p"), "new").unwrap();
}
