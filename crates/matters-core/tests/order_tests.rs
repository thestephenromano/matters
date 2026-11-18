//! Manual ordering (#53): lists keep the order the user arranged, Today keeps
//! its own order, and only the ordering operations change it.

use chrono::NaiveDate;
use matters_core::ordering::{add_task_below, move_task, place_area, place_in_today, place_project, place_task};
use matters_core::{Area, Database, Project, QueryEngine, Schedule, Task, View};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()
}

fn ids(db: &Database, view: &View) -> Vec<String> {
    QueryEngine::new(db).tasks_in(view, today()).unwrap().into_iter().map(|t| t.id).collect()
}

fn project(id: &str) -> View {
    View::Project(id.into())
}

/// Tasks in project `p1`, created the way the app used to (all keys 0), so
/// the first move has to renumber.
fn project_fixture(db: &Database, names: &[&str]) {
    db.create_project(&Project::new("p1", "Garden", 0.0)).unwrap();
    for (i, name) in names.iter().enumerate() {
        let mut t = Task::new(*name, *name, 0.0);
        t.project_id = Some("p1".into());
        t.created_at = i as i64;
        db.create_task(&t).unwrap();
    }
}

/// A small deterministic generator, so failures reproduce.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

#[test]
fn any_sequence_of_moves_keeps_the_order_the_user_made_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    let names = ["a", "b", "c", "d", "e", "f", "g"];
    let mut model: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    {
        let db = Database::open(&path).unwrap();
        project_fixture(&db, &names);
        assert_eq!(ids(&db, &project("p1")), model);

        let mut rng = Lcg(7);
        for round in 0..300 {
            let id = model[rng.next(model.len())].clone();
            if round % 2 == 0 {
                // Place after a random other task, or first.
                let others: Vec<String> = model.iter().filter(|x| **x != id).cloned().collect();
                let pick = rng.next(others.len() + 1);
                let after = (pick < others.len()).then(|| others[pick].clone());
                place_task(&db, &project("p1"), today(), &id, after.as_deref()).unwrap();
                model.retain(|x| *x != id);
                let at = after.map_or(0, |a| model.iter().position(|x| *x == a).unwrap() + 1);
                model.insert(at, id.clone());
            } else {
                let step = rng.next(5) as isize - 2;
                move_task(&db, &project("p1"), today(), &id, step).unwrap();
                let from = model.iter().position(|x| *x == id).unwrap();
                let to = (from as isize + step).clamp(0, model.len() as isize - 1) as usize;
                let item = model.remove(from);
                model.insert(to, item);
            }
            assert_eq!(ids(&db, &project("p1")), model, "after move {round} of {id}");
        }
    }
    let reopened = Database::open(&path).unwrap();
    assert_eq!(ids(&reopened, &project("p1")), model, "order survives a restart");
}

#[test]
fn many_moves_between_the_same_neighbours_stay_correct() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a", "x", "y", "b"]);
    // Squeeze x and y alternately between a and b: the midpoints run out and
    // the list is renumbered along the way.
    for i in 0..200 {
        let id = if i % 2 == 0 { "x" } else { "y" };
        place_task(&db, &project("p1"), today(), id, Some("a")).unwrap();
        let expected = if i % 2 == 0 { ["a", "x", "y", "b"] } else { ["a", "y", "x", "b"] };
        assert_eq!(ids(&db, &project("p1")), expected, "move {i}");
    }
}

#[test]
fn today_keeps_its_own_order() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a", "b", "c"]);
    for id in ["a", "b", "c"] {
        let mut t = db.get_task(id).unwrap().unwrap();
        t.apply_schedule(&Schedule::Today { evening: false }, today());
        db.update_task(&t).unwrap();
    }

    place_task(&db, &View::Today, today(), "c", None).unwrap();
    assert_eq!(ids(&db, &View::Today), ["c", "a", "b"]);
    assert_eq!(ids(&db, &project("p1")), ["a", "b", "c"], "planning the day leaves the project alone");

    place_task(&db, &project("p1"), today(), "a", Some("c")).unwrap();
    assert_eq!(ids(&db, &project("p1")), ["b", "c", "a"]);
    assert_eq!(ids(&db, &View::Today), ["c", "a", "b"], "and arranging the project leaves Today alone");
}

