//! Full-text search over messages, in msgs's own SQLite FTS5 index.
//!
//! `chat.db` is read-only and stays that way: the index is a separate file at
//! `~/Library/Application Support/msgs/index.db` that msgs owns outright. It is
//! built once on a background thread — the status line says how far along it is
//! — and topped up from the live-update stream after that, by `message.ROWID`,
//! so a catch-up reads only the rows that arrived since the last one.
//!
//! The index necessarily holds message bodies, so the directory is created
//! `0700` and the file `0600`, and nothing in this module ever writes a body,
//! a handle, or a name anywhere else: errors carry a reason and at most a path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use rusqlite::{Connection, OpenFlags};

use crate::db::{Db, DbError, Source, body_text};

/// Bumped whenever the index schema changes, which rebuilds it from scratch.
pub const INDEX_VERSION: i64 = 1;

/// Shortest query that reaches the message index. Below this the palette is
/// fuzzy chat matching only, which is what makes typing a name feel instant.
pub const MIN_QUERY: usize = 3;

/// Rows read out of `chat.db` per batch while indexing.
const BATCH: usize = 2_000;

/// Most rows one query pulls out of the index before ranking is re-sorted by
/// recency in the caller.
pub const QUERY_LIMIT: usize = 200;

/// How long a writer waits for the other connection before giving up.
const BUSY_MS: u32 = 2_000;

/// What an indexed row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Something somebody typed.
    Message,
    /// The name of a picture sent in a message.
    Photo,
    /// The name of any other file sent in a message.
    File,
}

impl Kind {
    /// The one-character tag stored in the index.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Message => "m",
            Self::Photo => "p",
            Self::File => "f",
        }
    }

    /// The kind behind a stored tag; anything unrecognized reads as a message.
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag {
            "p" => Self::Photo,
            "f" => Self::File,
            _ => Self::Message,
        }
    }

    /// Whether the row is a file of some sort rather than typed text.
    #[must_use]
    pub const fn is_attachment(self) -> bool {
        matches!(self, Self::Photo | Self::File)
    }
}

/// One row the index matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// `chat.ROWID` the message belongs to.
    pub chat_rowid: i64,
    /// `message.ROWID`, which is what the palette jumps to.
    pub message_rowid: i64,
    /// Raw Messages timestamp of the message.
    pub date: i64,
    /// Whether you sent it.
    pub is_from_me: bool,
    /// The sender's handle, absent for your own messages.
    pub handle: Option<String>,
    /// Whether the match was in a body or in a file name.
    pub kind: Kind,
    /// The indexed text: the body, or the file name.
    pub body: String,
}

/// How far along the index is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum State {
    /// No index has been asked for.
    #[default]
    Idle,
    /// Being built, with rows done out of rows expected.
    Building {
        /// Messages read so far.
        done: i64,
        /// Messages the database holds.
        total: i64,
    },
    /// Built and queryable.
    Ready,
    /// Could not be built; the string is a short, content-free reason.
    Failed(String),
}

impl State {
    /// The status-line note for this state, when there is one worth showing.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Idle | Self::Ready => None,
            Self::Building { done, total } => Some(match total {
                0 => "indexing messages…".to_string(),
                total => format!(
                    "indexing messages… {}%",
                    (done.saturating_mul(100) / (*total).max(1)).clamp(0, 100)
                ),
            }),
            Self::Failed(reason) => Some(format!("search index: {reason}")),
        }
    }

    /// Whether queries can be answered.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Why the index could not be built or read.
#[derive(Debug)]
pub enum SearchError {
    /// The message database could not be read.
    Db(DbError),
    /// The index itself refused a statement.
    Index(rusqlite::Error),
    /// The index directory could not be made.
    Io(std::io::Error),
    /// There is nowhere to put an index on this machine.
    NoHome,
}

impl SearchError {
    /// A short, content-free reason, for the status line.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Db(err) => format!("chat.db {}", err.summary()),
            Self::Index(_) => "index unusable".to_string(),
            Self::Io(_) => "index directory unwritable".to_string(),
            Self::NoHome => "nowhere to keep an index".to_string(),
        }
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for SearchError {}

impl From<DbError> for SearchError {
    fn from(err: DbError) -> Self {
        Self::Db(err)
    }
}

impl From<rusqlite::Error> for SearchError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Index(err)
    }
}

impl From<std::io::Error> for SearchError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// `~/Library/Application Support/msgs/index.db`.
///
/// Never inside `chat.db`, and never next to it.
///
/// # Errors
///
/// Fails when the platform has no data directory at all.
pub fn default_index_path() -> Result<PathBuf, SearchError> {
    let base = dirs::data_dir().ok_or(SearchError::NoHome)?;
    Ok(base.join("msgs").join("index.db"))
}

