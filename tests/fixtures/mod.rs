//! A small synthetic `chat.db`, built here and never copied from a real one.
//!
//! Every name, number, and body in this file is invented. The tables carry the
//! column names macOS actually uses, so the queries under test are the queries
//! that will run against the real database — but no test ever points at
//! `~/Library/Messages/chat.db`.
//!
//! The file is built once into `tests/fixtures/synthetic.db` (gitignored) and
//! opened read-only after that. Building goes to a uniquely named temporary
//! file and is renamed into place, so parallel test threads cannot see a
//! half-written database.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::Connection;

/// Handle rowids, so tests can name people without repeating numbers.
pub const HANDLE_ALEX: i64 = 1;
pub const HANDLE_BAILEY: i64 = 2;
pub const HANDLE_CASEY: i64 = 3;

/// Chat rowids.
pub const CHAT_DIRECT: i64 = 1;
pub const CHAT_GROUP: i64 = 2;
pub const CHAT_EMPTY: i64 = 3;

/// Message rowids worth naming.
pub const MSG_PLAIN: i64 = 1;
pub const MSG_ATTRIBUTED: i64 = 2;
pub const MSG_PHOTO: i64 = 3;
pub const MSG_UNREAD: i64 = 4;
pub const MSG_GROUP_FIRST: i64 = 10;
pub const MSG_GROUP_REPLY: i64 = 11;
pub const MSG_GROUP_RENAME: i64 = 12;

/// Body text that only exists inside `attributedBody`, never in `text`.
pub const ATTRIBUTED_BODY: &str = "recovered from the typedstream";

/// Nanoseconds per second, for readable timestamps.
const SECOND: i64 = 1_000_000_000;
/// An arbitrary but fixed instant in the Messages epoch: 2022-05-17 22:29:42Z.
const BASE: i64 = 674_526_582 * SECOND;

/// Path to the fixture database, building it if it is not there yet.
///
/// Test threads share one build: the `OnceLock` keeps the other threads waiting
/// rather than racing, and the rename means a concurrent test process sees
/// either the finished file or no file at all.
pub fn database() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("synthetic.db");
            if path.exists() {
                return path;
            }

            let staging = path.with_extension(format!("db.building-{}", std::process::id()));
            let _ = std::fs::remove_file(&staging);
            build(&staging).expect("build the synthetic fixture database");
            std::fs::rename(&staging, &path).expect("move the fixture into place");
            path
        })
        .clone()
}

/// Create the schema and fill it with invented conversations.
fn build(path: &Path) -> rusqlite::Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;

    handle(&conn, HANDLE_ALEX, "+15550000001", "iMessage")?;
    handle(&conn, HANDLE_BAILEY, "+15550000002", "iMessage")?;
    handle(&conn, HANDLE_CASEY, "casey@example.invalid", "iMessage")?;

    chat(&conn, CHAT_DIRECT, "iMessage;-;+15550000001", 45, None)?;
    chat(
        &conn,
        CHAT_GROUP,
        "iMessage;+;chat999",
        43,
        Some("Fixture Group"),
    )?;
    chat(&conn, CHAT_EMPTY, "iMessage;-;+15550000009", 45, None)?;

    for (chat_id, handle_id) in [
        (CHAT_DIRECT, HANDLE_ALEX),
        (CHAT_GROUP, HANDLE_ALEX),
        (CHAT_GROUP, HANDLE_BAILEY),
        (CHAT_GROUP, HANDLE_CASEY),
    ] {
        conn.execute(
            "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
            (chat_id, handle_id),
        )?;
    }

    // A one-to-one thread: they say something, you answer with an attached
    // photo, they answer with a body that only lives in `attributedBody`, and
    // one last incoming message stays unread.
    message(
        &conn,
        Row {
            rowid: MSG_PLAIN,
            chat: CHAT_DIRECT,
            handle: HANDLE_ALEX,
            from_me: false,
            date: BASE,
            text: Some("first fixture message"),
            read: true,
            ..Row::default()
        },
    )?;
    message(
        &conn,
        Row {
            rowid: MSG_ATTRIBUTED,
            chat: CHAT_DIRECT,
            handle: HANDLE_ALEX,
            from_me: false,
            date: BASE + 60 * SECOND,
            attributed: Some(ATTRIBUTED_BODY),
            read: true,
            ..Row::default()
        },
    )?;
    message(
        &conn,
        Row {
            rowid: MSG_PHOTO,
            chat: CHAT_DIRECT,
            handle: 0,
            from_me: true,
            date: BASE + 120 * SECOND,
            text: Some("\u{FFFC}"),
            date_delivered: BASE + 121 * SECOND,
            date_read: BASE + 200 * SECOND,
            read: true,
            ..Row::default()
        },
    )?;
    attachment(&conn, 1, MSG_PHOTO, "photo.png", "image/png", 2048)?;
    message(
        &conn,
        Row {
            rowid: MSG_UNREAD,
            chat: CHAT_DIRECT,
            handle: HANDLE_ALEX,
            from_me: false,
            date: BASE + 300 * SECOND,
            text: Some("still unread"),
            ..Row::default()
        },
    )?;

    // Tapbacks on the first message: Alex likes it, unlikes it, then laughs at
    // it, and you love it. Two reactions should survive.
    tapback(
        &conn,
        5,
        CHAT_DIRECT,
        HANDLE_ALEX,
        2001,
        MSG_PLAIN,
        BASE + 10 * SECOND,
    )?;
    tapback(
        &conn,
        6,
        CHAT_DIRECT,
        HANDLE_ALEX,
        3001,
        MSG_PLAIN,
        BASE + 20 * SECOND,
    )?;
    tapback(
        &conn,
        7,
        CHAT_DIRECT,
        HANDLE_ALEX,
        2003,
        MSG_PLAIN,
        BASE + 30 * SECOND,
    )?;
    tapback(
        &conn,
        8,
        CHAT_DIRECT,
        0,
        2000,
        MSG_PLAIN,
        BASE + 40 * SECOND,
    )?;

    // A group thread with a reply and a rename event.
    message(
        &conn,
        Row {
            rowid: MSG_GROUP_FIRST,
            chat: CHAT_GROUP,
            handle: HANDLE_BAILEY,
            from_me: false,
            date: BASE + 400 * SECOND,
            text: Some("group opener"),
            read: true,
            ..Row::default()
        },
    )?;
    let originator = guid(MSG_GROUP_FIRST);
    message(
        &conn,
        Row {
            rowid: MSG_GROUP_REPLY,
            chat: CHAT_GROUP,
            handle: HANDLE_CASEY,
            from_me: false,
            date: BASE + 500 * SECOND,
            text: Some("group reply"),
            thread_originator: Some(&originator),
            ..Row::default()
        },
    )?;
    message(
        &conn,
        Row {
            rowid: MSG_GROUP_RENAME,
            chat: CHAT_GROUP,
            handle: HANDLE_BAILEY,
            from_me: false,
            date: BASE + 600 * SECOND,
            item_type: 2,
            group_title: Some("Fixture Group"),
            ..Row::default()
        },
    )?;

    Ok(())
}

