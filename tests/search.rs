//! The jump palette and the FTS5 index behind it.
//!
//! Every database here is a synthetic fixture built by `tests/fixtures`, and
//! every index is written to a private temporary directory that the test
//! deletes when it ends. Nothing points at `~/Library/Messages/chat.db`, and
//! nothing writes an index into the user's Application Support directory.
//!
//! Message bodies stay inside assertions on structure — counts, rowids, and
//! the words a test itself invented — rather than being printed.

mod fixtures;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use msgs::app::{Action, App, Focus};
use msgs::config::Config;
use msgs::jump::{Filter, Kind};
use msgs::search::{self, Search};
use msgs::{keymap, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rusqlite::Connection;

/// A private directory for one test's index, deleted when the test ends.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "msgs-search-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Self { dir }
    }

    fn index(&self) -> PathBuf {
        self.dir.join("index.db")
    }

    /// A writable copy of the small fixture, for the tests that add a message.
    fn copy_of_fixture(&self) -> PathBuf {
        let to = self.dir.join("chat.db");
        std::fs::copy(fixtures::database(), &to).expect("copy the fixture");
        to
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn app_on(db: &Path, index: &Path) -> App {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(db.to_path_buf());
    app.enable_search(index);
    ready(&mut app);
    app
}

/// Pump `tick` until the index is built, or fail after a generous timeout.
fn ready(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        app.tick();
        if app.search_state().is_ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the index never finished building");
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if let Some(action) = keymap::resolve(KeyEvent::new(code, modifiers), app.key_focus()) {
        app.update(action);
    }
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// Draw one frame, so the panes have the geometry the palette measures against.
fn draw(app: &mut App) {
    let mut terminal = Terminal::new(TestBackend::new(120, 34)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw succeeded");
}

#[test]
fn the_index_is_built_beside_the_app_and_never_inside_chat_db() {
    let scratch = Scratch::new("beside");
    let db = fixtures::database();
    search::build(&db, &scratch.index()).expect("build the index");

    assert!(scratch.index().exists(), "the index file was created");
    // The database msgs read is untouched: same bytes, no sidecars left over.
    let untouched = Connection::open(&db).expect("reopen the fixture");
    let messages: i64 = untouched
        .query_row("SELECT COUNT(*) FROM message", [], |row| row.get(0))
        .expect("count");
    assert_eq!(messages, 11);
    assert!(
        !db.with_extension("db-wal").exists(),
        "the fixture gained no WAL"
    );

    // The default location is msgs's own directory, not the message store.
    let default = search::default_index_path().expect("a data directory");
    assert!(default.ends_with("msgs/index.db"), "{}", default.display());
    assert!(!default.to_string_lossy().contains("Library/Messages"));
}

#[test]
fn a_query_finds_the_message_it_matches_and_nothing_else() {
    let scratch = Scratch::new("query");
    let index_path = scratch.index();
    search::build(&fixtures::database(), &index_path).expect("build the index");
    let index = search::open_reader(&index_path).expect("open the index");

    let expression = search::match_expression(&search::tokens("fixture")).expect("terms");
    let hits = search::search(&index, &expression, None, 20).expect("query");
    assert_eq!(hits.len(), 1, "one message says it");
    assert_eq!(hits[0].message_rowid, fixtures::MSG_PLAIN);
    assert_eq!(hits[0].chat_rowid, fixtures::CHAT_DIRECT);
    assert!(!hits[0].is_from_me);

    // A body that only exists inside `attributedBody` is indexed too.
    let expression = search::match_expression(&search::tokens("typedstream")).expect("terms");
    let hits = search::search(&index, &expression, None, 20).expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_rowid, fixtures::MSG_ATTRIBUTED);

    // Half a word matches, which is what makes typing feel live.
    let expression = search::match_expression(&search::tokens("grou")).expect("terms");
    let hits = search::search(&index, &expression, None, 20).expect("query");
    assert_eq!(hits.len(), 2, "both group messages");
    // Newest first.
    assert!(hits[0].date >= hits[1].date);

    // Nothing invented matches nothing.
    let expression = search::match_expression(&search::tokens("nobodysaidthis")).expect("terms");
    assert!(
        search::search(&index, &expression, None, 20)
            .expect("query")
            .is_empty()
    );
}

#[test]
fn the_photo_filter_matches_file_names_only() {
    let scratch = Scratch::new("photos");
    let index_path = scratch.index();
    search::build(&fixtures::database(), &index_path).expect("build the index");
    let index = search::open_reader(&index_path).expect("open the index");

    let expression = search::match_expression(&search::tokens("photo")).expect("terms");
    let hits = search::search(&index, &expression, Some(search::Kind::Photo), 20).expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_rowid, fixtures::MSG_PHOTO);
    assert_eq!(hits[0].kind, search::Kind::Photo);

    // The same word under the message filter finds nothing: no body says it.
    let hits = search::search(&index, &expression, Some(search::Kind::Message), 20).expect("query");
    assert!(hits.is_empty());
}

#[test]
fn a_second_pass_indexes_only_what_arrived_since_the_first() {
    let scratch = Scratch::new("incremental");
    let db = scratch.copy_of_fixture();
    let index_path = scratch.index();
    search::build(&db, &index_path).expect("build the index");

    let rows_after_first = index_rows(&index_path);

    // Nothing changed: a second pass adds nothing at all.
    search::build(&db, &index_path).expect("second pass");
    assert_eq!(index_rows(&index_path), rows_after_first);

    // A message arrives, and only that message is read.
    let store = Connection::open(&db).expect("open the copy");
    store
        .execute(
            "INSERT INTO message (ROWID, guid, text, handle_id, service, is_from_me, is_read,
                                  date, associated_message_type, item_type)
             VALUES (900, 'FIXTURE0-0000-4000-8000-000000000900', 'a later arrival',
                     1, 'iMessage', 0, 0, ?1, 0, 0)",
            [fixtures::BASE + 900 * fixtures::SECOND],
        )
        .expect("insert");
    store
        .execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, 900, ?2)",
            (fixtures::CHAT_DIRECT, fixtures::BASE + 900 * fixtures::SECOND),
        )
        .expect("join");
    drop(store);

    search::build(&db, &index_path).expect("catch up");
    assert_eq!(index_rows(&index_path), rows_after_first + 1);

    let index = search::open_reader(&index_path).expect("open the index");
    let expression = search::match_expression(&search::tokens("arrival")).expect("terms");
    let hits = search::search(&index, &expression, None, 20).expect("query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_rowid, 900);
}

