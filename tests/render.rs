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
use ratatui::style::Modifier;

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

    assert!(contains(&buffer, "/ filter"), "chat list filter line");
    assert!(contains(&buffer, "no conversation"), "conversation header");
    assert!(
        contains(&buffer, "no messages yet"),
        "conversation empty state"
    );
    assert!(contains(&buffer, "❯ Message"), "composer placeholder");
    assert!(contains(&buffer, "? help"), "the footer");
    assert!(
        !contains(&buffer, "chat.db"),
        "the footer says nothing about the database"
    );
}

#[test]
fn narrow_terminal_opens_on_the_list_screen_and_ctrl_b_swaps_to_the_conversation() {
    let mut app = app();
    let buffer = frame(&mut app, 80, 30);

    // The list has focus at startup, so it is the whole screen.
    let list = app.panes.chat_list.expect("the list screen");
    assert_eq!(list.width, 80);
    assert!(contains(&buffer, "/ filter"));
    assert!(!contains(&buffer, "no messages yet"));
    assert!(!contains(&buffer, "❯ Message"));

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    let buffer = frame(&mut app, 80, 30);
    assert!(app.panes.chat_list.is_none());
    assert!(!contains(&buffer, "/ filter"));
    assert!(contains(&buffer, "no messages yet"));
    assert!(contains(&buffer, "❯ Message"));
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
    assert!(contains(&buffer, "Enter jump"), "palette footer");
    assert!(contains(&buffer, "Tab filter: all"), "filter in the footer");

    // Tab in the palette cycles the filter rather than moving focus.
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Palette);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "Tab filter: chats"), "filter cycled");

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
    assert!(contains(&frame(&mut app, 120, 34), "/ filter"));

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    let buffer = frame(&mut app, 120, 34);
    assert!(!contains(&buffer, "/ filter"));
    assert_eq!(app.focus, Focus::Conversation);

    press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert!(contains(&frame(&mut app, 120, 34), "/ filter"));
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
    // The box sits a column in from the pane's edge.
    let corner = &buffer[(composer.x + 1, composer.y)];
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
fn the_first_run_surface_offers_a_retry_and_takes_no_other_key() {
    let mut app = app();
    app.open_db(PathBuf::from("/nonexistent/msgs/chat.db"));

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "r retry"), "the retry key is offered");
    assert!(contains(&buffer, "press r"), "and what it is for");

    // Keys that steer panes are dead here: there are no panes.
    assert_eq!(app.key_focus(), Focus::DbError);
    assert_eq!(
        keymap::resolve(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            app.key_focus()
        ),
        None
    );
    assert_eq!(
        keymap::resolve(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            app.key_focus()
        ),
        Some(Action::RetryDb)
    );

    // Retrying a database that is still not there says so and stays put.
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    assert!(app.db_error.is_some());
    let (text, is_error) = app.status.active_toast().expect("a toast");
    assert!(text.contains("still cannot read"), "{text}");
    assert!(is_error);
    // There is no status line under this surface, so the toast lands on it.
    assert!(contains(&frame(&mut app, 120, 34), "still cannot read"));
}

#[test]
fn a_blocked_database_spells_out_the_full_disk_access_steps() {
    let mut app = app();
    app.db_error = Some(DbError::PermissionDenied(PathBuf::from(
        "/Users/someone/Library/Messages/chat.db",
    )));

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "1. Open System Settings"), "step one");
    assert!(contains(&buffer, "2. Switch on the app"), "step two");
    assert!(contains(&buffer, "3. Quit that app"), "step three");
    assert!(
        contains(&buffer, "Contacts"),
        "the same switch covers names"
    );
}

#[test]
fn startup_warnings_are_listed_in_the_help_modal() {
    let (config, warnings) = Config::parse("shwo_chat_list = true\n");
    let mut app = App::new(config, warnings);
    assert!(!app.status.warnings.is_empty());

    app.update(Action::OpenHelp);
    let buffer = frame(&mut app, 140, 44);
    assert!(contains(&buffer, "NOTES"), "the notes heading");
    assert!(contains(&buffer, "config: "), "and the warning itself");
}

/// The scope headings the help modal draws, in the order the table lists them.
fn help_headings() -> Vec<String> {
    let mut headings: Vec<String> = Vec::new();
    for binding in keymap::BINDINGS {
        let heading = binding.scope.to_uppercase();
        if !headings.contains(&heading) {
            headings.push(heading);
        }
    }
    headings
}

