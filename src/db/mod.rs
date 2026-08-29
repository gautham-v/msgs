//! Read-only access to the Messages store, `~/Library/Messages/chat.db`.
//!
//! Everything here opens SQLite with `SQLITE_OPEN_READ_ONLY`, a `?mode=ro` URI,
//! and `PRAGMA query_only`, so no code path in the crate can write to the
//! database even by accident. macOS keeps `chat.db` in WAL mode and Messages.app
//! may hold it; when a read is refused because of that, [`Db::open`] copies
//! `chat.db`, `chat.db-wal`, and `chat.db-shm` into a scratch directory and
//! reads the copy instead. The scratch directory is deleted when the [`Db`] is
//! dropped.
//!
//! - [`handle`] — the people behind phone numbers and emails
//! - [`chat`] — conversations, their participants, and unread counts
//! - [`message`] — message rows, attachments, tapbacks, and group actions
//!
//! Nothing in this module logs or formats message bodies, handles, or names.

pub mod chat;
pub mod handle;
pub mod message;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use imessage_database::util::dates::{get_local_time, get_offset};
use rusqlite::{Connection, OpenFlags};

pub use chat::{Chat, Preview};
pub use handle::{Handle, Name};
pub use message::{
    AttachmentKind, AttachmentRef, GroupAction, Message, Tapback, TapbackAction, TapbackKind,
    body_text,
};

/// How many messages a conversation page holds by default.
pub const PAGE: usize = 100;

/// Largest page any caller may ask for, so the `IN (…)` lists that fetch
/// attachments and tapbacks for a page stay a sane size.
pub const MAX_PAGE: usize = 500;

/// Unix seconds at the Messages epoch, `2001-01-01 00:00:00Z`.
static APPLE_EPOCH_OFFSET: LazyLock<i64> = LazyLock::new(get_offset);

/// Convert a raw Messages timestamp to local time.
///
/// Messages stores nanoseconds since `2001-01-01`; very old rows store plain
/// seconds, which [`get_local_time`] also handles. `0` means "never", so it maps
/// to `None` rather than to the epoch.
#[must_use]
pub fn local_time(raw: i64) -> Option<DateTime<Local>> {
    if raw == 0 {
        return None;
    }
    get_local_time(raw, *APPLE_EPOCH_OFFSET).ok()
}

/// Convert a raw Messages timestamp to Unix seconds, or `None` for `0`.
#[must_use]
pub fn unix_seconds(raw: i64) -> Option<i64> {
    if raw == 0 {
        return None;
    }
    let seconds = if raw.abs() >= 1_000_000_000_000 {
        raw / 1_000_000_000
    } else {
        raw
    };
    Some(seconds + *APPLE_EPOCH_OFFSET)
}

/// Convert local time to a raw Messages timestamp.
///
/// The inverse of [`local_time`], for the rows msgs invents itself: the
/// optimistic echo of a message that has been sent but is not in `chat.db`
/// yet. Nothing built this way is ever written to the database.
#[must_use]
pub fn raw_time(when: DateTime<Local>) -> i64 {
    (when.timestamp() - *APPLE_EPOCH_OFFSET).saturating_mul(1_000_000_000)
}

