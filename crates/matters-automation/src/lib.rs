//! Matters' automation interface (#203): one documented, versioned D-Bus
//! interface for everything outside the app (the command line, `matters://`
//! links, global capture, the MCP server). Either front end serves it over
//! its own database; the interface is described in docs/AUTOMATION.md.
//!
//! The version is in the interface's name. A change that breaks callers
//! gets a new interface (`Automation2`), and the old one keeps working for
//! at least one release.

mod changes;
pub mod clients;
pub mod error;
pub mod items;
mod service;

pub use changes::{MAX_CHANGES, MAX_REQUEST_ID};
pub use clients::{Client, Clients, MemoryClients, Permission, Permissions, Token, LINKS_CLIENT};
pub use error::Error;
pub use items::Item;
pub use service::{Automation, TodayEvents, LOGBOOK_LIMIT};

/// The well-known bus name: the app's ID, as `DBusActivatable` and Flatpak
/// require.
pub const BUS_NAME: &str = "io.github.thestephenromano.Matters";
/// Where the interface is served.
pub const OBJECT_PATH: &str = "/io/github/thestephenromano/Matters/Automation";
pub const INTERFACE: &str = "io.github.thestephenromano.Matters.Automation1";
