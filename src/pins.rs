//! Which conversations you pinned in Messages.app.
//!
//! `chat.db` has an `is_pinned` column in nobody's macOS: the pins live in
//! Messages.app's own preferences, at
//! `~/Library/Preferences/com.apple.messages.pinning.plist`. That file is read
//! here, read-only, and never written, copied, or printed.
//!
//! # The shape of the file
//!
//! The top-level dictionary carries a `pD` dictionary, and two keys of it
//! matter:
//!
//! - `pP` — the pinned conversations, in the order they sit on Messages's own
//!   grid. Each entry is either a plain address (`+1…`, or an email), which is
//!   a one-to-one chat, or a long string of hex digits, which is a group.
//! - `pZ` — a table keyed by exactly those hex strings. Each value is a
//!   dictionary with an `o` and an `h`, both opaque identifiers for the group.
//!
//! The hex string is ASCII: decoding it gives a UUID, and so does `o` — a
//! *different* UUID. On the machine this was written against, `o` is the one
//! `chat.original_group_id` holds, which is what makes a pinned group findable
//! at all. The decoded hex and `h` (forty hex digits, so plausibly a digest)
//! matched no column of `chat`, so both are kept as candidates and matched
//! against every id a chat carries rather than being claimed to mean anything
//! in particular.
//!
//! # What is matched
//!
//! An address entry is normalized the way [`crate::contacts`] normalizes one —
//! emails lowercased, numbers reduced to `+` and digits — and compared against
//! a one-to-one chat's own identifier and its single participant. A group entry
//! is compared, case-insensitively, against a chat's `guid`, `chat_identifier`,
//! `group_id`, and `original_group_id`.
//!
//! # What could not be resolved
//!
//! - The decoded-hex UUID and `h` are matched hopefully, not knowingly: no
//!   column of `chat` held either on the machine this was built against.
//! - A group whose `original_group_id` has been rotated by Messages — a
//!   re-created thread, a restore — is not found, and simply stays unpinned.
//! - `pP` is an *ordered* list, and that order is Messages's pin order. msgs
//!   does not use it: pinned chats stay sorted newest-first among themselves,
//!   like the rest of the list.
//!
//! Nothing in this module prints an address, a name, or an identifier. Errors
//! carry a reason and at most a path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use plist::Value;

use crate::contacts::normalize;
use crate::db::Chat;

/// `pD`: the dictionary the pin state lives in.
const CONFIG: &str = "pD";
/// `pP`: the pinned conversations, in Messages's own order.
const PINNED: &str = "pP";
/// `pZ`: hex identifier → the group it stands for.
const GROUPS: &str = "pZ";

/// Where the pins came from, for the status line and `--check`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    /// Nobody asked for pins — the tests, `--no-pins`, and [`Pins::off`].
    #[default]
    Off,
    /// The preference file was read.
    Ready {
        /// How many conversations it pins. A count, never an address.
        pinned: usize,
    },
    /// The file could not be read; nothing is pinned.
    Unavailable(String),
}

impl Status {
    /// One line for `--check`.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Off => "not loaded".to_string(),
            Self::Ready { pinned } => format!("{pinned} pinned"),
            Self::Unavailable(reason) => format!("unavailable — {reason}"),
        }
    }

    /// The warning the status line shows when the pins cannot be read.
    ///
    /// A file that simply is not there is not a warning: a Mac that has never
    /// pinned anything has no preference file, and that is the normal case.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Unavailable(reason) => {
                Some(format!("pins: {reason} — no chat will show as pinned"))
            }
            _ => None,
        }
    }

    /// Whether pins were read at all.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// The conversations Messages.app has pinned.
#[derive(Debug, Clone, Default)]
pub struct Pins {
    /// The file the pins were read from, so a reload can read it again.
    path: Option<PathBuf>,
    /// Normalized addresses of the pinned one-to-one chats.
    handles: HashSet<String>,
    /// Lowercased identifiers of the pinned groups: the hex entry itself, the
    /// UUID it decodes to, and the `o` and `h` behind it.
    groups: HashSet<String>,
    status: Status,
}

