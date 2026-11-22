//! The read methods and token checks, over a real D-Bus connection between
//! two peers (a socket pair), so no bus daemon is needed.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use jiff::civil::{date, Date, DateTime};
use matters_automation::{
    Automation, Item, MemoryClients, Permission, Permissions, BUS_NAME, OBJECT_PATH,
};
use matters_core::calendar::Event;
use matters_core::{Area, ChecklistItem, Database, Project, Schedule, Tag, Tagged, Task};
use zbus::{connection, proxy, Connection};

const TODAY: Date = date(2026, 11, 22);

#[proxy(
    interface = "io.github.thestephenromano.Matters.Automation1",
    default_service = "io.github.thestephenromano.Matters",
    default_path = "/io/github/thestephenromano/Matters/Automation"
)]
trait Automation1 {
    fn get_view(&self, token: &str, view: &str) -> zbus::Result<(String, Vec<Item>)>;
    fn find(&self, token: &str, query: &str) -> zbus::Result<Vec<Item>>;
    fn get(&self, token: &str, id: &str) -> zbus::Result<Item>;
    fn list_projects(&self, token: &str) -> zbus::Result<Vec<Item>>;
    fn list_areas(&self, token: &str) -> zbus::Result<Vec<Item>>;
}

struct Fixture {
    db: Database,
    clients: Arc<MemoryClients>,
    reader: String,
    // Keeps the serving end open.
    _service: Connection,
    client: Connection,
}

impl Fixture {
    fn proxy(&self) -> Automation1Proxy<'_> {
        zbus::block_on(Automation1Proxy::new(&self.client)).unwrap()
    }
}

fn task(db: &Database, id: &str, title: &str) -> Task {
    let t = Task::new(id, title, 0.0);
    db.create_task(&t).unwrap();
    t
}

fn fixture() -> Fixture {
    let db = Database::open_in_memory().unwrap();
    task(&db, "inbox-1", "Call the plumber");
    task(&db, "today-1", "Write the report");
    db.set_task_schedule("today-1", &Schedule::Today { evening: false }, TODAY)
        .unwrap();
    task(&db, "evening-1", "Read");
    db.set_task_schedule("evening-1", &Schedule::Today { evening: true }, TODAY)
        .unwrap();
    task(&db, "later-1", "Renew passport");
    db.set_task_schedule("later-1", &Schedule::Date(date(2026, 11, 25)), TODAY)
        .unwrap();
    task(&db, "someday-1", "Learn the cello");
    db.set_task_schedule("someday-1", &Schedule::Someday, TODAY)
        .unwrap();
    task(&db, "done-1", "File taxes");
    db.complete_task("done-1").unwrap();
    task(&db, "trash-1", "Old idea");
    db.trash_task("trash-1").unwrap();

    let mut tag = Tag::new("tag-1", "errand", None);
    db.add_tag(&mut tag).unwrap();
    db.add_tag_to(Tagged::Task("inbox-1"), "tag-1").unwrap();
    let mut item = ChecklistItem::new("check-1", "today-1", "Outline", 0.0);
    db.add_checklist_item(&mut item).unwrap();

    db.create_area(&Area::new("area-1", "Home", 0.0)).unwrap();
    let mut project = Project::new("project-1", "Move house", 0.0);
    project.area_id = Some("area-1".into());
    db.create_project(&project).unwrap();
    let mut in_project = Task::new("project-task-1", "Book the van", 0.0);
    in_project.project_id = Some("project-1".into());
    db.create_task(&in_project).unwrap();

    let clients = Arc::new(MemoryClients::default());
    let reader = clients
        .add("reader", Permissions::of(&[Permission::Read]))
        .unwrap()
        .as_str()
        .to_owned();

    let events: matters_automation::TodayEvents = Box::new(|day| {
        let at = |h| DateTime::from_parts(day, jiff::civil::time(h, 0, 0, 0));
        vec![Event {
            id: "event-1".into(),
            calendar_id: "work".into(),
            title: "Stand-up".into(),
            start: at(9),
            end: at(10),
            all_day: false,
        }]
    });
    let automation = Automation::new(db.clone(), clients.clone())
        .with_today(|| TODAY)
        .with_events(events);
    let (a, b) = UnixStream::pair().unwrap();
    let guid = zbus::Guid::generate();
    let (service, client) = zbus::block_on(async {
        futures_join(
            connection::Builder::unix_stream(a)
                .server(guid)
                .p2p()
                .serve_at(OBJECT_PATH, automation)
                .build(),
            connection::Builder::unix_stream(b).p2p().build(),
        )
        .await
    });
    Fixture {
        db,
        clients,
        reader,
        _service: service.unwrap(),
        client: client.unwrap(),
    }
}

