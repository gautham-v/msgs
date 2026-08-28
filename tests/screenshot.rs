//! The frame the README shows.
//!
//! `docs/screenshot.txt` is a real frame, drawn by the real widgets against the
//! synthetic fixture and its invented address book — never against
//! `~/Library/Messages/chat.db`, because a picture of a terminal is exactly the
//! thing that would carry somebody's messages into a repository. Every name,
//! number, and body in it was made up by `tests/fixtures/mod.rs`.
//!
//! The frame is regenerated when its file is missing or when
//! `UPDATE_SCREENSHOT` is set, and compared otherwise, so the README cannot
//! drift away from what the app draws. Both need `TZ=UTC`, because the times in
//! a transcript are drawn in the local zone: run
//! `TZ=UTC UPDATE_SCREENSHOT=1 cargo test --test screenshot` to refresh it.

mod fixtures;

use std::path::{Path, PathBuf};

use chrono::{Local, Offset};
use msgs::app::{App, Focus, WatcherStatus};
use msgs::config::Config;
use msgs::contacts::Contacts;
use msgs::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

/// The size of the frame in the README: wide enough for the chat list and the
/// conversation side by side, short enough to read in a page.
const WIDTH: u16 = 96;
const HEIGHT: u16 = 26;

/// A scratch directory for the invented address book, removed on the way out.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("msgs-screenshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }

    fn contacts(&self) -> Contacts {
        let store = fixtures::address_book(&self.0.join("Sources").join("FIXTURE-ACCOUNT"));
        Contacts::load_from(&[store], None)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn docs(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join(name)
}

/// The buffer as one right-trimmed string per row.
fn rows(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Whether the clocks in a frame drawn here would match the stored one.
///
/// Every time in the transcript is drawn in the local zone, so the frame is
/// only comparable in the zone it was written in — UTC, which is what a CI
/// runner uses. Anywhere else the test says so and stops rather than failing
/// over a time difference or rewriting the file into a local zone.
fn in_utc() -> bool {
    Local::now().offset().fix().local_minus_utc() == 0
}

/// An app on the fixture database, with the fixture's invented names on it.
fn app(scratch: &Scratch) -> App {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());
    app.enable_contacts(scratch.contacts());
    // A machine whose file watcher will not start draws `polling chat.db` on
    // the status line instead, which is true of it and not of the frame the
    // README is showing; the rest of the line is the database's own answer.
    app.status.watcher = WatcherStatus::Watching;
    app
}

/// Draw `app` twice: the first frame settles the conversation viewport, the
/// second is the one somebody looks at.
fn capture(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal");
    for _ in 0..2 {
        terminal
            .draw(|frame| ui::draw(frame, app))
            .expect("draw succeeded");
    }
    rows(terminal.backend().buffer())
}

/// Compare `drawn` with the file the README embeds, or write it when it is not
/// there yet.
fn store(name: &str, drawn: &str) {
    let path = docs(name);
    if !in_utc() {
        eprintln!("docs/{name} is only checked under TZ=UTC; skipping in this zone");
        return;
    }
    if std::env::var_os("UPDATE_SCREENSHOT").is_some() || !path.exists() {
        std::fs::write(&path, drawn).expect("write the README frame");
        return;
    }
    let stored = std::fs::read_to_string(&path).expect("read the README frame");
    assert_eq!(
        stored, drawn,
        "docs/{name} is not what the app draws any more — \
         rerun with TZ=UTC UPDATE_SCREENSHOT=1 and check the README block"
    );

    // The manual embeds the same frame, and a stale block there is the thing
    // this test exists to prevent. (The README may carry it too, or a video.)
    let manual = std::fs::read_to_string(docs("MANUAL.md")).expect("read the manual");
    assert!(
        manual.contains(drawn.trim_end()),
        "the block in docs/MANUAL.md is not docs/{name} any more"
    );
}

#[test]
fn the_readme_frame_is_what_the_app_draws() {
    let scratch = Scratch::new("main");
    let mut app = app(&scratch);
    // The one-to-one thread: a photo, two tapbacks, a read receipt, and an
    // unread message under them, which is most of what one frame can show.
    app.open_chat_row(fixtures::CHAT_DIRECT);
    app.focus = Focus::Conversation;
    store("screenshot.txt", &capture(&mut app));
}