/// The words a query is made of: runs of letters and digits, lowercased.
#[must_use]
pub fn tokens(raw: &str) -> Vec<String> {
    raw.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The FTS5 `MATCH` expression for a query: every word as a quoted prefix.
///
/// Quoting is what keeps a query out of FTS5's own syntax — `AND`, `*`, `:`,
/// and `^` are all just characters inside a string literal — and the trailing
/// `*` is what makes a half-typed word match.
#[must_use]
pub fn match_expression(words: &[String]) -> Option<String> {
    if words.is_empty() {
        return None;
    }
    let mut expression = String::new();
    for word in words {
        if !expression.is_empty() {
            expression.push(' ');
        }
        expression.push('"');
        // A double quote inside a literal is written twice, as in SQL.
        for c in word.chars() {
            if c == '"' {
                expression.push('"');
            }
            expression.push(c);
        }
        expression.push_str("\"*");
    }
    Some(expression)
}

/// Live search: a worker thread that owns the index, and a read connection.
///
/// Dropping it asks the worker to stop at the next batch boundary; the thread
/// is not joined, because a half-finished build has nothing worth waiting for.
pub struct Search {
    index_path: PathBuf,
    state: State,
    notes: Receiver<Note>,
    commands: Option<Sender<Command>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    reader: Option<Connection>,
}

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Search")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// What the worker reports back.
enum Note {
    Progress { done: i64, total: i64 },
    Ready,
    Failed(String),
}

/// What the app asks the worker to do.
enum Command {
    CatchUp,
}

impl Search {
    /// Start indexing `db_path` into `index_path` on a background thread.
    #[must_use]
    pub fn start(db_path: &Path, index_path: &Path) -> Self {
        let (notes_tx, notes) = channel();
        let (commands, orders) = channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let worker = {
            let db_path = db_path.to_path_buf();
            let index_path = index_path.to_path_buf();
            let cancel = Arc::clone(&cancel);
            std::thread::Builder::new()
                .name("msgs-index".to_string())
                .spawn(move || run(&db_path, &index_path, &notes_tx, &orders, &cancel))
                .ok()
        };

        Self {
            index_path: index_path.to_path_buf(),
            state: if worker.is_some() {
                State::Building { done: 0, total: 0 }
            } else {
                State::Failed("indexer could not start".to_string())
            },
            notes,
            commands: Some(commands),
            cancel,
            worker,
            reader: None,
        }
    }

    /// How far along the index is.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Whether queries can be answered right now.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.state.is_ready()
    }

    /// Take whatever the worker has said. Returns `true` if the state moved.
    pub fn poll(&mut self) -> bool {
        let mut moved = false;
        loop {
            match self.notes.try_recv() {
                Ok(Note::Progress { done, total }) => {
                    let next = State::Building { done, total };
                    moved |= self.state != next;
                    self.state = next;
                }
                Ok(Note::Ready) => {
                    moved |= self.state != State::Ready;
                    self.state = State::Ready;
                }
                Ok(Note::Failed(reason)) => {
                    let next = State::Failed(reason);
                    moved |= self.state != next;
                    self.state = next;
                }
                Err(TryRecvError::Empty) => return moved,
                Err(TryRecvError::Disconnected) => {
                    if matches!(self.state, State::Building { .. }) {
                        self.state = State::Failed("indexer stopped".to_string());
                        moved = true;
                    }
                    return moved;
                }
            }
        }
    }

    /// Ask the worker to index whatever `chat.db` has gained.
    ///
    /// Never blocks: a worker that has gone away simply stops being asked.
    pub fn catch_up(&mut self) {
        if self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.send(Command::CatchUp).is_err())
        {
            self.commands = None;
        }
    }

    /// Answer a query out of the index, most recent first.
    ///
    /// Returns nothing at all while the index is still being built, so a
    /// half-built index never shows half the answer as if it were the answer.
    pub fn query(&mut self, raw: &str, kind: Option<Kind>, limit: usize) -> Vec<Hit> {
        if !self.is_ready() {
            return Vec::new();
        }
        let words = tokens(raw);
        let Some(expression) = match_expression(&words) else {
            return Vec::new();
        };
        if self.reader.is_none() {
            self.reader = open_reader(&self.index_path).ok();
        }
        let Some(reader) = self.reader.as_ref() else {
            return Vec::new();
        };
        match search(reader, &expression, kind, limit) {
            Ok(hits) => hits,
            Err(_) => {
                // A reader that has lost its file is dropped rather than kept
                // and retried on every keystroke.
                self.reader = None;
                Vec::new()
            }
        }
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.commands = None;
        // The thread checks the flag between batches and exits on its own; a
        // long build is not worth blocking a quit on.
        drop(self.worker.take());
    }
}