/// The invented GUID of a message, derived from its rowid so tests can predict it.
pub fn guid(rowid: i64) -> String {
    format!("FIXTURE0-0000-4000-8000-{rowid:012}")
}

/// One row of `message`, with the columns a test cares about.
#[derive(Default)]
struct Row<'a> {
    rowid: i64,
    chat: i64,
    handle: i64,
    from_me: bool,
    read: bool,
    date: i64,
    date_delivered: i64,
    date_read: i64,
    date_edited: i64,
    text: Option<&'a str>,
    attributed: Option<&'a str>,
    thread_originator: Option<&'a str>,
    item_type: i64,
    group_action_type: i64,
    group_title: Option<&'a str>,
    other_handle: i64,
}

fn message(conn: &Connection, row: Row<'_>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO message (
             ROWID, guid, text, attributedBody, handle_id, service, is_from_me, is_read,
             date, date_delivered, date_read, date_edited, item_type, group_action_type,
             group_title, other_handle, thread_originator_guid, associated_message_type
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'iMessage', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 0)",
        rusqlite::params![
            row.rowid,
            guid(row.rowid),
            row.text,
            row.attributed.map(streamtyped_blob),
            row.handle,
            i64::from(row.from_me),
            i64::from(row.read),
            row.date,
            row.date_delivered,
            row.date_read,
            row.date_edited,
            row.item_type,
            row.group_action_type,
            row.group_title,
            row.other_handle,
            row.thread_originator,
        ],
    )?;
    conn.execute(
        "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, ?2, ?3)",
        (row.chat, row.rowid, row.date),
    )?;
    Ok(())
}

fn tapback(
    conn: &Connection,
    rowid: i64,
    chat: i64,
    handle: i64,
    kind: i64,
    target: i64,
    date: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO message (
             ROWID, guid, handle_id, service, is_from_me, is_read, date,
             associated_message_guid, associated_message_type
         ) VALUES (?1, ?2, ?3, 'iMessage', ?4, 1, ?5, ?6, ?7)",
        rusqlite::params![
            rowid,
            guid(rowid),
            handle,
            i64::from(handle == 0),
            date,
            format!("p:0/{}", guid(target)),
            kind,
        ],
    )?;
    conn.execute(
        "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, ?2, ?3)",
        (chat, rowid, date),
    )?;
    Ok(())
}

fn attachment(
    conn: &Connection,
    rowid: i64,
    message_id: i64,
    name: &str,
    mime: &str,
    bytes: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO attachment (
             ROWID, guid, original_guid, filename, uti, mime_type, transfer_name,
             total_bytes, transfer_state, is_sticker, hide_attachment
         ) VALUES (?1, ?2, ?2, ?3, 'public.png', ?4, ?5, ?6, 5, 0, 0)",
        rusqlite::params![
            rowid,
            format!("ATTACH-{rowid}"),
            format!("~/Library/Messages/Attachments/fixture/{name}"),
            mime,
            name,
            bytes,
        ],
    )?;
    conn.execute(
        "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (?1, ?2)",
        (message_id, rowid),
    )
    .map(|_| ())
}