fn index_rows(index_path: &Path) -> i64 {
    let index = search::open_reader(index_path).expect("open the index");
    index
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .expect("count")
}

#[test]
fn a_hundred_thousand_messages_index_off_the_ui_thread_and_answer_quickly() {
    let scratch = Scratch::new("large");
    let db = fixtures::large_database();
    let index_path = scratch.index();

    // The build runs on its own thread; the loop below is the UI thread, and
    // it stays responsive throughout — every poll is a channel drain.
    let mut search = Search::start(&db, &index_path);
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut slowest = Duration::ZERO;
    let mut saw_progress = false;
    while Instant::now() < deadline {
        let before = Instant::now();
        search.poll();
        slowest = slowest.max(before.elapsed());
        if let search::State::Building { done, .. } = search.state() {
            saw_progress |= *done > 0;
        }
        if search.is_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(search.is_ready(), "the index never finished: {:?}", {
        let state = search.state();
        state.clone()
    });
    assert!(saw_progress, "the build never reported progress");
    assert!(
        slowest < Duration::from_millis(20),
        "asking the indexer how it is going took {slowest:?}"
    );

    let count = index_rows(&index_path);
    assert_eq!(count, i64::try_from(fixtures::LARGE_MESSAGES).unwrap());

    // A rare word, a common word, and a prefix all come back well under 50ms.
    for needle in [fixtures::LARGE_NEEDLE, "coffee", "morn"] {
        let started = Instant::now();
        let hits = search.query(needle, None, search::QUERY_LIMIT);
        let elapsed = started.elapsed();
        assert!(!hits.is_empty(), "{needle} matched nothing");
        assert!(
            elapsed < Duration::from_millis(50),
            "{needle} took {elapsed:?}"
        );
    }
    let hits = search.query(fixtures::LARGE_NEEDLE, None, search::QUERY_LIMIT);
    assert_eq!(hits.len(), 1, "the rare word is on exactly one message");
}

#[test]
fn the_palette_lists_chats_before_messages_and_enter_opens_the_chat() {
    let scratch = Scratch::new("palette-chats");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(app.focus, Focus::Palette);
    // An empty query is the list you already have.
    assert!(app.jump.chats > 0, "chats with no query typed");

    type_text(&mut app, "Fixture Group");
    assert!(app.jump.chats >= 1, "the group matched by name");
    let first = app.jump.rows.first().expect("a row").clone();
    assert_eq!(first.kind, Kind::Chat);
    assert_eq!(first.chat_rowid, fixtures::CHAT_GROUP);
    assert!(!first.label_hits.is_empty(), "the name is highlighted");

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Conversation);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_GROUP));
    assert!(app.palette.is_empty(), "the query is cleared on close");
}

