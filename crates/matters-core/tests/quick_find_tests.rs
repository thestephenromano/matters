//! Quick Find (#142): the query is matched as typed, so `%`, `_` and `\`
//! are plain characters, not `LIKE` wildcards.

use matters_core::{Area, Database, Project, QueryEngine, Task};

fn titles(found: &[Task]) -> Vec<&str> {
    let mut titles: Vec<&str> = found.iter().map(|t| t.title.as_str()).collect();
    titles.sort();
    titles
}

#[test]
fn wildcard_characters_are_matched_literally() {
    let db = Database::open_in_memory().unwrap();
    for (i, title) in ["50% off", "500 sheets", "a_c", "abc", "C:\\temp", "C:temp"]
        .iter()
        .enumerate()
    {
        db.create_task(&Task::new(format!("t{i}"), *title, i as f64))
            .unwrap();
    }
    let engine = QueryEngine::new(&db);
    let find = |q: &str| engine.quick_find(q).unwrap().tasks;

    assert_eq!(titles(&find("50%")), ["50% off"]);
    assert_eq!(titles(&find("a_c")), ["a_c"]);
    assert_eq!(titles(&find("\\")), ["C:\\temp"]);
    assert_eq!(titles(&find("%")), ["50% off"]);
    assert_eq!(titles(&find("_")), ["a_c"]);
    assert_eq!(
        titles(&find("50")),
        ["50% off", "500 sheets"],
        "plain text as before"
    );
    assert_eq!(
        titles(&find("ABC")),
        ["abc"],
        "ASCII letters in either case"
    );
}

#[test]
fn projects_and_areas_are_matched_literally_too() {
    let db = Database::open_in_memory().unwrap();
    db.create_project(&Project::new("p1", "Q4_plan", 0.0))
        .unwrap();
    db.create_project(&Project::new("p2", "Q4 plan", 1.0))
        .unwrap();
    db.create_area(&Area::new("a1", "100% health", 0.0))
        .unwrap();
    db.create_area(&Area::new("a2", "100 days", 1.0)).unwrap();
    let engine = QueryEngine::new(&db);

    let found = engine.quick_find("Q4_").unwrap();
    let projects: Vec<&str> = found.projects.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(projects, ["Q4_plan"]);
    let found = engine.quick_find("100%").unwrap();
    let areas: Vec<&str> = found.areas.iter().map(|a| a.title.as_str()).collect();
    assert_eq!(areas, ["100% health"]);
}
