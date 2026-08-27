//! Render the whole UI against a headless backend.
//!
//! The interactive TUI cannot be driven from a test, so these tests drive the
//! same three pieces the event loop drives — `keymap::resolve`, `App::update`,
//! and `ui::draw` — and read the resulting cell buffer back.
//!
//! No message database is involved: the panes render their empty states, and
//! nothing here touches `chat.db`. The two database tests below use a path that
//! does not exist and a hand-built error value, never a real store.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use msgs::app::{Action, App, Focus};
use msgs::config::Config;
use msgs::db::DbError;
use msgs::{keymap, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

fn app() -> App {
    App::new(Config::default(), Vec::new())
}

/// Draw one frame at `width` × `height` and return the cell buffer.
fn frame(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw succeeded");
    terminal.backend().buffer().clone()
}

/// The buffer as one string per row.
fn rows(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

fn contains(buffer: &Buffer, needle: &str) -> bool {
    rows(buffer).iter().any(|row| row.contains(needle))
}

/// Press a key the way the event loop would.
fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let key = KeyEvent::new(code, modifiers);
    if let Some(action) = keymap::resolve(key, app.key_focus()) {
        app.update(action);
    }
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

#[test]
fn default_screen_has_every_pane_from_the_mockup() {
    let mut app = app();
    let buffer = frame(&mut app, 120, 34);

    assert!(contains(&buffer, "search chats"), "chat list filter line");
    assert!(contains(&buffer, "no conversation"), "conversation header");
    assert!(
        contains(&buffer, "no messages yet"),
        "conversation empty state"
    );
    assert!(contains(&buffer, "message…"), "composer placeholder");
    assert!(contains(&buffer, "chat.db not opened"), "status line");
    assert!(contains(&buffer, "focus list/convo"), "shortcuts bar");
}

#[test]
fn narrow_terminal_drops_the_chat_list_but_keeps_the_conversation() {
    let mut app = app();
    let buffer = frame(&mut app, 80, 30);

    assert!(app.panes.chat_list.is_none());
    assert!(!contains(&buffer, "search chats"));
    assert!(contains(&buffer, "no messages yet"));
    assert!(contains(&buffer, "message…"));
}

#[test]
fn every_terminal_size_down_to_a_sliver_renders_without_panicking() {
    let mut app = app();
    for width in [10u16, 20, 40, 89, 90, 120, 240] {
        for height in [1u16, 2, 3, 5, 8, 10, 12, 24, 60] {
            let buffer = frame(&mut app, width, height);
            assert_eq!(buffer.area.width, width);
            assert_eq!(buffer.area.height, height);
        }
    }
}

#[test]
fn help_modal_opens_over_the_screen_and_closes_again() {
    let mut app = app();
    press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Help);

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "keys"), "modal title");
    assert!(contains(&buffer, "Ctrl+K"), "a binding row");
    assert!(contains(&buffer, "Esc close"), "modal footer");

    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::ChatList);
    let buffer = frame(&mut app, 120, 34);
    assert!(!contains(&buffer, "Esc close"));
}

#[test]
fn palette_opens_from_the_composer_and_returns_focus_there() {
    let mut app = app();
    app.update(Action::FocusPane(Focus::Composer));

    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(app.focus, Focus::Palette);
    type_text(&mut app, "thai");

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "thai"), "typed query");
    assert!(contains(&buffer, "Esc close"), "palette footer");

    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Composer);
    assert!(app.palette.is_empty());
}

#[test]
fn typing_in_the_composer_shows_the_draft_and_places_the_cursor() {
    let mut app = app();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Composer);

    type_text(&mut app, "on my way");
    press(&mut app, KeyCode::Enter, KeyModifiers::ALT);
    type_text(&mut app, "in 20");

    let mut terminal = Terminal::new(TestBackend::new(120, 34)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw succeeded");
    let buffer = terminal.backend().buffer().clone();

    assert!(contains(&buffer, "on my way"));
    assert!(contains(&buffer, "in 20"));
    // The cursor sits after the second line's text, inside the composer.
    let cursor = terminal.backend().cursor_position();
    let composer = app.panes.composer;
    assert!(
        cursor.x > composer.x && cursor.x < composer.x + composer.width,
        "cursor {cursor:?} outside composer {composer:?}"
    );
    assert!(cursor.y > composer.y && cursor.y < composer.y + composer.height);
}

#[test]
fn slash_opens_the_chat_filter_and_letters_go_into_it() {
    let mut app = app();
    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    type_text(&mut app, "jq");

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "/ jq"), "filter query echoed");
    assert!(contains(&buffer, "no chats match"), "filtered empty state");

    // `q` typed into the filter must not have quit the app.
    assert!(!app.should_quit);
}

#[test]
fn ctrl_b_toggles_the_chat_list() {
    let mut app = app();
    assert!(contains(&frame(&mut app, 120, 34), "search chats"));

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    let buffer = frame(&mut app, 120, 34);
    assert!(!contains(&buffer, "search chats"));
    assert_eq!(app.focus, Focus::Conversation);

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert!(contains(&frame(&mut app, 120, 34), "search chats"));
}

#[test]
fn q_quits_from_the_panes() {
    let mut app = app();
    press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(app.should_quit);
}

#[test]
fn theme_overrides_from_config_reach_the_rendered_cells() {
    let (config, warnings) = Config::parse("[theme]\nborder_active = \"#ff0000\"\n");
    let mut app = App::new(config, warnings);
    app.update(Action::FocusPane(Focus::Composer));

    let buffer = frame(&mut app, 120, 34);
    let composer = app.panes.composer;
    let corner = &buffer[(composer.x, composer.y)];
    assert_eq!(
        corner.fg,
        ratatui::style::Color::Rgb(0xff, 0x00, 0x00),
        "the focused composer border should use the overridden color"
    );
}

#[test]
fn an_unreadable_database_replaces_the_panes_with_an_explanation() {
    let mut app = app();
    app.open_db(PathBuf::from("/nonexistent/msgs/chat.db"));

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "no message database here"), "headline");
    assert!(contains(&buffer, "--db <PATH>"), "what to do about it");
    assert!(contains(&buffer, "q quit"), "the way out");
    assert!(!contains(&buffer, "search chats"), "panes are gone");
    assert_eq!(app.panes.chat_list, None);

    // Help still opens over the explanation.
    press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(contains(&frame(&mut app, 120, 34), "Esc close"));
}

#[test]
fn a_blocked_database_names_full_disk_access_and_the_settings_path() {
    let mut app = app();
    app.db_error = Some(DbError::PermissionDenied(PathBuf::from(
        "/Users/someone/Library/Messages/chat.db",
    )));

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "cannot read your messages"), "headline");
    assert!(contains(&buffer, "Full Disk Access"), "what is missing");
    assert!(
        contains(&buffer, "Privacy & Security"),
        "where to turn it on"
    );
}

#[test]
fn the_error_screen_survives_every_terminal_size() {
    let mut app = app();
    app.open_db(PathBuf::from("/nonexistent/msgs/chat.db"));
    for width in [10u16, 40, 120, 240] {
        for height in [1u16, 3, 8, 34] {
            let buffer = frame(&mut app, width, height);
            assert_eq!(buffer.area.width, width);
        }
    }
}
