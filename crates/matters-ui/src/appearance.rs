//! The desktop's accent colour and contrast preference (#76), from the XDG
//! desktop portal's appearance settings (`org.freedesktop.portal.Settings`,
//! namespace `org.freedesktop.appearance`), read over D-Bus with zbus.
//!
//! The portal is read on a worker thread ([`start_portal`]), which then
//! follows its `SettingChanged` signal; the latest values are kept here
//! ([`current`]) and a listener is told of each change ([`on_change`]).
//! Without a portal, or one that doesn't offer the settings, nothing is
//! published: there is no accent and no contrast preference, and Matters
//! keeps its own palette. Tests publish values themselves ([`publish`]).

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// A colour from the portal: red, green and blue, each 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
}

impl Rgb {
    /// `#rrggbb`.
    pub fn hex(self) -> String {
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(self.red),
            byte(self.green),
            byte(self.blue)
        )
    }

    /// The colour `#rrggbb` names, if it is one.
    pub fn from_hex(text: &str) -> Option<Rgb> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let part = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb {
            red: f64::from(part(0)?) / 255.0,
            green: f64::from(part(2)?) / 255.0,
            blue: f64::from(part(4)?) / 255.0,
        })
    }
}

/// What the desktop asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Appearance {
    /// The accent colour, when the desktop has one.
    pub accent: Option<Rgb>,
    /// Whether the user asked for higher contrast.
    pub high_contrast: bool,
}

/// The portal's `accent-color`: three values from 0 to 1. Any outside that
/// range means the desktop has no accent, as the portal specifies.
pub fn accent_from_portal(red: f64, green: f64, blue: f64) -> Option<Rgb> {
    let valid = |v: f64| (0.0..=1.0).contains(&v);
    (valid(red) && valid(green) && valid(blue)).then_some(Rgb { red, green, blue })
}

/// The portal's `contrast`: 1 asks for higher contrast; 0, or anything
/// else, doesn't.
pub fn high_contrast_from_portal(value: u32) -> bool {
    value == 1
}

struct State {
    appearance: Appearance,
    listener: Option<Box<dyn Fn() + Send>>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> MutexGuard<'static, State> {
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                appearance: Appearance::default(),
                listener: None,
            })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The latest values: none until something is published.
pub fn current() -> Appearance {
    state().appearance
}

/// Call `listener` whenever the values change (one listener: the window's
/// controller). It runs on the thread that published them.
pub fn on_change(listener: Box<dyn Fn() + Send>) {
    state().listener = Some(listener);
}

/// Keep `appearance`, and tell the listener if it differs from what was
/// kept.
pub fn publish(appearance: Appearance) {
    let state = &mut *state();
    if state.appearance == appearance {
        return;
    }
    state.appearance = appearance;
    if let Some(listener) = &state.listener {
        listener();
    }
}

/// Read the portal on a worker thread, then follow its changes. Says why
/// on stderr and stops when there is no portal; nothing is published then.
pub fn start_portal() {
    let started = std::thread::Builder::new()
        .name("matters-appearance".into())
        .spawn(|| {
            if let Err(err) = zbus::block_on(portal::follow()) {
                eprintln!(
                    "matters: the desktop's accent colour and contrast aren't followed: {err}"
                );
            }
        });
    if let Err(err) = started {
        eprintln!("matters: the desktop's accent colour and contrast aren't followed: {err}");
    }
}

mod portal {
    use super::{accent_from_portal, high_contrast_from_portal, publish, Appearance};
    use std::pin::Pin;
    use zbus::{OwnedVariant, Variant};

    const NAMESPACE: &str = "org.freedesktop.appearance";

    /// A setting's value, under however many variants the portal wraps it
    /// in (`Read`, the older call, adds one).
    fn innermost<'v, 'a>(value: &'v Variant<'a>) -> &'v Variant<'a> {
        match value {
            Variant::Variant(inner) => innermost(inner),
            other => other,
        }
    }

