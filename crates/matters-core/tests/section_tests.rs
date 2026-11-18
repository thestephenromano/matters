//! Sections in lists (#25): which heading each row sits under, and Anytime's
//! grouping by area and project.

use jiff::civil::Date;
use matters_core::{Area, Database, Project, QueryEngine, Schedule, Task, View};

/// Wednesday 18 November 2026.
fn today() -> Date {
    jiff::civil::date(2026, 11, 18)
}

fn listed(db: &Database, view: &View) -> Vec<(String, String)> {
    let engine = QueryEngine::new(db);
    let tasks = engine.tasks_in(view, today()).unwrap();
    let keys = engine.section_keys(view, &tasks).unwrap();
    tasks.into_iter().map(|t| t.id).zip(keys).collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn today_marks_this_evening() {
    let db = Database::open_in_memory().unwrap();
    for (i, (id, evening)) in [("d", false), ("e", true)].iter().enumerate() {
        let mut t = Task::new(*id, *id, i as f64);
        t.apply_schedule(&Schedule::Today { evening: *evening }, today());
        db.create_task(&t).unwrap();
    }
    assert_eq!(
        listed(&db, &View::Today),
        pairs(&[("d", ""), ("e", "evening")])
    );
}

#[test]
fn upcoming_is_grouped_by_date() {
    let db = Database::open_in_memory().unwrap();
    let day = |d| jiff::civil::date(2026, 11, d);
    for (id, d) in [("c", 21), ("a", 19), ("b", 19)] {
        let mut t = Task::new(id, id, 0.0);
        t.start_date = Some(day(d));
        db.create_task(&t).unwrap();
    }
    assert_eq!(
        listed(&db, &View::Upcoming),
        pairs(&[
            ("a", "date:2026-11-19"),
            ("b", "date:2026-11-19"),
            ("c", "date:2026-11-21")
        ])
    );
}

#[test]
fn anytime_is_grouped_by_area_then_project() {
    let db = Database::open_in_memory().unwrap();
    // Sidebar order: Work, then Home.
    db.create_area(&Area::new("home", "Home", 2.0)).unwrap();
    db.create_area(&Area::new("work", "Work", 1.0)).unwrap();
    let mut garden = Project::new("garden", "Garden", 2.0);
    garden.area_id = Some("home".into());
    let mut launch = Project::new("launch", "Launch", 1.0);
    launch.area_id = Some("work".into());
    let solo = Project::new("solo", "Solo", 0.0);
    for p in [&garden, &launch, &solo] {
        db.create_project(p).unwrap();
    }
    // Created in an order that interleaves the groups.
    let tasks: [(&str, Option<&str>, Option<&str>); 6] = [
        ("garden-1", Some("garden"), None),
        ("solo-1", Some("solo"), None),
        ("work-loose", None, Some("work")),
        ("launch-1", Some("launch"), None),
        ("home-loose", None, Some("home")),
        ("launch-2", Some("launch"), None),
    ];
    for (i, (id, project, area)) in tasks.iter().enumerate() {
        let mut t = Task::new(*id, *id, i as f64);
        t.project_id = project.map(str::to_string);
        t.area_id = area.map(str::to_string);
        db.create_task(&t).unwrap();
    }
    assert_eq!(
        listed(&db, &View::Anytime),
        pairs(&[
            ("work-loose", "area:work:Work"),
            ("launch-1", "project:launch:Launch"),
            ("launch-2", "project:launch:Launch"),
            ("home-loose", "area:home:Home"),
            ("garden-1", "project:garden:Garden"),
            ("solo-1", "project:solo:Solo"),
        ])
    );
}

#[test]
fn the_logbook_is_grouped_by_the_day_tasks_were_finished() {
    let db = Database::open_in_memory().unwrap();
    let at = |d: i8, h: i8| {
        jiff::civil::date(2026, 11, d)
            .at(h, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::system())
            .unwrap()
            .timestamp()
            .as_millisecond()
    };
    for (id, finished) in [
        ("old", at(16, 9)),
        ("late", at(17, 23)),
        ("early", at(17, 8)),
    ] {
        let mut t = Task::new(id, id, 0.0);
        t.status = matters_core::Status::Completed;
        t.completed_at = Some(finished);
        db.create_task(&t).unwrap();
    }
    assert_eq!(
        listed(&db, &View::Logbook),
        pairs(&[
            ("late", "done:2026-11-17"),
            ("early", "done:2026-11-17"),
            ("old", "done:2026-11-16")
        ])
    );
}
