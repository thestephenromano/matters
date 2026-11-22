//! Matters on the session bus (#203): one instance, started on demand.
//!
//! The app owns its ID as a bus name ([`BUS_NAME`]) and serves two
//! interfaces:
//! - **`org.freedesktop.Application`**, the standard one GApplication
//!   implements and `DBusActivatable` desktop entries use: `Activate` shows
//!   and raises the window, `Open` does the same until `matters://` links
//!   arrive (#69), and `ActivateAction` is not supported.
//! - **The automation interface** (`matters_automation`).
//!
//! Starting Matters while it is already running hands over to the running
//! instance: the new process asks it to `Activate` and exits. A D-Bus call
//! while Matters is closed starts it with [`SERVICE_FLAG`] and no window;
//! that instance quits once nothing has called it for [`IDLE_EXIT`], unless a
//! window is open or sync is on (decided on #203). Without a session bus the
//! app runs as before, alone and unreachable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use matters_automation::{Automation, MemoryClients, BUS_NAME, OBJECT_PATH};
use matters_core::Database;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::{interface, Connection, OwnedVariant, Variant};

/// The flag a D-Bus service file starts the app with: no window. The name is
/// GApplication's, which desktops already use for `DBusActivatable` apps.
pub const SERVICE_FLAG: &str = "--gapplication-service";

/// How long an instance started by a call stays up after the last one. Long
/// enough that a burst of calls (an agent planning the day, a script) starts
/// Qt once, short enough not to hold a Qt process all day.
pub const IDLE_EXIT: Duration = Duration::from_secs(5 * 60);

/// Where `org.freedesktop.Application` is served: the bus name as a path.
pub const APP_PATH: &str = "/io/github/thestephenromano/Matters";

/// Milliseconds since the Unix epoch of the last call to either interface,
/// or of startup.
static LAST_CALL: AtomicI64 = AtomicI64::new(0);

/// Started with [`SERVICE_FLAG`].
static BACKGROUND: AtomicBool = AtomicBool::new(false);

/// Record how the app was started, before the window loads.
pub fn set_started_in_background(background: bool) {
    BACKGROUND.store(background, Ordering::SeqCst);
}

pub fn started_in_background() -> bool {
    BACKGROUND.load(Ordering::SeqCst)
}

/// What `Activate` does: set by the controller once the window exists.
static ACTIVATE_LISTENER: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn touch() {
    LAST_CALL.store(now_ms(), Ordering::SeqCst);
}

/// Route `Activate` to the window, from any thread.
pub fn on_activate(listener: Box<dyn Fn() + Send>) {
    *ACTIVATE_LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
}

fn activate() {
    touch();
    if let Some(listener) = ACTIVATE_LISTENER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        listener();
    }
}

/// Whether an instance with no window should quit now: nothing has called it
/// for [`IDLE_EXIT`] and sync is off.
pub fn idle_expired(sync_on: bool) -> bool {
    idle_expired_at(now_ms(), LAST_CALL.load(Ordering::SeqCst), sync_on)
}

fn idle_expired_at(now: i64, last_call: i64, sync_on: bool) -> bool {
    !sync_on && now.saturating_sub(last_call) >= IDLE_EXIT.as_millis() as i64
}

/// `org.freedesktop.Application`.
pub struct DesktopApp;

#[interface(interface = "org.freedesktop.Application")]
impl DesktopApp {
    fn activate(&self, _platform_data: HashMap<String, OwnedVariant>) {
        activate();
    }

    /// Links (`matters://`, #69) are not handled yet: show the window.
    fn open(&self, _uris: Vec<String>, _platform_data: HashMap<String, OwnedVariant>) {
        activate();
    }

    fn activate_action(
        &self,
        action_name: String,
        _parameter: Vec<OwnedVariant>,
        _platform_data: HashMap<String, OwnedVariant>,
    ) -> zbus::fdo::Result<()> {
        touch();
        Err(zbus::fdo::Error::NotSupported(format!(
            "Matters has no action {action_name:?}"
        )))
    }
}

/// How this process starts.
pub enum Claim {
    /// This is the instance. Keep the connection (if there is a bus) for as
    /// long as the app runs.
    Primary(Option<Connection>),
    /// Another instance was running and has been asked to show its window
    /// (unless this start was a [`SERVICE_FLAG`] one); exit.
    Running,
}

/// Claim the bus name on the session bus, or hand over to the instance that
/// holds it.
pub fn claim(background: bool) -> Claim {
    touch();
    match zbus::block_on(claim_on(None, background)) {
        Ok(claim) => claim,
        Err(err) => {
            eprintln!("matters: not on the session bus, so not reachable by other programs: {err}");
            Claim::Primary(None)
        }
    }
}

