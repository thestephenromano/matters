//! Per-device settings (#72), kept in a TOML file in the user's
//! configuration directory (`$XDG_CONFIG_HOME/matters/settings.toml`).
//!
//! Settings that change what the data means would belong with the data (and
//! sync with it); these change only how this device shows it. A missing file
//! means the defaults, which keep the behaviour Matters had before the
//! setting existed. A file that can't be read or understood also gives the
//! defaults, with a message for the user ([`SettingsFile::load`]).

use std::path::{Path, PathBuf};

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::error::{MattersError, Result};

/// When finished tasks move from their lists to the Logbook (#67).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogPolicy {
    /// A task ticked in a list stays there, ticked, until the user leaves
    /// the list (SPEC §3, `Visit`).
    #[default]
    WhenLeaving,
    /// A ticked task leaves its list at once.
    Immediately,
    /// Tasks finished today stay in their lists until the day is over.
    Daily,
    /// Finished tasks stay in their lists until the user logs them (Log
    /// Completed).
    Manually,
}

impl LogPolicy {
    /// Every policy, in the order a frontend offers them.
    pub const ALL: [LogPolicy; 4] = [
        LogPolicy::WhenLeaving,
        LogPolicy::Immediately,
        LogPolicy::Daily,
        LogPolicy::Manually,
    ];

    /// Stable identifier, as stored and used across the UI boundary.
    pub fn key(self) -> &'static str {
        match self {
            LogPolicy::WhenLeaving => "when-leaving",
            LogPolicy::Immediately => "immediately",
            LogPolicy::Daily => "daily",
            LogPolicy::Manually => "manually",
        }
    }

    pub fn from_key(key: &str) -> Option<LogPolicy> {
        LogPolicy::ALL.into_iter().find(|p| p.key() == key)
    }

    /// Whether a list keeps the tasks ticked during a visit in place until
    /// the user leaves it ([`crate::Visit::keep_ticked`]).
    pub fn keeps_ticked(self) -> bool {
        self == LogPolicy::WhenLeaving
    }
}

/// The Logbook settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LogbookSettings {
    pub policy: LogPolicy,
    /// When finished tasks were last logged (milliseconds since the Unix
    /// epoch): by Log Completed, or by choosing a policy. Tasks finished
    /// since then are the ones still in their lists, under the daily and
    /// manual policies.
    pub logged_at: i64,
}

/// Which calendar events Today and Upcoming show (#62).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct CalendarSettings {
    /// Whether events are shown at all.
    pub show_events: bool,
    /// The calendars not shown, by id. Recording the hidden ones, not the
    /// shown, means a calendar added later is shown.
    pub hidden: Vec<String>,
}

impl Default for CalendarSettings {
    fn default() -> Self {
        Self {
            show_events: true,
            hidden: Vec::new(),
        }
    }
}

impl CalendarSettings {
    /// Whether the events of calendar `id` are shown.
    pub fn shows(&self, id: &str) -> bool {
        self.show_events && !self.is_hidden(id)
    }

    /// Whether calendar `id` was chosen not to be shown (whether or not
    /// events are shown at all).
    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.iter().any(|h| h == id)
    }

    /// Show or hide calendar `id`.
    pub fn set_shown(&mut self, id: &str, shown: bool) {
        self.hidden.retain(|h| h != id);
        if !shown {
            self.hidden.push(id.to_string());
        }
    }
}

/// Light or dark (#72): the desktop's choice, or one of Matters' own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    /// Whatever the desktop chooses, following it when it changes.
    #[default]
    System,
    Light,
    Dark,
}

impl ColorScheme {
    /// Every scheme, in the order a frontend offers them.
    pub const ALL: [ColorScheme; 3] = [ColorScheme::System, ColorScheme::Light, ColorScheme::Dark];

    /// Stable identifier, as stored and used across the UI boundary.
    pub fn key(self) -> &'static str {
        match self {
            ColorScheme::System => "system",
            ColorScheme::Light => "light",
            ColorScheme::Dark => "dark",
        }
    }

    pub fn from_key(key: &str) -> Option<ColorScheme> {
        ColorScheme::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// How Matters looks on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct AppearanceSettings {
    pub scheme: ColorScheme,
    /// Whether the desktop's accent colour (#76) replaces Matters' blue,
    /// when the desktop has one.
    pub follow_accent: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            scheme: ColorScheme::System,
            follow_accent: true,
        }
    }
}

/// Everything this device remembers. Missing keys take their defaults and
/// unknown ones are ignored, so files written by other versions still load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    pub logbook: LogbookSettings,
    pub calendar: CalendarSettings,
    pub appearance: AppearanceSettings,
}

impl Settings {
    /// Choose when finished tasks move to the Logbook, at `now`
    /// (milliseconds). What is finished so far counts as logged, so changing
    /// the policy never brings finished tasks back into their lists.
    pub fn set_log_policy(&mut self, policy: LogPolicy, now: i64) {
        self.logbook.policy = policy;
        self.logbook.logged_at = now;
    }

    /// Log Completed, at `now`: every task finished so far moves to the
    /// Logbook.
    pub fn log_completed(&mut self, now: i64) {
        self.logbook.logged_at = now;
    }

    /// The time up to which finished tasks are in the Logbook, on local day
    /// `today` ([`crate::QueryEngine::logged_until`]): those finished later
    /// stay in their lists. `None` when every finished task is.
    pub fn logged_until(&self, today: Date) -> Option<i64> {
        let logged = self.logbook.logged_at;
        match self.logbook.policy {
            LogPolicy::WhenLeaving | LogPolicy::Immediately => None,
            LogPolicy::Daily => Some(logged.max(crate::clock::start_of_local_day_millis(today))),
            LogPolicy::Manually => Some(logged),
        }
    }
}

/// Why a settings file gave the defaults: where it is and what was wrong
/// with it. A frontend says so in the user's language (#75); `Display`
/// says it in English.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsProblem {
    pub path: PathBuf,
    pub cause: String,
}

impl std::fmt::Display for SettingsProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Your settings in {} couldn't be read ({}), so the defaults are used.",
            self.path.display(),
            self.cause
        )
    }
}

/// Where the settings are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `matters/settings.toml` in the user's configuration directory
    /// (`$XDG_CONFIG_HOME`, or `~/.config`).
    pub fn in_config_dir() -> Result<Self> {
        let dir = dirs::config_dir().ok_or(MattersError::NoConfigDir)?;
        Ok(Self::new(dir.join("matters").join("settings.toml")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The settings, and what was wrong if the file exists but can't be
    /// read or understood, in which case the settings are the defaults (and
    /// saving replaces the file). No file means the defaults and no
    /// problem.
    pub fn load(&self) -> (Settings, Option<SettingsProblem>) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Settings::default(), None);
            }
            Err(err) => return (Settings::default(), Some(self.problem(&err.to_string()))),
        };
        match toml::from_str(&text) {
            Ok(settings) => (settings, None),
            Err(err) => (Settings::default(), Some(self.problem(err.message()))),
        }
    }

    fn problem(&self, cause: &str) -> SettingsProblem {
        SettingsProblem {
            path: self.path.clone(),
            cause: cause.to_string(),
        }
    }

    /// Write `settings`, replacing the file in one step so a crash never
    /// leaves half of it.
    pub fn save(&self, settings: &Settings) -> Result<()> {
        let text = toml::to_string(settings)
            .map_err(|err| MattersError::Validation(format!("settings: {err}")))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut partial = self.path.clone().into_os_string();
        partial.push(".partial");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, &self.path)?;
        Ok(())
    }
}
