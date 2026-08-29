//! `msgs` — a terminal client for iMessage on macOS.
//!
//! The binary is a thin shell around this library: it sets the terminal up,
//! pumps events into [`app::App::update`], and asks [`ui::draw`] for a frame.
//! Everything that decides *what* happens lives here, so it can be tested
//! without a terminal and without the real message database.
//!
//! - [`app`] — state, actions, and the single `update` entry point
//! - [`config`] — the optional `~/.config/msgs/config.toml`
//! - [`contacts`] — names for handles, out of the macOS Contacts stores
//! - [`db`] — read-only queries against `chat.db`
//! - [`jump`] — what the `Ctrl+K` palette matches and shows
//! - [`keymap`] — keys to actions, and the table the help modal renders
//! - [`media`] — inline pictures, and the files behind the attachment chips
//! - [`paste`] — what a bracketed paste is: dropped files, or text
//! - [`pins`] — which chats Messages.app has pinned, out of its preferences
//! - [`search`] — the FTS5 message index msgs keeps of its own
//! - [`seen`] — the local read state, kept beside `chat.db` rather than in it
//! - [`send`] — outbound messages, through Messages.app
//! - [`shell`] — the clipboard and the browser, the only two things msgs asks
//!   the rest of the machine to do
//! - [`theme`] — named color slots
//! - [`ui`] — layout and drawing
//! - [`watch`] — noticing that `chat.db` changed, so the screen keeps up
//!
//! Reading `chat.db` is strictly read-only; nothing in this crate opens it for
//! writing.

pub mod app;
pub mod config;
pub mod contacts;
pub mod db;
pub mod jump;
pub mod keymap;
pub mod media;
pub mod paste;
pub mod pins;
pub mod search;
pub mod seen;
pub mod send;
pub mod shell;
pub mod theme;
pub mod ui;
pub mod watch;

use std::path::{Path, PathBuf};

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

/// Tighten a path's permissions so a file msgs owns is readable only by the
/// person it belongs to.
///
/// The search index holds message bodies and the contacts cache holds names and
/// numbers, so both are written `0600` inside a `0700` directory, and the read
/// state that sits beside them goes the same way.
#[cfg(unix)]
pub(crate) fn private(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
pub(crate) fn private(_path: &Path, _mode: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_db_path_points_at_the_messages_store() {
        assert!(default_db_path().ends_with("Library/Messages/chat.db"));
    }
}