/// [`claim`] on the bus at `address`, or the session bus.
pub async fn claim_on(address: Option<&str>, background: bool) -> zbus::Result<Claim> {
    let builder = match address {
        Some(address) => zbus::connection::Builder::address(address),
        None => zbus::connection::Builder::session(),
    };
    // Served before the name is owned, so no call can arrive first.
    let conn = builder.serve_at(APP_PATH, DesktopApp).build().await?;
    // zbus reports another owner, with DO_NOT_QUEUE, as `NameTaken`.
    match conn
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DO_NOT_QUEUE)
        .await
    {
        Ok(RequestNameReply::PrimaryOwner) => return Ok(Claim::Primary(Some(conn))),
        Ok(_) | Err(zbus::Error::NameTaken) => {}
        Err(err) => return Err(err),
    }
    if !background {
        let platform_data: HashMap<String, Variant> = HashMap::new();
        conn.call_method(
            Some(BUS_NAME),
            APP_PATH,
            Some("org.freedesktop.Application"),
            "Activate",
            &(platform_data,),
        )
        .await?;
    }
    Ok(Claim::Running)
}

/// Serve the automation interface over the app's database. No client is
/// allowed until preferences can allow one (#200): agent access is off on a
/// new install, so every call is refused with `UnknownToken`.
pub fn serve_automation(conn: &Connection, db: Database) -> zbus::Result<()> {
    let automation = Automation::new(db, Arc::new(MemoryClients::default())).with_activity(touch);
    zbus::block_on(conn.object_server().at(OBJECT_PATH, automation))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_instance_quits_after_the_idle_period_unless_sync_is_on() {
        let period = IDLE_EXIT.as_millis() as i64;
        assert!(!idle_expired_at(1_000 + period - 1, 1_000, false));
        assert!(idle_expired_at(1_000 + period, 1_000, false));
        assert!(!idle_expired_at(1_000 + period * 10, 1_000, true));
        // A clock that went back never makes it quit early.
        assert!(!idle_expired_at(0, 1_000, false));
    }

    /// A private bus daemon for one test, stopped when dropped.
    struct Bus {
        child: std::process::Child,
        address: String,
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn private_bus() -> Bus {
        use std::io::BufRead;
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon (ci/fedora-deps.sh installs it)");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Bus {
            child,
            address: line.trim().to_owned(),
        }
    }

    // One test, so the process-wide listener and clock aren't shared
    // between threads.
    #[test]
    fn a_second_start_hands_over_and_the_first_serves_activation_and_automation() {
        let bus = private_bus();
        let (tx, rx) = std::sync::mpsc::channel();
        on_activate(Box::new(move || {
            let _ = tx.send(());
        }));

        let Claim::Primary(Some(first)) =
            zbus::block_on(claim_on(Some(&bus.address), false)).unwrap()
        else {
            panic!("the first start should own the name");
        };
        let db = Database::open_in_memory().unwrap();
        serve_automation(&first, db).unwrap();

        // A background start while it runs: hands over without showing.
        let started = zbus::block_on(claim_on(Some(&bus.address), true)).unwrap();
        assert!(matches!(started, Claim::Running));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        // A normal start: hands over and the first shows its window.
        let started = zbus::block_on(claim_on(Some(&bus.address), false)).unwrap();
        assert!(matches!(started, Claim::Running));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the running instance was asked to activate");

        // The automation interface answers on the name, and a new install
        // allows no client.
        let caller = zbus::block_on(
            zbus::connection::Builder::address(bus.address.as_str()).build(),
        )
        .unwrap();
        LAST_CALL.store(0, Ordering::SeqCst);
        let reply = zbus::block_on(caller.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(matters_automation::INTERFACE),
            "GetView",
            &("", "today"),
        ));
        match reply {
            Err(zbus::Error::MethodError(name, _, _)) => assert_eq!(
                name.as_str(),
                "io.github.thestephenromano.Matters.Automation1.Error.UnknownToken"
            ),
            other => panic!("{other:?}"),
        }
        // That call counted as activity.
        assert!(!idle_expired(false));

        let refused = zbus::block_on(caller.call_method(
            Some(BUS_NAME),
            APP_PATH,
            Some("org.freedesktop.Application"),
            "ActivateAction",
            &(
                "quit",
                Vec::<Variant>::new(),
                HashMap::<String, Variant>::new(),
            ),
        ));
        let refused = format!("{:?}", refused.expect_err("no actions are supported"));
        assert!(refused.contains("NotSupported"), "{refused}");
        drop(first);
    }
}
