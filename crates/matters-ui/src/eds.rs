//! Calendar events from Evolution Data Server (#62; EDS through `libecal`,
//! decided there).
//!
//! `libecal` is loaded when Matters starts rather than linked, so Matters
//! builds without EDS's development files and runs where EDS isn't
//! installed: the source then says why there are no events. Every call is
//! made on the calendar worker thread (`crate::calendar`): the registry and
//! clients belong to the main context that thread pushes, which is pumped
//! before each read so changes to the calendar list arrive.
//!
//! Ownership follows the libraries' annotations: `list_enabled`,
//! `connect_sync`, `dup_color` and `i_cal_time_clone` return references the
//! caller frees; every other pointer read here is borrowed.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use matters_core::calendar::{Calendar, CalendarSource, Event, Unavailable};

type Gboolean = c_int;
type Pointer = *mut c_void;

#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

#[repr(C)]
struct GList {
    data: Pointer,
    next: *mut GList,
    prev: *mut GList,
}

/// `ECalRecurInstanceCb`.
type InstanceCallback = unsafe extern "C" fn(
    component: Pointer,
    start: Pointer,
    end: Pointer,
    data: Pointer,
    cancellable: Pointer,
    error: *mut *mut GError,
) -> Gboolean;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> Pointer;
    fn dlsym(handle: Pointer, symbol: *const c_char) -> Pointer;
}

const RTLD_NOW: c_int = 2;
/// `E_CAL_CLIENT_SOURCE_TYPE_EVENTS`.
const SOURCE_TYPE_EVENTS: c_int = 0;
/// How long a connection waits for its backend to say it is online. Local
/// calendars never do, so this is spent in full on each first connection;
/// 0 would wait forever.
const CONNECT_WAIT_SECONDS: u32 = 1;
/// The sonames of `libecal-2.0` shipped by the distributions we know of
/// (EDS 3.60 has `.so.3`; Ubuntu 24.04's 3.52 has `.so.2`).
const LIBECAL: [&str; 3] = ["libecal-2.0.so.3", "libecal-2.0.so.2", "libecal-2.0.so.1"];
const CALENDAR_EXTENSION: &CStr = c"Calendar";

macro_rules! api {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// The functions used, looked up in `libecal` and the libraries it
        /// links (`libedataserver`, `libical-glib`, GLib and GObject).
        #[allow(non_snake_case)]
        struct Api {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl Api {
            /// # Safety
            /// `handle` is a loaded `libecal`, and every signature above
            /// matches the library's.
            unsafe fn load(handle: Pointer) -> Result<Api, String> {
                Ok(Api {
                    $($name: {
                        let name = CString::new(stringify!($name)).expect("no NUL");
                        let symbol = unsafe { dlsym(handle, name.as_ptr()) };
                        if symbol.is_null() {
                            return Err(format!("{} is missing", stringify!($name)));
                        }
                        unsafe {
                            std::mem::transmute::<Pointer, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol)
                        }
                    },)*
                })
            }
        }
    };
}

api! {
    g_main_context_new: fn() -> Pointer;
    g_main_context_push_thread_default: fn(Pointer);
    g_main_context_iteration: fn(Pointer, Gboolean) -> Gboolean;
    g_list_free_full: fn(*mut GList, unsafe extern "C" fn(Pointer));
    g_object_unref: fn(Pointer);
    g_error_free: fn(*mut GError);
    g_free: fn(Pointer);
    e_source_registry_new_sync: fn(Pointer, *mut *mut GError) -> Pointer;
    e_source_registry_list_enabled: fn(Pointer, *const c_char) -> *mut GList;
    e_source_get_uid: fn(Pointer) -> *const c_char;
    e_source_get_display_name: fn(Pointer) -> *const c_char;
    e_source_get_extension: fn(Pointer, *const c_char) -> Pointer;
    e_source_selectable_dup_color: fn(Pointer) -> *mut c_char;
    e_cal_client_connect_sync: fn(Pointer, c_int, u32, Pointer, *mut *mut GError) -> Pointer;
    e_cal_client_generate_instances_sync: fn(Pointer, i64, i64, Pointer, InstanceCallback, Pointer);
    i_cal_component_get_summary: fn(Pointer) -> *const c_char;
    i_cal_component_get_uid: fn(Pointer) -> *const c_char;
    i_cal_time_is_date: fn(Pointer) -> Gboolean;
    i_cal_time_get_timezone: fn(Pointer) -> Pointer;
    i_cal_time_clone: fn(Pointer) -> Pointer;
    i_cal_time_convert_to_zone_inplace: fn(Pointer, Pointer);
    i_cal_timezone_get_utc_timezone: fn() -> Pointer;
    i_cal_time_get_year: fn(Pointer) -> c_int;
    i_cal_time_get_month: fn(Pointer) -> c_int;
    i_cal_time_get_day: fn(Pointer) -> c_int;
    i_cal_time_get_hour: fn(Pointer) -> c_int;
    i_cal_time_get_minute: fn(Pointer) -> c_int;
    i_cal_time_get_second: fn(Pointer) -> c_int;
}

