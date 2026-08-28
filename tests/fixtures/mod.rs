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
pub const SECOND: i64 = 1_000_000_000;
/// An arbitrary but fixed instant in the Messages epoch: 2022-05-17 22:29:42Z.
pub const BASE: i64 = 674_526_582 * SECOND;

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

/// How many messages [`large_database`] invents.
pub const LARGE_MESSAGES: usize = 100_000;

/// A word that appears on exactly one message of [`large_database`].
pub const LARGE_NEEDLE: &str = "quokka";

/// A hundred thousand invented messages, for the tests that care about scale.
///
/// Built once into `tests/fixtures/large.db` (gitignored) the same way the
/// small fixture is: every body is made up here, and nothing is ever copied
/// out of a real database.
pub fn large_database() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("large.db");
            if path.exists() {
                return path;
            }
            let staging = path.with_extension(format!("db.building-{}", std::process::id()));
            let _ = std::fs::remove_file(&staging);
            build_large(&staging).expect("build the large fixture database");
            std::fs::rename(&staging, &path).expect("move the large fixture into place");
            path
        })
        .clone()
}

/// Fill a database with [`LARGE_MESSAGES`] invented bodies in one chat.
fn build_large(path: &Path) -> rusqlite::Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
    conn.execute_batch(SCHEMA)?;
    handle(&conn, HANDLE_ALEX, "+15550000001", "iMessage")?;
    chat(&conn, CHAT_DIRECT, "iMessage;-;+15550000001", 45, None)?;
    conn.execute(
        "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
        (CHAT_DIRECT, HANDLE_ALEX),
    )?;

    // A small vocabulary, combined by rowid, so every body is different and
    // none of it came from anywhere real.
    const WORDS: [&str; 8] = [
        "morning", "dinner", "later", "office", "train", "coffee", "tomorrow", "photos",
    ];
    let tx = conn.unchecked_transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO message (ROWID, guid, text, handle_id, service, is_from_me, is_read,
                                  date, associated_message_type, item_type)
             VALUES (?1, ?2, ?3, ?4, 'iMessage', ?5, 1, ?6, 0, 0)",
        )?;
        let mut join = tx.prepare(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, ?2, ?3)",
        )?;
        for rowid in 1..=LARGE_MESSAGES {
            let n = rowid as i64;
            let body = if rowid == LARGE_MESSAGES / 2 {
                format!("the {LARGE_NEEDLE} sends its regards")
            } else {
                format!(
                    "{} {} {n}",
                    WORDS[rowid % WORDS.len()],
                    WORDS[(rowid / 3) % WORDS.len()]
                )
            };
            insert.execute(rusqlite::params![
                n,
                guid(n),
                body,
                if rowid % 2 == 0 { HANDLE_ALEX } else { 0 },
                i64::from(rowid % 2 != 0),
                BASE + n * SECOND,
            ])?;
            join.execute(rusqlite::params![CHAT_DIRECT, n, BASE + n * SECOND])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// How many messages [`perf_database`] invents: the size the perf budgets in
/// `tests/perf.rs` are stated against.
pub const PERF_MESSAGES: i64 = 200_000;

/// How many chats those messages are spread across.
pub const PERF_CHATS: i64 = 60;

/// The chat holding half of [`PERF_MESSAGES`], so a page can be measured
/// against a thread far deeper than any page it loads.
pub const PERF_DEEP_CHAT: i64 = 1;

/// Two hundred thousand invented messages across [`PERF_CHATS`] chats.
///
/// The shape a busy Mac has after a decade: one enormous thread, dozens of
/// smaller ones interleaved with it, a scattering of photos, and some unread.
/// Built once into `tests/fixtures/perf.db` (gitignored) exactly the way the
/// other two fixtures are — every body, number, and name is invented here and
/// nothing is ever copied out of a real store.
pub fn perf_database() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("perf.db");
            if path.exists() {
                return path;
            }
            let staging = path.with_extension(format!("db.building-{}", std::process::id()));
            let _ = std::fs::remove_file(&staging);
            build_perf(&staging).expect("build the perf fixture database");
            std::fs::rename(&staging, &path).expect("move the perf fixture into place");
            path
        })
        .clone()
}

/// Which chat message `n` of the perf fixture belongs to: every other message
/// lands in the deep thread, and the rest go round the other chats.
fn perf_chat_of(n: i64) -> i64 {
    if n % 2 == 0 {
        PERF_DEEP_CHAT
    } else {
        2 + (n / 2) % (PERF_CHATS - 1)
    }
}