impl Pins {
    /// Pins turned off: nothing is pinned and [`Pins::apply`] leaves every chat
    /// exactly as the database handed it over.
    #[must_use]
    pub fn off() -> Self {
        Self::default()
    }

    /// `~/Library/Preferences/com.apple.messages.pinning.plist`.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        Some(
            dirs::home_dir()?
                .join("Library")
                .join("Preferences")
                .join("com.apple.messages.pinning.plist"),
        )
    }

    /// Read the pins from Messages.app's preference file.
    ///
    /// Never fails: a missing, unreadable, or unrecognized file comes back with
    /// nothing pinned, and the list is drawn the way it always was.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let mut pins = Self {
            path: Some(path.to_path_buf()),
            ..Self::default()
        };
        pins.reload();
        pins
    }

    /// Read the file again, in place.
    ///
    /// Cheap enough to do on every reload of the chat list — the file is a few
    /// hundred bytes — which is how a pin made in Messages while msgs is open
    /// reaches the screen without a second filesystem watcher.
    pub fn reload(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let read = read(&path);
        self.handles = read.handles;
        self.groups = read.groups;
        self.status = read.status;
    }

    /// A set built in memory, for tests and for a caller with its own source.
    #[must_use]
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, Option<(S, S)>)>,
        S: AsRef<str>,
    {
        let mut pins = Self::default();
        let mut pinned = 0usize;
        for (entry, group) in entries {
            if pins.add(
                entry.as_ref(),
                group.as_ref().map(|(o, h)| (o.as_ref(), h.as_ref())),
            ) {
                pinned += 1;
            }
        }
        pins.status = Status::Ready { pinned };
        pins
    }

    /// Where the pins came from.
    #[must_use]
    pub const fn status(&self) -> &Status {
        &self.status
    }

    /// The preference file the pins are read from, once one has been named.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether nothing is pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty() && self.groups.is_empty()
    }

    /// Whether [`Pins::apply`] will touch a chat at all.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.path.is_some() || self.status.is_ready()
    }

    /// Whether `chat` is one of the pinned conversations.
    #[must_use]
    pub fn covers(&self, chat: &Chat) -> bool {
        if ids(chat).any(|id| self.groups.contains(&id)) {
            return true;
        }
        // A group is pinned as a group. Matching a participant's address here
        // would pin every group that one pinned person happens to be in.
        if chat.is_group {
            return false;
        }
        if let Some(key) = chat.identifier.as_deref().and_then(normalize)
            && self.handles.contains(&key)
        {
            return true;
        }
        match chat.participants.as_slice() {
            [only] => normalize(&only.id).is_some_and(|key| self.handles.contains(&key)),
            _ => false,
        }
    }

    /// Say of every chat whether it is pinned.
    ///
    /// This is the one place [`Chat::is_pinned`] is written outside the
    /// database, the way [`crate::contacts::Contacts::apply`] is the one place
    /// a name is attached. A [`Pins::off`] set writes nothing, so a database
    /// that does record pinning in SQL keeps its own answer.
    pub fn apply(&self, chats: &mut [Chat]) {
        if !self.is_on() {
            return;
        }
        for chat in chats {
            chat.is_pinned = Some(self.covers(chat));
        }
    }

    /// Record one `pP` entry, with its `pZ` row when it has one.
    ///
    /// Answers whether the entry pinned anything, which is what `--check`
    /// counts: one conversation, however many identifiers it is known by.
    fn add(&mut self, entry: &str, group: Option<(&str, &str)>) -> bool {
        let entry = entry.trim();
        if entry.is_empty() {
            return false;
        }
        let decoded = hex_ascii(entry);
        if group.is_some() || decoded.is_some() {
            self.groups.insert(entry.to_lowercase());
            if let Some(decoded) = decoded {
                self.groups.insert(decoded.to_lowercase());
            }
            for id in group.into_iter().flat_map(|(o, h)| [o, h]) {
                let id = id.trim();
                if !id.is_empty() {
                    self.groups.insert(id.to_lowercase());
                }
            }
            return true;
        }
        let Some(key) = normalize(entry) else {
            return false;
        };
        self.handles.insert(key);
        true
    }
}