/// A borrowed C string as text; empty for NULL.
///
/// # Safety
/// `text` is NULL or a valid NUL-terminated string.
unsafe fn text(text: *const c_char) -> String {
    if text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Take a `GError`'s message, freeing it.
fn take_error(api: &Api, error: *mut GError) -> String {
    if error.is_null() {
        return "unknown error".into();
    }
    // SAFETY: a GError set by a call, freed once here.
    unsafe {
        let message = text((*error).message);
        (api.g_error_free)(error);
        message
    }
}

/// Connect to calendar `source` (borrowed).
fn connect(api: &Api, source: Pointer) -> Result<Pointer, String> {
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: `source` is a live ESource from the registry's list.
    let client = unsafe {
        (api.e_cal_client_connect_sync)(
            source,
            SOURCE_TYPE_EVENTS,
            CONNECT_WAIT_SECONDS,
            ptr::null_mut(),
            &mut error,
        )
    };
    if client.is_null() {
        return Err(take_error(api, error));
    }
    Ok(client)
}

/// The user's calendars in Evolution Data Server.
pub struct EdsSource {
    api: Api,
    /// The registry and the main context it belongs to, once connected.
    registry: Option<(Pointer, Pointer)>,
    /// A connected client for each calendar, by id.
    clients: BTreeMap<String, Pointer>,
}

// Every pointer is used only from the thread that reads the source (the
// calendar worker), which also creates them: the source moves to that thread
// before its first call.
unsafe impl Send for EdsSource {}

impl EdsSource {
    /// Load `libecal`, or say why it can't be.
    pub fn load() -> Result<EdsSource, Unavailable> {
        let handle = LIBECAL.iter().find_map(|name| {
            let name = CString::new(*name).expect("no NUL");
            let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
            (!handle.is_null()).then_some(handle)
        });
        let Some(handle) = handle else {
            return Err(Unavailable(
                "Calendar events come from Evolution Data Server, which isn't installed.".into(),
            ));
        };
        // SAFETY: the signatures match libecal 3.x and its dependencies.
        let api = unsafe { Api::load(handle) }.map_err(|missing| {
            Unavailable(format!(
                "This version of Evolution Data Server can't be used ({missing})."
            ))
        })?;
        Ok(EdsSource {
            api,
            registry: None,
            clients: BTreeMap::new(),
        })
    }

    /// The registry, connecting on first use. Pumps its main context first
    /// so it has caught up with changes.
    fn registry(&mut self) -> Result<Pointer, Unavailable> {
        if let Some((registry, context)) = self.registry {
            // SAFETY: the context this thread pushed.
            unsafe { while (self.api.g_main_context_iteration)(context, 0) != 0 {} }
            return Ok(registry);
        }
        let mut error: *mut GError = ptr::null_mut();
        // SAFETY: a new context, pushed for this (the worker) thread for
        // good, so the registry and clients made here deliver to it.
        let registry = unsafe {
            let context = (self.api.g_main_context_new)();
            (self.api.g_main_context_push_thread_default)(context);
            let registry = (self.api.e_source_registry_new_sync)(ptr::null_mut(), &mut error);
            if !registry.is_null() {
                self.registry = Some((registry, context));
            }
            registry
        };
        if registry.is_null() {
            return Err(Unavailable(format!(
                "Evolution Data Server couldn't be reached: {}.",
                take_error(&self.api, error)
            )));
        }
        Ok(registry)
    }
}

impl CalendarSource for EdsSource {
    fn calendars(&mut self) -> Result<Vec<Calendar>, Unavailable> {
        let registry = self.registry()?;
        let api = &self.api;
        let mut calendars = Vec::new();
        let mut connected = BTreeMap::new();
        // SAFETY: the list and its sources are owned here and freed below;
        // the strings are borrowed from sources still alive while read.
        unsafe {
            let list = (api.e_source_registry_list_enabled)(registry, CALENDAR_EXTENSION.as_ptr());
            let mut node = list;
            while !node.is_null() {
                let source = (*node).data;
                node = (*node).next;
                let id = text((api.e_source_get_uid)(source));
                let name = text((api.e_source_get_display_name)(source));
                let extension = (api.e_source_get_extension)(source, CALENDAR_EXTENSION.as_ptr());
                let color = if extension.is_null() {
                    None
                } else {
                    let color = (api.e_source_selectable_dup_color)(extension);
                    let read = text(color);
                    (api.g_free)(color.cast());
                    (!read.is_empty()).then_some(read)
                };
                let client = match self.clients.remove(&id) {
                    Some(client) => client,
                    None => match connect(api, source) {
                        Ok(client) => client,
                        Err(err) => {
                            eprintln!("matters: calendar '{name}' can't be read: {err}");
                            continue;
                        }
                    },
                };
                connected.insert(id.clone(), client);
                calendars.push(Calendar { id, name, color });
            }
            (api.g_list_free_full)(list, api.g_object_unref);
            // Calendars that are gone or turned off.
            for client in std::mem::replace(&mut self.clients, connected).into_values() {
                (api.g_object_unref)(client);
            }
        }
        calendars.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        Ok(calendars)
    }

    fn events(&mut self, from: Date, until: Date) -> Result<Vec<Event>, Unavailable> {
        let zone = TimeZone::system();
        let seconds = |day: Date| {
            day.to_zoned(zone.clone())
                .map(|z| z.timestamp().as_second())
                .map_err(|err| Unavailable(format!("The calendar's days can't be read: {err}.")))
        };
        let (start, end) = (seconds(from)?, seconds(until)?);
        let mut events = Vec::new();
        for (calendar_id, client) in &self.clients {
            let mut found = Found {
                api: &self.api,
                zone: &zone,
                calendar_id,
                events: Vec::new(),
            };
            // SAFETY: a connected client; `found` outlives the call, which
            // runs the callback synchronously.
            unsafe {
                (self.api.e_cal_client_generate_instances_sync)(
                    *client,
                    start,
                    end,
                    ptr::null_mut(),
                    instance,
                    (&mut found as *mut Found).cast(),
                );
            }
            events.append(&mut found.events);
        }
        Ok(events)
    }
}

impl Drop for EdsSource {
    fn drop(&mut self) {
        // SAFETY: references this source owns.
        unsafe {
            for client in std::mem::take(&mut self.clients).into_values() {
                (self.api.g_object_unref)(client);
            }
            if let Some((registry, _)) = self.registry.take() {
                (self.api.g_object_unref)(registry);
            }
        }
    }
}

/// What the instance callback collects for one calendar.
struct Found<'a> {
    api: &'a Api,
    zone: &'a TimeZone,
    calendar_id: &'a str,
    events: Vec<Event>,
}

