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

use chrono::{Duration, Local};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use msgs::app::{Action, App, Focus};
use msgs::config::Config;
use msgs::db::{Chat, DbError, Preview};
use msgs::{keymap, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;

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

/// Click the left button at an absolute terminal cell.
fn click(app: &mut App, column: u16, row: u16) {
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    });
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

/// A chat with a name, an age, and a one-line preview. Every name and body
/// here is invented; no test in this file opens a real database.
fn chat(rowid: i64, name: &str, minutes_ago: i64, preview: &str) -> Chat {
    let when = Local::now() - Duration::minutes(minutes_ago);
    // Messages stores nanoseconds since 2001-01-01.
    let raw = (when.timestamp() - 978_307_200) * 1_000_000_000;
    Chat {
        rowid,
        guid: format!("iMessage;-;chat{rowid}"),
        identifier: Some(format!("chat{rowid}")),
        display_name: Some(name.to_string()),
        service: Some("iMessage".to_string()),
        style: 45,
        is_group: false,
        participants: Vec::new(),
        last_message_date: raw,
        last_message_rowid: rowid,
        preview: Some(Preview {
            message_rowid: rowid,
            text: Some(preview.to_string()),
            ..Preview::default()
        }),
        message_count: 12,
        unread_count: 0,
        is_pinned: None,
    }
}

fn with_chats(chats: Vec<Chat>) -> App {
    let mut app = app();
    app.chat_rows = chats;
    app.refresh_chat_view();
    app
}

/// The cell at `(x, y)` of the last drawn frame.
fn cell(buffer: &Buffer, x: u16, y: u16) -> ratatui::buffer::Cell {
    buffer[(x, y)].clone()
}

#[test]
fn chat_rows_carry_a_name_a_preview_a_time_and_an_unread_badge() {
    let mut first = chat(1, "Alpha Person", 2, "sounds good, see you at 7");
    first.unread_count = 2;
    let mut app = with_chats(vec![
        first,
        chat(2, "Bravo Group", 61, "the second one"),
        chat(3, "Charlie", 60 * 24 * 9, "an old one"),
    ]);

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "Alpha Person"), "name");
    assert!(contains(&buffer, "sounds good"), "preview");
    assert!(contains(&buffer, "2m"), "relative time");
    assert!(contains(&buffer, " 2 "), "unread badge");
    assert!(contains(&buffer, "1h"), "an hour-old chat");
    assert!(contains(&buffer, "Charlie"), "a nine-day-old chat");
    // No pinning in this database, so no section headings.
    assert!(!contains(&buffer, "PINNED"));
}

#[test]
fn the_selected_row_is_outlined_and_the_hovered_one_is_tinted() {
    let mut app = with_chats(vec![
        chat(1, "Alpha", 2, "one"),
        chat(2, "Bravo", 5, "two"),
        chat(3, "Charlie", 9, "three"),
    ]);
    let rows = {
        let buffer = frame(&mut app, 120, 34);
        let _ = buffer;
        app.panes.chat_list_rows.expect("the rows area")
    };

    // Hover over the third chat, which starts four rows into the list.
    app.hover = Some(Position::new(rows.x + 2, rows.y + 4));
    let buffer = frame(&mut app, 120, 34);

    let selected = cell(&buffer, rows.x, rows.y);
    assert_eq!(selected.fg, app.theme.border_active, "selection bar");
    assert_eq!(selected.bg, app.theme.bg_highlight, "selection background");

    let hovered = cell(&buffer, rows.x + 2, rows.y + 4);
    assert_eq!(hovered.bg, app.theme.bg_hover, "hover tint");

    let plain = cell(&buffer, rows.x + 2, rows.y + 2);
    assert_eq!(plain.bg, app.theme.bg_dark, "an untouched row");
}

#[test]
fn arrows_move_the_chat_selection_and_a_click_lands_on_either_of_its_rows() {
    let mut app = with_chats(vec![
        chat(1, "Alpha", 2, "one"),
        chat(2, "Bravo", 5, "two"),
        chat(3, "Charlie", 9, "three"),
    ]);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "Alpha"));
    assert_eq!(app.chats.selected, 0);

    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.chats.selected, 1);
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(app.chats.selected, 2);
    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(app.chats.selected, 0);

    let rows = app.panes.chat_list_rows.expect("the rows area");
    // The preview line of the second chat.
    click(&mut app, rows.x + 3, rows.y + 3);
    assert_eq!(app.focus, Focus::ChatList);
    assert_eq!(app.chats.selected, 1);
    assert_eq!(app.selected_chat().map(|chat| chat.rowid), Some(2));
}

