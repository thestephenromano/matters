//! `Apply`: changes in one batch, over a real D-Bus connection between two
//! peers (a socket pair), so no bus daemon is needed.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use jiff::civil::{date, Date};
use matters_automation::{
    Automation, Item, MemoryClients, Permission, Permissions, LINKS_CLIENT, MAX_CHANGES,
    OBJECT_PATH,
};
use matters_core::{Area, Database, Tag, Task};
use zbus::{connection, proxy, Connection, OwnedVariant, Str};

const TODAY: Date = date(2026, 11, 22);

type Dict = HashMap<String, OwnedVariant>;

#[proxy(
    interface = "io.github.thestephenromano.Matters.Automation1",
    default_service = "io.github.thestephenromano.Matters",
    default_path = "/io/github/thestephenromano/Matters/Automation"
)]
trait Automation1 {
    fn get_view(&self, token: &str, view: &str) -> zbus::Result<(String, Vec<Item>)>;
    fn get(&self, token: &str, id: &str) -> zbus::Result<Item>;
    fn apply(&self, token: &str, changes: Vec<Dict>, options: Dict) -> zbus::Result<Vec<Dict>>;
}

struct Fixture {
    db: Database,
    clients: Arc<MemoryClients>,
    /// Every permission.
    all: String,
    changed: Arc<AtomicUsize>,
    _service: Connection,
    client: Connection,
}

/// A dictionary of string values, plus `extra` of any type.
fn dict(pairs: &[(&str, &str)]) -> Dict {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OwnedVariant::from(Str::from(*v))))
        .collect()
}

fn with(mut d: Dict, key: &str, value: OwnedVariant) -> Dict {
    d.insert(key.to_owned(), value);
    d
}

fn strings(values: &[&str]) -> OwnedVariant {
    OwnedVariant::try_from(zbus::Variant::from(
        values.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
    ))
    .unwrap()
}

fn every() -> Permissions {
    Permissions::of(&[
        Permission::Read,
        Permission::Create,
        Permission::Organise,
        Permission::Areas,
        Permission::Trash,
    ])
}

