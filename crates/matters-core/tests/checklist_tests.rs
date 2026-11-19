//! Checklists (#21): items are added, renamed, ticked, ordered and counted
//! per task.

use matters_core::ordering::{add_checklist_item_below, move_checklist_item, place_checklist_item};
use matters_core::{ChecklistItem, ChecklistProgress, Database, MattersError, Task};

fn db_with_tasks(ids: &[&str]) -> Database {
    let db = Database::open_in_memory().unwrap();
    for id in ids {
        db.create_task(&Task::new(*id, *id, 0.0)).unwrap();
    }
    db
}

fn add(db: &Database, task: &str, id: &str) {
    db.add_checklist_item(&mut ChecklistItem::new(id, task, id, 0.0))
        .unwrap();
}

fn titles(db: &Database, task: &str) -> Vec<String> {
    db.list_checklist_items(task)
        .unwrap()
        .into_iter()
        .map(|i| i.title)
        .collect()
}

#[test]
fn items_are_added_at_the_end_with_a_clean_title() {
    let db = db_with_tasks(&["t", "other"]);
    add(&db, "other", "x");
    add(&db, "t", "a");
    add(&db, "t", "b");
    let mut item = ChecklistItem::new("c", "t", "  pack \n the   charger ", 0.0);
    db.add_checklist_item(&mut item).unwrap();
    assert_eq!(item.title, "pack the charger");
    assert_eq!(titles(&db, "t"), ["a", "b", "pack the charger"]);
    assert_eq!(titles(&db, "other"), ["x"]);
}

#[test]
fn a_blank_item_or_one_without_a_task_is_refused() {
    let db = db_with_tasks(&["t"]);
    let mut blank = ChecklistItem::new("c", "t", " \t ", 0.0);
    assert!(matches!(
        db.add_checklist_item(&mut blank),
        Err(MattersError::Validation(_))
    ));
    let mut orphan = ChecklistItem::new("o", "no-such-task", "step", 0.0);
    assert!(db.add_checklist_item(&mut orphan).is_err());
    assert!(db.list_checklist_items("t").unwrap().is_empty());
    assert!(db.get_checklist_item("o").unwrap().is_none());
}

#[test]
fn renaming_cleans_the_title_and_refuses_a_blank_one() {
    let db = db_with_tasks(&["t"]);
    add(&db, "t", "a");
    db.rename_checklist_item("a", "  buy  stamps ").unwrap();
    assert_eq!(titles(&db, "t"), ["buy stamps"]);
    assert!(matches!(
        db.rename_checklist_item("a", "   "),
        Err(MattersError::Validation(_))
    ));
    assert_eq!(titles(&db, "t"), ["buy stamps"], "the old title is kept");
    assert!(matches!(
        db.rename_checklist_item("gone", "x"),
        Err(MattersError::NotFound { .. })
    ));
}

#[test]
fn ticking_sets_the_state_and_progress_counts_it() {
    let db = db_with_tasks(&["t", "u", "empty"]);
    for id in ["a", "b", "c"] {
        add(&db, "t", id);
    }
    add(&db, "u", "d");
    db.set_checklist_item_done("a", true).unwrap();
    db.set_checklist_item_done("c", true).unwrap();
    db.set_checklist_item_done("c", false).unwrap();
    assert!(matches!(
        db.set_checklist_item_done("gone", true),
        Err(MattersError::NotFound { .. })
    ));
    let done: Vec<bool> = db
        .list_checklist_items("t")
        .unwrap()
        .iter()
        .map(|i| i.is_completed)
        .collect();
    assert_eq!(done, [true, false, false]);

    let progress = db.checklist_progress(&["t", "u", "empty"]).unwrap();
    assert_eq!(progress.len(), 2, "a task without a checklist has no entry");
    assert_eq!(progress["t"], ChecklistProgress { done: 1, total: 3 });
    assert_eq!(progress["u"], ChecklistProgress { done: 0, total: 1 });
}

#[test]
fn items_are_placed_and_moved_within_their_checklist() {
    let db = db_with_tasks(&["t", "u"]);
    for id in ["a", "b", "c", "d"] {
        add(&db, "t", id);
    }
    add(&db, "u", "x");

    place_checklist_item(&db, "d", None).unwrap();
    assert_eq!(titles(&db, "t"), ["d", "a", "b", "c"]);
    place_checklist_item(&db, "d", Some("b")).unwrap();
    assert_eq!(titles(&db, "t"), ["a", "b", "d", "c"]);

    move_checklist_item(&db, "a", 2).unwrap();
    assert_eq!(titles(&db, "t"), ["b", "d", "a", "c"]);
    move_checklist_item(&db, "a", -1).unwrap();
    assert_eq!(titles(&db, "t"), ["b", "a", "d", "c"]);
    move_checklist_item(&db, "c", 1).unwrap();
    move_checklist_item(&db, "b", -5).unwrap();
    assert_eq!(
        titles(&db, "t"),
        ["b", "a", "d", "c"],
        "the ends stop a move"
    );
    move_checklist_item(&db, "b", 9).unwrap();
    assert_eq!(titles(&db, "t"), ["a", "d", "c", "b"]);

    assert!(
        place_checklist_item(&db, "a", Some("x")).is_err(),
        "only within its own checklist"
    );
    assert!(matches!(
        move_checklist_item(&db, "gone", 1),
        Err(MattersError::NotFound { .. })
    ));
    assert_eq!(titles(&db, "u"), ["x"]);
}

#[test]
fn checklists_added_with_tied_keys_are_listed_in_the_order_added() {
    let db = db_with_tasks(&["t"]);
    for (i, id) in ["z", "y", "x"].iter().enumerate() {
        let mut item = ChecklistItem::new(*id, "t", *id, 0.0);
        item.created_at = i as i64;
        db.create_checklist_item(&item).unwrap();
    }
    assert_eq!(titles(&db, "t"), ["z", "y", "x"]);
    move_checklist_item(&db, "x", -1).unwrap();
    assert_eq!(titles(&db, "t"), ["z", "x", "y"]);
}

#[test]
fn an_item_added_below_another_lands_right_under_it() {
    let db = db_with_tasks(&["t", "u"]);
    for id in ["a", "b"] {
        add(&db, "t", id);
    }
    add(&db, "u", "x");
    let mut new = ChecklistItem::new("n", "t", "new", 0.0);
    add_checklist_item_below(&db, &mut new, "a").unwrap();
    assert_eq!(titles(&db, "t"), ["a", "new", "b"]);

    let mut stray = ChecklistItem::new("s", "t", "stray", 0.0);
    assert!(add_checklist_item_below(&db, &mut stray, "x").is_err());
    assert!(
        db.get_checklist_item("s").unwrap().is_none(),
        "nothing is added when below is in another checklist"
    );
}