impl Found<'_> {
    /// An instance's start or end (borrowed `ICalTime`) in local time: a
    /// date is midnight; a floating time is taken as local; any other is
    /// converted from its zone.
    ///
    /// # Safety
    /// `time` is a live ICalTime.
    unsafe fn local(&self, time: Pointer) -> Option<DateTime> {
        let api = self.api;
        let fields = |t: Pointer| unsafe {
            let n = |f: unsafe extern "C" fn(Pointer) -> c_int| f(t);
            (
                n(api.i_cal_time_get_year),
                n(api.i_cal_time_get_month),
                n(api.i_cal_time_get_day),
                n(api.i_cal_time_get_hour),
                n(api.i_cal_time_get_minute),
                n(api.i_cal_time_get_second),
            )
        };
        let civil = |(y, mo, d, h, mi, s): (c_int, c_int, c_int, c_int, c_int, c_int)| {
            DateTime::new(
                i16::try_from(y).ok()?,
                i8::try_from(mo).ok()?,
                i8::try_from(d).ok()?,
                i8::try_from(h).ok()?,
                i8::try_from(mi).ok()?,
                i8::try_from(s).ok()?,
                0,
            )
            .ok()
        };
        unsafe {
            if (api.i_cal_time_is_date)(time) != 0 {
                let (y, mo, d, ..) = fields(time);
                return civil((y, mo, d, 0, 0, 0));
            }
            // Converting a floating time only relabels it, so it must be
            // told apart first.
            if (api.i_cal_time_get_timezone)(time).is_null() {
                return civil(fields(time));
            }
            let utc = (api.i_cal_time_clone)(time);
            (api.i_cal_time_convert_to_zone_inplace)(utc, (api.i_cal_timezone_get_utc_timezone)());
            let read = civil(fields(utc));
            (api.g_object_unref)(utc);
            let instant = read?.to_zoned(TimeZone::UTC).ok()?;
            Some(instant.with_time_zone(self.zone.clone()).datetime())
        }
    }
}