/// How many scope headings share `row`.
///
/// Longest first, blanking each one as it is found, so `CHAT LIST` is not also
/// counted as `LIST`.
fn headings_on(row: &str, headings: &[String]) -> usize {
    let mut ordered: Vec<&String> = headings.iter().collect();
    ordered.sort_by_key(|heading| std::cmp::Reverse(heading.len()));
    let mut rest = row.to_string();
    let mut found = 0;
    for heading in ordered {
        if let Some(at) = rest.find(heading.as_str()) {
            rest.replace_range(at..at + heading.len(), &" ".repeat(heading.len()));
            found += 1;
        }
    }
    found
}

#[test]
fn a_wide_terminal_puts_the_help_modal_into_two_columns() {
    let mut app = app();
    app.update(Action::OpenHelp);
    let headings = help_headings();

    // Two scopes on one row is what a second column means.
    let wide = rows(&frame(&mut app, 200, 44));
    assert!(
        wide.iter().any(|row| headings_on(row, &headings) >= 2),
        "two headings share a row"
    );

    // Narrow, they stack, and every binding is still reachable by scrolling.
    let narrow = rows(&frame(&mut app, 90, 44));
    assert!(narrow.iter().all(|row| headings_on(row, &headings) <= 1));
}

#[test]
fn the_help_modal_names_each_scope_once() {
    let mut app = app();
    app.update(Action::OpenHelp);
    let headings = help_headings();
    let lines = rows(&frame(&mut app, 90, 60));
    let total: usize = lines.iter().map(|row| headings_on(row, &headings)).sum();
    assert_eq!(
        total,
        headings.len(),
        "one heading per scope, not {total} for {} scopes",
        headings.len()
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
        group_id: None,
        original_group_id: None,
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
        unread: 0,
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
fn chat_rows_carry_a_name_a_preview_and_a_time_and_unread_is_bold() {
    let mut first = chat(1, "Alpha Person", 2, "sounds good, see you at 7");
    first.unread_count = 2;
    first.unread = 2;
    let mut app = with_chats(vec![
        first,
        chat(2, "Bravo Group", 61, "the second one"),
        chat(3, "Charlie", 60 * 24 * 9, "an old one"),
    ]);

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "Alpha Person"), "name");
    assert!(contains(&buffer, "sounds good"), "preview");
    assert!(contains(&buffer, "2m"), "relative time");
    assert!(contains(&buffer, "1h"), "an hour-old chat");
    let rows = app.panes.chat_list_rows.expect("the rows area");
    let name = cell(&buffer, rows.x + 2, rows.y);
    assert!(name.modifier.contains(Modifier::BOLD), "unread is bold");
    let unread_time: Vec<_> = (0..rows.width)
        .map(|x| cell(&buffer, rows.x + x, rows.y).fg)
        .collect();
    assert!(
        unread_time.contains(&app.theme.accent_me),
        "the time of an unread chat takes the accent"
    );
    let read_time: Vec<_> = (0..rows.width)
        .map(|x| cell(&buffer, rows.x + x, rows.y + 3).fg)
        .collect();
    assert!(
        !read_time.contains(&app.theme.accent_me),
        "a read chat has no accent"
    );
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

    // Hover over the third chat, which starts six rows into the list.
    app.hover = Some(Position::new(rows.x + 2, rows.y + 6));
    let buffer = frame(&mut app, 120, 34);

    let selected = cell(&buffer, rows.x, rows.y);
    assert_eq!(selected.bg, app.theme.bg_highlight, "selection background");
    assert_eq!(selected.symbol(), " ", "no edge, no marker");

    let hovered = cell(&buffer, rows.x + 2, rows.y + 6);
    assert_eq!(hovered.bg, app.theme.bg_hover, "hover tint");

    let plain = cell(&buffer, rows.x + 2, rows.y + 3);
    assert_eq!(plain.bg, app.theme.bg_dark, "an untouched row");
    let air = cell(&buffer, rows.x + 2, rows.y + 2);
    assert_eq!(
        air.bg, app.theme.bg_dark,
        "the blank row under the selection is not tinted"
    );
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
    click(&mut app, rows.x + 3, rows.y + 4);
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
    click(&mut app, rows.x + 3, rows.y + 5);
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

// ---------------------------------------------------------------------------
// The conversation pane. Every body, name, and number below is invented; no
// test in this file opens a real database.
// ---------------------------------------------------------------------------

use msgs::db::{AttachmentRef, Handle, Message, Tapback, TapbackAction, TapbackKind};

/// Raw Messages timestamp `minutes` before now.
/// The furthest back any test here reaches, in minutes.
const SPAN: i64 = 60;

/// `minutes` before an anchor that keeps the whole invented span inside today.
///
/// The day tests assert that the band says `Today`, so a suite that happens to
/// run in the first [`SPAN`] minutes after midnight would otherwise date its
/// oldest messages to yesterday and fail on the clock rather than on the code.
/// Every message shifts by the same amount, so the gaps a run depends on are
/// exactly the gaps the caller asked for.
fn ago(minutes: i64) -> i64 {
    let now = Local::now();
    let earliest = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(Local).single())
        .map_or(now, |midnight| midnight + Duration::minutes(SPAN));
    let when = now.max(earliest) - Duration::minutes(minutes);
    (when.timestamp() - 978_307_200) * 1_000_000_000
}

