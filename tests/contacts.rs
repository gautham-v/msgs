//! Names for handles: reading the Contacts stores, caching the result, and
//! getting the names onto the screen.
//!
//! Every Contacts store here is built by `tests/fixtures::address_book` into a
//! private temporary directory, and every cache is written into that same
//! directory. Nothing points at `~/Library/Application Support/AddressBook` and
//! nothing writes into the real Application Support directory.
//!
//! The names and numbers asserted on are the ones the fixture itself invented.

mod fixtures;

use std::path::PathBuf;

use msgs::app::{Action, App};
use msgs::config::Config;
use msgs::contacts::{Contacts, Status};
use msgs::db::Name;
use msgs::ui;
use msgs::ui::conversation::subtitle;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

/// A private directory for one test's Contacts store and cache.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "msgs-contacts-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Self { dir }
    }

    /// A Contacts store laid out the way macOS lays one out: an account
    /// directory under `Sources`, which is where the real names live.
    ///
    /// Built once, so two loads in one test see the same stamps.
    fn store(&self) -> PathBuf {
        let account = self.dir.join("Sources").join("FIXTURE-ACCOUNT");
        let path = account.join("AddressBook-v22.abcddb");
        if path.is_file() {
            return path;
        }
        fixtures::address_book(&account)
    }

    fn cache(&self) -> PathBuf {
        self.dir.join("contacts.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn loaded(scratch: &Scratch) -> Contacts {
    scratch.store();
    let stores = msgs::contacts::store_paths(&scratch.dir);
    assert_eq!(stores.len(), 1, "the account store is the only one found");
    Contacts::load_from(&stores, Some(&scratch.cache()))
}

/// The fixture database with the fixture Contacts applied to it.
fn app_with_contacts(scratch: &Scratch) -> App {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app.enable_contacts(loaded(scratch));
    app
}

fn frame(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw succeeded");
    terminal.backend().buffer().clone()
}

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

#[test]
fn every_way_a_number_is_written_in_contacts_finds_the_handle_it_belongs_to() {
    let scratch = Scratch::new("normalize");
    let contacts = loaded(&scratch);
    assert!(contacts.status().is_ready());

    // `chat.db` stores E.164; the fixture wrote the same numbers with
    // parentheses, dashes, and no country code.
    assert_eq!(contacts.label("+15550000001"), "Alex Nakamura");
    assert_eq!(contacts.label("+15550000002"), "Bailey Okonkwo");
    assert_eq!(contacts.short("+15550000002"), "Bailey");
    // An email matches whatever case either side wrote it in.
    assert_eq!(contacts.label("casey@example.invalid"), "Casey Lindqvist");
    // A record with no personal name is called by its organization.
    assert_eq!(contacts.label("+15550000009"), "Fixture Coffee");
    // Somebody who is not in Contacts keeps a readable address.
    assert_eq!(contacts.label("+15559999999"), "+1 (555) 999-9999");
    assert_eq!(contacts.short("stranger@example.invalid"), "stranger");
}

#[test]
fn the_second_load_comes_from_the_cache_and_a_changed_store_rebuilds_it() {
    let scratch = Scratch::new("cache");
    let first = loaded(&scratch);
    assert_eq!(
        first.status(),
        &Status::Ready {
            addresses: first.len(),
            cached: false
        }
    );
    assert!(scratch.cache().is_file(), "the cache was written");

    let second = loaded(&scratch);
    assert_eq!(
        second.status(),
        &Status::Ready {
            addresses: first.len(),
            cached: true
        }
    );
    assert_eq!(second.label("+15550000001"), first.label("+15550000001"));

    // Rewriting the store moves its length and its modification time, which is
    // what the stamps in the cache are for.
    let store = scratch.store();
    let mut bytes = std::fs::read(&store).expect("read the store");
    bytes.extend_from_slice(&[0u8; 4096]);
    std::fs::write(&store, bytes).expect("grow the store");
    let third = Contacts::load_from(&[store], Some(&scratch.cache()));
    assert!(matches!(
        third.status(),
        Status::Ready { cached: false, .. }
    ));
}

#[cfg(unix)]
#[test]
fn the_cache_is_private_to_the_person_it_describes() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("private");
    let _ = loaded(&scratch);
    let mode = std::fs::metadata(scratch.cache())
        .expect("cache metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the cache holds names and numbers");
}