/// Why the database could not be read.
///
/// Every variant carries at most a path, never anything out of the database.
#[derive(Debug)]
pub enum DbError {
    /// No file at that path.
    NotFound(PathBuf),
    /// The file exists but the process may not read it — on macOS this almost
    /// always means the terminal lacks Full Disk Access.
    PermissionDenied(PathBuf),
    /// Copying the database to a scratch directory failed.
    Copy {
        /// Where the copy was being written.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// Some other I/O failure while looking at the file.
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// SQLite refused to open the file.
    Open {
        /// The file involved.
        path: PathBuf,
        /// The underlying SQLite failure.
        source: rusqlite::Error,
    },
    /// A query failed against an open database.
    Query(rusqlite::Error),
}

impl DbError {
    /// A short, path-free reason, for the status line.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::NotFound(_) => "not found".to_string(),
            Self::PermissionDenied(_) => "permission denied".to_string(),
            Self::Copy { .. } => "could not be copied".to_string(),
            Self::Io { .. } => "unreadable".to_string(),
            Self::Open { .. } => "could not be opened".to_string(),
            Self::Query(_) => "query failed".to_string(),
        }
    }

    /// The headline for the full-screen error surface.
    #[must_use]
    pub const fn headline(&self) -> &'static str {
        match self {
            Self::PermissionDenied(_) => "msgs cannot read your messages",
            Self::NotFound(_) => "no message database here",
            _ => "msgs could not open the message database",
        }
    }

    /// One line of explanation under the headline.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::PermissionDenied(_) => "macOS is blocking access to chat.db.".to_string(),
            Self::NotFound(_) => {
                "Messages has not created a database at this path yet.".to_string()
            }
            Self::Copy { source, .. } | Self::Io { source, .. } => source.to_string(),
            Self::Open { source, .. } => source.to_string(),
            Self::Query(source) => source.to_string(),
        }
    }

    /// What the reader should do about it, when there is something to do.
    #[must_use]
    pub const fn hint(&self) -> Option<&'static str> {
        match self {
            Self::PermissionDenied(_) => Some(concat!(
                "Give your terminal Full Disk Access:\n",
                "\n",
                "1. Open System Settings → Privacy & Security → Full Disk Access\n",
                "2. Switch on the app you run msgs in — Terminal, iTerm2, Ghostty\n",
                "3. Quit that app and open it again; macOS applies it on launch\n",
                "\n",
                "The same switch is what lets msgs read Contacts for names.",
            )),
            Self::NotFound(_) => Some(
                "Open Messages.app and sign in to create it,\n\
                 or point msgs at another file with --db <PATH>.",
            ),
            _ => None,
        }
    }

    /// The file the error is about, when the error names one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::NotFound(path) | Self::PermissionDenied(path) => Some(path),
            Self::Copy { path, .. } | Self::Io { path, .. } | Self::Open { path, .. } => Some(path),
            Self::Query(_) => None,
        }
    }

    /// Whether this is the Full Disk Access case.
    #[must_use]
    pub const fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied(_))
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.path() {
            Some(path) => write!(f, "{}: {}", path.display(), self.summary()),
            None => write!(f, "{}", self.summary()),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Copy { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Open { source, .. } => Some(source),
            Self::Query(source) => Some(source),
            Self::NotFound(_) | Self::PermissionDenied(_) => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(source: rusqlite::Error) -> Self {
        Self::Query(source)
    }
}

/// Whether the rows came from the live database or from a scratch copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Reading `chat.db` in place.
    Live,
    /// Reading a copy, because the live file refused a read-only reader.
    Copy,
}

/// Aggregate row counts, for `--check`. Counts only — never content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Rows in `chat`.
    pub chats: i64,
    /// Rows in `message`, tapbacks included.
    pub messages: i64,
    /// Rows in `handle`.
    pub handles: i64,
    /// Rows in `attachment`.
    pub attachments: i64,
}

/// Which optional columns this particular database has.
///
/// `chat.db` gains columns with each macOS release and Messages never
/// backfills old databases, so the queries ask once at open time rather than
/// assuming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Schema {
    /// `message.associated_message_emoji`, which carries custom-emoji tapbacks.
    pub tapback_emoji: bool,
    /// `chat.is_pinned`. Absent on every macOS to date — pinning lives in
    /// Messages.app's preferences, not in the database. See [`crate::pins`].
    pub chat_is_pinned: bool,
    /// `chat.original_group_id`, the identifier the pin state names a group by.
    pub chat_original_group_id: bool,
}

/// An open, read-only connection to a Messages database.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
    path: PathBuf,
    source: Source,
    schema: Schema,
    // Dropping this removes the scratch copy, so it must outlive `conn`'s use.
    scratch: Option<Scratch>,
}