fn message(rowid: i64, from_me: bool, handle_rowid: i64, text: &str, minutes: i64) -> Message {
    Message {
        rowid,
        guid: format!("G{rowid}"),
        chat_rowid: 1,
        handle_rowid: (!from_me).then_some(handle_rowid),
        handle: (!from_me).then(|| format!("someone{handle_rowid}@example.invalid")),
        service: Some("iMessage".to_string()),
        is_from_me: from_me,
        is_read: true,
        date: ago(minutes),
        date_delivered: 0,
        date_read: 0,
        date_edited: 0,
        is_edited: false,
        error: 0,
        text: (!text.is_empty()).then(|| text.to_string()),
        subject: None,
        attachments: Vec::new(),
        reply_to_guid: None,
        thread_originator_guid: None,
        tapbacks: Vec::new(),
        item_type: 0,
        group_action_type: 0,
        group_title: None,
        other_handle: None,
        group_action: None,
        link_preview: None,
    }
}

fn handle(rowid: i64) -> Handle {
    Handle::new(
        rowid,
        format!("someone{rowid}@example.invalid"),
        "iMessage".to_string(),
    )
}

/// An app with one open conversation, focused on it and scrolled to the newest
/// message, the way opening a chat leaves it.
fn with_conversation(group: bool, participants: usize, messages: Vec<Message>) -> App {
    let mut room = chat(1, "Fixture Chat", 2, "last line");
    room.is_group = group;
    room.style = if group { 43 } else { 45 };
    room.participants = (1..=participants as i64).map(handle).collect();
    room.message_count = messages.len() as i64;

    let mut app = with_chats(vec![room]);
    app.message_rows = messages;
    app.messages.set_len(app.message_rows.len());
    app.update(Action::FocusPane(Focus::Conversation));
    // One frame measures the blocks at the size these tests draw at, which is
    // what lets the jump to the newest message land where the real app lands.
    let _ = frame(&mut app, 120, 34);
    app.update(Action::ToBottom);
    app
}

#[test]
fn a_conversation_draws_blocks_a_day_header_and_meta_lines() {
    let mut first = message(1, true, 0, "dinner tonight? that thai place near you", 40);
    first.date_delivered = ago(39);
    first.date_read = ago(38);
    let second = message(2, false, 1, "yes!! 7?", 30);
    let mut third = message(3, true, 0, "perfect", 20);
    third.date_delivered = ago(19);

    let mut app = with_conversation(false, 1, vec![first, second, third]);
    let buffer = frame(&mut app, 120, 34);

    assert!(contains(&buffer, "dinner tonight"), "a body");
    assert!(contains(&buffer, "Delivered"), "a delivery stamp");
    // The receipt goes under the last thing you said and nowhere else, so the
    // older message's read stamp is not drawn.
    assert!(
        !contains(&buffer, "Read "),
        "one stamp, on the newest of yours"
    );
    assert!(contains(&buffer, "Today"), "the day header");
    assert!(!contains(&buffer, "no messages yet"));
}

#[test]
fn the_day_band_takes_a_row_of_its_own_rather_than_covering_a_message() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "hello", 5)]);
    let buffer = frame(&mut app, 120, 34);
    let band = app.panes.day.expect("a day band");
    let convo = app.panes.conversation;

    // One row, directly above the messages, on the same ground as them.
    assert_eq!(band.height, 1);
    assert_eq!(band.y + 1, convo.y);
    assert_eq!(buffer[(band.x, band.y)].bg, app.theme.bg_base);

    // The day is said, and nothing the message says is hidden under the band.
    assert!(contains(&buffer, "Today"));
    assert!(contains(&buffer, "hello"));
}

/// Forty messages of one day, which is more rows than any pane these tests
/// draw at.
fn a_long_day() -> Vec<Message> {
    (1..=40)
        .map(|n| message(n, n % 2 == 0, 1, "a line of a long day", 5))
        .collect()
}

#[test]
fn the_day_band_names_the_day_once_the_separator_has_scrolled_off() {
    let mut app = with_conversation(false, 1, a_long_day());
    let buffer = frame(&mut app, 120, 34);
    let band = app.panes.day.expect("a day band");

    let row: String = (0..band.width)
        .map(|x| buffer[(x + band.x, band.y)].symbol().to_string())
        .collect();
    assert!(row.contains("Today"), "day band row: {row:?}");
}

