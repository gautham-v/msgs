//! `msgs` — a terminal client for iMessage on macOS.
//!
//! The binary is a thin shell around this library: it sets the terminal up,
//! pumps events into [`app::App::update`], and asks [`ui::draw`] for a frame.
//! Everything that decides *what* happens lives here, so it can be tested
//! without a terminal and without the real message database.
//!
//! - [`app`] — state, actions, and the single `update` entry point
//! - [`config`] — the optional `~/.config/msgs/config.toml`
//! - [`db`] — read-only queries against `chat.db`
//! - [`keymap`] — keys to actions, and the table the help modal renders
//! - [`shell`] — the clipboard and the browser, the only two things msgs asks
//!   the rest of the machine to do
//! - [`theme`] — named color slots
//! - [`ui`] — layout and drawing
//!
//! Reading `chat.db` is strictly read-only; nothing in this crate opens it for
//! writing.

pub mod app;
pub mod config;
pub mod db;
pub mod keymap;
pub mod shell;
pub mod theme;
pub mod ui;

use std::path::PathBuf;

/// The system message store: `~/Library/Messages/chat.db`.
///
/// Only ever opened read-only.
#[must_use]
pub fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library")
        .join("Messages")
        .join("chat.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_db_path_points_at_the_messages_store() {
        assert!(default_db_path().ends_with("Library/Messages/chat.db"));
    }
}
