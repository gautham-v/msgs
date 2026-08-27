//! The local read state, driven through the app the way the event loop drives
//! it.
//!
//! Everything here runs against the synthetic fixture database and writes its
//! state into a scratch directory of its own, so no test reads
//! `~/Library/Messages/chat.db` or writes anything into the user's home. The
//! last test asserts what the whole feature promises: the database is not
//! touched.

mod fixtures;

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use msgs::app::{Action, App, Focus};
use msgs::config::Config;
use msgs::keymap;

/// A private directory for one test's read state.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "msgs-seen-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("seen.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The fixture database, opened but with no read state yet.
fn app() -> App {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app
}

/// Press a key the way the event loop would.
fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let key = KeyEvent::new(code, modifiers);
    if let Some(action) = keymap::resolve(key, app.key_focus()) {
        app.update(action);
    }
}

#[test]
fn opening_a_chat_clears_its_badge_here() {
    let scratch = Scratch::new("opening");
    let mut app = app();

    // Two chats hold one unread message each before msgs has shown either.
    assert_eq!(app.status.unread_total, 2);
    assert_eq!(app.status.unread_chats, 2);

    // Enabling the state marks whatever is already on screen, which is the
    // conversation the list opens on.
    app.enable_seen(&scratch.path());
    assert_eq!(app.status.unread_total, 1);

    app.open_chat_row(fixtures::CHAT_DIRECT);
    app.open_chat_row(fixtures::CHAT_GROUP);
    assert_eq!(app.status.unread_total, 0);
    assert_eq!(app.status.unread_chats, 0);
    assert!(app.chat_rows.iter().all(|chat| !chat.is_unread()));

    // The database's own counts are untouched by any of it.
    assert_eq!(
        app.chat_rows
            .iter()
            .map(|chat| chat.unread_count)
            .sum::<i64>(),
        2
    );
}

#[test]
fn the_state_survives_a_restart() {
    let scratch = Scratch::new("restart");

    let mut first = app();
    first.enable_seen(&scratch.path());
    first.open_chat_row(fixtures::CHAT_DIRECT);
    first.open_chat_row(fixtures::CHAT_GROUP);
    assert_eq!(first.status.unread_total, 0);
    assert!(scratch.path().is_file(), "the state was written");

    let mut again = app();
    again.enable_seen(&scratch.path());
    assert_eq!(again.status.unread_total, 0, "still seen after a restart");
}

#[test]
fn ctrl_u_marks_everything_seen_and_gives_it_back() {
    let scratch = Scratch::new("toggle");
    let mut app = app();
    app.enable_seen(&scratch.path());
    app.focus = Focus::ChatList;

    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.status.unread_total, 0, "everything marked seen");

    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.status.unread_total, 2, "the database's counts are back");

    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.status.unread_total, 0);
}

#[test]
fn ctrl_u_still_clears_the_line_in_the_composer() {
    let mut app = app();
    app.focus = Focus::Composer;
    for c in "half a message".chars() {
        app.update(Action::Insert(c));
    }
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert!(app.composer.text().is_empty());
}

#[test]
fn without_a_state_file_nothing_is_tracked() {
    let mut app = app();
    app.open_chat_row(fixtures::CHAT_DIRECT);
    app.open_chat_row(fixtures::CHAT_GROUP);
    assert_eq!(
        app.status.unread_total, 2,
        "read state is off until something asks for it"
    );
}

#[test]
fn the_state_holds_row_numbers_and_never_touches_the_database() {
    let scratch = Scratch::new("private");
    let db = fixtures::database();
    let before = std::fs::metadata(&db).expect("the fixture");

    let mut app = app();
    app.enable_seen(&scratch.path());
    app.focus = Focus::ChatList;
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);

    let after = std::fs::metadata(&db).expect("the fixture");
    assert_eq!(before.len(), after.len(), "chat.db is the size it was");
    assert_eq!(
        before.modified().ok(),
        after.modified().ok(),
        "chat.db was not written"
    );

    // Row numbers and counts only: no address, no name, no body.
    let written = std::fs::read_to_string(scratch.path()).expect("the state file");
    assert!(!written.contains('@'), "no email address is stored");
    assert!(!written.contains('+'), "no phone number is stored");
}

/// A writable copy of the fixture, so a message can "arrive" without the
/// shared fixture — or any real store — ever being written to.
fn copy_of_the_fixture(scratch: &Scratch) -> PathBuf {
    std::fs::create_dir_all(&scratch.dir).expect("scratch directory");
    let db = scratch.dir.join("chat.db");
    std::fs::copy(fixtures::database(), &db).expect("copy the fixture");
    db
}

/// One invented incoming message, straight into the copy.
fn arrives(db: &std::path::Path, rowid: i64, chat: i64, at: i64) {
    let conn = rusqlite::Connection::open(db).expect("open the copy for writing");
    conn.execute(
        "INSERT INTO message (
             ROWID, guid, text, handle_id, service, is_from_me, is_read, date,
             associated_message_type
         ) VALUES (?1, ?2, 'and one more', ?3, 'iMessage', 0, 0, ?4, 0)",
        rusqlite::params![rowid, fixtures::guid(rowid), fixtures::HANDLE_ALEX, at],
    )
    .expect("insert the arriving message");
    conn.execute(
        "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, ?2, ?3)",
        (chat, rowid, at),
    )
    .expect("join the arriving message to its chat");
}

#[test]
fn a_message_arriving_after_you_looked_counts_and_the_open_thread_does_not() {
    let scratch = Scratch::new("arrival");
    let db = copy_of_the_fixture(&scratch);

    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(db.clone());
    app.enable_seen(&scratch.path());
    app.focus = Focus::ChatList;
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.status.unread_total, 0);

    let open = app.open_chat.expect("a conversation is open");
    let other = if open == fixtures::CHAT_DIRECT {
        fixtures::CHAT_GROUP
    } else {
        fixtures::CHAT_DIRECT
    };

    // One arrives in the thread on screen: it is being read, so no badge.
    arrives(&db, 901, open, fixtures::BASE + 900 * fixtures::SECOND);
    app.on_db_change();
    assert_eq!(app.status.unread_total, 0, "you are looking at it");

    // One arrives somewhere else: exactly that one comes back as a badge, not
    // the messages that were already seen.
    arrives(&db, 902, other, fixtures::BASE + 960 * fixtures::SECOND);
    app.on_db_change();
    assert_eq!(app.status.unread_total, 1);
    assert_eq!(app.status.unread_chats, 1);
    let badge = app
        .chat_rows
        .iter()
        .find(|chat| chat.rowid == other)
        .expect("the other chat");
    assert_eq!(badge.unread, 1, "only what arrived since you looked");
    assert_eq!(badge.unread_count, 2, "the database still counts both");
}