impl Db {
    /// Open `path` read-only, falling back to a scratch copy if the live file
    /// will not serve a reader.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::NotFound`] or [`DbError::PermissionDenied`] when the
    /// file cannot even be opened by the OS — the second of which is what a
    /// terminal without Full Disk Access sees — and [`DbError::Open`] when
    /// SQLite rejects both the original and the copy.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        probe(path)?;
        match connect(path) {
            Ok(conn) => Ok(Self::assembled(conn, path, Source::Live, None)),
            Err(err) if is_lock_error(&err) => Self::open_copy(path),
            Err(source) => Err(DbError::Open {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Copy the database and its WAL sidecars somewhere private, then read that.
    fn open_copy(path: &Path) -> Result<Self, DbError> {
        let scratch = Scratch::new(path)?;
        let conn = connect(&scratch.db).map_err(|source| DbError::Open {
            path: scratch.db.clone(),
            source,
        })?;
        Ok(Self::assembled(conn, path, Source::Copy, Some(scratch)))
    }

    /// Finish an open by asking the file which optional columns it has.
    fn assembled(conn: Connection, path: &Path, source: Source, scratch: Option<Scratch>) -> Self {
        let mut db = Self {
            conn,
            path: path.to_path_buf(),
            source,
            schema: Schema::default(),
            scratch,
        };
        db.schema = Schema {
            tapback_emoji: db.has_column("message", "associated_message_emoji"),
            chat_is_pinned: db.has_column("chat", "is_pinned"),
            chat_original_group_id: db.has_column("chat", "original_group_id"),
        };
        db
    }

    /// Which optional columns this database has.
    #[must_use]
    pub const fn schema(&self) -> Schema {
        self.schema
    }

    /// The read-only connection, for queries this module does not provide.
    #[must_use]
    pub const fn conn(&self) -> &Connection {
        &self.conn
    }

    /// The database the caller asked for, not the scratch copy.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether rows are coming from the live file or a copy of it.
    #[must_use]
    pub const fn source(&self) -> Source {
        self.source
    }

    /// The scratch copy actually being read, when there is one.
    ///
    /// It is deleted when this [`Db`] is dropped, so nothing should hold on to
    /// the path.
    #[must_use]
    pub fn scratch_path(&self) -> Option<&Path> {
        self.scratch.as_ref().map(|scratch| scratch.db.as_path())
    }

    /// Aggregate row counts. Counts only, so it is safe to print.
    ///
    /// # Errors
    ///
    /// Fails if any of the four tables is missing or unreadable.
    pub fn counts(&self) -> Result<Counts, DbError> {
        Ok(Counts {
            chats: self.count("chat")?,
            messages: self.count("message")?,
            handles: self.count("handle")?,
            attachments: self.count("attachment")?,
        })
    }

    fn count(&self, table: &str) -> Result<i64, DbError> {
        // `table` is one of four literals chosen above, never user input.
        let sql = format!("SELECT COUNT(*) FROM {table}");
        Ok(self.conn.query_row(&sql, [], |row| row.get(0))?)
    }

    /// Whether `table` has a column named `column`.
    ///
    /// Used to stay compatible with older and newer schemas rather than
    /// assuming a column exists.
    #[must_use]
    pub fn has_column(&self, table: &str, column: &str) -> bool {
        let sql = format!("PRAGMA table_info({table})");
        let Ok(mut statement) = self.conn.prepare(&sql) else {
            return false;
        };
        let Ok(mut rows) = statement.query([]) else {
            return false;
        };
        while let Ok(Some(row)) = rows.next() {
            if row.get::<_, String>(1).is_ok_and(|name| name == column) {
                return true;
            }
        }
        false
    }
}

/// Ask the OS about the file before SQLite does, so "missing" and "Full Disk
/// Access" come back as their own errors instead of a generic `unable to open
/// database file`.
fn probe(path: &Path) -> Result<(), DbError> {
    match std::fs::File::open(path) {
        Ok(_) => Ok(()),
        Err(source) => Err(match source.kind() {
            std::io::ErrorKind::NotFound => DbError::NotFound(path.to_path_buf()),
            std::io::ErrorKind::PermissionDenied => DbError::PermissionDenied(path.to_path_buf()),
            _ => DbError::Io {
                path: path.to_path_buf(),
                source,
            },
        }),
    }
}

/// Open one read-only connection and prove it can actually read.
///
/// The probe query matters: a WAL that needs recovery opens fine and only fails
/// on the first read, and that is the case the scratch copy exists for.
fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(read_only_uri(path), flags)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(conn)
}

/// `file:/percent/encoded/path?mode=ro`.
///
/// [`crate::contacts`] opens the Contacts stores the same way, so the escaping
/// lives here rather than being written twice.
#[must_use]
pub fn read_only_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut uri = String::with_capacity(raw.len() + 16);
    uri.push_str("file:");
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(byte as char);
            }
            _ => {
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    uri.push_str("?mode=ro");
    uri
}

/// Whether an open failed for a reason a private copy would fix.
fn is_lock_error(err: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode::{CannotOpen, DatabaseBusy, DatabaseLocked, ReadOnly};
    match err {
        rusqlite::Error::SqliteFailure(error, _) => {
            matches!(
                error.code,
                DatabaseBusy | DatabaseLocked | ReadOnly | CannotOpen
            )
        }
        _ => false,
    }
}

/// A private copy of a SQLite database and its WAL sidecars, removed when it is
/// dropped.
///
/// Used for `chat.db` when Messages.app holds it, and by [`crate::contacts`]
/// for a Contacts store that refuses a reader for the same reason. Whatever
/// holds the connection must hold this too: dropping it deletes the files.
#[derive(Debug)]
pub struct Scratch {
    dir: PathBuf,
    db: PathBuf,
}

impl Scratch {
    /// Copy `source` and its sidecars somewhere private.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Copy`] if the directory or the copy cannot be made.
    pub fn new(source: &Path) -> Result<Self, DbError> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let dir = std::env::temp_dir().join(format!("msgs-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|err| DbError::Copy {
            path: dir.clone(),
            source: err,
        })?;

