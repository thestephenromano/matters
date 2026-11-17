//! A task whose completion status changes stays in place until the user
//! leaves the view; other departures are immediate.

use chrono::NaiveDate;
use matters_core::{Database, QueryEngine, Schedule, Status, Task, View, Visit};

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()
}

struct Fixture {
    db: Database,
    visit: Visit,
    shown: Vec<Task>,
}

impl Fixture {
    fn new(titles: &[&str]) -> Self {
        let db = Database::open_in_memory().unwrap();
        for (i, title) in titles.iter().enumerate() {
            let mut t = Task::new(*title, *title, i as f64);
            t.start_date = Some(today());
            db.create_task(&t).unwrap();
        }
        Self { db, visit: Visit::new(), shown: Vec::new() }
    }

    /// Reload `view` the way a frontend does and return the displayed ids.
    fn show(&mut self, view: View) -> Vec<String> {
        let members = QueryEngine::new(&self.db).tasks_in(&view, today()).unwrap();
        let db = &self.db;
        self.shown = self.visit.rows(&view, members, &self.shown, |id| db.get_task(id)).unwrap();
        self.shown.iter().map(|t| t.id.clone()).collect()
    }

    fn status(&self, id: &str) -> Status {
        self.shown.iter().find(|t| t.id == id).unwrap().status
    }
}

#[test]
fn completed_task_stays_in_place_until_the_view_is_left() {
    let mut f = Fixture::new(&["a", "b", "c"]);
    assert_eq!(f.show(View::Today), ["a", "b", "c"]);

    f.db.complete_task("b").unwrap();
    assert_eq!(f.show(View::Today), ["a", "b", "c"], "b keeps its place");
    assert_eq!(f.status("b"), Status::Completed, "shown as done");
    assert_eq!(f.show(View::Today), ["a", "b", "c"], "still kept on later reloads");

    f.db.uncomplete_task("b").unwrap();
    assert_eq!(f.show(View::Today), ["a", "b", "c"]);
    assert_eq!(f.status("b"), Status::Open, "unticking restores it");

    f.db.complete_task("b").unwrap();
    f.show(View::Today);
    f.show(View::Inbox); // leave
    assert_eq!(f.show(View::Today), ["a", "c"], "gone after leaving and returning");

    f.db.complete_task("a").unwrap();
    assert_eq!(f.show(View::Today), ["a", "c"]);
    f.visit.restart(); // navigated to Today again
    assert_eq!(f.show(View::Today), ["c"], "re-entering the same view also starts a new visit");
}

#[test]
fn first_and_consecutive_kept_tasks_keep_their_order() {
    let mut f = Fixture::new(&["a", "b", "c", "d"]);
    f.show(View::Today);
    for id in ["a", "b", "d"] {
        f.db.complete_task(id).unwrap();
    }
    assert_eq!(f.show(View::Today), ["a", "b", "c", "d"]);
}

#[test]
fn rescheduled_or_deleted_tasks_leave_immediately() {
    let mut f = Fixture::new(&["a", "b", "c"]);
    f.show(View::Today);

    let mut b = f.db.get_task("b").unwrap().unwrap();
    b.apply_schedule(&Schedule::Someday, today());
    f.db.update_task(&b).unwrap();
    f.db.delete_task("c").unwrap();
    assert_eq!(f.show(View::Today), ["a"]);
}

#[test]
fn a_kept_task_that_is_then_deleted_disappears() {
    let mut f = Fixture::new(&["a", "b"]);
    f.show(View::Today);
    f.db.complete_task("a").unwrap();
    assert_eq!(f.show(View::Today), ["a", "b"]);
    f.db.delete_task("a").unwrap();
    assert_eq!(f.show(View::Today), ["b"]);
}

#[test]
fn reopened_task_stays_in_the_logbook_until_left() {
    let mut f = Fixture::new(&["a"]);
    f.db.complete_task("a").unwrap();
    assert_eq!(f.show(View::Logbook), ["a"]);
    f.db.uncomplete_task("a").unwrap();
    assert_eq!(f.show(View::Logbook), ["a"]);
    assert_eq!(f.status("a"), Status::Open);
}
