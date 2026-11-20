use matters_core::{Area, ChecklistItem, Database, Home, Project, QueryEngine, Status, Task};

#[test]
fn test_inbox_flow() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);

    // 1. Create an inbox task
    let task1 = Task::new("task-1", "Buy oat milk", 1.0);
    db.create_task(&task1).unwrap();

    let task2 = Task::new("task-2", "Review Q4 goals", 2.0);
    db.create_task(&task2).unwrap();

    let inbox = query.inbox().unwrap();
    assert_eq!(inbox.len(), 2);
    assert_eq!(inbox[0].title, "Buy oat milk");
    assert_eq!(inbox[1].title, "Review Q4 goals");

    // 2. Schedule task1 for today -> it leaves the inbox
    let day = jiff::civil::date(2026, 11, 17);
    db.set_task_schedule(&task1.id, &matters_core::Schedule::Date(day), day)
        .unwrap();

    let inbox = query.inbox().unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].id, "task-2");

    // 3. Move task2 to a project -> it leaves the inbox
    let project = Project::new("proj-1", "Personal Finances", 1.0);
    db.create_project(&project).unwrap();

    db.set_task_home(
        &task2.id,
        &Home::Project {
            project: project.id,
            heading: None,
        },
    )
    .unwrap();

    let inbox = query.inbox().unwrap();
    assert!(inbox.is_empty(), "Inbox should be processed to zero");
}

#[test]
fn test_today_and_evening_separation() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);
    let today = jiff::civil::date(2026, 11, 17);
    let yesterday = jiff::civil::date(2026, 11, 16);
    let tomorrow = jiff::civil::date(2026, 11, 18);

    // Regular daytime today task
    let mut t_day = Task::new("t-day", "Prepare slide deck", 1.0);
    t_day.start_date = Some(today);
    db.create_task(&t_day).unwrap();

    // This evening task
    let mut t_eve = Task::new("t-eve", "Take out recycling", 2.0);
    t_eve.start_date = Some(today);
    t_eve.time_of_day = Some("evening".to_string());
    db.create_task(&t_eve).unwrap();

    // Overdue task from yesterday
    let mut t_overdue = Task::new("t-overdue", "File expense report", 3.0);
    t_overdue.start_date = Some(yesterday);
    db.create_task(&t_overdue).unwrap();

    // Future task for tomorrow
    let mut t_future = Task::new("t-future", "Dentist appointment", 4.0);
    t_future.start_date = Some(tomorrow);
    db.create_task(&t_future).unwrap();

    let today_view = query.today(today).unwrap();

    // Daytime should contain: t_day + t_overdue (2 items)
    assert_eq!(today_view.daytime.len(), 2);
    let daytime_ids: Vec<_> = today_view.daytime.iter().map(|t| t.id.as_str()).collect();
    assert!(daytime_ids.contains(&"t-day"));
    assert!(daytime_ids.contains(&"t-overdue"));

    // Evening should contain ONLY t_eve
    assert_eq!(today_view.evening.len(), 1);
    assert_eq!(today_view.evening[0].id, "t-eve");
    assert_eq!(today_view.evening[0].title, "Take out recycling");
}

#[test]
fn test_upcoming_groups() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);
    let today = jiff::civil::date(2026, 11, 17);

    let d1 = jiff::civil::date(2026, 11, 19);
    let d2 = jiff::civil::date(2026, 11, 20);

    let mut t1 = Task::new("t1", "Ship sprint release", 1.0);
    t1.start_date = Some(d1);
    db.create_task(&t1).unwrap();

    let mut t2 = Task::new("t2", "Team retrospective", 2.0);
    t2.start_date = Some(d1);
    db.create_task(&t2).unwrap();

    let mut t3 = Task::new("t3", "Weekly review", 3.0);
    t3.start_date = Some(d2);
    db.create_task(&t3).unwrap();

    let upcoming = query.upcoming(today).unwrap();
    assert_eq!(upcoming.len(), 2, "Should have 2 date groups");

    assert_eq!(upcoming[0].date, d1);
    assert_eq!(upcoming[0].tasks.len(), 2);

    assert_eq!(upcoming[1].date, d2);
    assert_eq!(upcoming[1].tasks.len(), 1);
}

#[test]
fn test_someday_isolation() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);
    let today = jiff::civil::date(2026, 11, 17);

    let mut t_someday = Task::new("t-someday", "Learn Norwegian", 1.0);
    t_someday.is_someday = true;
    db.create_task(&t_someday).unwrap();

    // Verify it doesn't appear in inbox, today, or anytime
    assert!(query.inbox().unwrap().is_empty());
    assert!(query.today(today).unwrap().daytime.is_empty());
    assert!(query.today(today).unwrap().evening.is_empty());
    assert!(query.anytime(today).unwrap().is_empty());

    // Appears strictly in someday
    let someday = query.someday_tasks().unwrap();
    assert_eq!(someday.len(), 1);
    assert_eq!(someday[0].title, "Learn Norwegian");
}