/// `ECalRecurInstanceCb`: one occurrence, kept in the `Found` at `data`.
/// Always continues; nothing here may unwind into C.
unsafe extern "C" fn instance(
    component: Pointer,
    start: Pointer,
    end: Pointer,
    data: Pointer,
    _cancellable: Pointer,
    _error: *mut *mut GError,
) -> Gboolean {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `data` is the `Found` passed to generate_instances_sync;
        // the component and times are live for this call.
        unsafe {
            let found = &mut *data.cast::<Found>();
            let api = found.api;
            let Some(start_at) = found.local(start) else {
                return;
            };
            let end_at = if end.is_null() {
                None
            } else {
                found.local(end)
            };
            let all_day = (api.i_cal_time_is_date)(start) != 0;
            let uid = text((api.i_cal_component_get_uid)(component));
            let title = text((api.i_cal_component_get_summary)(component));
            found.events.push(Event {
                id: format!("{}/{uid}/{start_at}", found.calendar_id),
                calendar_id: found.calendar_id.to_string(),
                title,
                start: start_at,
                end: end_at.unwrap_or(start_at),
                all_day,
            });
        }
    }));
    1
}

/// Against the desktop's own Evolution Data Server, so not run by default:
/// `cargo test -p matters-ui --lib eds -- --ignored`.
#[cfg(test)]
mod desktop_tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    #[ignore = "needs Evolution Data Server"]
    fn reads_the_desktop_calendars() {
        let mut source = EdsSource::load().expect("libecal loads");
        let calendars = source.calendars().expect("the registry answers");
        eprintln!("calendars: {calendars:?}");
        let today = matters_core::local_today();
        let events = source
            .events(today, today.tomorrow().unwrap())
            .expect("events are read");
        eprintln!("events today: {events:?}");
        // Read again: the clients are kept, and the context pumped.
        assert_eq!(source.calendars().unwrap(), calendars);
    }

    #[test]
    #[ignore = "needs Evolution Data Server"]
    fn instance_times_become_local_times() {
        let source = EdsSource::load().expect("libecal loads");
        let handle = LIBECAL
            .iter()
            .find_map(|name| {
                let name = CString::new(*name).unwrap();
                let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
                (!handle.is_null()).then_some(handle)
            })
            .unwrap();
        let sym = |name: &str| {
            let name = CString::new(name).unwrap();
            let symbol = unsafe { dlsym(handle, name.as_ptr()) };
            assert!(!symbol.is_null());
            symbol
        };
        type New = unsafe extern "C" fn(*const c_char) -> Pointer;
        type Builtin = unsafe extern "C" fn(*const c_char) -> Pointer;
        type SetZone = unsafe extern "C" fn(Pointer, Pointer);
        let (new, builtin, set_zone) = unsafe {
            (
                std::mem::transmute::<Pointer, New>(sym("i_cal_time_new_from_string")),
                std::mem::transmute::<Pointer, Builtin>(sym("i_cal_timezone_get_builtin_timezone")),
                std::mem::transmute::<Pointer, SetZone>(sym("i_cal_time_set_timezone")),
            )
        };
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        let found = Found {
            api: &source.api,
            zone: &tokyo,
            calendar_id: "test",
            events: Vec::new(),
        };
        let time = |text: &CStr| unsafe { new(text.as_ptr()) };
        unsafe {
            // A date is midnight on that day, wherever the user is.
            let day = time(c"20261121");
            assert_eq!(found.local(day), Some(date(2026, 11, 21).at(0, 0, 0, 0)));
            // A floating time is the same wall time anywhere.
            let floating = time(c"20261121T090000");
            assert_eq!(
                found.local(floating),
                Some(date(2026, 11, 21).at(9, 0, 0, 0))
            );
            // 09:00 in Berlin (UTC+1 in November) is 17:00 in Tokyo.
            let zoned = time(c"20261121T090000");
            set_zone(zoned, builtin(c"Europe/Berlin".as_ptr()));
            assert_eq!(found.local(zoned), Some(date(2026, 11, 21).at(17, 0, 0, 0)));
            // And the instance itself is left as it was.
            assert_eq!((source.api.i_cal_time_get_hour)(zoned), 9);
            // 09:00 UTC is 18:00 in Tokyo.
            let utc = time(c"20261121T090000Z");
            assert_eq!(found.local(utc), Some(date(2026, 11, 21).at(18, 0, 0, 0)));
            for t in [day, floating, zoned, utc] {
                (source.api.g_object_unref)(t);
            }
        }
    }
}