fn today_with(db: &Database, tasks: &[(&str, bool)]) {
    for (i, (id, evening)) in tasks.iter().enumerate() {
        let mut t = Task::new(*id, *id, 0.0);
        t.created_at = i as i64;
        t.apply_schedule(&Schedule::Today { evening: *evening }, today());
        db.create_task(&t).unwrap();
    }
}

fn evening_of(db: &Database, id: &str) -> bool {
    db.get_task(id).unwrap().unwrap().is_evening()
}

#[test]
fn this_evening_is_ordered_within_itself() {
    let db = Database::open_in_memory().unwrap();
    today_with(&db, &[("d1", false), ("d2", false), ("e1", true), ("e2", true)]);
    assert_eq!(ids(&db, &View::Today), ["d1", "d2", "e1", "e2"]);

    move_task(&db, &View::Today, today(), "e2", -1).unwrap();
    assert_eq!(ids(&db, &View::Today), ["d1", "d2", "e2", "e1"]);
    assert!(evening_of(&db, "e2"), "moving within This Evening keeps it there");

    assert!(place_task(&db, &View::Today, today(), "e1", Some("d1")).is_err(),
            "place_task never changes a task's section");
}

#[test]
fn moving_past_the_this_evening_boundary_changes_section() {
    let db = Database::open_in_memory().unwrap();
    // The reported case: one daytime task above two evening tasks.
    today_with(&db, &[("d1", false), ("e1", true), ("e2", true)]);

    move_task(&db, &View::Today, today(), "d1", 1).unwrap();
    assert_eq!(ids(&db, &View::Today), ["d1", "e1", "e2"], "first step: top of This Evening");
    assert!(evening_of(&db, "d1"));
    move_task(&db, &View::Today, today(), "d1", 1).unwrap();
    assert_eq!(ids(&db, &View::Today), ["e1", "d1", "e2"]);

    move_task(&db, &View::Today, today(), "e1", -1).unwrap();
    assert_eq!(ids(&db, &View::Today), ["e1", "d1", "e2"], "first step up: last daytime task");
    assert!(!evening_of(&db, "e1"));

    // The ends of Today stop a move.
    move_task(&db, &View::Today, today(), "e1", -1).unwrap();
    assert_eq!(ids(&db, &View::Today), ["e1", "d1", "e2"]);
    assert!(!evening_of(&db, "e1"));
    move_task(&db, &View::Today, today(), "e2", 1).unwrap();
    assert!(evening_of(&db, "e2"));
    assert_eq!(ids(&db, &View::Today), ["e1", "d1", "e2"]);
}

#[test]
fn placing_in_the_other_section_keeps_the_date() {
    let db = Database::open_in_memory().unwrap();
    let overdue = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
    let mut late = Task::new("late", "late", 0.0);
    late.start_date = Some(overdue);
    db.create_task(&late).unwrap();
    let mut due = Task::new("due", "due", 0.0);
    due.deadline = Some(today()); // in Today only because of its deadline
    db.create_task(&due).unwrap();
    today_with(&db, &[("e1", true)]);

    place_in_today(&db, today(), "late", true, Some("e1")).unwrap();
    let late = db.get_task("late").unwrap().unwrap();
    assert!(late.is_evening());
    assert_eq!(late.start_date, Some(overdue), "an overdue task stays overdue");

    place_in_today(&db, today(), "due", true, None).unwrap();
    let due = db.get_task("due").unwrap().unwrap();
    assert_eq!((due.is_evening(), due.start_date), (true, Some(today())));
    assert_eq!(ids(&db, &View::Today), ["due", "e1", "late"]);

    assert!(place_in_today(&db, today(), "due", false, Some("e1")).is_err(), "after must be in the target section");
}