        let name = source
            .file_name()
            .map_or_else(|| std::ffi::OsString::from("chat.db"), ToOwned::to_owned);
        let db = dir.join(&name);
        let scratch = Self {
            dir: dir.clone(),
            db: db.clone(),
        };

        std::fs::copy(source, &db).map_err(|err| DbError::Copy {
            path: db.clone(),
            source: err,
        })?;
        // The sidecars hold everything written since the last checkpoint. A
        // missing one is normal, not a failure.
        for suffix in ["-wal", "-shm"] {
            let mut from = source.as_os_str().to_owned();
            from.push(suffix);
            let from = PathBuf::from(from);
            if from.exists() {
                let mut to = db.as_os_str().to_owned();
                to.push(suffix);
                let _ = std::fs::copy(&from, PathBuf::from(to));
            }
        }

        Ok(scratch)
    }

    /// The copy to open.
    #[must_use]
    pub fn db(&self) -> &Path {
        &self.db
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `674526582885055488` ns after 2001-01-01 is 2022-05-17 22:29:42 UTC.
    const SAMPLE: i64 = 674_526_582_885_055_488;
    const SAMPLE_UNIX: i64 = 1_652_833_782;

    #[test]
    fn nanosecond_and_second_stamps_convert_to_the_same_instant() {
        assert_eq!(unix_seconds(SAMPLE), Some(SAMPLE_UNIX));
        assert_eq!(unix_seconds(674_526_582), Some(SAMPLE_UNIX));
        assert_eq!(
            local_time(SAMPLE).map(|when| when.timestamp()),
            Some(SAMPLE_UNIX)
        );
    }

    #[test]
    fn a_zero_stamp_is_never_rather_than_the_epoch() {
        assert_eq!(unix_seconds(0), None);
        assert!(local_time(0).is_none());
    }

    #[test]
    fn the_uri_is_read_only_and_escapes_awkward_paths() {
        let uri = read_only_uri(Path::new("/tmp/my db?x/chat.db"));
        assert!(uri.starts_with("file:/tmp/my%20db%3Fx/chat.db"));
        assert!(uri.ends_with("?mode=ro"));
    }

    #[test]
    fn a_missing_file_is_not_found_and_names_the_path() {
        let err = Db::open(Path::new("/nonexistent/msgs-test/chat.db")).unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
        assert_eq!(err.summary(), "not found");
        assert!(err.hint().is_some());
        assert!(!err.is_permission_denied());
    }
}
