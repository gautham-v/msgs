//! Live updates, driven against a private copy of the synthetic fixture.
//!
//! The event loop cannot be run from a test, so these drive the two halves of
//! it separately: `watch::Watcher` decides *when* to re-read, and
//! `App::on_db_change` does the re-reading. The tests below call the second
//! directly and let the unit tests in `watch` cover the first.
//!
//! Every test copies `tests/fixtures/synthetic.db` somewhere private and writes
//! to the copy, which is how a message "arrives". Nothing here goes anywhere
//! near `~/Library/Messages/chat.db`, and the shared fixture itself is only
//! ever read.

mod fixtures;

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use msgs::app::{Action, App, Focus, WatcherStatus};
use msgs::config::Config;
use msgs::keymap;
use msgs::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use rusqlite::Connection;

/// A writable copy of the fixture, deleted when the test ends.
struct Store {
    dir: PathBuf,
    db: PathBuf,
}

impl Store {
    /// Copy the fixture into a directory of this test's own.
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("msgs-live-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        let db = dir.join("chat.db");
        std::fs::copy(fixtures::database(), &db).expect("copy the fixture");
        Self { dir, db }
    }

    fn path(&self) -> &Path {
        &self.db
    }

    /// A message arrives: one row in `message`, one in `chat_message_join`.
    ///
    /// Writes to the copy, never to the fixture and never to a real store.
    fn arrives(&self, rowid: i64, chat: i64, handle: i64, body: &str, at: i64) {
        let conn = Connection::open(&self.db).expect("open the copy for writing");
        conn.execute(
            "INSERT INTO message (
                 ROWID, guid, text, handle_id, service, is_from_me, is_read, date,
                 associated_message_type
             ) VALUES (?1, ?2, ?3, ?4, 'iMessage', 0, 0, ?5, 0)",
            rusqlite::params![rowid, fixtures::guid(rowid), body, handle, at],
        )
        .expect("insert the arriving message");
        conn.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (?1, ?2, ?3)",
            (chat, rowid, at),
        )
        .expect("join the arriving message to its chat");
    }

    /// A tapback arrives on `target`.
    fn reacts(&self, rowid: i64, chat: i64, handle: i64, kind: i64, target: i64, at: i64) {
        let conn = Connection::open(&self.db).expect("open the copy for writing");
        conn.execute(
            "INSERT INTO message (
                 ROWID, guid, handle_id, service, is_from_me, is_read, date,
                 associated_message_guid, associated_message_type
             ) VALUES (?1, ?2, ?3, 'iMessage', 0, 1, ?4, ?5, ?6)",
            rusqlite::params![
                rowid,
                fixtures::guid(rowid),
                handle,
                at,
                format!("p:0/{}", fixtures::guid(target)),
                kind,
            ],
        )
        .expect("insert the tapback");
        conn.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (?1, ?2, ?3)",
            (chat, rowid, at),
        )
        .expect("join the tapback to its chat");
    }

    /// An already-delivered message is edited in place.
    /// A tapback of your own arrives on `target`, as Messages records it once
    /// the reaction has been round-tripped through the service.
    fn i_react(&self, rowid: i64, chat: i64, kind: i64, target: i64, at: i64) {
        let conn = Connection::open(&self.db).expect("open the copy for writing");
        conn.execute(
            "INSERT INTO message (
                 ROWID, guid, handle_id, service, is_from_me, is_read, date,
                 associated_message_guid, associated_message_type
             ) VALUES (?1, ?2, 0, 'iMessage', 1, 1, ?3, ?4, ?5)",
            rusqlite::params![
                rowid,
                fixtures::guid(rowid),
                at,
                format!("p:0/{}", fixtures::guid(target)),
                kind,
            ],
        )
        .expect("insert my tapback");
        conn.execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date)
             VALUES (?1, ?2, ?3)",
            (chat, rowid, at),
        )
        .expect("join my tapback to its chat");
    }

    fn edits(&self, rowid: i64, body: &str, at: i64) {
        let conn = Connection::open(&self.db).expect("open the copy for writing");
        conn.execute(
            "UPDATE message SET text = ?2, date_edited = ?3 WHERE ROWID = ?1",
            rusqlite::params![rowid, body, at],
        )
        .expect("edit the message");
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An app reading `store`, with the one-to-one fixture chat open.
fn app_on(store: &Store) -> App {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(store.path().to_path_buf());
    assert!(app.db_error.is_none(), "the copy must open read-only");
    // Open the one-to-one thread wherever recency has put it.
    let row = app
        .visible_chats
        .iter()
        .position(|index| app.chat_rows[*index].rowid == fixtures::CHAT_DIRECT)
        .expect("the direct chat is in the list");
    app.chats.selected = row;
    app.refresh_chat_view();
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    app
}

/// Draw one frame, which is what measures the blocks and settles the viewport.
fn frame(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw succeeded");
    terminal.backend().buffer().clone()
}

fn contains(buffer: &Buffer, needle: &str) -> bool {
    let area = buffer.area;
    (0..area.height).any(|y| {
        let row: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        row.contains(needle)
    })
}

/// Fill a chat out past the height of any pane a test draws.
fn fill(store: &Store, chat: i64, handle: i64, count: i64) {
    for n in 0..count {
        store.arrives(
            100 + n,
            chat,
            handle,
            "filler",
            fixtures::BASE + (1000 + n) * fixtures::SECOND,
        );
    }
}

#[test]
fn opening_a_database_starts_the_watcher() {
    let store = Store::new("watcher");
    let app = app_on(&store);
    assert_eq!(app.status.watcher, WatcherStatus::Watching);
    assert_eq!(app.watcher.status(), WatcherStatus::Watching);
    assert!(app.status.last_update.is_none());
}

#[test]
fn a_message_arriving_in_the_open_chat_is_appended_and_followed() {
    let store = Store::new("append");
    let mut app = app_on(&store);
    let before = app.message_rows.len();

    store.arrives(
        200,
        fixtures::CHAT_DIRECT,
        fixtures::HANDLE_ALEX,
        "arrived while msgs was running",
        fixtures::BASE + 900 * fixtures::SECOND,
    );
    assert!(app.on_db_change());

    assert_eq!(app.message_rows.len(), before + 1);
    assert_eq!(
        app.message_rows.last().map(|message| message.rowid),
        Some(200)
    );
    // The reader was at the bottom, so the view follows rather than offering
    // a pill.
    assert_eq!(app.new_below, 0);
    assert!(app.status.last_update.is_some());
}

#[test]
fn an_edit_to_a_loaded_row_moves_the_chat_list_preview_with_it() {
    let store = Store::new("edited-preview");
    let mut app = app_on(&store);
    let newest = app
        .message_rows
        .last()
        .map(|message| message.rowid)
        .expect("the open chat has messages");

    let edited = "edited after it was already on screen";
    {
        let conn = Connection::open(&store.db).expect("open the copy for writing");
        conn.execute(
            "UPDATE message SET text = ?1 WHERE ROWID = ?2",
            rusqlite::params![edited, newest],
        )
        .expect("edit the newest message in place");
    }
    assert!(app.on_db_change());

    // The change landed in place, so nothing was appended.
    assert_eq!(
        app.message_rows.last().map(|message| message.rowid),
        Some(newest)
    );
    assert_eq!(
        app.message_rows
            .last()
            .and_then(|message| message.text.as_deref()),
        Some(edited)
    );
    // ...and the chat list's preview line moved with it.
    let open = app
        .chat_rows
        .iter()
        .find(|chat| Some(chat.rowid) == app.open_chat)
        .expect("the open chat is in the list");
    assert_eq!(
        open.preview
            .as_ref()
            .and_then(|preview| preview.text.as_deref()),
        Some(edited)
    );
}

#[test]
fn a_message_arriving_elsewhere_bumps_that_chat_to_the_top() {
    let store = Store::new("bump");
    let mut app = app_on(&store);
    // The empty chat sorts last until something lands in it.
    assert_eq!(
        app.chat_rows.last().map(|chat| chat.rowid),
        Some(fixtures::CHAT_EMPTY)
    );
    let unread_before = app.status.unread_total;

    store.arrives(
        300,
        fixtures::CHAT_EMPTY,
        fixtures::HANDLE_BAILEY,
        "first word in a quiet thread",
        fixtures::BASE + 900 * fixtures::SECOND,
    );
    assert!(app.on_db_change());

    assert_eq!(
        app.chat_rows.first().map(|chat| chat.rowid),
        Some(fixtures::CHAT_EMPTY)
    );
    assert_eq!(app.status.unread_total, unread_before + 1);
    assert!(app.chat_rows[0].is_unread());
    // The chat list reordered under the selection, which must stay on the
    // conversation the reader was reading.
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    assert_eq!(
        app.selected_chat().map(|chat| chat.rowid),
        Some(fixtures::CHAT_DIRECT)
    );
}

#[test]
fn a_message_below_the_viewport_raises_a_pill_that_clears_at_the_bottom() {
    let store = Store::new("pill");
    fill(&store, fixtures::CHAT_DIRECT, fixtures::HANDLE_ALEX, 40);
    let mut app = app_on(&store);
    frame(&mut app, 100, 30);

    // Read back from the top: the newest message is now off screen.
    app.update(Action::FocusPane(msgs::app::Focus::Conversation));
    app.update(Action::ToTop);
    frame(&mut app, 100, 30);
    assert!(!app.at_bottom(), "the fixture must overflow the pane");

    store.arrives(
        400,
        fixtures::CHAT_DIRECT,
        fixtures::HANDLE_ALEX,
        "landed below the fold",
        fixtures::BASE + 2000 * fixtures::SECOND,
    );
    assert!(app.on_db_change());

    assert_eq!(app.new_below, 1);
    let buffer = frame(&mut app, 100, 30);
    assert!(contains(&buffer, "↓ 1 new"), "the pill must be drawn");
    // The view did not jump: what was being read is still what is on screen.
    assert!(!app.at_bottom());

    store.arrives(
        401,
        fixtures::CHAT_DIRECT,
        fixtures::HANDLE_ALEX,
        "and another",
        fixtures::BASE + 2100 * fixtures::SECOND,
    );
    app.on_db_change();
    assert_eq!(app.new_below, 2);
    assert!(contains(&frame(&mut app, 100, 30), "↓ 2 new"));

    app.update(Action::ToBottom);
    let buffer = frame(&mut app, 100, 30);
    assert_eq!(app.new_below, 0);
    assert!(!contains(&buffer, "new"), "the pill must be gone");
}

#[test]
fn clicking_the_pill_jumps_to_what_it_is_counting() {
    let store = Store::new("pill-click");
    fill(&store, fixtures::CHAT_DIRECT, fixtures::HANDLE_ALEX, 40);
    let mut app = app_on(&store);
    frame(&mut app, 100, 30);
    app.update(Action::FocusPane(msgs::app::Focus::Conversation));
    app.update(Action::ToTop);
    frame(&mut app, 100, 30);

    store.arrives(
        400,
        fixtures::CHAT_DIRECT,
        fixtures::HANDLE_ALEX,
        "landed below the fold",
        fixtures::BASE + 2000 * fixtures::SECOND,
    );
    app.on_db_change();
    frame(&mut app, 100, 30);

    // Press and release on the one cell: the press anchors a drag that could
    // have been over the words, and the release is what makes it a click.
    let pill = app.hits.pill.expect("the pill was drawn");
    for kind in [
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
    ] {
        app.on_mouse(crossterm::event::MouseEvent {
            kind,
            column: pill.x,
            row: pill.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
    }
    frame(&mut app, 100, 30);

    assert_eq!(app.new_below, 0);
    assert!(app.at_bottom());
}

#[test]
fn a_tapback_lands_on_the_block_it_belongs_to() {
    let store = Store::new("tapback");
    let mut app = app_on(&store);
    let before = app.message_rows.len();
    let target = fixtures::MSG_UNREAD;
    assert!(
        app.message_rows
            .iter()
            .find(|message| message.rowid == target)
            .is_some_and(|message| message.tapbacks.is_empty())
    );

    store.reacts(
        500,
        fixtures::CHAT_DIRECT,
        fixtures::HANDLE_ALEX,
        2000,
        target,
        fixtures::BASE + 900 * fixtures::SECOND,
    );
    assert!(app.on_db_change());

    // A reaction is not a message: the thread is the same length, and the
    // block it points at is the one that changed.
    assert_eq!(app.message_rows.len(), before);
    assert_eq!(app.new_below, 0);
    let reacted = app
        .message_rows
        .iter()
        .find(|message| message.rowid == target)
        .expect("the reacted-to message is still loaded");
    assert_eq!(reacted.tapbacks.len(), 1);
}

#[test]
fn an_optimistic_chip_retires_into_the_row_the_database_brings() {
    let store = Store::new("own-tapback");
    let mut app = app_on(&store);
    app.outbox = msgs::send::Outbox::inert();
    let target = fixtures::MSG_UNREAD;
    let row = app
        .message_rows
        .iter()
        .position(|message| message.rowid == target)
        .expect("the target is loaded");
    app.update(Action::FocusPane(msgs::app::Focus::Conversation));
    app.messages.selected = row;

    // React: the chip goes up before anything is on the wire.
    app.update(Action::React);
    app.update(Action::Activate);
    assert_eq!(app.pending_tapbacks.len(), 1);
    assert_eq!(
        app.outbox.recorded().len(),
        1,
        "one imsg call was asked for"
    );
    assert!(
        app.message_rows[row].tapbacks.is_empty(),
        "the loaded page is still the database's own answer"
    );
    frame(&mut app, 100, 30);

    // Messages round-trips it and the row lands in `chat.db`.
    store.i_react(
        501,
        fixtures::CHAT_DIRECT,
        2000,
        target,
        fixtures::BASE + 901 * fixtures::SECOND,
    );
    assert!(app.on_db_change());
    assert!(
        app.pending_tapbacks.is_empty(),
        "the optimistic chip has nothing left to stand for"
    );
    let reacted = app
        .message_rows
        .iter()
        .find(|message| message.rowid == target)
        .expect("the target is still loaded");
    assert_eq!(reacted.tapbacks.len(), 1);
    assert!(reacted.tapbacks[0].is_from_me);
}

#[test]
fn an_edit_replaces_the_message_where_it_stands() {
    let store = Store::new("edit");
    let mut app = app_on(&store);
    let before = app.message_rows.len();
    let target = fixtures::MSG_PLAIN;
    assert!(
        app.message_rows
            .iter()
            .find(|message| message.rowid == target)
            .is_some_and(|message| !message.is_edited)
    );

    store.edits(
        target,
        "edited after the fact",
        fixtures::BASE + 900 * fixtures::SECOND,
    );
    assert!(app.on_db_change());

    assert_eq!(app.message_rows.len(), before);
    let edited = app
        .message_rows
        .iter()
        .find(|message| message.rowid == target)
        .expect("the edited message is still loaded");
    assert!(edited.is_edited);
    assert!(edited.edited_at().is_some());
}

#[test]
fn a_quiet_database_changes_nothing() {
    let store = Store::new("quiet");
    let mut app = app_on(&store);
    let rows = app.message_rows.clone();
    let order: Vec<i64> = app.chat_rows.iter().map(|chat| chat.rowid).collect();

    app.on_db_change();
    app.on_db_change();

    assert_eq!(app.message_rows, rows);
    assert_eq!(
        app.chat_rows
            .iter()
            .map(|chat| chat.rowid)
            .collect::<Vec<_>>(),
        order
    );
    assert_eq!(app.new_below, 0);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
}

#[test]
fn the_status_line_says_how_fresh_the_screen_is() {
    let store = Store::new("status");
    let mut app = app_on(&store);
    let segments = msgs::ui::status::segments(&app);
    assert_eq!(segments[0], "watching chat.db");

    app.on_db_change();
    let segments = msgs::ui::status::segments(&app);
    assert!(
        segments[0].starts_with("watching chat.db · "),
        "the watcher segment must carry the age of the last read"
    );
    assert!(segments[0].ends_with("just now"));
}

#[test]
fn retrying_picks_up_a_database_that_was_not_there_at_launch() {
    // What granting Full Disk Access looks like from inside msgs: the file
    // that could not be read a moment ago can be read now, and `r` is the key
    // that finds out.
    let store = Store::new("retry");
    let later = store.dir.join("appears-later.db");

    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(later.clone());
    assert!(app.db_error.is_some());
    assert!(app.chat_rows.is_empty());
    assert_eq!(app.key_focus(), Focus::DbError);

    std::fs::copy(fixtures::database(), &later).expect("put a database there");

    let action = keymap::resolve(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        app.key_focus(),
    )
    .expect("r is bound on the first-run surface");
    assert_eq!(action, Action::RetryDb);
    app.update(action);

    assert!(app.db_error.is_none(), "the database opened");
    assert!(!app.chat_rows.is_empty(), "and its chats loaded");
    // Keys go back to the panes, and live updates are running.
    assert_eq!(app.key_focus(), Focus::ChatList);
    assert_ne!(app.status.watcher, WatcherStatus::Off);
}