#[test]
fn a_message_hit_opens_its_conversation_with_the_message_selected() {
    let scratch = Scratch::new("palette-jump");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    draw(&mut app);
    // Start somewhere else so the jump has to move.
    app.open_chat_row(fixtures::CHAT_GROUP);
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, "typedstream");

    let row = app
        .jump
        .rows
        .iter()
        .find(|row| row.kind == Kind::Message)
        .expect("a message row")
        .clone();
    assert_eq!(row.chat_rowid, fixtures::CHAT_DIRECT);
    assert_eq!(row.message_rowid, Some(fixtures::MSG_ATTRIBUTED));
    assert!(!row.body_hits.is_empty(), "the matched word is highlighted");
    assert!(!row.meta.is_empty(), "a message row carries its date");

    let index = app
        .jump
        .rows
        .iter()
        .position(|candidate| candidate == &row)
        .expect("the row is in the list");
    app.jump.list.selected = index;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(app.focus, Focus::Conversation);
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    assert_eq!(
        app.selected_message().map(|message| message.rowid),
        Some(fixtures::MSG_ATTRIBUTED)
    );
}

#[test]
fn a_jump_reaches_a_message_far_above_the_newest_page() {
    let scratch = Scratch::new("palette-deep");
    let mut app = app_on(&fixtures::large_database(), &scratch.index());
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, fixtures::LARGE_NEEDLE);
    let row = app
        .jump
        .rows
        .iter()
        .find(|row| row.kind == Kind::Message)
        .expect("the rare word matched")
        .clone();
    let target = row.message_rowid.expect("a message rowid");
    assert_eq!(target, i64::try_from(fixtures::LARGE_MESSAGES / 2).unwrap());

    let index = app
        .jump
        .rows
        .iter()
        .position(|candidate| candidate == &row)
        .expect("the row is in the list");
    app.jump.list.selected = index;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    // Fifty thousand messages back is further than the palette will page, and
    // it says so rather than reading the whole thread.
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    let reached = app
        .selected_message()
        .is_some_and(|message| message.rowid == target);
    if !reached {
        assert!(
            app.status
                .active_toast()
                .is_some_and(|(text, _)| text.contains("further back")),
            "an unreachable message is explained"
        );
    }

    // A message inside the pages it will load is reached and selected.
    let near = i64::try_from(fixtures::LARGE_MESSAGES).unwrap() - 5;
    app.open_message(fixtures::CHAT_DIRECT, near);
    draw(&mut app);
    assert_eq!(
        app.selected_message().map(|message| message.rowid),
        Some(near)
    );
}

#[test]
fn tab_cycles_the_filter_and_narrows_the_results() {
    let scratch = Scratch::new("palette-filter");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, "group");
    assert_eq!(app.jump.filter, Filter::All);
    assert!(app.jump.chats > 0 && app.jump.messages > 0, "both kinds");

    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.jump.filter, Filter::Chats);
    assert_eq!(app.jump.messages, 0, "chats only");

    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.jump.filter, Filter::Messages);
    assert_eq!(app.jump.chats, 0, "messages only");
    assert!(app.jump.messages > 0);

    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.jump.filter, Filter::Photos);
    assert_eq!(app.jump.messages, 0, "no picture is called group");

    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.jump.filter, Filter::All);
}