/// Every identifier a chat can be recognized by, lowercased.
fn ids(chat: &Chat) -> impl Iterator<Item = String> + '_ {
    [
        Some(chat.guid.as_str()),
        chat.identifier.as_deref(),
        chat.group_id.as_deref(),
        chat.original_group_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|id| !id.is_empty())
    .map(str::to_lowercase)
}

/// Parse the preference file into the sets and the status behind them.
///
/// The returned [`Pins`] has no path of its own; [`Pins::reload`] keeps the one
/// it was asked to read.
fn read(path: &Path) -> Pins {
    let unavailable = |reason: &str| Pins {
        status: Status::Unavailable(reason.to_string()),
        ..Pins::default()
    };
    if !path.exists() {
        // Nothing has ever been pinned on this Mac. Not a problem.
        return Pins {
            status: Status::Ready { pinned: 0 },
            ..Pins::default()
        };
    }
    let Ok(value) = Value::from_file(path) else {
        return unavailable("unreadable");
    };
    let Some(config) = value
        .as_dictionary()
        .and_then(|dict| dict.get(CONFIG))
        .and_then(Value::as_dictionary)
    else {
        return unavailable("unrecognized layout");
    };

    let groups_table = config.get(GROUPS).and_then(Value::as_dictionary);
    let entries = config
        .get(PINNED)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut pins = Pins::default();
    let mut pinned = 0usize;
    for entry in entries.iter().filter_map(Value::as_string) {
        let group = groups_table
            .and_then(|table| table.get(entry))
            .and_then(Value::as_dictionary)
            .map(|row| {
                (
                    row.get("o").and_then(Value::as_string).unwrap_or_default(),
                    row.get("h").and_then(Value::as_string).unwrap_or_default(),
                )
            });
        if pins.add(entry, group) {
            pinned += 1;
        }
    }
    pins.status = Status::Ready { pinned };
    pins
}