#[test]
fn test_project_progress_ring_calculation() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);

    let project = Project::new("proj-app", "Build Matters App", 1.0);
    db.create_project(&project).unwrap();

    // Add 4 tasks to this project
    for i in 1..=4 {
        let mut t = Task::new(format!("task-{}", i), format!("Step {}", i), i as f64);
        t.project_id = Some(project.id.clone());
        db.create_task(&t).unwrap();
    }

    // Initial state: 0/4 (0.0 ratio)
    let p0 = query.project_progress(&project.id).unwrap();
    assert_eq!(p0.completed_tasks, 0);
    assert_eq!(p0.total_tasks, 4);
    assert!((p0.ratio() - 0.0).abs() < f64::EPSILON);

    // Complete 1 task: 1/4 (0.25 ratio)
    db.complete_task("task-1").unwrap();
    let p1 = query.project_progress(&project.id).unwrap();
    assert_eq!(p1.completed_tasks, 1);
    assert_eq!(p1.total_tasks, 4);
    assert!((p1.ratio() - 0.25).abs() < f64::EPSILON);

    // Complete another: 2/4 (0.50 ratio)
    db.complete_task("task-2").unwrap();
    let p2 = query.project_progress(&project.id).unwrap();
    assert_eq!(p2.completed_tasks, 2);
    assert_eq!(p2.total_tasks, 4);
    assert!((p2.ratio() - 0.50).abs() < f64::EPSILON);

    // Uncomplete task-1: back to 1/4 (0.25 ratio)
    db.uncomplete_task("task-1").unwrap();
    let p3 = query.project_progress(&project.id).unwrap();
    assert_eq!(p3.completed_tasks, 1);
    assert!((p3.ratio() - 0.25).abs() < f64::EPSILON);
}

#[test]
fn test_logbook_records_completions() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);

    let t = Task::new("t-done", "Submit taxes", 1.0);
    db.create_task(&t).unwrap();

    assert!(query.logbook(10).unwrap().is_empty());

    db.complete_task("t-done").unwrap();

    let logbook = query.logbook(10).unwrap();
    assert_eq!(logbook.len(), 1);
    assert_eq!(logbook[0].id, "t-done");
    assert_eq!(logbook[0].status, Status::Completed);
    assert!(logbook[0].completed_at.is_some());
}

#[test]
fn test_checklist_items_toggle() {
    let db = Database::open_in_memory().expect("open memory db");

    let t = Task::new("t-pack", "Pack suitcase", 1.0);
    db.create_task(&t).unwrap();

    let item1 = ChecklistItem::new("c1", "t-pack", "Passport", 1.0);
    let item2 = ChecklistItem::new("c2", "t-pack", "Charger", 2.0);
    db.create_checklist_item(&item1).unwrap();
    db.create_checklist_item(&item2).unwrap();

    let items = db.list_checklist_items("t-pack").unwrap();
    assert_eq!(items.len(), 2);
    assert!(!items[0].is_completed);

    db.set_checklist_item_done("c1", true).unwrap();
    db.set_checklist_item_done("c1", true).unwrap(); // setting, not flipping
    let items_after = db.list_checklist_items("t-pack").unwrap();
    assert!(items_after[0].is_completed);
    assert!(!items_after[1].is_completed);
    db.set_checklist_item_done("c1", false).unwrap();
    assert!(!db.list_checklist_items("t-pack").unwrap()[0].is_completed);
}

#[test]
fn test_quick_find_fuzzy_search() {
    let db = Database::open_in_memory().expect("open memory db");
    let query = QueryEngine::new(&db);

    let area = Area::new("a1", "Finances", 1.0);
    db.create_area(&area).unwrap();

    let proj = Project::new("p1", "Kitchen Renovation", 1.0);
    db.create_project(&proj).unwrap();

    let mut t = Task::new("t1", "Order new kitchen tiles", 1.0);
    t.notes = "Look for ceramic or porcelain options".to_string();
    db.create_task(&t).unwrap();

    let res = query.quick_find("kitchen").unwrap();
    assert_eq!(res.projects.len(), 1);
    assert_eq!(res.projects[0].title, "Kitchen Renovation");
    assert_eq!(res.tasks.len(), 1);
    assert_eq!(res.tasks[0].title, "Order new kitchen tiles");

    let res_notes = query.quick_find("porcelain").unwrap();
    assert_eq!(res_notes.tasks.len(), 1);
    assert_eq!(res_notes.tasks[0].id, "t1");
}