#[test]
fn a_thread_longer_than_the_pane_gets_the_mockups_scrollbar() {
    let mut app = with_conversation(false, 1, a_long_day());
    let buffer = frame(&mut app, 120, 34);
    let convo = app.panes.conversation;
    let x = convo.x + convo.width - 1;

    let bar: Vec<String> = (0..convo.height)
        .map(|y| buffer[(x, convo.y + y)].symbol().to_string())
        .collect();
    assert!(bar.iter().any(|cell| cell == "┃"), "a thumb: {bar:?}");
    assert!(bar.iter().any(|cell| cell == "│"), "a track: {bar:?}");
    // Scrolled to the newest message, the thumb sits at the bottom.
    assert_eq!(bar.last().map(String::as_str), Some("┃"), "{bar:?}");

    // A thread that fits leaves the column alone.
    let mut short = with_conversation(false, 1, vec![message(1, false, 1, "hi", 5)]);
    let buffer = frame(&mut short, 120, 34);
    let convo = short.panes.conversation;
    let x = convo.x + convo.width - 1;
    assert_eq!(buffer[(x, convo.y)].symbol(), " ");
}

#[test]
fn names_open_every_run_and_yours_is_the_one_accent() {
    let mut app = with_conversation(
        false,
        1,
        vec![
            message(1, false, 1, "theirs", 20),
            message(2, true, 0, "mine", 10),
            message(3, true, 0, "and more", 9),
        ],
    );
    let buffer = frame(&mut app, 120, 34);
    let area = app.panes.conversation;
    assert!(
        contains(&buffer, "someone1  theirs"),
        "their name opens their run"
    );
    assert!(contains(&buffer, "You  mine"), "your name opens yours");
    assert!(
        contains(&buffer, "  and more"),
        "the run's second message is set in, nameless"
    );

    // The only accent in the pane is on `You`; nothing else is colored.
    let accented: Vec<(u16, u16)> = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .filter(|&(x, y)| buffer[(area.x + x, area.y + y)].fg == app.theme.accent_me)
        .collect();
    assert_eq!(accented.len(), 3, "`You`, once: {accented:?}");

    // The clock sits at the right edge of the pane.
    let right: Vec<String> = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(area.x + x, area.y + y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let clocks = right
        .iter()
        .filter(|row| row.ends_with(" AM") || row.ends_with(" PM"))
        .count();
    assert_eq!(clocks, 3, "one clock per message: {right:?}");

    let mut group = with_conversation(
        true,
        3,
        vec![
            message(1, false, 1, "one", 30),
            message(2, false, 2, "two", 20),
            message(3, false, 3, "three", 10),
        ],
    );
    let buffer = frame(&mut group, 120, 34);
    for who in ["someone1", "someone2", "someone3"] {
        assert!(contains(&buffer, who), "{who} is named");
    }
}

#[test]
fn a_group_event_is_dim_italic_and_nameless() {
    let mut event = message(2, false, 1, "", 10);
    event.item_type = 2;
    event.group_action = Some(msgs::db::GroupAction::NameChange("Sunday".to_string()));

    let mut app = with_conversation(true, 2, vec![message(1, false, 1, "hello", 20), event]);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "named the conversation"));
    assert!(
        contains(&buffer, "someone1  hello"),
        "the message above has a name"
    );
}

#[test]
fn tapbacks_and_replies_show_under_and_above_the_message() {
    let mut target = message(1, false, 1, "who's in for the draft?", 30);
    target.tapbacks = vec![Tapback {
        rowid: 90,
        target_guid: "G1".to_string(),
        target_part: 0,
        action: TapbackAction::Added,
        kind: TapbackKind::Liked,
        is_from_me: false,
        handle_rowid: Some(2),
        handle: Some("someone2@example.invalid".to_string()),
        date: 0,
    }];
    let mut reply = message(2, false, 2, "can we do 4?", 20);
    reply.thread_originator_guid = Some("p:0/G1".to_string());

    let mut app = with_conversation(true, 3, vec![target, reply]);
    let buffer = frame(&mut app, 120, 34);

    assert!(contains(&buffer, "👍"), "a tapback chip");
    assert!(contains(&buffer, "↳"), "a quoted reply");
    assert!(
        contains(&buffer, "who's in for the draft?"),
        "the quote body"
    );
}

#[test]
fn an_attachment_without_words_still_says_what_was_sent() {
    let mut photo = message(1, false, 1, "", 10);
    photo.attachments = vec![AttachmentRef {
        rowid: 1,
        guid: "A1".to_string(),
        message_rowid: 1,
        filename: None,
        mime_type: Some("application/pdf".to_string()),
        uti: None,
        transfer_name: Some("draft-order.pdf".to_string()),
        total_bytes: 86_016,
        transfer_state: 5,
        is_sticker: false,
        hide_attachment: false,
    }];
    let mut app = with_conversation(false, 1, vec![photo]);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "draft-order.pdf · 84 KB"));
}