#[test]
fn the_wheel_scrolls_the_chat_list_without_moving_the_selection() {
    let chats = (0..40)
        .map(|index| chat(index + 1, &format!("Chat {index}"), index, "body"))
        .collect();
    let mut app = with_chats(chats);
    let _ = frame(&mut app, 120, 34);

    let rows = app.panes.chat_list_rows.expect("the rows area");
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: rows.x + 2,
        row: rows.y + 2,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(app.chats.selected, 0, "the wheel does not select");
    assert!(app.chats.offset > 0, "but it does scroll");

    // Moving the selection pulls the window back to it, and no further than
    // it has to: the selection lands on the top row.
    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.chats.selected, 1);
    assert_eq!(app.chats.offset, 1);
}

#[test]
fn slash_filters_the_list_by_name_without_case() {
    let mut app = with_chats(vec![
        chat(1, "Alpha", 2, "one"),
        chat(2, "Bravo", 5, "two"),
        chat(3, "Bravado", 9, "three"),
    ]);

    press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    type_text(&mut app, "BRAV");

    let buffer = frame(&mut app, 120, 34);
    assert!(!contains(&buffer, "Alpha"), "filtered out");
    assert!(contains(&buffer, "Bravo"));
    assert!(contains(&buffer, "Bravado"));
    assert_eq!(app.visible_chats.len(), 2);
    assert_eq!(app.selected_chat().map(|chat| chat.rowid), Some(2));

    // Narrowing further keeps the selection on the chat it was already on.
    type_text(&mut app, "ado");
    assert_eq!(app.visible_chats.len(), 1);
    assert_eq!(app.selected_chat().map(|chat| chat.rowid), Some(3));

    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.visible_chats.len(), 3);
    assert!(contains(&frame(&mut app, 120, 34), "Alpha"));
}

#[test]
fn a_pinned_chat_opens_a_section_and_the_rest_follow_under_another() {
    let mut pinned = chat(1, "Pinned One", 30, "kept on top");
    pinned.is_pinned = Some(true);
    let mut newer = chat(2, "Newer", 1, "more recent, but not pinned");
    newer.is_pinned = Some(false);
    let mut app = with_chats(vec![pinned, newer]);
    // `Db::chats` does this ordering; the fixture list is built by hand.
    app.chat_rows.sort_by_key(|chat| !chat.is_pinned());
    app.refresh_chat_view();

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "PINNED"));
    assert!(contains(&buffer, "RECENT"));
    assert_eq!(app.pinned_visible, 1);

    // The heading rows are not selectable.
    let rows = app.panes.chat_list_rows.expect("the rows area");
    click(&mut app, rows.x + 3, rows.y);
    assert_eq!(app.chats.selected, 0);
    click(&mut app, rows.x + 3, rows.y + 4);
    assert_eq!(app.chats.selected, 1);
}

#[test]
fn five_hundred_chats_draw_only_what_fits_and_do_it_quickly() {
    let chats = (0..500)
        .map(|index| {
            chat(
                index + 1,
                &format!("Chat number {index}"),
                index,
                "a preview line of about the length a real one has",
            )
        })
        .collect();
    let mut app = with_chats(chats);
    assert_eq!(app.visible_chats.len(), 500);

    let started = std::time::Instant::now();
    for _ in 0..20 {
        let buffer = frame(&mut app, 120, 34);
        // Only the chats that fit are drawn: 32 rows of list, two rows each.
        assert!(!contains(&buffer, "Chat number 20"));
    }
    press(&mut app, KeyCode::End, KeyModifiers::NONE);
    let buffer = frame(&mut app, 120, 34);
    assert!(
        contains(&buffer, "Chat number 499"),
        "the bottom is reachable"
    );
    assert!(!contains(&buffer, "Chat number 0"));

    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "twenty frames of 500 chats took {elapsed:?}"
    );
}
