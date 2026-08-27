//! Local read state: what msgs itself has put in front of you.
//!
//! `chat.db` is opened read-only and nothing here changes that. Messages.app
//! owns the real read flags — `message.is_read`, and the badge on its Dock
//! icon — and there is no supported way to clear either from outside the app,
//! so msgs keeps a small state of its own beside it: for every chat, the
//! number of unread messages that were already on screen the last time you
//! looked at it.
//!
//! That number is a floor, not a count. What the chat list draws is
//! `chat.unread_count` — the database's own answer — less the floor, never
//! below zero, so:
//!
//! - opening a chat sets the floor to what the database says right now, and the
//!   badge goes to zero;
//! - a message arriving afterwards lifts the database's count above the floor,
//!   and the badge comes back showing exactly the new ones;
//! - Messages.app reading the thread on another device drops the database's
//!   count, and [`Seen::apply`] drops the floor with it, so the next arrival
//!   still counts.
//!
//! The state lives at `~/Library/Application Support/msgs/seen.json`, `0600`
//! inside a `0700` directory, and holds nothing but `chat.ROWID`s and counts —
//! no names, numbers, or message text. It records which database it was built
//! from, so pointing `--db` at a copy starts a fresh state rather than reading
//! another database's row numbers as if they were this one's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::Chat;

/// Bumped when the shape of the file changes; an older one is discarded.
const VERSION: u32 = 1;

/// The file as it is written.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    version: u32,
    /// The database the row numbers belong to.
    db: String,
    /// `chat.ROWID` to the unread count that had already been seen.
    chats: BTreeMap<i64, i64>,
}

/// How many unread messages msgs has already shown you, per chat.
///
/// [`Seen::off`] is the inert one: it remembers nothing and changes no count,
/// which is how the tests and any code that has not asked for read state run.
#[derive(Debug, Default)]
pub struct Seen {
    /// Where the state is written. `None` means nothing is tracked at all.
    path: Option<PathBuf>,
    /// The database the floors belong to.
    db: String,
    /// `chat.ROWID` to the unread count already seen.
    floors: BTreeMap<i64, i64>,
    /// Whether [`Seen::save`] has anything to write.
    dirty: bool,
}

impl Seen {
    /// Read state turned off: nothing is remembered and every count is the
    /// database's own.
    #[must_use]
    pub fn off() -> Self {
        Self::default()
    }

    /// Load the state at `path` for the database at `db_path`.
    ///
    /// Never fails: an unreadable, malformed, or outdated file — or one built
    /// from a different database — simply starts empty.
    #[must_use]
    pub fn load(path: &Path, db_path: &Path) -> Self {
        let db = db_path.to_string_lossy().into_owned();
        let floors = read(path)
            .filter(|store| store.version == VERSION && store.db == db)
            .map(|store| store.chats)
            .unwrap_or_default();
        Self {
            path: Some(path.to_path_buf()),
            db,
            floors,
            dirty: false,
        }
    }

    /// `~/Library/Application Support/msgs/seen.json`.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        Some(dirs::data_dir()?.join("msgs").join("seen.json"))
    }

    /// Whether anything is being tracked at all.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.path.is_some()
    }

    /// Whether every chat is back to the database's own count.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.floors.is_empty()
    }

    /// How many chats carry a floor, for `--check`.
    #[must_use]
    pub fn marked(&self) -> usize {
        self.floors.len()
    }

    /// Where the state is written, once one has been loaded.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// What msgs should show for `chat`: the database's count, less whatever
    /// was already on screen.
    #[must_use]
    pub fn unread(&self, chat: &Chat) -> i64 {
        let floor = self.floors.get(&chat.rowid).copied().unwrap_or(0);
        (chat.unread_count - floor).max(0)
    }

    /// Fill in [`Chat::unread`] on every row, and pull the floors down to
    /// follow a database whose own counts have dropped.
    ///
    /// The list is the whole list, so a chat that has gone is forgotten here
    /// rather than accumulating in the file forever.
    pub fn apply(&mut self, chats: &mut [Chat]) {
        if !self.is_on() {
            return;
        }
        let before = self.floors.len();
        self.floors.retain(|rowid, floor| {
            let Some(chat) = chats.iter().find(|chat| chat.rowid == *rowid) else {
                return false;
            };
            // Messages.app reading the thread somewhere else lowers the
            // database's count; the floor follows it down so the next message
            // to arrive is counted as new.
            *floor = (*floor).min(chat.unread_count);
            *floor > 0
        });
        if self.floors.len() != before {
            self.dirty = true;
        }
        for chat in chats {
            chat.unread = self.unread(chat);
        }
    }

    /// Record that everything unread in `chat_rowid` right now has been seen.
    ///
    /// Returns whether anything moved, so a caller can skip a redraw and a
    /// write when nothing did.
    pub fn mark(&mut self, chat_rowid: i64, unread_count: i64) -> bool {
        if !self.is_on() {
            return false;
        }
        let floor = unread_count.max(0);
        let previous = self.floors.get(&chat_rowid).copied().unwrap_or(0);
        if previous == floor {
            return false;
        }
        if floor == 0 {
            self.floors.remove(&chat_rowid);
        } else {
            self.floors.insert(chat_rowid, floor);
        }
        self.dirty = true;
        true
    }

    /// `Ctrl+U`: mark every chat in the list seen.
    pub fn mark_all(&mut self, chats: &[Chat]) -> bool {
        let mut moved = false;
        for chat in chats {
            moved |= self.mark(chat.rowid, chat.unread_count);
        }
        moved
    }

    /// `Ctrl+U` again: forget every floor, so the database's own counts come
    /// back.
    pub fn forget_all(&mut self) -> bool {
        if !self.is_on() || self.floors.is_empty() {
            return false;
        }
        self.floors.clear();
        self.dirty = true;
        true
    }

    /// Write the state out, if it has moved since it was last written.
    ///
    /// Failure is silent and harmless: the worst a lost write costs is one
    /// badge coming back after a restart.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        self.dirty = false;
        let store = Store {
            version: VERSION,
            db: self.db.clone(),
            chats: self.floors.clone(),
        };
        let _ = write(&path, &store);
    }
}