#[test]
fn new_tasks_and_projects_are_added_at_the_end() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a", "b", "c"]);
    for id in ["a", "b", "c"] {
        let mut t = db.get_task(id).unwrap().unwrap();
        t.apply_schedule(&Schedule::Today { evening: false }, today());
        db.update_task(&t).unwrap();
    }
    // Renumber both lists by moving.
    place_task(&db, &project("p1"), today(), "c", None).unwrap();
    place_task(&db, &View::Today, today(), "b", None).unwrap();

    let mut new = View::Project("p1".into()).new_task("new", "new", today()).unwrap();
    new.apply_schedule(&Schedule::Today { evening: false }, today());
    db.add_task(&mut new).unwrap();
    assert_eq!(ids(&db, &project("p1")).last().map(String::as_str), Some("new"));
    assert_eq!(ids(&db, &View::Today).last().map(String::as_str), Some("new"));

    let mut p2 = Project::new("p2", "Kitchen", 0.0);
    db.add_project(&mut p2).unwrap();
    place_project(&db, "p2", None).unwrap();
    let mut p3 = Project::new("p3", "Shed", 0.0);
    db.add_project(&mut p3).unwrap();
    let order: Vec<_> = db.list_projects().unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(order, ["p2", "p1", "p3"]);
}

#[test]
fn saving_a_task_never_changes_its_order() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a", "b", "c"]);
    let mut stale = db.get_task("c").unwrap().unwrap();
    place_task(&db, &project("p1"), today(), "c", None).unwrap();
    stale.notes = "late autosave".into();
    db.update_task(&stale).unwrap();
    assert_eq!(ids(&db, &project("p1")), ["c", "a", "b"]);
}

#[test]
fn lists_with_their_own_order_cannot_be_rearranged() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a", "b"]);
    for view in [View::Upcoming, View::Anytime, View::Someday, View::Logbook, View::Trash] {
        assert!(!view.is_reorderable());
        assert!(place_task(&db, &view, today(), "a", None).is_err(), "{view:?}");
    }
    assert!(place_task(&db, &View::Inbox, today(), "a", None).is_err(), "a is not in the Inbox");
    assert!(place_task(&db, &project("p1"), today(), "a", Some("nope")).is_err());
}

#[test]
fn areas_are_reordered_in_the_sidebar() {
    let db = Database::open_in_memory().unwrap();
    for id in ["home", "work", "health"] {
        let mut a = Area::new(id, id, 0.0);
        db.add_area(&mut a).unwrap();
    }
    place_area(&db, "health", None).unwrap();
    place_area(&db, "home", Some("work")).unwrap();
    let order: Vec<_> = db.list_areas().unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(order, ["health", "work", "home"]);
    assert!(place_area(&db, "home", Some("gone")).is_err());
}

#[test]
fn a_task_added_below_another_lands_right_under_it() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a", "b", "c"]);
    let view = project("p1");
    let mut new = view.new_task("n1", "n1", today()).unwrap();
    add_task_below(&db, &view, today(), &mut new, "a").unwrap();
    assert_eq!(ids(&db, &view), ["a", "n1", "b", "c"]);
    let mut next = view.new_task("n2", "n2", today()).unwrap();
    add_task_below(&db, &view, today(), &mut next, "n1").unwrap();
    assert_eq!(ids(&db, &view), ["a", "n1", "n2", "b", "c"], "typing a list keeps going down");
}

#[test]
fn in_today_a_task_added_below_joins_that_section() {
    let db = Database::open_in_memory().unwrap();
    today_with(&db, &[("d1", false), ("e1", true), ("e2", true)]);
    let mut day = View::Today.new_task("nd", "nd", today()).unwrap();
    add_task_below(&db, &View::Today, today(), &mut day, "d1").unwrap();
    let mut eve = View::Today.new_task("ne", "ne", today()).unwrap();
    add_task_below(&db, &View::Today, today(), &mut eve, "e1").unwrap();
    assert_eq!(ids(&db, &View::Today), ["d1", "nd", "e1", "ne", "e2"]);
    assert!(!evening_of(&db, "nd"));
    assert!(evening_of(&db, "ne"), "added below an evening task: This Evening");
}

#[test]
fn adding_below_needs_a_listed_task_in_a_list_arranged_by_hand() {
    let db = Database::open_in_memory().unwrap();
    project_fixture(&db, &["a"]);
    let mut task = project("p1").new_task("x", "x", today()).unwrap();
    assert!(add_task_below(&db, &project("p1"), today(), &mut task, "missing").is_err());
    let mut someday = View::Someday.new_task("y", "y", today()).unwrap();
    assert!(add_task_below(&db, &View::Someday, today(), &mut someday, "a").is_err());
    assert!(db.get_task("x").unwrap().is_none() && db.get_task("y").unwrap().is_none(), "nothing created");
}