/// A picture whose rows all differ, so the half-block fallback draws real
/// glyphs rather than collapsing every cell to a colored space.
fn gradient(width: u32, height: u32) -> image::RgbImage {
    image::ImageBuffer::from_fn(width, height, |x, y| {
        let shade = u8::try_from((x + y * 3) % 256).unwrap_or(0);
        image::Rgb([shade, 255 - shade, 128])
    })
}

/// How many cells the half-block renderer painted a picture into.
fn half_blocks(buffer: &Buffer) -> usize {
    buffer
        .content()
        .iter()
        .filter(|cell| cell.symbol() == "▀" || cell.symbol() == "▄")
        .count()
}

#[test]
fn an_attachment_that_never_arrived_says_so_where_its_size_would_be() {
    let mut photo = message(1, false, 1, "", 10);
    photo.attachments = vec![AttachmentRef {
        rowid: 2,
        guid: "A2".to_string(),
        message_rowid: 1,
        // `chat.db` holds no filename for bytes that were never downloaded.
        filename: None,
        mime_type: Some("image/jpeg".to_string()),
        uti: None,
        transfer_name: Some("IMG_4412.jpg".to_string()),
        total_bytes: 2_202_009,
        transfer_state: 1,
        is_sticker: false,
        hide_attachment: false,
    }];
    let mut app = with_conversation(false, 1, vec![photo]);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "IMG_4412.jpg"));
    assert!(contains(&buffer, "(not downloaded on this Mac)"));
}

#[test]
fn a_photo_is_drawn_in_the_pane_and_named_on_the_meta_line() {
    // A picture written for this test, in a temp directory. No test in this
    // file reads the real attachment store.
    let dir = std::env::temp_dir().join(format!("msgs-render-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp directory");
    let path = dir.join("IMG_4412.jpg");
    image::DynamicImage::from(gradient(320, 160))
        .save(&path)
        .expect("a written jpeg");

    let mut photo = message(1, false, 1, "", 10);
    photo.attachments = vec![AttachmentRef {
        rowid: 3,
        guid: "A3".to_string(),
        message_rowid: 1,
        filename: Some(path.display().to_string()),
        mime_type: Some("image/jpeg".to_string()),
        uti: None,
        transfer_name: Some("IMG_4412.jpg".to_string()),
        total_bytes: 2_202_009,
        transfer_state: 5,
        is_sticker: false,
        hide_attachment: false,
    }];

    let mut plain = with_conversation(false, 1, vec![photo.clone()]);
    let without = frame(&mut plain, 120, 34);
    assert!(
        contains(&without, "IMG_4412.jpg · 2.1 MB"),
        "the chip fallback names the file"
    );
    assert!(contains(&without, "┄"), "and draws it as a dashed chip");

    let mut app = with_conversation(false, 1, vec![photo]);
    app.enable_images(msgs::media::Images::halfblocks());
    let buffer = frame(&mut app, 120, 34);

    // The name and size move onto the meta line, and the chip goes away.
    assert!(contains(&buffer, "IMG_4412.jpg · 2.1 MB"), "the meta line");
    assert!(!contains(&buffer, "┄"), "no chip once the picture is drawn");

    // 320×160 pixels is thirty-two by eight cells at the half-block picker's
    // font, and every one of them is painted rather than left blank.
    let painted = half_blocks(&buffer);
    assert_eq!(
        painted,
        32 * 8,
        "the picture filled the rows reserved for it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_photo_scrolled_off_the_top_keeps_the_rows_that_are_still_in_view() {
    let dir = std::env::temp_dir().join(format!("msgs-render-scroll-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp directory");
    let path = dir.join("IMG_4412.jpg");
    image::DynamicImage::from(gradient(320, 160))
        .save(&path)
        .expect("a written jpeg");

    let mut photo = message(1, false, 1, "", 10);
    photo.attachments = vec![AttachmentRef {
        rowid: 4,
        guid: "A4".to_string(),
        message_rowid: 1,
        filename: Some(path.display().to_string()),
        mime_type: Some("image/jpeg".to_string()),
        uti: None,
        transfer_name: Some("IMG_4412.jpg".to_string()),
        total_bytes: 2_202_009,
        transfer_state: 5,
        is_sticker: false,
        hide_attachment: false,
    }];
    let mut app = with_conversation(false, 1, vec![photo]);
    app.enable_images(msgs::media::Images::halfblocks());

    // A pane only a few rows tall cuts the picture off at both edges; the
    // protocol clips it rather than refusing to draw.
    // The first short frame measures at the new height; the jump to the
    // newest message then lands the way it would in the real app.
    let _ = frame(&mut app, 120, 12);
    app.update(Action::ToBottom);
    let short = frame(&mut app, 120, 12);
    let painted = half_blocks(&short);
    assert!(painted > 0, "some of the picture is still on screen");
    assert!(painted < 32 * 8, "and not all of it fits");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_header_names_the_chat_the_service_and_nothing_more() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "hi", 5)]);
    let buffer = frame(&mut app, 120, 34);

    assert!(contains(&buffer, "Fixture Chat"), "the name");
    assert!(contains(&buffer, "iMessage"), "the service");
    assert!(contains(&buffer, "someone1"), "the other party");
    assert!(!contains(&buffer, " msg"), "no counts");
}