/// Fill a database with [`PERF_MESSAGES`] invented bodies across
/// [`PERF_CHATS`] chats.
fn build_perf(path: &Path) -> rusqlite::Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
    conn.execute_batch(SCHEMA)?;

    // One handle per chat, plus three that sit in every group, so a person can
    // be in more than one conversation the way a real address book is.
    let shared = PERF_CHATS + 1..=PERF_CHATS + 3;
    for rowid in 1..=PERF_CHATS + 3 {
        handle(&conn, rowid, &format!("+1555{rowid:07}"), "iMessage")?;
    }
    for rowid in 1..=PERF_CHATS {
        let group = rowid % 5 == 0;
        let (style, name) = if group {
            (43, Some(format!("Fixture Group {rowid}")))
        } else {
            (45, None)
        };
        chat(
            &conn,
            rowid,
            &format!("iMessage;{};perf{rowid:04}", if group { '+' } else { '-' }),
            style,
            name.as_deref(),
        )?;
        conn.execute(
            "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
            (rowid, rowid),
        )?;
        if group {
            for handle_id in shared.clone() {
                conn.execute(
                    "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
                    (rowid, handle_id),
                )?;
            }
        }
    }

    const WORDS: [&str; 8] = [
        "morning", "dinner", "later", "office", "train", "coffee", "tomorrow", "photos",
    ];
    let tx = conn.unchecked_transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO message (ROWID, guid, text, handle_id, service, is_from_me, is_read,
                                  date, date_delivered, cache_has_attachments,
                                  associated_message_type, item_type)
             VALUES (?1, ?2, ?3, ?4, 'iMessage', ?5, ?6, ?7, ?8, ?9, 0, 0)",
        )?;
        let mut join = tx.prepare(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, ?2, ?3)",
        )?;
        let mut attach = tx.prepare(
            "INSERT INTO attachment (ROWID, guid, original_guid, filename, uti, mime_type,
                                     transfer_name, total_bytes, transfer_state,
                                     is_sticker, hide_attachment)
             VALUES (?1, ?2, ?2, ?3, 'public.png', 'image/png', ?4, 4096, 5, 0, 0)",
        )?;
        let mut attach_join = tx.prepare(
            "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (?1, ?2)",
        )?;

        let mut attachments = 0i64;
        for n in 1..=PERF_MESSAGES {
            let chat_id = perf_chat_of(n);
            let from_me = n % 3 == 0;
            let photo = n % 500 == 0;
            let body = if photo {
                "\u{FFFC}".to_string()
            } else {
                format!(
                    "{} {} {n}",
                    WORDS[(n as usize) % WORDS.len()],
                    WORDS[(n as usize / 3) % WORDS.len()]
                )
            };
            // A scattering of incoming messages nobody has read, so the unread
            // totals and their badges have something to add up.
            let read = from_me || n % 997 != 0;
            insert.execute(rusqlite::params![
                n,
                guid(n),
                body,
                if from_me { 0 } else { chat_id },
                i64::from(from_me),
                i64::from(read),
                BASE + n * SECOND,
                if from_me { BASE + n * SECOND } else { 0 },
                i64::from(photo),
            ])?;
            join.execute(rusqlite::params![chat_id, n, BASE + n * SECOND])?;
            if photo {
                attachments += 1;
                attach.execute(rusqlite::params![
                    attachments,
                    format!("PERF-ATTACH-{attachments}"),
                    format!("~/Library/Messages/Attachments/perf/photo-{attachments}.png"),
                    format!("photo-{attachments}.png"),
                ])?;
                attach_join.execute(rusqlite::params![n, attachments])?;
            }
        }
    }
    tx.commit()?;
    conn.execute_batch("ANALYZE;")?;
    Ok(())
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

/// The addresses [`address_book`] invents names for, in the order it writes
/// them. Every one of them is one of the fixture `chat.db` handles.
pub const CONTACT_ALEX: (&str, &str, &str) = ("+1 (555) 000-0001", "Alex", "Nakamura");
pub const CONTACT_BAILEY: (&str, &str, &str) = ("555-000-0002", "Bailey", "Okonkwo");
pub const CONTACT_CASEY: (&str, &str, &str) = ("Casey@Example.Invalid", "Casey", "Lindqvist");