fn fixture_on(db: Database) -> Fixture {
    db.add_tag(&mut Tag::new(uuid(1), "errand", None)).unwrap();
    let clients = Arc::new(MemoryClients::default());
    let all = clients.add("agent", every()).unwrap().as_str().to_owned();
    let changed = Arc::new(AtomicUsize::new(0));
    let seen = changed.clone();
    let automation = Automation::new(db.clone(), clients.clone())
        .with_today(|| TODAY)
        .with_changed(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
    let (a, b) = UnixStream::pair().unwrap();
    let guid = zbus::Guid::generate();
    let (service, client) = zbus::block_on(futures_join(
        connection::Builder::unix_stream(a)
            .server(guid)
            .p2p()
            .serve_at(OBJECT_PATH, automation)
            .build(),
        connection::Builder::unix_stream(b).p2p().build(),
    ));
    Fixture {
        db,
        clients,
        all,
        changed,
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

fn fixture() -> Fixture {
    fixture_on(Database::open_in_memory().unwrap())
}

fn uuid(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

impl Fixture {
    fn proxy(&self) -> Automation1Proxy<'_> {
        zbus::block_on(Automation1Proxy::new(&self.client)).unwrap()
    }

    fn apply_as(&self, token: &str, changes: Vec<Dict>, options: Dict) -> zbus::Result<Vec<Dict>> {
        zbus::block_on(self.proxy().apply(token, changes, options))
    }

    fn apply(&self, changes: Vec<Dict>) -> zbus::Result<Vec<Dict>> {
        self.apply_as(&self.all, changes, Dict::new())
    }

    fn view(&self, name: &str) -> (String, Vec<Item>) {
        zbus::block_on(self.proxy().get_view(&self.all, name)).unwrap()
    }

    fn get(&self, id: &str) -> Item {
        zbus::block_on(self.proxy().get(&self.all, id)).unwrap()
    }

    /// Every view's revision: equal before and after means nothing changed.
    fn everything(&self) -> Vec<String> {
        [
            "inbox", "today", "upcoming", "anytime", "someday", "logbook", "trash",
        ]
        .iter()
        .map(|v| self.view(v).0)
        .chain(std::iter::once(format!(
            "{:?}{:?}",
            self.db.list_areas().unwrap(),
            self.db.list_projects().unwrap()
        )))
        .collect()
    }
}

fn id_of(result: &[Dict], i: usize) -> String {
    String::try_from(result[i]["id"].try_clone().unwrap()).unwrap()
}

/// The D-Bus error name of a failed call, and its message.
fn error<T: std::fmt::Debug>(result: zbus::Result<T>) -> (String, String) {
    match result {
        Err(zbus::Error::MethodError(name, message, _)) => {
            (name.to_string(), message.unwrap_or_default())
        }
        other => panic!("expected a method error, got {other:?}"),
    }
}

fn named(result: (String, String), error: &str) -> String {
    assert_eq!(
        result.0,
        format!("io.github.thestephenromano.Matters.Automation1.Error.{error}"),
        "{}",
        result.1
    );
    result.1
}

#[test]
fn a_project_and_its_tasks_are_made_in_one_batch_by_ref() {
    let f = fixture();
    let made = f
        .apply(vec![
            dict(&[
                ("op", "create_project"),
                ("title", "Move house"),
                ("ref", "p"),
            ]),
            dict(&[
                ("op", "create_task"),
                ("title", "Book the van"),
                ("project_id", "ref:p"),
            ]),
            with(
                dict(&[
                    ("op", "create_task"),
                    ("title", "Buy boxes"),
                    ("project_id", "ref:p"),
                    ("when", "tomorrow"),
                ]),
                "tags",
                strings(&["Errand"]),
            ),
        ])
        .unwrap();

    let project = id_of(&made, 0);
    let (_, tasks) = f.view(&format!("project:{project}"));
    let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Book the van", "Buy boxes"]);
    assert_eq!(tasks[1].id, id_of(&made, 2));
    assert_eq!(tasks[1].start_date.as_deref(), Some("2026-11-23"));
    assert_eq!(tasks[1].tags.as_deref(), Some(&["errand".to_owned()][..]));
    assert_eq!(f.changed.load(Ordering::SeqCst), 1);
}

#[test]
fn every_operation_does_what_the_window_would() {
    let f = fixture();
    let made = f
        .apply(vec![
            dict(&[("op", "create_area"), ("title", "Home"), ("ref", "a")]),
            dict(&[
                ("op", "create_project"),
                ("title", "Garden"),
                ("area_id", "ref:a"),
                ("ref", "p"),
            ]),
            dict(&[
                ("op", "create_heading"),
                ("project_id", "ref:p"),
                ("title", "Spring"),
                ("ref", "h"),
            ]),
            dict(&[("op", "create_task"), ("title", "One"), ("ref", "t1")]),
            dict(&[("op", "create_task"), ("title", "Two"), ("ref", "t2")]),
            dict(&[
                ("op", "add_checklist_item"),
                ("task_id", "ref:t1"),
                ("title", "Step"),
                ("ref", "c"),
            ]),
        ])
        .unwrap();
    let [area, project, heading, one, two, item] = [0, 1, 2, 3, 4, 5].map(|i| id_of(&made, i));

    // Two comes before One in the Inbox.
    f.apply(vec![dict(&[
        ("op", "place"),
        ("id", &two),
        ("view", "inbox"),
    ])])
    .unwrap();
    let inbox: Vec<String> = f.view("inbox").1.into_iter().map(|i| i.id).collect();
    assert_eq!(inbox, [two.clone(), one.clone()]);

    f.apply(vec![
        dict(&[
            ("op", "update"),
            ("id", &one),
            ("title", "One, renamed"),
            ("notes", "Some notes"),
            ("when", "2026-12-01"),
            ("deadline", "2026-12-24"),
        ]),
        dict(&[("op", "tag"), ("id", &one), ("tag", "errand")]),
        with(
            dict(&[("op", "check"), ("id", &item)]),
            "done",
            OwnedVariant::from(true),
        ),
        dict(&[
            ("op", "move"),
            ("id", &two),
            ("to", &format!("project:{project}")),
            ("heading_id", &heading),
        ]),
        dict(&[("op", "rename_area"), ("id", &area), ("title", "House")]),
    ])
    .unwrap();
    let t = f.get(&one);
    assert_eq!(t.title, "One, renamed");
    assert_eq!(t.notes.as_deref(), Some("Some notes"));
    assert_eq!(t.start_date.as_deref(), Some("2026-12-01"));
    assert_eq!(t.deadline.as_deref(), Some("2026-12-24"));
    assert_eq!(t.tags.as_deref(), Some(&["errand".to_owned()][..]));
    assert_eq!((t.checklist_done, t.checklist_total), (Some(1), Some(1)));
    let t = f.get(&two);
    assert_eq!(t.project_id.as_deref(), Some(project.as_str()));
    assert_eq!(t.heading_id.as_deref(), Some(heading.as_str()));
    assert_eq!(f.get(&area).title, "House");

    f.apply(vec![
        dict(&[("op", "untag"), ("id", &one), ("tag", "errand")]),
        dict(&[
            ("op", "update"),
            ("id", &one),
            ("when", "anytime"),
            ("deadline", "none"),
        ]),
        dict(&[("op", "complete"), ("id", &one)]),
        dict(&[("op", "move"), ("id", &two), ("to", "inbox")]),
        dict(&[("op", "move"), ("id", &project), ("to", "none")]),
    ])
    .unwrap();
    let t = f.get(&one);
    assert_eq!((t.tags, t.start_date, t.deadline), (None, None, None));
    assert_eq!(t.status.as_deref(), Some("completed"));
    assert_eq!(f.get(&two).project_id, None);
    assert_eq!(f.get(&project).area_id, None);

    f.apply(vec![
        dict(&[("op", "reopen"), ("id", &one)]),
        dict(&[("op", "cancel"), ("id", &two)]),
        dict(&[
            ("op", "complete"),
            ("id", &project),
            ("open_tasks", "cancel"),
        ]),
        dict(&[("op", "trash"), ("id", &one)]),
        dict(&[("op", "trash"), ("id", &area)]),
    ])
    .unwrap();
    assert_eq!(f.get(&one).trashed, Some(true));
    assert_eq!(f.get(&two).status.as_deref(), Some("canceled"));
    assert_eq!(f.get(&project).status.as_deref(), Some("completed"));
    assert_eq!(f.get(&area).trashed, Some(true));

    f.apply(vec![
        dict(&[("op", "restore"), ("id", &one)]),
        dict(&[("op", "restore"), ("id", &area)]),
        dict(&[("op", "reopen"), ("id", &project)]),
        dict(&[("op", "trash"), ("id", &project)]),
        dict(&[("op", "restore"), ("id", &project)]),
    ])
    .unwrap();
    assert_eq!(f.get(&one).trashed, Some(false));
    assert_eq!(f.get(&area).trashed, Some(false));
    assert_eq!(f.get(&project).status.as_deref(), Some("open"));
}

#[test]
fn one_refused_change_leaves_everything_as_it_was() {
    let f = fixture();
    let made = f
        .apply(vec![dict(&[("op", "create_task"), ("title", "Kept")])])
        .unwrap();
    let kept = id_of(&made, 0);
    let before = f.everything();
    let calls = f.changed.load(Ordering::SeqCst);

    let missing = f.apply(vec![
        dict(&[("op", "update"), ("id", &kept), ("title", "Changed")]),
        dict(&[("op", "create_task"), ("title", "New")]),
        dict(&[("op", "complete"), ("id", "no-such-task")]),
    ]);
    assert!(named(error(missing), "NotFound").starts_with("change 2: "));

    // Refused while parsing: nothing runs.
    let unknown_op = f.apply(vec![
        dict(&[("op", "create_task"), ("title", "New")]),
        dict(&[("op", "empty_trash")]),
    ]);
    assert!(named(error(unknown_op), "InvalidChange").starts_with("change 1: "));
    let misspelt = f.apply(vec![dict(&[("op", "create_task"), ("titel", "New")])]);
    named(error(misspelt), "InvalidChange");
    let blank = f.apply(vec![
        dict(&[("op", "create_task"), ("title", "New")]),
        dict(&[("op", "update"), ("id", &kept), ("title", "  ")]),
    ]);
    named(error(blank), "InvalidChange");
    let unknown_ref = f.apply(vec![dict(&[
        ("op", "create_task"),
        ("title", "New"),
        ("project_id", "ref:nope"),
    ])]);
    named(error(unknown_ref), "InvalidChange");

    assert_eq!(f.everything(), before);
    assert_eq!(f.changed.load(Ordering::SeqCst), calls);
}

#[test]
fn nothing_can_empty_the_trash_or_delete_anything() {
    let f = fixture();
    for op in [
        "empty_trash",
        "delete",
        "delete_heading",
        "delete_checklist_item",
        "delete_tag",
    ] {
        let refused = f.apply(vec![dict(&[("op", op), ("id", "x")])]);
        assert!(named(error(refused), "InvalidChange").contains("is not an operation"));
    }
}

#[test]
fn a_batch_built_from_an_old_revision_is_refused() {
    let f = fixture();
    let (revision, _) = f.view("inbox");
    let options = || dict(&[("view", "inbox"), ("revision", &revision)]);
    let change = || vec![dict(&[("op", "create_task"), ("title", "Planned")])];

    // Someone else changes the Inbox in the window.
    f.db.add_task(&mut Task::new("elsewhere", "Typed in the window", 0.0))
        .unwrap();
    let before = f.everything();
    let stale = f.apply_as(&f.all, change(), options());
    named(error(stale), "StaleRevision");
    assert_eq!(f.everything(), before);

    let (fresh, _) = f.view("inbox");
    f.apply_as(
        &f.all,
        change(),
        dict(&[("view", "inbox"), ("revision", &fresh)]),
    )
    .unwrap();
    assert_eq!(f.view("inbox").1.len(), 2);
}

#[test]
fn each_change_needs_its_own_permission() {
    let f = fixture();
    let token = |name: &str, p: &[Permission]| {
        f.clients
            .add(name, Permissions::of(p))
            .unwrap()
            .as_str()
            .to_owned()
    };
    let creator = token("creator", &[Permission::Create]);
    let organiser = token("organiser", &[Permission::Organise]);
    let trasher = token("trasher", &[Permission::Trash]);
    let made = f
        .apply_as(
            &creator,
            vec![dict(&[("op", "create_task"), ("title", "Mine")])],
            Dict::new(),
        )
        .unwrap();
    let task = id_of(&made, 0);
    let area = id_of(
        &f.apply(vec![dict(&[("op", "create_area"), ("title", "Home")])])
            .unwrap(),
        0,
    );
    let before = f.everything();

    let cases: [(&str, Dict); 6] = [
        (&creator, dict(&[("op", "complete"), ("id", &task)])),
        (&creator, dict(&[("op", "trash"), ("id", &task)])),
        (&organiser, dict(&[("op", "create_task"), ("title", "No")])),
        (&organiser, dict(&[("op", "create_area"), ("title", "No")])),
        (&trasher, dict(&[("op", "trash"), ("id", &area)])),
        (
            &trasher,
            dict(&[("op", "update"), ("id", &task), ("title", "No")]),
        ),
    ];
    for (token, change) in cases {
        let refused = f.apply_as(
            token,
            vec![dict(&[("op", "create_task"), ("title", "Fine")]), change],
            Dict::new(),
        );
        assert_eq!(
            error(refused).0,
            "io.github.thestephenromano.Matters.Automation1.Error.PermissionDenied"
        );
    }
    // A revision check reads, so it needs Read.
    let (revision, _) = f.view("inbox");
    let unread = f.apply_as(
        &creator,
        vec![dict(&[("op", "create_task"), ("title", "No")])],
        dict(&[("view", "inbox"), ("revision", &revision)]),
    );
    named(error(unread), "PermissionDenied");
    assert_eq!(f.everything(), before);

    // Trash alone does trash a task; an area needs Areas too.
    f.apply_as(
        &trasher,
        vec![dict(&[("op", "trash"), ("id", &task)])],
        Dict::new(),
    )
    .unwrap();
}

#[test]
fn the_links_client_never_trashes_or_changes_areas() {
    let f = fixture();
    let links = f
        .clients
        .add_as(LINKS_CLIENT, "Links", every())
        .unwrap()
        .as_str()
        .to_owned();
    let made = f
        .apply_as(
            &links,
            vec![dict(&[("op", "create_task"), ("title", "From a link")])],
            Dict::new(),
        )
        .unwrap();
    let task = id_of(&made, 0);
    for change in [
        dict(&[("op", "trash"), ("id", &task)]),
        dict(&[("op", "create_area"), ("title", "No")]),
    ] {
        named(
            error(f.apply_as(&links, vec![change], Dict::new())),
            "PermissionDenied",
        );
    }
}

#[test]
fn apply_refuses_missing_unknown_and_revoked_tokens() {
    let f = fixture();
    let change = || vec![dict(&[("op", "create_task"), ("title", "No")])];
    named(error(f.apply_as("", change(), Dict::new())), "UnknownToken");
    let unknown = format!("matters1_{}", "0".repeat(64));
    named(
        error(f.apply_as(&unknown, change(), Dict::new())),
        "UnknownToken",
    );
    let gone = f.clients.add("gone", every()).unwrap().as_str().to_owned();
    f.clients.revoke("gone");
    named(
        error(f.apply_as(&gone, change(), Dict::new())),
        "RevokedToken",
    );
    assert!(f.view("inbox").1.is_empty());
}

#[test]
fn a_retried_request_returns_its_first_answer_and_changes_nothing() {
    let f = fixture();
    let request = || dict(&[("request", "plan-monday")]);
    let change = |title: &str| vec![dict(&[("op", "create_task"), ("title", title)])];

    let first = f.apply_as(&f.all, change("Once"), request()).unwrap();
    let again = f.apply_as(&f.all, change("Once"), request()).unwrap();
    assert_eq!(id_of(&first, 0), id_of(&again, 0));
    assert_eq!(f.view("inbox").1.len(), 1);

    let reused = f.apply_as(&f.all, change("Other"), request());
    named(error(reused), "InvalidArgument");

    // Another client's IDs are its own.
    let other = f.clients.add("other", every()).unwrap().as_str().to_owned();
    f.apply_as(&other, change("Theirs"), request()).unwrap();
    assert_eq!(f.view("inbox").1.len(), 2);
}

#[test]
fn batches_and_options_have_limits() {
    let f = fixture();
    named(error(f.apply(Vec::new())), "InvalidArgument");
    let too_many = (0..=MAX_CHANGES)
        .map(|_| dict(&[("op", "create_task"), ("title", "x")]))
        .collect();
    named(error(f.apply(too_many)), "InvalidArgument");
    let change = || vec![dict(&[("op", "create_task"), ("title", "x")])];
    let long = "r".repeat(matters_automation::MAX_REQUEST_ID + 1);
    for options in [
        dict(&[("request", &long)]),
        dict(&[("view", "inbox")]),
        dict(&[("colour", "red")]),
    ] {
        named(
            error(f.apply_as(&f.all, change(), options)),
            "InvalidArgument",
        );
    }
    assert!(f.view("inbox").1.is_empty());
}

#[test]
fn dates_that_read_differently_by_locale_are_refused() {
    let f = fixture();
    let made = f
        .apply(vec![dict(&[("op", "create_task"), ("title", "x")])])
        .unwrap();
    let id = id_of(&made, 0);
    let when = |text: &str| f.apply(vec![dict(&[("op", "update"), ("id", &id), ("when", text)])]);
    assert!(named(error(when("3/4")), "InvalidChange").contains("YYYY-MM-DD"));
    named(error(when("someday soon")), "InvalidChange");
    when("2026-12-03").unwrap();
    when("30/12").unwrap();
    assert_eq!(f.get(&id).start_date.as_deref(), Some("2026-12-30"));
    when("someday").unwrap();
    assert_eq!(f.get(&id).someday, Some(true));
}

#[test]
fn with_sync_on_a_batch_is_one_record_the_engine_agrees_with() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    db.create_area(&Area::new(uuid(9), "Home", 0.0)).unwrap();
    let f = fixture_on(db);
    let rows = || -> i64 {
        f.db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_journal", [], |r| r.get(0))?))
            .unwrap()
    };
    let journal = rows();
    f.apply(vec![
        dict(&[
            ("op", "create_project"),
            ("title", "Garden"),
            ("area_id", &uuid(9)),
            ("ref", "p"),
        ]),
        dict(&[
            ("op", "create_task"),
            ("title", "Dig"),
            ("project_id", "ref:p"),
            ("ref", "t"),
        ]),
        dict(&[
            ("op", "create_task"),
            ("title", "Plant"),
            ("project_id", "ref:p"),
        ]),
        dict(&[("op", "tag"), ("id", "ref:t"), ("tag", "errand")]),
    ])
    .unwrap();
    assert_eq!(rows(), journal + 1);
    f.db.verify_rows_match_engine().unwrap();
}