/// A string of hex digit pairs read back as the ASCII it encodes.
///
/// Messages writes a group's identifier that way, so `pP` can hold both plain
/// addresses and groups in one array of strings.
fn hex_ascii(text: &str) -> Option<String> {
    if text.len() < 4 || !text.len().is_multiple_of(2) {
        return None;
    }
    if !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = text
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)? as u8;
            let lo = (pair[1] as char).to_digit(16)? as u8;
            Some(hi * 16 + lo)
        })
        .collect::<Option<Vec<u8>>>()?;
    let decoded = String::from_utf8(bytes).ok()?;
    decoded
        .chars()
        .all(|c| c.is_ascii_graphic())
        .then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Handle;

    fn chat(rowid: i64) -> Chat {
        Chat {
            rowid,
            guid: format!("iMessage;-;chat{rowid}"),
            identifier: Some(format!("chat{rowid}")),
            ..Chat::default()
        }
    }

    fn direct(rowid: i64, address: &str) -> Chat {
        let mut chat = chat(rowid);
        chat.guid = format!("iMessage;-;{address}");
        chat.identifier = Some(address.to_string());
        chat.participants = vec![Handle::new(
            rowid,
            address.to_string(),
            "iMessage".to_string(),
        )];
        chat
    }

    #[test]
    fn a_hex_entry_is_a_group_and_an_address_is_not() {
        // "A-1" as hex, which is what a group entry looks like.
        assert_eq!(hex_ascii("412D313"), None); // an odd number of digits
        assert_eq!(hex_ascii("41"), None); // too short to be an identifier
        assert_eq!(hex_ascii("412D3100"), None); // not printable ASCII
        assert_eq!(hex_ascii("412D3132"), Some("A-12".to_string()));
        assert_eq!(hex_ascii("+15550000001"), None);
        assert_eq!(hex_ascii("sam@example.invalid"), None);
    }

    #[test]
    fn a_pinned_number_matches_however_it_was_written() {
        let pins = Pins::from_entries([("(555) 000-0001", None)]);
        assert!(pins.covers(&direct(1, "+15550000001")));
        assert!(!pins.covers(&direct(2, "+15550000002")));
        assert!(pins.status().is_ready());
        assert!(!pins.is_empty());
    }

    #[test]
    fn a_pinned_address_reaches_a_chat_through_its_participant() {
        let pins = Pins::from_entries([("Sam@Example.Invalid", None)]);
        let mut chat = direct(1, "sam@example.invalid");
        chat.identifier = None;
        assert!(pins.covers(&chat));
    }

    #[test]
    fn a_pinned_person_does_not_pin_every_group_they_are_in() {
        let pins = Pins::from_entries([("+15550000001", None)]);
        let mut group = chat(1);
        group.is_group = true;
        group.participants = vec![
            Handle::new(1, "+15550000001".to_string(), "iMessage".to_string()),
            Handle::new(2, "+15550000002".to_string(), "iMessage".to_string()),
        ];
        assert!(!pins.covers(&group));
    }

    #[test]
    fn a_pinned_group_matches_the_id_behind_its_hex_entry() {
        // "GRP-1" in hex, standing for a group whose `o` is an id chat.db
        // carries as `original_group_id`.
        let pins = Pins::from_entries([("4752502D31", Some(("O-ID-1", "H-ID-1")))]);
        let mut group = chat(1);
        group.is_group = true;
        group.original_group_id = Some("o-id-1".to_string());
        assert!(pins.covers(&group));

        let mut other = chat(2);
        other.is_group = true;
        other.group_id = Some("H-ID-1".to_string());
        assert!(pins.covers(&other));

        let mut third = chat(3);
        third.is_group = true;
        third.identifier = Some("GRP-1".to_string());
        assert!(pins.covers(&third));

        let mut unpinned = chat(4);
        unpinned.is_group = true;
        unpinned.original_group_id = Some("o-id-2".to_string());
        assert!(!pins.covers(&unpinned));
    }

    #[test]
    fn apply_says_yes_or_no_for_every_chat_and_off_says_nothing() {
        let pins = Pins::from_entries([("+15550000001", None)]);
        let mut chats = vec![direct(1, "+15550000001"), direct(2, "+15550000002")];
        pins.apply(&mut chats);
        assert!(chats[0].is_pinned());
        assert_eq!(chats[1].is_pinned, Some(false));

        // The database's own answer survives an off set untouched.
        let mut chats = vec![direct(1, "+15550000001")];
        chats[0].is_pinned = Some(true);
        Pins::off().apply(&mut chats);
        assert!(chats[0].is_pinned());
        assert!(Pins::off().is_empty());
        assert_eq!(Pins::off().status(), &Status::Off);
    }

    #[test]
    fn a_missing_file_pins_nothing_and_is_not_a_warning() {
        let pins = Pins::load(Path::new("/nonexistent/com.apple.messages.pinning.plist"));
        assert!(pins.is_empty());
        assert_eq!(pins.status(), &Status::Ready { pinned: 0 });
        assert!(pins.status().warning().is_none());
        assert!(pins.is_on());
    }

    #[test]
    fn a_file_that_is_not_a_plist_is_a_warning_rather_than_a_failure() {
        let dir = std::env::temp_dir().join(format!("msgs-pins-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let path = dir.join("com.apple.messages.pinning.plist");
        std::fs::write(&path, b"not a plist at all").expect("write the scratch file");

        let pins = Pins::load(&path);
        assert!(pins.is_empty());
        assert!(matches!(pins.status(), Status::Unavailable(_)));
        assert!(
            pins.status()
                .warning()
                .is_some_and(|line| line.starts_with("pins: "))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_status_summary_counts_and_never_names() {
        assert_eq!(Status::Off.summary(), "not loaded");
        assert_eq!(Status::Ready { pinned: 3 }.summary(), "3 pinned");
        assert!(
            Status::Unavailable("unreadable".to_string())
                .summary()
                .contains("unreadable")
        );
    }
}
