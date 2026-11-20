//! Arranging the sidebar by dragging (#164): areas among themselves, and a
//! project within its area, into another area at a place, or out of every
//! area.

use matters_core::ordering::{place_area, place_project_in_area};
use matters_core::{Area, Database, MattersError, Project};

/// Areas `a1`, `a2`; projects `x`, `y` with no area, `p`, `q`, `r` in
/// `a1`, `s` in `a2`.
fn fixture() -> Database {
    let db = Database::open_in_memory().unwrap();
    for (i, id) in ["a1", "a2"].into_iter().enumerate() {
        db.create_area(&Area::new(id, id, i as f64)).unwrap();
    }
    for (i, (id, area)) in [
        ("x", None),
        ("p", Some("a1")),
        ("y", None),
        ("q", Some("a1")),
        ("s", Some("a2")),
        ("r", Some("a1")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut p = Project::new(id, id, i as f64);
        p.area_id = area.map(str::to_string);
        db.create_project(&p).unwrap();
    }
    db
}

/// The sidebar: the projects without an area, then each area's.
fn sidebar(db: &Database) -> Vec<String> {
    let projects = db.list_projects().unwrap();
    let mut out: Vec<String> = projects
        .iter()
        .filter(|p| p.area_id.is_none())
        .map(|p| p.id.clone())
        .collect();
    for area in db.list_areas().unwrap() {
        out.push(format!("[{}]", area.id));
        out.extend(
            projects
                .iter()
                .filter(|p| p.area_id.as_deref() == Some(area.id.as_str()))
                .map(|p| p.id.clone()),
        );
    }
    out
}

#[test]
fn a_project_is_placed_within_its_area() {
    let db = fixture();
    place_project_in_area(&db, "r", Some("a1"), None).unwrap();
    assert_eq!(sidebar(&db), ["x", "y", "[a1]", "r", "p", "q", "[a2]", "s"]);
    place_project_in_area(&db, "r", Some("a1"), Some("q")).unwrap();
    assert_eq!(sidebar(&db), ["x", "y", "[a1]", "p", "q", "r", "[a2]", "s"]);
}

#[test]
fn a_project_is_moved_to_a_place_in_another_area_or_out_of_every_area() {
    let db = fixture();
    place_project_in_area(&db, "q", Some("a2"), None).unwrap();
    assert_eq!(sidebar(&db), ["x", "y", "[a1]", "p", "r", "[a2]", "q", "s"]);
    place_project_in_area(&db, "p", Some("a2"), Some("q")).unwrap();
    assert_eq!(sidebar(&db), ["x", "y", "[a1]", "r", "[a2]", "q", "p", "s"]);
    place_project_in_area(&db, "s", None, Some("x")).unwrap();
    assert_eq!(sidebar(&db), ["x", "s", "y", "[a1]", "r", "[a2]", "q", "p"]);
    place_project_in_area(&db, "y", Some("a1"), Some("r")).unwrap();
    assert_eq!(
        sidebar(&db),
        ["x", "s", "[a1]", "r", "y", "[a2]", "q", "p"],
        "at the end"
    );
}

#[test]
fn a_misplaced_move_is_refused_and_changes_nothing() {
    let db = fixture();
    let before = sidebar(&db);
    db.create_area(&Area::new("gone", "gone", 9.0)).unwrap();
    db.trash_area("gone").unwrap();
    assert!(matches!(
        place_project_in_area(&db, "p", Some("gone"), None),
        Err(MattersError::NotFound { entity: "area", .. })
    ));
    assert!(matches!(
        place_project_in_area(&db, "missing", None, None),
        Err(MattersError::NotFound {
            entity: "project",
            ..
        })
    ));
    assert!(
        place_project_in_area(&db, "p", Some("a2"), Some("q")).is_err(),
        "q is not in a2"
    );
    assert_eq!(sidebar(&db), before);
    assert_eq!(
        db.get_project("p").unwrap().unwrap().area_id.as_deref(),
        Some("a1")
    );
}

#[test]
fn areas_are_placed_among_themselves() {
    let db = fixture();
    db.create_area(&Area::new("a3", "a3", 2.0)).unwrap();
    place_area(&db, "a3", None).unwrap();
    let order: Vec<String> = db.list_areas().unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(order, ["a3", "a1", "a2"]);
    place_area(&db, "a3", Some("a2")).unwrap();
    let order: Vec<String> = db.list_areas().unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(order, ["a1", "a2", "a3"]);
}

#[test]
fn a_project_dropped_after_another_areas_last_project_goes_after_it() {
    // q sorts before s by id, so only its key can put it after s.
    let db = fixture();
    place_project_in_area(&db, "q", Some("a2"), Some("s")).unwrap();
    assert_eq!(sidebar(&db), ["x", "y", "[a1]", "p", "r", "[a2]", "s", "q"]);
    // And into an empty area, first.
    db.create_area(&Area::new("a3", "a3", 2.0)).unwrap();
    place_project_in_area(&db, "p", Some("a3"), None).unwrap();
    assert_eq!(
        sidebar(&db),
        ["x", "y", "[a1]", "r", "[a2]", "s", "q", "[a3]", "p"]
    );
}