fn handle(conn: &Connection, rowid: i64, id: &str, service: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO handle (ROWID, id, service) VALUES (?1, ?2, ?3)",
        (rowid, id, service),
    )
    .map(|_| ())
}

fn chat(
    conn: &Connection,
    rowid: i64,
    guid: &str,
    style: i64,
    display_name: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO chat (ROWID, guid, style, chat_identifier, service_name, display_name)
         VALUES (?1, ?2, ?3, ?4, 'iMessage', ?5)",
        rusqlite::params![rowid, guid, style, guid, display_name],
    )
    .map(|_| ())
}

/// Wrap `text` in the smallest blob `streamtyped::parse` will accept.
///
/// Real blobs are a full `typedstream` archive; the fallback parser only looks
/// for the start marker, a length byte, the text, and the end marker, so that
/// is exactly what this writes.
pub fn streamtyped_blob(text: &str) -> Vec<u8> {
    let mut blob = b"streamtyped".to_vec();
    blob.extend_from_slice(&[0x81, 0xe8, 0x03, 0x84, 0x40]);
    blob.extend_from_slice(&[0x01, 0x2b]);
    blob.push(u8::try_from(text.len()).unwrap_or(0x7f));
    blob.extend_from_slice(text.as_bytes());
    blob.extend_from_slice(&[0x86, 0x84]);
    blob.extend_from_slice(&[0x02, 0x69, 0x49, 0x01]);
    blob
}

/// The subset of the macOS schema the queries touch, with the real column names.
const SCHEMA: &str = "
CREATE TABLE handle (
    ROWID INTEGER PRIMARY KEY AUTOINCREMENT UNIQUE,
    id TEXT NOT NULL,
    country TEXT,
    service TEXT NOT NULL,
    uncanonicalized_id TEXT,
    person_centric_id TEXT,
    UNIQUE (id, service)
);

CREATE TABLE chat (
    ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
    guid TEXT UNIQUE NOT NULL,
    style INTEGER,
    state INTEGER,
    account_id TEXT,
    chat_identifier TEXT,
    service_name TEXT,
    room_name TEXT,
    account_login TEXT,
    is_archived INTEGER DEFAULT 0,
    display_name TEXT,
    group_id TEXT,
    last_read_message_timestamp INTEGER DEFAULT 0
);

CREATE TABLE message (
    ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
    guid TEXT UNIQUE NOT NULL,
    text TEXT,
    handle_id INTEGER DEFAULT 0,
    subject TEXT,
    attributedBody BLOB,
    service TEXT,
    date INTEGER,
    date_read INTEGER,
    date_delivered INTEGER,
    is_from_me INTEGER DEFAULT 0,
    is_read INTEGER DEFAULT 0,
    is_sent INTEGER DEFAULT 0,
    cache_has_attachments INTEGER DEFAULT 0,
    item_type INTEGER DEFAULT 0,
    other_handle INTEGER DEFAULT 0,
    group_title TEXT,
    group_action_type INTEGER DEFAULT 0,
    associated_message_guid TEXT,
    associated_message_type INTEGER DEFAULT 0,
    associated_message_emoji TEXT DEFAULT NULL,
    reply_to_guid TEXT,
    thread_originator_guid TEXT,
    thread_originator_part TEXT,
    date_edited INTEGER DEFAULT 0
);

CREATE TABLE chat_message_join (
    chat_id INTEGER REFERENCES chat (ROWID) ON DELETE CASCADE,
    message_id INTEGER REFERENCES message (ROWID) ON DELETE CASCADE,
    message_date INTEGER DEFAULT 0,
    PRIMARY KEY (chat_id, message_id)
);

CREATE TABLE chat_handle_join (
    chat_id INTEGER REFERENCES chat (ROWID) ON DELETE CASCADE,
    handle_id INTEGER REFERENCES handle (ROWID) ON DELETE CASCADE,
    UNIQUE (chat_id, handle_id)
);

CREATE TABLE attachment (
    ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
    guid TEXT UNIQUE NOT NULL,
    created_date INTEGER DEFAULT 0,
    filename TEXT,
    uti TEXT,
    mime_type TEXT,
    transfer_state INTEGER DEFAULT 0,
    is_outgoing INTEGER DEFAULT 0,
    transfer_name TEXT,
    total_bytes INTEGER DEFAULT 0,
    is_sticker INTEGER DEFAULT 0,
    hide_attachment INTEGER DEFAULT 0,
    original_guid TEXT UNIQUE NOT NULL
);

CREATE TABLE message_attachment_join (
    message_id INTEGER REFERENCES message (ROWID) ON DELETE CASCADE,
    attachment_id INTEGER REFERENCES attachment (ROWID) ON DELETE CASCADE,
    UNIQUE (message_id, attachment_id)
);
";