/// Run both handshakes at once: each side waits for the other.
async fn futures_join<A, B>(a: A, b: B) -> (A::Output, B::Output)
where
    A: std::future::Future,
    B: std::future::Future,
{
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none() {
            if let std::task::Poll::Ready(v) = a.as_mut().poll(cx) {
                ra = Some(v);
            }
        }
        if rb.is_none() {
            if let std::task::Poll::Ready(v) = b.as_mut().poll(cx) {
                rb = Some(v);
            }
        }
        if ra.is_some() && rb.is_some() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    (ra.unwrap(), rb.unwrap())
}

fn ids(items: &[Item]) -> Vec<&str> {
    items.iter().map(|i| i.id.as_str()).collect()
}

fn view(f: &Fixture, name: &str) -> (String, Vec<Item>) {
    zbus::block_on(f.proxy().get_view(&f.reader, name)).unwrap()
}

/// The D-Bus error name of a failed call.
fn error_name<T: std::fmt::Debug>(result: zbus::Result<T>) -> String {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        other => panic!("expected a method error, got {other:?}"),
    }
}

const ERROR: &str = "io.github.thestephenromano.Matters.Automation1.Error";

#[test]
fn names_follow_the_app_id() {
    assert_eq!(BUS_NAME, "io.github.thestephenromano.Matters");
    assert!(OBJECT_PATH.starts_with("/io/github/thestephenromano/Matters"));
}

#[test]
fn every_view_returns_what_the_window_shows() {
    let f = fixture();
    assert_eq!(ids(&view(&f, "inbox").1), ["inbox-1"]);
    let (_, today) = view(&f, "today");
    assert_eq!(ids(&today), ["event-1", "today-1", "evening-1"]);
    let sections: Vec<_> = today.iter().map(|i| i.section.as_deref()).collect();
    assert_eq!(sections, [Some("events"), None, Some("evening")]);
    assert_eq!(today[0].kind, "event");
    assert_eq!(today[0].start.as_deref(), Some("2026-11-22T09:00"));
    let (_, upcoming) = view(&f, "upcoming");
    let later = upcoming.iter().find(|i| i.id == "later-1").unwrap();
    assert_eq!(later.section.as_deref(), Some("2026-11-25"));
    // Anytime lists placed tasks only (in an area or project), as the window does.
    assert_eq!(ids(&view(&f, "anytime").1), ["project-task-1"]);
    assert_eq!(ids(&view(&f, "someday").1), ["someday-1"]);
    assert_eq!(ids(&view(&f, "logbook").1), ["done-1"]);
    assert_eq!(ids(&view(&f, "trash").1), ["trash-1"]);
    assert_eq!(ids(&view(&f, "project:project-1").1), ["project-task-1"]);
    // An area view lists the tasks filed directly in it, not its projects'.
    assert!(view(&f, "area:area-1").1.is_empty());
}