    fn accent(value: &Variant<'_>) -> Option<super::Rgb> {
        match innermost(value) {
            Variant::Structure(s) => match s.fields() {
                [Variant::F64(r), Variant::F64(g), Variant::F64(b)] => {
                    accent_from_portal(*r, *g, *b)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn contrast(value: &Variant<'_>) -> bool {
        matches!(innermost(value), Variant::U32(v) if high_contrast_from_portal(*v))
    }

    async fn read(proxy: &zbus::Proxy<'_>, key: &str) -> Option<OwnedVariant> {
        if let Ok(value) = proxy.call("ReadOne", &(NAMESPACE, key)).await {
            return Some(value);
        }
        proxy.call("Read", &(NAMESPACE, key)).await.ok()
    }

    pub async fn follow() -> zbus::Result<()> {
        let conn = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )
        .await?;
        // Subscribed before reading, so no change is missed in between.
        let mut changes = proxy.receive_signal("SettingChanged").await?;
        let mut now = Appearance {
            accent: read(&proxy, "accent-color").await.and_then(|v| accent(&v)),
            high_contrast: read(&proxy, "contrast").await.is_some_and(|v| contrast(&v)),
        };
        publish(now);
        loop {
            let next = std::future::poll_fn(|cx| {
                futures_core::Stream::poll_next(Pin::new(&mut changes), cx)
            })
            .await;
            let Some(message) = next else {
                return Ok(());
            };
            let Ok((namespace, key, value)) = message
                .body()
                .deserialize::<(String, String, OwnedVariant)>()
            else {
                continue;
            };
            if namespace != NAMESPACE {
                continue;
            }
            match key.as_str() {
                "accent-color" => now.accent = accent(&value),
                "contrast" => now.high_contrast = contrast(&value),
                _ => continue,
            }
            publish(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_portal_accent_is_a_colour_unless_it_is_out_of_range() {
        let accent = accent_from_portal(0.2, 0.4, 1.0).expect("a colour");
        assert_eq!(accent.hex(), "#3366ff");
        assert_eq!(
            accent_from_portal(0.0, 0.0, 0.0).map(Rgb::hex),
            Some("#000000".into())
        );
        assert_eq!(
            accent_from_portal(1.0, 1.0, 1.0).map(Rgb::hex),
            Some("#ffffff".into())
        );
        // Out of range means unset.
        assert_eq!(accent_from_portal(-1.0, -1.0, -1.0), None);
        assert_eq!(accent_from_portal(0.5, 1.5, 0.5), None);
        assert_eq!(accent_from_portal(0.5, 0.5, -0.01), None);
        assert_eq!(accent_from_portal(f64::NAN, 0.5, 0.5), None);
    }

    #[test]
    fn only_one_asks_for_higher_contrast() {
        assert!(high_contrast_from_portal(1));
        assert!(!high_contrast_from_portal(0));
        assert!(!high_contrast_from_portal(2));
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(
            Rgb::from_hex("#1864ab").map(Rgb::hex),
            Some("#1864ab".into())
        );
        assert_eq!(
            Rgb::from_hex("#1864AB").map(Rgb::hex),
            Some("#1864ab".into())
        );
        assert_eq!(Rgb::from_hex("1864ab"), None);
        assert_eq!(Rgb::from_hex("#1864a"), None);
        assert_eq!(Rgb::from_hex("#gg64ab"), None);
        assert_eq!(Rgb::from_hex("#é864a"), None);
    }

    /// Reads this desktop's portal: run with `--ignored` in a desktop
    /// session that has one.
    #[test]
    #[ignore = "needs a desktop session with the XDG desktop portal"]
    fn the_desktop_portal_is_read() {
        start_portal();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while current() == Appearance::default() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let now = current();
        eprintln!(
            "accent {:?}, high contrast {}",
            now.accent.map(Rgb::hex),
            now.high_contrast
        );
        assert!(now.accent.is_some(), "this desktop reports an accent");
    }

    #[test]
    fn a_change_is_told_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let told = Arc::new(AtomicUsize::new(0));
        let counter = told.clone();
        on_change(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let red = Appearance {
            accent: Rgb::from_hex("#e01b24"),
            high_contrast: false,
        };
        publish(red);
        publish(red);
        assert_eq!(current(), red);
        assert_eq!(
            told.load(Ordering::SeqCst),
            1,
            "the same values again aren't a change"
        );
        publish(Appearance::default());
        assert_eq!(told.load(Ordering::SeqCst), 2);
        assert_eq!(current(), Appearance::default());
    }
}
