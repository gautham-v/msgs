//! Pinned conversations, driven through the app the way `main` drives it.
//!
//! Everything here runs against the synthetic fixture database and a synthetic
//! preference file written into a scratch directory of its own, so no test
//! reads `~/Library/Messages/chat.db` or `~/Library/Preferences`, and nothing
//! is written into the user's home.

mod fixtures;

use std::path::PathBuf;

use msgs::app::App;
use msgs::config::Config;
use msgs::pins::{Pins, Status};
use msgs::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// A private directory for one test's preference file.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "msgs-pins-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self { dir }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The fixture database, opened but with nothing pinned yet.
fn app() -> App {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app
}

/// The chat list as rowids, in the order the pane draws it.
fn order(app: &App) -> Vec<i64> {
    app.chat_rows.iter().map(|chat| chat.rowid).collect()
}

#[test]
fn nothing_is_pinned_until_the_preference_file_is_read() {
    let app = app();
    assert!(app.chat_rows.iter().all(|chat| !chat.is_pinned()));
    assert_eq!(app.pinned_visible, 0);
    // The database records no pinning at all, so the column stays absent.
    assert!(app.chat_rows.iter().all(|chat| chat.is_pinned.is_none()));
}

#[test]
fn a_pinned_group_is_found_by_the_id_behind_its_hex_entry() {
    let scratch = Scratch::new("group");
    let plist = fixtures::pinning_plist(
        &scratch.dir,
        &[],
        &[("Fixture Group", fixtures::GROUP_ORIGINAL_ID)],
    );

    let mut app = app();
    let before = order(&app);
    app.enable_pins(Pins::load(&plist));

    assert_eq!(app.chat_rows[0].rowid, fixtures::CHAT_GROUP);
    assert!(app.chat_rows[0].is_pinned());
    assert_eq!(app.pinned_visible, 1);
    // Every other chat gets an answer too, rather than being left unknown.
    assert!(
        app.chat_rows[1..]
            .iter()
            .all(|chat| chat.is_pinned == Some(false))
    );
    // And the rest of the list keeps the order it already had.
    let rest: Vec<i64> = order(&app).into_iter().skip(1).collect();
    let expected: Vec<i64> = before
        .into_iter()
        .filter(|rowid| *rowid != fixtures::CHAT_GROUP)
        .collect();
    assert_eq!(rest, expected);
}

#[test]
fn a_pinned_person_is_found_by_their_address() {
    let scratch = Scratch::new("person");
    // Written the way somebody types it, not the way `chat.db` stores it.
    let plist = fixtures::pinning_plist(&scratch.dir, &["(555) 000-0001"], &[]);

    let mut app = app();
    app.enable_pins(Pins::load(&plist));

    assert_eq!(app.chat_rows[0].rowid, fixtures::CHAT_DIRECT);
    assert_eq!(app.pinned_visible, 1);
}

#[test]
fn two_pinned_chats_stay_newest_first_among_themselves() {
    let scratch = Scratch::new("both");
    let plist = fixtures::pinning_plist(
        &scratch.dir,
        &[fixtures::DIRECT_ADDRESS],
        &[("Fixture Group", fixtures::GROUP_ORIGINAL_ID)],
    );

    let mut app = app();
    let before = order(&app);
    app.enable_pins(Pins::load(&plist));
    assert_eq!(app.pinned_visible, 2);

    // Whichever of the two spoke last is still the one on top: the pin state
    // says which section a chat is in, never where it sits inside one.
    let pinned: Vec<i64> = order(&app).into_iter().take(2).collect();
    let expected: Vec<i64> = before
        .into_iter()
        .filter(|rowid| pinned.contains(rowid))
        .collect();
    assert_eq!(pinned, expected);
}

#[test]
fn a_chat_that_is_not_pinned_anywhere_is_left_where_it_was() {
    let scratch = Scratch::new("miss");
    // A group nobody in the fixture is: no chat may move.
    let plist = fixtures::pinning_plist(
        &scratch.dir,
        &["+15559999999"],
        &[("Some Other Group", "00000000-0000-4000-8000-000000000000")],
    );

    let mut app = app();
    let before = order(&app);
    app.enable_pins(Pins::load(&plist));

    assert_eq!(order(&app), before);
    assert_eq!(app.pinned_visible, 0);
    assert!(app.chat_rows.iter().all(|chat| !chat.is_pinned()));
}

