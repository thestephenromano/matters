//! Whether the desktop asks for reduced motion (#77).
//!
//! Matters animates adding, removing, reordering, completing and opening
//! tasks. People who turn animations off on their desktop get none:
//!
//! * `MATTERS_REDUCE_MOTION` (`1`/`true`/`yes`, or `0`/`false`/`no`) decides
//!   outright when set;
//! * KDE Plasma: an animation speed of "instant" (`AnimationDurationFactor=0`
//!   in the `[KDE]` group of `kdeglobals`);
//! * GNOME and desktops using its settings: `enable-animations` off.
//!
//! The preference is read once, at startup.

use std::path::PathBuf;
use std::process::Command;

pub fn prefers_reduced_motion() -> bool {
    if let Ok(value) = std::env::var("MATTERS_REDUCE_MOTION") {
        return matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes");
    }
    kde_animations_off() || gnome_animations_off()
}

fn kde_animations_off() -> bool {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    config
        .and_then(|dir| std::fs::read_to_string(dir.join("kdeglobals")).ok())
        .is_some_and(|text| kde_animation_factor_is_zero(&text))
}

/// Whether a `kdeglobals` file sets `AnimationDurationFactor=0` in `[KDE]`.
pub(crate) fn kde_animation_factor_is_zero(kdeglobals: &str) -> bool {
    let mut in_kde = false;
    for line in kdeglobals.lines().map(str::trim) {
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
        } else if in_kde {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "AnimationDurationFactor" {
                    return value.trim().parse::<f64>().is_ok_and(|factor| factor == 0.0);
                }
            }
        }
    }
    false
}

fn gnome_animations_off() -> bool {
    Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .is_ok_and(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "false")
}

#[cfg(test)]
mod tests {
    use super::kde_animation_factor_is_zero;

    #[test]
    fn kde_instant_animations_mean_reduced_motion() {
        assert!(kde_animation_factor_is_zero("[General]\nfoo=1\n\n[KDE]\nAnimationDurationFactor=0\n"));
        assert!(kde_animation_factor_is_zero("[KDE]\n AnimationDurationFactor = 0.0 \n"));
        assert!(!kde_animation_factor_is_zero("[KDE]\nAnimationDurationFactor=0.5\n"));
        assert!(!kde_animation_factor_is_zero("[KDE]\nSingleClick=false\n"));
        assert!(!kde_animation_factor_is_zero("[Other]\nAnimationDurationFactor=0\n"), "only the [KDE] group");
        assert!(!kde_animation_factor_is_zero(""));
    }
}
