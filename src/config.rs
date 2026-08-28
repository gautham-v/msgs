//! Optional user config, read once at startup from `~/.config/msgs/config.toml`.
//!
//! Everything in the file is optional; a missing or malformed file never stops
//! the app from starting. Problems come back as warnings that the status line
//! shows, so a typo in a color is visible rather than fatal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Narrowest terminal that still shows the chat list, in columns.
pub const MIN_WIDTH_FOR_CHAT_LIST: u16 = 90;

const DEFAULT_CHAT_LIST_WIDTH: u16 = 30;
const MIN_CHAT_LIST_WIDTH: u16 = 18;
const MAX_CHAT_LIST_WIDTH: u16 = 60;
const DEFAULT_PAGE_STEP: u16 = 10;

/// Parsed `config.toml`, with defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Show the chat list pane on startup (`Ctrl+B` toggles it at runtime).
    pub show_chat_list: bool,
    /// Width of the chat list pane in columns.
    pub chat_list_width: u16,
    /// Rows `PageUp` / `PageDown` move a list by. The conversation pages by
    /// its own height instead, and the wheel is always three rows.
    pub page_step: u16,
    /// Capture the mouse. `--no-mouse` overrides this to `false`.
    pub mouse: bool,
    /// Draw pictures inline where the terminal can. `--no-images` overrides
    /// this to `false`.
    pub images: bool,
    /// Read the macOS Contacts stores so handles become names.
    /// `--no-contacts` overrides this to `false`.
    pub contacts: bool,
    /// The `[theme]` table: `base = "light"` picks a palette, and any other
    /// key is a per-slot color override, e.g. `accent_me = "#ff8800"`.
    #[serde(default)]
    pub theme: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_chat_list: true,
            chat_list_width: DEFAULT_CHAT_LIST_WIDTH,
            page_step: DEFAULT_PAGE_STEP,
            mouse: true,
            images: true,
            contacts: true,
            theme: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Load the config from `path`, or return defaults if it does not exist.
    ///
    /// Never fails: read and parse errors become warnings.
    #[must_use]
    pub fn load_from(path: &Path) -> (Self, Vec<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), Vec::new());
            }
            Err(err) => {
                return (
                    Self::default(),
                    vec![format!("config: cannot read {}: {err}", path.display())],
                );
            }
        };
        Self::parse(&text)
    }

    /// Load from the default location, or from `override_path` when given.
    #[must_use]
    pub fn load(override_path: Option<&Path>) -> (Self, Vec<String>) {
        match override_path {
            Some(path) => Self::load_from(path),
            None => match default_path() {
                Some(path) => Self::load_from(&path),
                None => (Self::default(), Vec::new()),
            },
        }
    }

    /// Parse config text, clamping values into usable ranges.
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        match toml::from_str::<Self>(text) {
            Ok(mut config) => {
                let mut warnings = Vec::new();
                let width = config
                    .chat_list_width
                    .clamp(MIN_CHAT_LIST_WIDTH, MAX_CHAT_LIST_WIDTH);
                if width != config.chat_list_width {
                    warnings.push(format!(
                        "config: chat_list_width clamped to {width} (allowed {MIN_CHAT_LIST_WIDTH}–{MAX_CHAT_LIST_WIDTH})"
                    ));
                    config.chat_list_width = width;
                }
                if config.page_step == 0 {
                    warnings.push("config: page_step must be at least 1".to_string());
                    config.page_step = DEFAULT_PAGE_STEP;
                }
                (config, warnings)
            }
            Err(err) => {
                let message = err.to_string();
                let first_line = message.lines().next().unwrap_or("invalid TOML").to_string();
                (Self::default(), vec![format!("config: {first_line}")])
            }
        }
    }
}

/// `$XDG_CONFIG_HOME/msgs/config.toml`, else `~/.config/msgs/config.toml`.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("msgs").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_defaults() {
        let (config, warnings) = Config::parse("");
        assert_eq!(config, Config::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn reads_every_documented_key() {
        let (config, warnings) = Config::parse(
            r##"
            show_chat_list = false
            chat_list_width = 24
            page_step = 5
            mouse = false
            contacts = false

            [theme]
            base = "light"
            accent_me = "#ff8800"
            participant1 = "#00ffcc"
            "##,
        );
        assert!(warnings.is_empty());
        assert!(!config.show_chat_list);
        assert_eq!(config.chat_list_width, 24);
        assert_eq!(config.page_step, 5);
        assert!(!config.mouse);
        assert!(!config.contacts);
        assert_eq!(
            config.theme.get("accent_me").map(String::as_str),
            Some("#ff8800")
        );
        assert_eq!(config.theme.get("base").map(String::as_str), Some("light"));
        assert_eq!(config.theme.len(), 3);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let (config, warnings) = Config::parse("chat_list_width = 900\npage_step = 0\n");
        assert_eq!(config.chat_list_width, MAX_CHAT_LIST_WIDTH);
        assert_eq!(config.page_step, DEFAULT_PAGE_STEP);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn broken_toml_falls_back_to_defaults_with_a_warning() {
        let (config, warnings) = Config::parse("show_chat_list = yes");
        assert_eq!(config, Config::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("config: "));
    }

    #[test]
    fn unknown_key_is_reported_not_ignored() {
        let (_, warnings) = Config::parse("shwo_chat_list = true");
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let (config, warnings) = Config::load_from(Path::new("/nonexistent/msgs/config.toml"));
        assert_eq!(config, Config::default());
        assert!(warnings.is_empty());
    }
}