/// The worker thread: one full pass, then a pass per catch-up request.
fn run(
    db_path: &Path,
    index_path: &Path,
    notes: &Sender<Note>,
    orders: &Receiver<Command>,
    cancel: &AtomicBool,
) {
    let mut index = match open_index(index_path) {
        Ok(index) => index,
        Err(err) => {
            let _ = notes.send(Note::Failed(err.summary()));
            return;
        }
    };
    let mut chat = match Db::open(db_path) {
        Ok(db) => db,
        Err(err) => {
            let _ = notes.send(Note::Failed(SearchError::Db(err).summary()));
            return;
        }
    };

    let mut report = |done, total| {
        let _ = notes.send(Note::Progress { done, total });
    };
    match ingest(&chat, &mut index, cancel, &mut report) {
        Ok(()) => {
            let _ = notes.send(Note::Ready);
        }
        Err(err) => {
            let _ = notes.send(Note::Failed(err.summary()));
            return;
        }
    }

    while orders.recv().is_ok() {
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        // A database read through a scratch copy is a still photograph, so it
        // has to be taken again before it can show anything new.
        if chat.source() == Source::Copy {
            match Db::open(db_path) {
                Ok(fresh) => chat = fresh,
                Err(_) => continue,
            }
        }
        if ingest(&chat, &mut index, cancel, &mut |_, _| {}).is_ok() {
            let _ = notes.send(Note::Ready);
        }
    }
}

/// Open the index for writing, creating it and its directory if need be.
///
/// # Errors
///
/// Fails if the directory cannot be made or SQLite refuses the file.
pub fn open_index(path: &Path) -> Result<Connection, SearchError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        crate::private(dir, 0o700);
    }
    let conn = Connection::open(path)?;
    crate::private(path, 0o600);
    conn.busy_timeout(std::time::Duration::from_millis(u64::from(BUSY_MS)))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    // A schema change is a rebuild: the old rows describe columns that are
    // gone, and re-reading `chat.db` is cheaper than migrating them.
    let version: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'version'), 0)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if version != INDEX_VERSION {
        conn.execute_batch(
            "DROP TABLE IF EXISTS entries; \
             DROP TABLE IF EXISTS meta;",
        )?;
    }
    conn.execute_batch(SCHEMA)?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('version', ?1) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [INDEX_VERSION],
    )?;
    Ok(conn)
}

/// Open the index read-only, for querying.
///
/// # Errors
///
/// Fails if the file is missing or SQLite refuses it.
pub fn open_reader(path: &Path) -> Result<Connection, SearchError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(std::time::Duration::from_millis(u64::from(BUSY_MS)))?;
    Ok(conn)
}

/// Read everything `chat.db` has gained since the last pass into the index.
///
/// Both watermarks are `ROWID`s, which Messages only ever hands out upward, so
/// a catch-up reads exactly the rows that arrived since the last one.
///
/// # Errors
///
/// Fails if either database refuses a query.
pub fn ingest(
    chat: &Db,
    index: &mut Connection,
    cancel: &AtomicBool,
    report: &mut impl FnMut(i64, i64),
) -> Result<(), SearchError> {
    let total: i64 = chat
        .conn()
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .unwrap_or(0);
    let mut done = watermark(index, "messages");
    report(done.min(total), total);

    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let batch = read_messages(chat, done, BATCH)?;
        if batch.is_empty() {
            break;
        }
        done = batch.last().map_or(done, |row| row.rowid);
        write_batch(index, &batch, "messages", done)?;
        report(done, total);
    }

    let mut attachments = watermark(index, "attachments");
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let batch = read_attachments(chat, attachments, BATCH)?;
        if batch.is_empty() {
            break;
        }
        attachments = batch.last().map_or(attachments, |row| row.rowid);
        write_batch(index, &batch, "attachments", attachments)?;
    }
    Ok(())
}

/// One row on its way into the index.
struct Entry {
    /// The `ROWID` the watermark advances to — a message or an attachment.
    rowid: i64,
    chat_rowid: i64,
    message_rowid: i64,
    date: i64,
    is_from_me: bool,
    handle: Option<String>,
    kind: Kind,
    body: String,
}

fn watermark(index: &Connection, name: &str) -> i64 {
    index
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1",
            [name],
            |row| row.get(0),
        )
        .unwrap_or(0)
}