#[test]
fn j_and_k_move_the_message_selection_and_g_jumps_to_the_ends() {
    let messages = (1..=30)
        .map(|index| {
            message(
                index,
                index % 2 == 0,
                1,
                &format!("line {index}"),
                60 - index,
            )
        })
        .collect();
    let mut app = with_conversation(false, 1, messages);
    let _ = frame(&mut app, 120, 34);
    assert_eq!(app.messages.selected, 29);

    press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(app.messages.selected, 28);
    press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(app.messages.selected, 29);

    press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(app.messages.selected, 0);
    assert_eq!(app.convo.top, 0);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "line 1"));

    press(&mut app, KeyCode::Char('G'), KeyModifiers::NONE);
    assert_eq!(app.messages.selected, 29);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "line 30"));
    assert!(!contains(&buffer, "line 1 "), "the top has scrolled away");
}

#[test]
fn the_selected_block_is_highlighted_and_a_click_moves_the_selection() {
    let messages = (1..=10)
        .map(|index| message(index, false, 1, &format!("line {index}"), 60 - index))
        .collect();
    let mut app = with_conversation(false, 1, messages);
    let buffer = frame(&mut app, 120, 34);
    let area = app.panes.conversation;

    let selected_row = app
        .hits
        .rows
        .iter()
        .position(|row| *row == Some(app.messages.selected))
        .expect("the selected block is on screen");
    let y = area.y + selected_row as u16;
    assert_eq!(buffer[(area.x + 4, y)].bg, app.theme.bg_highlight);

    // Click the first block that is not the selected one.
    let (other_row, other_index) = app
        .hits
        .rows
        .iter()
        .enumerate()
        .find_map(|(row, index)| match index {
            Some(index) if *index != app.messages.selected => Some((row, *index)),
            _ => None,
        })
        .expect("another block on screen");
    click(&mut app, area.x + 6, area.y + other_row as u16);
    assert_eq!(app.messages.selected, other_index);
}

#[test]
fn the_wheel_scrolls_the_conversation_without_moving_the_selection() {
    let messages = (1..=60)
        .map(|index| message(index, false, 1, &format!("line {index}"), 120 - index))
        .collect();
    let mut app = with_conversation(false, 1, messages);
    let _ = frame(&mut app, 120, 34);
    let selected = app.messages.selected;
    let before = app.convo;

    let area = app.panes.conversation;
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: area.x + 5,
        row: area.y + 5,
        modifiers: KeyModifiers::NONE,
    });

    assert_ne!(app.convo, before, "the wheel scrolls");
    assert_eq!(app.messages.selected, selected, "but does not select");
    assert_eq!(app.focus, Focus::Conversation);
}

#[test]
fn links_are_underlined_and_a_click_on_one_is_a_link_not_a_selection() {
    let mut app = with_conversation(
        false,
        1,
        vec![message(
            1,
            true,
            0,
            "menu here https://example.invalid/menu",
            5,
        )],
    );
    let buffer = frame(&mut app, 120, 34);

    assert_eq!(app.hits.links.len(), 1, "one link was recorded");
    let hit = &app.hits.links[0];
    assert_eq!(hit.url, "https://example.invalid/menu");
    let cell = &buffer[(hit.start, hit.y)];
    assert!(
        cell.modifier.contains(ratatui::style::Modifier::UNDERLINED),
        "links are underlined"
    );
    assert_eq!(cell.fg, app.theme.accent_me);
}

#[test]
fn copying_the_selected_message_reports_what_it_did() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "sounds good", 5)]);
    let _ = frame(&mut app, 120, 34);
    press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);

    let (toast, _) = app.status.active_toast().expect("a toast");
    assert!(
        toast.contains("clipboard") || toast.contains("could not copy"),
        "unexpected toast: {toast}"
    );
    // Nothing about the message body leaks onto the status line.
    assert!(!toast.contains("sounds good"));
}