/// Build a synthetic macOS Contacts store at `dir/AddressBook-v22.abcddb`.
///
/// The tables carry the column names Core Data actually uses, so the queries
/// under test are the queries that will run against a real Mac — but every name
/// and number in it is invented here, and no test ever opens
/// `~/Library/Application Support/AddressBook`.
///
/// The numbers are deliberately written the way a person types them into
/// Contacts — spaces, parentheses, a missing country code — rather than in the
/// E.164 form `chat.db` stores, because joining those two is the whole job.
pub fn address_book(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("contacts directory");
    let path = dir.join("AddressBook-v22.abcddb");
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).expect("open the contacts fixture");
    conn.execute_batch(ADDRESS_BOOK_SCHEMA)
        .expect("contacts schema");

    let people = [CONTACT_ALEX, CONTACT_BAILEY, CONTACT_CASEY];
    for (index, (address, first, last)) in people.into_iter().enumerate() {
        let pk = i64::try_from(index).unwrap_or(0) + 1;
        conn.execute(
            "INSERT INTO ZABCDRECORD (Z_PK, ZFIRSTNAME, ZLASTNAME, ZORGANIZATION)
             VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params![pk, first, last],
        )
        .expect("insert a contact");
        if address.contains('@') {
            conn.execute(
                "INSERT INTO ZABCDEMAILADDRESS (Z_PK, ZOWNER, ZADDRESS) VALUES (?1, ?2, ?3)",
                rusqlite::params![pk, pk, address],
            )
            .expect("insert an email address");
        } else {
            conn.execute(
                "INSERT INTO ZABCDPHONENUMBER (Z_PK, ZOWNER, ZFULLNUMBER) VALUES (?1, ?2, ?3)",
                rusqlite::params![pk, pk, address],
            )
            .expect("insert a phone number");
        }
    }

    // A business, which Contacts stores with no personal name at all.
    conn.execute(
        "INSERT INTO ZABCDRECORD (Z_PK, ZFIRSTNAME, ZLASTNAME, ZORGANIZATION)
         VALUES (99, NULL, NULL, 'Fixture Coffee')",
        [],
    )
    .expect("insert an organization");
    conn.execute(
        "INSERT INTO ZABCDPHONENUMBER (Z_PK, ZOWNER, ZFULLNUMBER) VALUES (99, 99, '+15550000009')",
        [],
    )
    .expect("insert the organization number");

    drop(conn);
    path
}

/// The slice of the Core Data schema the contacts queries touch.
const ADDRESS_BOOK_SCHEMA: &str = "
CREATE TABLE ZABCDRECORD (
    Z_PK INTEGER PRIMARY KEY,
    Z_ENT INTEGER,
    Z_OPT INTEGER,
    ZFIRSTNAME VARCHAR,
    ZLASTNAME VARCHAR,
    ZMIDDLENAME VARCHAR,
    ZNICKNAME VARCHAR,
    ZORGANIZATION VARCHAR
);

CREATE TABLE ZABCDPHONENUMBER (
    Z_PK INTEGER PRIMARY KEY,
    Z_ENT INTEGER,
    Z_OPT INTEGER,
    ZOWNER INTEGER,
    ZISPRIMARY INTEGER,
    ZFULLNUMBER VARCHAR,
    ZLABEL VARCHAR
);

CREATE TABLE ZABCDEMAILADDRESS (
    Z_PK INTEGER PRIMARY KEY,
    Z_ENT INTEGER,
    Z_OPT INTEGER,
    ZOWNER INTEGER,
    ZISPRIMARY INTEGER,
    ZADDRESS VARCHAR,
    ZLABEL VARCHAR
);
";

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
    error INTEGER DEFAULT 0,
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

-- The indexes macOS keeps on its own store, under the names it uses. A query
-- plan measured here is then the plan that runs against the real database.
CREATE INDEX chat_message_join_idx_message_id ON chat_message_join (message_id);
CREATE INDEX chat_message_join_idx_chat_id ON chat_message_join (chat_id, message_date);
CREATE INDEX message_idx_date ON message (date);
CREATE INDEX message_idx_handle_id ON message (handle_id);
CREATE INDEX message_idx_associated_message ON message (associated_message_guid);
CREATE INDEX message_attachment_join_idx_message_id ON message_attachment_join (message_id);
CREATE INDEX chat_handle_join_idx_handle_id ON chat_handle_join (handle_id);
";