fn write_batch(
    index: &mut Connection,
    batch: &[Entry],
    name: &str,
    mark: i64,
) -> Result<(), SearchError> {
    let tx = index.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO entries \
                 (body, kind, chat_rowid, message_rowid, handle, is_from_me, date) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for entry in batch {
            // Nothing to match on: the watermark has already stepped over it.
            if entry.body.trim().is_empty() {
                continue;
            }
            insert.execute(rusqlite::params![
                entry.body,
                entry.kind.tag(),
                entry.chat_rowid,
                entry.message_rowid,
                entry.handle,
                i64::from(entry.is_from_me),
                entry.date,
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        rusqlite::params![name, mark],
    )?;
    tx.commit()?;
    Ok(())
}

/// The next batch of message bodies after `after`, oldest first.
fn read_messages(chat: &Db, after: i64, limit: usize) -> Result<Vec<Entry>, SearchError> {
    let sql = "SELECT m.ROWID, j.chat_id, m.text, m.attributedBody, h.id, m.is_from_me, m.date \
               FROM message m \
               JOIN chat_message_join j ON j.message_id = m.ROWID \
               LEFT JOIN handle h ON h.ROWID = m.handle_id \
               WHERE m.ROWID > ?1 \
                 AND COALESCE(m.associated_message_type, 0) = 0 \
                 AND COALESCE(m.item_type, 0) = 0 \
               ORDER BY m.ROWID LIMIT ?2";
    let mut statement = chat.conn().prepare(sql).map_err(chat_query)?;
    let rows = statement
        .query_map(
            rusqlite::params![after, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                let text: Option<String> = row.get(2)?;
                let attributed: Option<Vec<u8>> = row.get(3)?;
                let rowid: i64 = row.get(0)?;
                Ok(Entry {
                    rowid,
                    chat_rowid: row.get(1)?,
                    message_rowid: rowid,
                    date: row.get::<_, Option<i64>>(6)?.unwrap_or_default(),
                    is_from_me: row.get::<_, Option<i64>>(5)?.unwrap_or_default() != 0,
                    handle: row.get(4)?,
                    kind: Kind::Message,
                    body: body_text(text.as_deref(), attributed.as_deref()).unwrap_or_default(),
                })
            },
        )
        .map_err(chat_query)?;
    // Rows with nothing to match stay in the batch so the watermark still
    // steps over them; `write_batch` is what leaves them out of the index.
    let mut batch = Vec::with_capacity(limit.min(BATCH));
    for row in rows {
        batch.push(row.map_err(chat_query)?);
    }
    Ok(batch)
}

/// The next batch of attachment names after `after`, oldest first.
fn read_attachments(chat: &Db, after: i64, limit: usize) -> Result<Vec<Entry>, SearchError> {
    let sql = "SELECT a.ROWID, j.chat_id, m.ROWID, a.transfer_name, a.mime_type, a.uti, \
                      h.id, m.is_from_me, m.date \
               FROM attachment a \
               JOIN message_attachment_join k ON k.attachment_id = a.ROWID \
               JOIN message m ON m.ROWID = k.message_id \
               JOIN chat_message_join j ON j.message_id = m.ROWID \
               LEFT JOIN handle h ON h.ROWID = m.handle_id \
               WHERE a.ROWID > ?1 \
                 AND COALESCE(a.is_sticker, 0) = 0 \
                 AND COALESCE(a.hide_attachment, 0) = 0 \
               ORDER BY a.ROWID LIMIT ?2";
    let mut statement = chat.conn().prepare(sql).map_err(chat_query)?;
    let rows = statement
        .query_map(
            rusqlite::params![after, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                let mime: Option<String> = row.get(4)?;
                let uti: Option<String> = row.get(5)?;
                let is_image = mime.as_deref().is_some_and(|m| m.starts_with("image/"))
                    || uti.as_deref().is_some_and(|u| u.contains("image"));
                Ok(Entry {
                    rowid: row.get(0)?,
                    chat_rowid: row.get(1)?,
                    message_rowid: row.get(2)?,
                    date: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                    is_from_me: row.get::<_, Option<i64>>(7)?.unwrap_or_default() != 0,
                    handle: row.get(6)?,
                    kind: if is_image { Kind::Photo } else { Kind::File },
                    body: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                })
            },
        )
        .map_err(chat_query)?;
    let mut batch = Vec::with_capacity(limit.min(BATCH));
    for row in rows {
        batch.push(row.map_err(chat_query)?);
    }
    Ok(batch)
}

/// A failure reading `chat.db`, told apart from a failure in the index itself.
fn chat_query(err: rusqlite::Error) -> SearchError {
    SearchError::Db(DbError::Query(err))
}

/// Run one `MATCH` against an open index.
///
/// FTS5 ranks by `bm25`, which is what bounds the work; the caller re-sorts the
/// bounded result by recency, because a jump palette is read newest first.
///
/// # Errors
///
/// Fails if the index refuses the statement.
pub fn search(
    index: &Connection,
    expression: &str,
    kind: Option<Kind>,
    limit: usize,
) -> Result<Vec<Hit>, SearchError> {
    let filter = match kind {
        Some(Kind::Photo) => " AND kind = 'p'",
        Some(Kind::File) => " AND kind = 'f'",
        Some(Kind::Message) => " AND kind = 'm'",
        None => "",
    };
    let sql = format!(
        "SELECT body, kind, chat_rowid, message_rowid, handle, is_from_me, date \
         FROM entries WHERE entries MATCH ?1{filter} ORDER BY rank LIMIT ?2"
    );
    let mut statement = index.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params![expression, i64::try_from(limit).unwrap_or(i64::MAX)],
        |row| {
            Ok(Hit {
                body: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                kind: Kind::from_tag(&row.get::<_, Option<String>>(1)?.unwrap_or_default()),
                chat_rowid: row.get::<_, Option<i64>>(2)?.unwrap_or_default(),
                message_rowid: row.get::<_, Option<i64>>(3)?.unwrap_or_default(),
                handle: row.get(4)?,
                is_from_me: row.get::<_, Option<i64>>(5)?.unwrap_or_default() != 0,
                date: row.get::<_, Option<i64>>(6)?.unwrap_or_default(),
            })
        },
    )?;
    let mut hits = rows.collect::<Result<Vec<_>, _>>()?;
    hits.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| b.message_rowid.cmp(&a.message_rowid))
    });
    Ok(hits)
}