#[test]
fn ctrl_l_says_so_when_the_selected_message_has_no_link() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "no links here", 5)]);
    let _ = frame(&mut app, 120, 34);
    press(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL);

    let (toast, _) = app.status.active_toast().expect("a toast");
    assert!(toast.contains("no link"), "unexpected toast: {toast}");
}

#[test]
fn five_thousand_messages_lay_out_only_what_is_on_screen() {
    let messages = (1..=5000)
        .map(|index| {
            message(
                index,
                index % 3 == 0,
                1,
                "a line of about the length a real message has in it",
                6000 - index,
            )
        })
        .collect();
    let mut app = with_conversation(false, 1, messages);

    let started = std::time::Instant::now();
    for _ in 0..20 {
        let buffer = frame(&mut app, 120, 34);
        assert!(!contains(&buffer, "no messages yet"));
    }
    for _ in 0..40 {
        press(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        let _ = frame(&mut app, 120, 34);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "sixty frames of a 5,000-message conversation took {elapsed:?}"
    );
    assert!(app.convo.top < 5000);
}

// ---------------------------------------------------------------------------
// The composer and the send path. Every test below puts an inert outbox on the
// app first, so the whole path runs and nothing is ever handed to Messages.app.
// ---------------------------------------------------------------------------

use msgs::send::{Delivery, Outbox, SendError};

/// An app with one chat open and an outbox that records instead of sending.
fn ready_to_send() -> App {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "yes!! 7?", 30)]);
    app.outbox = Outbox::inert();
    app.update(Action::FocusPane(Focus::Composer));
    app
}

#[test]
fn an_empty_composer_names_the_chat_it_will_send_to() {
    let mut app = with_chats(vec![chat(1, "Fixture Chat", 2, "last line")]);
    app.update(Action::FocusPane(Focus::Composer));
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "Message Fixture Chat"));
    assert!(
        !contains(&buffer, "Enter send"),
        "no hint bar; the keys are in ? help"
    );
}

#[test]
fn enter_sends_the_draft_and_the_block_says_it_is_on_its_way() {
    let mut app = ready_to_send();
    type_text(&mut app, "on my way in 20");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "on my way in 20"), "the echo is drawn");
    assert!(contains(&buffer, "Sending…"));
    assert!(!contains(&buffer, "› on my way"), "the box is empty again");
    assert_eq!(app.pending.len(), 1);

    // Messages takes it: the note goes, the block stays.
    app.outbox.answer(app.pending[0].id, Ok(()));
    app.tick();
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "on my way in 20"));
    assert!(!contains(&buffer, "Sending…"));
}

#[test]
fn a_refused_send_says_why_on_the_block_and_gives_the_draft_back() {
    let mut app = ready_to_send();
    type_text(&mut app, "on my way in 20");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    app.outbox.answer(
        app.pending[0].id,
        Err(SendError::Script("Messages is not signed in".to_string())),
    );
    app.tick();

    let buffer = frame(&mut app, 140, 34);
    assert!(contains(&buffer, "Failed — Messages is not signed in"));
    assert!(
        contains(&buffer, "❯ on my way in 20"),
        "the draft came back"
    );
    assert_eq!(
        app.pending[0].state,
        Delivery::Failed("Messages is not signed in".to_string())
    );
}

#[test]
fn ctrl_a_turns_the_composer_into_a_path_prompt() {
    let mut app = ready_to_send();
    type_text(&mut app, "look at this");
    press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "attach"), "the box says what it wants");
    assert!(contains(&buffer, "Path to a file"));
    assert!(!contains(&buffer, "look at this"), "the draft is put aside");

    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "look at this"), "and comes back");
    assert_eq!(app.focus, Focus::Composer);
}

#[test]
fn r_quotes_the_selected_message_and_moves_to_the_composer() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "yes!! 7?", 30)]);
    app.outbox = Outbox::inert();
    press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);

    assert_eq!(app.focus, Focus::Composer);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "> yes!! 7?"));
}

// ---------------------------------------------------------------------------
// Tapbacks. The picker draws, `Enter` chips the reaction on before `chat.db`
// knows about it, and the inert outbox means no `imsg` ever runs.
// ---------------------------------------------------------------------------

/// Whether a chip saying you reacted with `glyph` is on screen.
///
/// An emoji takes two terminal cells, so the row it lands on carries the glyph
/// and a blank rather than the string the chip was built from; matching the
/// two halves on one row is what survives that.
fn my_chip(buffer: &Buffer, glyph: &str) -> bool {
    rows(buffer)
        .iter()
        .any(|row| row.contains(glyph) && row.contains("You"))
}

