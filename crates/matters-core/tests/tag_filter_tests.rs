//! Narrowing a list to a tag (#58): the filtered rows are the view's rows
//! that have the tag (by the same rule as `task_has_tag`), in the same
//! order, and the tags offered are those present in the view.

use jiff::civil::Date;
use matters_core::{
    Area, Database, Heading, Home, ListRow, Project, QueryEngine, Schedule, Tag, Tagged, Task, View,
};

fn today() -> Date {
    jiff::civil::date(2026, 11, 20)
}

fn tag(db: &Database, id: &str, parent: Option<&str>) {
    db.add_tag(&mut Tag::new(id, id, parent.map(str::to_string)))
        .unwrap();
}

/// Area `home` (tagged `house`) with project `p` (tagged `garden`, heading
/// `h`); tasks in Today in all three places and loose, some tagged.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.create_area(&Area::new("home", "Home", 0.0)).unwrap();
    let mut p = Project::new("p", "Garden", 0.0);
    p.area_id = Some("home".into());
    db.create_project(&p).unwrap();
    db.add_heading(&mut Heading::new("h", "p", "Later", 0.0))
        .unwrap();
    for (id, home) in [
        ("loose-errand", Home::Inbox),
        ("loose", Home::Inbox),
        ("in-area", Home::Area("home".into())),
        (
            "in-p",
            Home::Project {
                project: "p".into(),
                heading: None,
            },
        ),
    ] {
        let mut t = Task::new(id, id, 0.0);
        home.apply(&mut t);
        t.apply_schedule(&Schedule::Today { evening: false }, today());
        db.add_task(&mut t).unwrap();
    }
    tag(&db, "errands", None);
    tag(&db, "hardware", Some("errands"));
    tag(&db, "house", None);
    tag(&db, "garden", None);
    tag(&db, "unused", None);
    db.add_tag_to(Tagged::Task("loose-errand"), "hardware")
        .unwrap();
    db.add_tag_to(Tagged::Area("home"), "house").unwrap();
    db.add_tag_to(Tagged::Project("p"), "garden").unwrap();
    db
}

fn ids(rows: &[ListRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            ListRow::Task(t) => t.id.clone(),
            ListRow::Project(p) => format!("<{}>", p.id),
            ListRow::EmptyHeading(h) => format!("[{}]", h.id),
            ListRow::Event(e) => format!("({})", e.key()),
            ListRow::Preview(p) => format!("~{}", p.key()),
        })
        .collect()
}

fn rows(db: &Database, view: &View) -> Vec<ListRow> {
    let engine = QueryEngine::new(db);
    let tasks = engine.tasks_in(view, today()).unwrap();
    engine.list_rows(view, tasks, today()).unwrap()
}

#[test]
fn a_filtered_list_is_the_lists_tasks_that_have_the_tag_in_the_same_order() {
    let db = fixture();
    let engine = QueryEngine::new(&db);
    for view in [
        View::Today,
        View::Anytime,
        View::Inbox,
        View::Project("p".into()),
    ] {
        let all = rows(&db, &view);
        for tag in ["errands", "hardware", "house", "garden", "unused"] {
            let filtered = engine.filter_rows(all.clone(), tag).unwrap();
            let expected: Vec<String> = all
                .iter()
                .filter_map(|row| match row {
                    ListRow::Task(t) if engine.task_has_tag(&t.id, tag).unwrap() => {
                        Some(t.id.clone())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(ids(&filtered), expected, "{view:?} by {tag}");
        }
    }
}

#[test]
fn a_parent_tag_narrows_to_its_children_too_and_inheritance_counts() {
    let db = fixture();
    let engine = QueryEngine::new(&db);
    let today = rows(&db, &View::Today);
    assert_eq!(
        ids(&engine.filter_rows(today.clone(), "errands").unwrap()),
        ["loose-errand"]
    );
    assert_eq!(
        ids(&engine.filter_rows(today.clone(), "house").unwrap()),
        ["in-area", "in-p"],
        "the area's tag reaches its project's task"
    );
    assert_eq!(ids(&engine.filter_rows(today, "garden").unwrap()), ["in-p"]);
}

#[test]
fn project_rows_stay_when_their_project_has_the_tag_and_empty_headings_go() {
    let db = fixture();
    let engine = QueryEngine::new(&db);
    let mut due = Project::new("due", "Due", 1.0);
    due.deadline = Some(today());
    db.create_project(&due).unwrap();
    let mut other = Project::new("other", "Other", 2.0);
    other.deadline = Some(today());
    other.area_id = Some("home".into());
    db.create_project(&other).unwrap();
    db.add_tag_to(Tagged::Project("due"), "hardware").unwrap();

    let today_rows = rows(&db, &View::Today);
    assert!(ids(&today_rows).contains(&"<due>".to_string()));
    assert_eq!(
        ids(&engine.filter_rows(today_rows.clone(), "errands").unwrap()),
        ["<due>", "loose-errand"],
        "a nested tag on the project"
    );
    assert_eq!(
        ids(&engine.filter_rows(today_rows, "house").unwrap()),
        ["<other>", "in-area", "in-p"],
        "through the project's area"
    );

    let project = rows(&db, &View::Project("p".into()));
    assert!(
        ids(&project).contains(&"[h]".to_string()),
        "an empty heading, unfiltered"
    );
    assert_eq!(
        ids(&engine.filter_rows(project, "garden").unwrap()),
        ["in-p"]
    );
}

#[test]
fn the_tags_offered_are_those_in_the_list_with_their_parents() {
    let db = fixture();
    let engine = QueryEngine::new(&db);
    let names = |view: &View| -> Vec<String> {
        let tasks = engine.tasks_in(view, today()).unwrap();
        engine
            .tags_present(&tasks)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(
        names(&View::Today),
        ["errands", "hardware", "house", "garden"],
        "own, inherited and the parent of a nested one; not the unused tag"
    );
    assert_eq!(
        names(&View::Area("home".into())),
        ["house"],
        "the area's own task"
    );
    assert!(names(&View::Someday).is_empty());
}

#[test]
fn a_deeply_nested_tag_offers_every_tag_above_it() {
    let db = Database::open_in_memory().unwrap();
    tag(&db, "a", None);
    tag(&db, "b", Some("a"));
    tag(&db, "c", Some("b"));
    tag(&db, "c2", Some("b"));
    for id in ["t1", "t2"] {
        db.add_task(&mut Task::new(id, id, 0.0)).unwrap();
    }
    db.add_tag_to(Tagged::Task("t1"), "c").unwrap();
    let engine = QueryEngine::new(&db);
    let tasks = engine.tasks_in(&View::Inbox, today()).unwrap();
    let offered: Vec<String> = engine
        .tags_present(&tasks)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(
        offered,
        ["a", "b", "c"],
        "every tag above it, not its sibling"
    );
}