fn read(path: &Path) -> Option<Store> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write(path: &Path, store: &Store) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        crate::private(dir, 0o700);
    }
    let text = serde_json::to_string(store).map_err(std::io::Error::other)?;
    std::fs::write(path, text)?;
    crate::private(path, 0o600);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(rowid: i64, unread: i64) -> Chat {
        Chat {
            rowid,
            unread_count: unread,
            unread,
            last_message_rowid: rowid * 100,
            ..Chat::default()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "msgs-seen-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("seen.json")
    }

    #[test]
    fn an_inert_state_changes_nothing() {
        let mut seen = Seen::off();
        let mut chats = vec![chat(1, 3)];
        assert!(!seen.mark(1, 3));
        seen.apply(&mut chats);
        assert_eq!(chats[0].unread, 3);
        assert!(!seen.is_on());
    }

    #[test]
    fn marking_a_chat_seen_clears_its_badge_and_new_messages_bring_it_back() {
        let path = scratch("marking");
        let mut seen = Seen::load(&path, Path::new("/fixture/chat.db"));
        let mut chats = vec![chat(1, 3)];

        assert!(seen.mark(1, 3));
        seen.apply(&mut chats);
        assert_eq!(chats[0].unread, 0);
        assert!(!chats[0].is_unread());

        // Two more arrive without Messages.app being opened.
        chats[0].unread_count = 5;
        seen.apply(&mut chats);
        assert_eq!(chats[0].unread, 2);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn a_thread_read_elsewhere_lowers_the_floor_with_it() {
        let path = scratch("elsewhere");
        let mut seen = Seen::load(&path, Path::new("/fixture/chat.db"));
        let mut chats = vec![chat(1, 3)];
        seen.mark(1, 3);

        // Messages.app reads the thread on another device: the database's own
        // count goes to zero, and the floor has to follow or the next message
        // would be swallowed.
        chats[0].unread_count = 0;
        seen.apply(&mut chats);
        assert!(seen.is_empty(), "a floor of nothing is not kept");

        chats[0].unread_count = 1;
        seen.apply(&mut chats);
        assert_eq!(chats[0].unread, 1);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn a_chat_that_is_gone_is_forgotten() {
        let path = scratch("forgotten");
        let mut seen = Seen::load(&path, Path::new("/fixture/chat.db"));
        seen.mark(1, 2);
        seen.mark(2, 2);
        let mut chats = vec![chat(2, 2)];
        seen.apply(&mut chats);
        assert_eq!(seen.marked(), 1);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn the_state_survives_a_restart_and_belongs_to_one_database() {
        let path = scratch("restart");
        let db = Path::new("/fixture/chat.db");
        let mut seen = Seen::load(&path, db);
        seen.mark_all(&[chat(1, 3), chat(2, 1)]);
        seen.save();
        assert!(path.is_file(), "the state was written");

        let again = Seen::load(&path, db);
        assert_eq!(again.marked(), 2);
        assert_eq!(again.unread(&chat(1, 3)), 0);
        assert_eq!(again.unread(&chat(1, 4)), 1);

        // Another database's row numbers are not this one's.
        let elsewhere = Seen::load(&path, Path::new("/fixture/other.db"));
        assert!(elsewhere.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn forgetting_everything_brings_the_database_counts_back() {
        let path = scratch("forget");
        let mut seen = Seen::load(&path, Path::new("/fixture/chat.db"));
        let mut chats = vec![chat(1, 3), chat(2, 1)];
        seen.mark_all(&chats);
        seen.apply(&mut chats);
        assert_eq!(chats.iter().map(|chat| chat.unread).sum::<i64>(), 0);

        assert!(seen.forget_all());
        seen.apply(&mut chats);
        assert_eq!(chats.iter().map(|chat| chat.unread).sum::<i64>(), 4);
        assert!(!seen.forget_all(), "nothing left to forget");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn a_malformed_file_starts_empty_rather_than_failing() {
        let path = scratch("malformed");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("scratch directory");
        std::fs::write(&path, "{ not json").expect("write");
        assert!(Seen::load(&path, Path::new("/fixture/chat.db")).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }
}