#[test]
fn ctrl_r_opens_a_picker_holding_every_sendable_reaction() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "yes!! 7?", 30)]);
    app.outbox = Outbox::inert();
    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(app.focus, Focus::Reactions);

    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "react"), "the picker is titled");
    for glyph in ["❤️", "👍", "👎", "😂", "‼️", "❓"] {
        assert!(contains(&buffer, glyph), "{glyph} is missing");
    }
    assert!(contains(&buffer, "love"), "the cursor names what it is on");

    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Conversation);
    let buffer = frame(&mut app, 120, 34);
    assert!(!contains(&buffer, "Esc close"), "the picker is gone");
}

#[test]
fn a_chosen_reaction_is_chipped_on_before_the_database_has_it() {
    let mut app = with_conversation(false, 1, vec![message(1, false, 1, "yes!! 7?", 30)]);
    app.outbox = Outbox::inert();
    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    // Straight to the second glyph and send it.
    press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);

    assert_eq!(
        app.focus,
        Focus::Conversation,
        "the picker closes behind it"
    );
    assert_eq!(app.pending_tapbacks.len(), 1);
    assert_eq!(app.outbox.recorded().len(), 1);

    let buffer = frame(&mut app, 120, 34);
    assert!(my_chip(&buffer, "👍"), "the chip stands on the block");
    assert!(
        app.message_rows[0].tapbacks.is_empty(),
        "the loaded page is still exactly what the database said"
    );

    // The database catches up: the optimistic chip retires into the real one,
    // and nothing on screen moves. Retiring it against a live database is what
    // `tests/live.rs` covers.
    app.message_rows[0].tapbacks = vec![Tapback {
        rowid: 9,
        target_guid: "G1".to_string(),
        target_part: 0,
        action: TapbackAction::Added,
        kind: TapbackKind::Liked,
        is_from_me: true,
        handle_rowid: None,
        handle: None,
        date: ago(1),
    }];
    app.pending_tapbacks.clear();
    app.measured.stale = true;
    let buffer = frame(&mut app, 120, 34);
    assert!(my_chip(&buffer, "👍"), "and the chip does not flicker");
}

#[test]
fn taking_a_reaction_back_removes_the_chip_before_the_database_does() {
    let mut liked = message(1, false, 1, "yes!! 7?", 30);
    liked.tapbacks = vec![Tapback {
        rowid: 9,
        target_guid: "G1".to_string(),
        target_part: 0,
        action: TapbackAction::Added,
        kind: TapbackKind::Loved,
        is_from_me: true,
        handle_rowid: None,
        handle: None,
        date: ago(20),
    }];
    let mut app = with_conversation(false, 1, vec![liked]);
    app.outbox = Outbox::inert();
    let buffer = frame(&mut app, 120, 34);
    assert!(my_chip(&buffer, "❤"));

    press(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        app.pending_tapbacks[0].remove,
        "Enter on yours takes it back"
    );

    let buffer = frame(&mut app, 120, 34);
    assert!(!my_chip(&buffer, "❤"), "the chip goes at once");
}

/// A throwaway tree for the `@` picker, removed when the test ends. Never the
/// reader's own folders: `App::picker_roots` is pointed at this instead.
struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "msgs-render-picker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Downloads")).expect("a temporary tree");
        std::fs::write(base.join("Downloads/report.pdf"), b"x").expect("a file");
        Self(base)
    }

    fn root(&self) -> PathBuf {
        self.0.join("Downloads")
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_at_opens_the_file_picker_over_the_composer_and_enter_chips_the_file() {
    let tree = TempTree::new();
    let mut app = app();
    app.update(Action::FocusPane(Focus::Composer));
    app.picker_roots = Some(vec![tree.root()]);

    type_text(&mut app, "look at @rep");
    assert_eq!(app.focus, Focus::FilePicker);

    let mut terminal = Terminal::new(TestBackend::new(120, 34)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw succeeded");
    let buffer = terminal.backend().buffer().clone();
    assert!(contains(&buffer, "report.pdf"), "the matched file");
    assert!(contains(&buffer, "Enter attach"), "the picker footer");
    assert!(
        contains(&buffer, "look at @rep"),
        "the draft is still there"
    );

    // The picker sits above the send box, and the draft keeps the cursor.
    let composer = app.panes.composer;
    let listed = rows(&buffer)
        .iter()
        .position(|row| row.contains("report.pdf"))
        .expect("a row with the file on it");
    assert!(
        u16::try_from(listed).expect("a row number") < composer.y,
        "the picker is drawn above the composer"
    );
    let cursor = terminal.backend().cursor_position();
    assert!(cursor.y > composer.y && cursor.y < composer.y + composer.height);

    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Composer);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "report.pdf"), "the chip names the file");
    assert!(contains(&buffer, "look at "), "and the draft carries on");
}