/// Build or refresh the index at `index_path` from `db_path`, in this thread.
///
/// This is what the worker runs; it is public so a test can index a fixture
/// without a thread and without a clock.
///
/// # Errors
///
/// Fails if either database refuses a query, or the index cannot be created.
pub fn build(db_path: &Path, index_path: &Path) -> Result<(), SearchError> {
    let chat = Db::open(db_path)?;
    let mut index = open_index(index_path)?;
    let cancel = AtomicBool::new(false);
    ingest(&chat, &mut index, &cancel, &mut |_, _| {})
}

/// The index schema. `body` is the only searched column; everything else rides
/// along so a hit can be turned into a jump without touching `chat.db`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS entries USING fts5(
    body,
    kind UNINDEXED,
    chat_rowid UNINDEXED,
    message_rowid UNINDEXED,
    handle UNINDEXED,
    is_from_me UNINDEXED,
    date UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_becomes_quoted_prefix_terms() {
        assert_eq!(tokens("thai food"), vec!["thai", "food"]);
        assert_eq!(tokens("  "), Vec::<String>::new());
        assert_eq!(
            match_expression(&tokens("thai food")).as_deref(),
            Some("\"thai\"* \"food\"*")
        );
        assert_eq!(match_expression(&tokens("!!!")), None);
    }

    #[test]
    fn fts_syntax_in_a_query_is_data_not_syntax() {
        // Everything an FTS5 expression could mean is stripped or quoted, so a
        // query full of operators is still just words.
        let expression = match_expression(&tokens("a AND b* OR ^c")).expect("terms");
        assert_eq!(expression, "\"a\"* \"and\"* \"b\"* \"or\"* \"c\"*");
    }

    #[test]
    fn progress_notes_read_as_percentages() {
        assert!(State::Idle.note().is_none());
        assert!(State::Ready.note().is_none());
        assert_eq!(
            State::Building {
                done: 50,
                total: 200
            }
            .note()
            .as_deref(),
            Some("indexing messages… 25%")
        );
        assert_eq!(
            State::Building { done: 0, total: 0 }.note().as_deref(),
            Some("indexing messages…")
        );
        assert!(
            State::Failed("index unusable".to_string())
                .note()
                .is_some_and(|note| note.contains("unusable"))
        );
    }

    #[test]
    fn kinds_round_trip_through_their_tags() {
        for kind in [Kind::Message, Kind::Photo, Kind::File] {
            assert_eq!(Kind::from_tag(kind.tag()), kind);
        }
        assert_eq!(Kind::from_tag("?"), Kind::Message);
        assert!(Kind::Photo.is_attachment());
        assert!(!Kind::Message.is_attachment());
    }
}
