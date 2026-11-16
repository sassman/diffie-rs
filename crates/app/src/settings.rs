//! The reviewer's machine-wide settings, as opposed to the review files,
//! which live with the reviewed repository and are shared.
//!
//! One TOML file under the user's config directory: with dirs-lite's
//! favor-xdg-style that is `~/.config/diffie/settings.toml` on Linux and
//! macOS alike (Windows: `%APPDATA%\diffie\settings.toml`). Missing keys
//! default and unknown ones are ignored, so builds can share the file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The reviewer's split-or-unified choice. Only the manual choice:
    /// the narrow-window fallback is a fallback, never a preference.
    pub mode: Option<LayoutMode>,
    pub window: Option<WindowSettings>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    Split,
    Unified,
}

/// The window's last windowed bounds, in the global coordinate space:
/// the origin names the screen too, so the window returns to the display
/// it was left on.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSettings {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub fn path() -> Option<PathBuf> {
    dirs_lite::config_dir().map(|dir| dir.join("diffie").join("settings.toml"))
}

/// Missing or unreadable settings are the defaults: a broken file must
/// never keep the app from opening. Unreadable ones are only logged.
pub fn load() -> Settings {
    match path() {
        Some(path) => load_from(&path),
        None => Settings::default(),
    }
}

fn load_from(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(%error, path = %path.display(), "settings unreadable, using defaults");
            Settings::default()
        }),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %path.display(), "settings unreadable, using defaults");
            }
            Settings::default()
        }
    }
}

impl Settings {
    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no config directory on this platform")?;
        self.save_to(&path)
    }

    /// Atomic like the review file: a crash never leaves half a file.
    fn save_to(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diffie").join("settings.toml");
        let settings = Settings {
            mode: Some(LayoutMode::Unified),
            window: Some(WindowSettings {
                x: -1440.,
                y: 120.5,
                width: 1440.,
                height: 900.,
            }),
        };
        settings.save_to(&path).unwrap();
        assert_eq!(load_from(&path), settings);
    }

    #[test]
    fn missing_or_broken_settings_are_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        assert_eq!(load_from(&path), Settings::default());
        std::fs::write(&path, "mode = \"diagonal\"").unwrap();
        assert_eq!(load_from(&path), Settings::default());
    }

    #[test]
    fn partial_and_future_files_still_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        // Only one key, plus one from a newer build: both must be fine.
        std::fs::write(&path, "mode = \"split\"\nfrom_the_future = true").unwrap();
        let settings = load_from(&path);
        assert_eq!(settings.mode, Some(LayoutMode::Split));
        assert_eq!(settings.window, None);
    }
}