#[test]
fn a_task_carries_its_schedule_tags_and_checklist() {
    let f = fixture();
    let p = f.proxy();
    let inbox = zbus::block_on(p.get(&f.reader, "inbox-1")).unwrap();
    assert_eq!(inbox.kind, "task");
    assert_eq!(inbox.tags, Some(vec!["errand".to_string()]));
    assert_eq!(inbox.status.as_deref(), Some("open"));
    assert_eq!(inbox.start_date, None);
    let today = zbus::block_on(p.get(&f.reader, "today-1")).unwrap();
    assert_eq!(today.start_date.as_deref(), Some("2026-11-22"));
    assert_eq!(today.evening, Some(false));
    assert_eq!(
        (today.checklist_done, today.checklist_total),
        (Some(0), Some(1))
    );
    assert_eq!(today.tags, None);
    let project = zbus::block_on(p.get(&f.reader, "project-1")).unwrap();
    assert_eq!(
        (project.kind.as_str(), project.area_id.as_deref()),
        ("project", Some("area-1"))
    );
    let area = zbus::block_on(p.get(&f.reader, "area-1")).unwrap();
    assert_eq!((area.kind.as_str(), area.title.as_str()), ("area", "Home"));
    assert_eq!(
        error_name(zbus::block_on(p.get(&f.reader, "missing"))),
        format!("{ERROR}.NotFound")
    );
}

#[test]
fn find_lists_and_sidebar_reads() {
    let f = fixture();
    let p = f.proxy();
    let found = zbus::block_on(p.find(&f.reader, "move")).unwrap();
    assert_eq!(ids(&found), ["project-1"]);
    assert_eq!(
        ids(&zbus::block_on(p.list_projects(&f.reader)).unwrap()),
        ["project-1"]
    );
    assert_eq!(
        ids(&zbus::block_on(p.list_areas(&f.reader)).unwrap()),
        ["area-1"]
    );
}

#[test]
fn a_view_s_revision_changes_with_its_contents_and_only_then() {
    let f = fixture();
    let (first, _) = view(&f, "inbox");
    let (again, _) = view(&f, "inbox");
    assert_eq!(first, again);
    // A change elsewhere leaves the Inbox's revision alone.
    f.db.complete_task("today-1").unwrap();
    assert_eq!(view(&f, "inbox").0, first);
    f.db.rename_task("inbox-1", "Call the plumber today")
        .unwrap();
    assert_ne!(view(&f, "inbox").0, first);
}

#[test]
fn unknown_views_and_items_are_refused() {
    let f = fixture();
    let p = f.proxy();
    for bad in ["", "Inbox", "project:", "tag:x", "project"] {
        assert_eq!(
            error_name(zbus::block_on(p.get_view(&f.reader, bad))),
            format!("{ERROR}.InvalidArgument"),
            "{bad:?}"
        );
    }
    assert_eq!(
        error_name(zbus::block_on(p.get_view(&f.reader, "project:nope"))),
        format!("{ERROR}.NotFound")
    );
}

#[test]
fn every_method_refuses_a_missing_unknown_or_revoked_token_and_a_missing_permission() {
    let f = fixture();
    let p = f.proxy();
    let writer = f
        .clients
        .add("writer", Permissions::of(&[Permission::Create]))
        .unwrap()
        .as_str()
        .to_owned();
    let revoked = f
        .clients
        .add("old agent", Permissions::of(&[Permission::Read]))
        .unwrap()
        .as_str()
        .to_owned();
    f.clients.revoke("old agent");
    let (forged, _) = matters_automation::Token::generate().unwrap();
    let cases = [
        ("", "UnknownToken"),
        ("not a token", "UnknownToken"),
        (forged.as_str(), "UnknownToken"),
        (revoked.as_str(), "RevokedToken"),
        (writer.as_str(), "PermissionDenied"),
    ];
    for (token, expected) in cases {
        let expected = format!("{ERROR}.{expected}");
        let calls: [(&str, zbus::Result<()>); 5] = [
            (
                "GetView",
                zbus::block_on(p.get_view(token, "inbox")).map(drop),
            ),
            ("Find", zbus::block_on(p.find(token, "x")).map(drop)),
            ("Get", zbus::block_on(p.get(token, "inbox-1")).map(drop)),
            (
                "ListProjects",
                zbus::block_on(p.list_projects(token)).map(drop),
            ),
            ("ListAreas", zbus::block_on(p.list_areas(token)).map(drop)),
        ];
        for (method, result) in calls {
            assert_eq!(error_name(result), expected, "{method} with {token:?}");
        }
    }
    let denied = zbus::block_on(p.get_view(&writer, "inbox"));
    match denied {
        Err(zbus::Error::MethodError(_, Some(message), _)) => {
            assert!(message.contains("Read"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}