#[test]
fn a_mac_that_has_never_pinned_anything_is_not_a_warning() {
    let scratch = Scratch::new("absent");
    let missing = scratch.dir.join("com.apple.messages.pinning.plist");

    let mut app = app();
    let before = order(&app);
    app.enable_pins(Pins::load(&missing));

    assert_eq!(order(&app), before);
    assert_eq!(app.pins.status(), &Status::Ready { pinned: 0 });
    assert!(
        app.status.warnings.is_empty(),
        "a missing preference file is the normal case, not a complaint"
    );
}

#[test]
fn an_unreadable_preference_file_is_a_warning_rather_than_a_failure() {
    let scratch = Scratch::new("broken");
    std::fs::create_dir_all(&scratch.dir).expect("scratch directory");
    let path = scratch.dir.join("com.apple.messages.pinning.plist");
    std::fs::write(&path, b"this is not a plist").expect("write the scratch file");

    let mut app = app();
    let before = order(&app);
    app.enable_pins(Pins::load(&path));

    assert_eq!(order(&app), before, "the list still loads");
    assert_eq!(app.status.warnings.len(), 1);
    assert!(app.status.warnings[0].starts_with("pins: "));
}

#[test]
fn a_pin_made_while_msgs_is_open_reaches_the_next_reload() {
    let scratch = Scratch::new("live");
    let plist = fixtures::pinning_plist(&scratch.dir, &[], &[]);

    let mut app = app();
    app.enable_pins(Pins::load(&plist));
    assert_eq!(app.pinned_visible, 0);

    // Messages.app rewrites the file; msgs re-reads it on its next reload of
    // the chat list rather than holding the pins it read at startup.
    fixtures::pinning_plist(&scratch.dir, &[fixtures::DIRECT_ADDRESS], &[]);
    app.reload_chats();

    assert_eq!(app.chat_rows[0].rowid, fixtures::CHAT_DIRECT);
    assert_eq!(app.pinned_visible, 1);
}

#[test]
fn the_list_opens_a_pinned_section_and_marks_the_row() {
    let scratch = Scratch::new("render");
    let plist = fixtures::pinning_plist(&scratch.dir, &[fixtures::DIRECT_ADDRESS], &[]);

    let mut app = app();
    app.enable_pins(Pins::load(&plist));

    let mut terminal = Terminal::new(TestBackend::new(110, 24)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("draw succeeded");
    let buffer = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();

    assert!(
        rows.iter().any(|row| row.contains("PINNED")),
        "the list opens a pinned section"
    );
    assert!(
        rows.iter().any(|row| row.contains("RECENT")),
        "and closes it with the rest of the list"
    );
    // The marker itself: one middle dot, in the two columns the name was
    // already indented by, so nothing on the row shifts.
    assert!(
        rows.iter().any(|row| row.trim_start().starts_with('·')),
        "a pinned row carries its marker in the gutter"
    );
}

/// The whole promise of the feature, in one line: nothing is written anywhere.
#[test]
fn the_preference_file_is_only_ever_read() {
    let scratch = Scratch::new("readonly");
    let plist = fixtures::pinning_plist(&scratch.dir, &[fixtures::DIRECT_ADDRESS], &[]);
    let before = std::fs::read(&plist).expect("read the fixture");
    let stamp = std::fs::metadata(&plist)
        .and_then(|meta| meta.modified())
        .expect("stamp the fixture");

    let mut app = app();
    app.enable_pins(Pins::load(&plist));
    app.reload_chats();

    assert_eq!(std::fs::read(&plist).expect("read again"), before);
    assert_eq!(
        std::fs::metadata(&plist)
            .and_then(|meta| meta.modified())
            .expect("stamp again"),
        stamp
    );
}

#[test]
fn a_pinned_person_with_two_service_rows_is_pinned_once() {
    let scratch = Scratch::new("merged");
    let plist = fixtures::pinning_plist(&scratch.dir, &[fixtures::DIRECT_ADDRESS], &[]);

    let mut app = app();
    app.enable_pins(Pins::load(&plist));

    assert_eq!(app.pinned_visible, 1, "one entry, not one per service");
    let pinned: Vec<i64> = app
        .chat_rows
        .iter()
        .filter(|chat| chat.is_pinned())
        .map(|chat| chat.rowid)
        .collect();
    assert_eq!(pinned, vec![fixtures::CHAT_DIRECT]);
    // The row that was merged away is not a second entry anywhere in the list.
    assert!(
        app.chat_rows
            .iter()
            .all(|chat| chat.rowid != fixtures::CHAT_DIRECT_SMS)
    );
}