#[test]
fn a_mac_with_no_readable_contacts_says_so_once_and_shows_numbers() {
    let contacts = Contacts::load_from(
        &[PathBuf::from("/nonexistent/msgs/AddressBook.abcddb")],
        None,
    );
    assert!(matches!(contacts.status(), Status::Unavailable(_)));
    let warning = contacts.status().warning().expect("a one-line hint");
    assert!(warning.starts_with("contacts: "));

    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app.enable_contacts(contacts);
    assert!(
        app.status.warnings.iter().any(|line| line == &warning),
        "the hint reaches the status line"
    );

    // Every handle falls back to a spaced-out number.
    let direct = app
        .chat_rows
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_DIRECT)
        .expect("the direct chat");
    assert_eq!(direct.title(), "+1 (555) 000-0001");
}

#[test]
fn the_chat_list_and_the_header_call_people_by_name() {
    let scratch = Scratch::new("panes");
    let mut app = app_with_contacts(&scratch);

    let direct = app
        .chat_rows
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_DIRECT)
        .expect("the direct chat")
        .clone();
    assert_eq!(direct.title(), "Alex Nakamura");
    // The header says the name, and the subtitle beside it says the address the
    // name is hiding.
    assert_eq!(subtitle(&direct), " · iMessage · +1 (555) 000-0001");

    app.open_chat_row(fixtures::CHAT_DIRECT);
    let buffer = frame(&mut app, 120, 34);
    assert!(
        contains(&buffer, "Alex Nakamura"),
        "the chat list and the header show the name"
    );
    assert!(
        !contains(&buffer, "+15550000001"),
        "and never the raw handle"
    );
}

#[test]
fn a_group_names_its_senders_and_previews_them_by_first_name() {
    let scratch = Scratch::new("group");
    let mut app = app_with_contacts(&scratch);

    let group = app
        .chat_rows
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_GROUP)
        .expect("the group");
    let people: Vec<String> = group
        .participants
        .iter()
        .map(msgs::db::Handle::short_name)
        .collect();
    assert_eq!(people, vec!["Alex", "Bailey", "Casey"]);

    // Open it and draw: the sender label on a group block is a first name.
    app.open_chat_row(fixtures::CHAT_GROUP);
    let buffer = frame(&mut app, 120, 34);
    assert!(contains(&buffer, "Alex"), "the sender is named");
    assert!(
        !contains(&buffer, "+1 (555) 000-0001"),
        "the number is gone"
    );
}

#[test]
fn a_person_is_found_in_the_palette_by_the_name_contacts_has_for_them() {
    let scratch = Scratch::new("palette");
    let mut app = app_with_contacts(&scratch);

    app.update(Action::OpenPalette);
    for c in "Nakamura".chars() {
        app.update(Action::Insert(c));
    }
    assert!(
        app.jump
            .rows
            .iter()
            .any(|row| row.chat_rowid == fixtures::CHAT_DIRECT),
        "the conversation with that person is offered"
    );
}

#[test]
fn one_person_reached_at_two_addresses_keeps_one_color_in_a_group() {
    // Contacts joins the two addresses under one name, so the rail follows the
    // person rather than the row.
    let contacts = Contacts::from_names(std::collections::BTreeMap::from([
        (
            "+15550000001".to_string(),
            Name {
                first: "Alex".to_string(),
                last: "Nakamura".to_string(),
            },
        ),
        (
            "alex@example.invalid".to_string(),
            Name {
                first: "Alex".to_string(),
                last: "Nakamura".to_string(),
            },
        ),
    ]));
    let mut handles = vec![
        msgs::db::Handle::new(1, "+15550000001".to_string(), "SMS".to_string()),
        msgs::db::Handle::new(
            2,
            "alex@example.invalid".to_string(),
            "iMessage".to_string(),
        ),
    ];
    contacts.apply_handles(&mut handles);
    assert_eq!(handles[0].name, handles[1].name);
    assert_eq!(handles[1].short_name(), "Alex");
}