#[test]
fn a_short_query_stays_out_of_the_index() {
    let scratch = Scratch::new("palette-short");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, "gr");
    assert_eq!(app.jump.messages, 0, "two letters is not a message search");

    type_text(&mut app, "o");
    assert!(app.jump.messages > 0, "three letters reaches the index");
}

#[test]
fn ctrl_n_on_an_address_opens_a_conversation_that_does_not_exist_yet() {
    let scratch = Scratch::new("palette-new");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    app.outbox = msgs::send::Outbox::inert();
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, "+15550009999");
    press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);

    assert_eq!(app.focus, Focus::Composer);
    assert!(app.draft_target.is_some(), "a target to send to");
    assert!(app.open_chat.is_none(), "no conversation is open");

    // The composer sends to the typed address rather than refusing.
    type_text(&mut app, "hello");
    app.update(Action::Activate);
    let sent = app.outbox.recorded();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.identifier.as_deref(), Some("+15550009999"));
    assert!(sent[0].1.guid.is_none(), "no chat guid exists for it");
    assert_eq!(app.pending.len(), 1, "the echo is on screen");
    draw(&mut app);

    // Moving in the chat list leaves the draft behind.
    app.update(Action::FocusPane(Focus::ChatList));
    app.update(Action::SelectNext);
    assert!(app.draft_target.is_none());
}

#[test]
fn ctrl_n_on_an_address_you_already_have_opens_that_thread() {
    let scratch = Scratch::new("palette-existing");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    app.open_chat_row(fixtures::CHAT_GROUP);
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, "+1 (555) 000-0001");
    press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);

    assert!(app.draft_target.is_none(), "no draft: the thread exists");
    assert_eq!(app.open_chat, Some(fixtures::CHAT_DIRECT));
    assert_eq!(app.focus, Focus::Composer);
}

#[test]
fn ctrl_n_on_something_that_is_not_an_address_says_so() {
    let scratch = Scratch::new("palette-not-address");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    draw(&mut app);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    type_text(&mut app, "group");
    press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);

    assert_eq!(app.focus, Focus::Palette, "the palette stays open");
    assert!(app.draft_target.is_none());
    assert!(
        app.status
            .active_toast()
            .is_some_and(|(text, _)| text.contains("phone number or email"))
    );
}

#[test]
fn escape_closes_the_palette_and_puts_focus_back_where_it_was() {
    let scratch = Scratch::new("palette-escape");
    let mut app = app_on(&fixtures::database(), &scratch.index());
    draw(&mut app);

    for origin in [Focus::ChatList, Focus::Conversation, Focus::Composer] {
        app.update(Action::FocusPane(origin));
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        type_text(&mut app, "fix");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, origin, "from {origin:?}");
        assert!(app.palette.is_empty());
        assert!(app.jump.rows.is_empty());
    }
}

#[test]
fn a_new_message_reaches_the_index_through_the_live_update_stream() {
    let scratch = Scratch::new("live-index");
    let db = scratch.copy_of_fixture();
    let mut app = app_on(&db, &scratch.index());
    draw(&mut app);

    let store = Connection::open(&db).expect("open the copy");
    store
        .execute(
            "INSERT INTO message (ROWID, guid, text, handle_id, service, is_from_me, is_read,
                                  date, associated_message_type, item_type)
             VALUES (901, 'FIXTURE0-0000-4000-8000-000000000901', 'aardvark arrived',
                     1, 'iMessage', 0, 0, ?1, 0, 0)",
            [fixtures::BASE + 901 * fixtures::SECOND],
        )
        .expect("insert");
    store
        .execute(
            "INSERT INTO chat_message_join (chat_id, message_id, message_date) VALUES (?1, 901, ?2)",
            (fixtures::CHAT_DIRECT, fixtures::BASE + 901 * fixtures::SECOND),
        )
        .expect("join");
    drop(store);

    // The watcher is not driven from a test; `on_db_change` is what it calls.
    app.on_db_change();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut found = false;
    while Instant::now() < deadline && !found {
        app.tick();
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        app.palette = msgs::app::TextField::from_text("aardvark");
        app.update(Action::CursorEnd);
        found = app.jump.rows.iter().any(|row| row.kind == Kind::Message);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        if !found {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(found, "the message never reached the index");
}
