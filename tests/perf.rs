//! The budgets msgs is meant to hold, measured against two hundred thousand
//! invented messages.
//!
//! The fixture is `tests/fixtures/mod.rs`'s own 200k-message database — built
//! here, never copied from `~/Library/Messages/chat.db` — and nothing in this
//! file prints a body, an address, or a name: the failures carry timings and
//! byte counts only.
//!
//! Numbers are stated for a release build, which is what `cargo install` and
//! the release workflow produce. A debug test binary runs the same work with
//! the optimizer off, so every budget is multiplied by [`SLACK`] there; the
//! point of the assertions is to catch a change of shape — a query that starts
//! walking the whole thread, a frame that starts measuring every block — not
//! to time the compiler.

mod fixtures;

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use msgs::app::{Action, App, Focus};
use msgs::config::Config;
use msgs::db::{Db, PAGE};
use msgs::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// How much longer an unoptimized build is allowed to take.
const SLACK: u32 = 4;

/// A budget, relaxed for a debug test binary.
fn budget(release: Duration) -> Duration {
    if cfg!(debug_assertions) {
        release * SLACK
    } else {
        release
    }
}

/// The perf fixture, built once before anything is timed so the build itself
/// never lands inside a measurement.
fn database() -> &'static Path {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(fixtures::perf_database)
}

/// A terminal the size of a comfortable window.
const WIDTH: u16 = 140;
const HEIGHT: u16 = 44;

fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("terminal")
}

fn draw(terminal: &mut Terminal<TestBackend>, app: &mut App) {
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw succeeded");
}

/// Resident memory of this process, in bytes.
///
/// `ps` is what macOS offers without a C dependency; a platform that answers
/// differently returns `None` and the memory budget is skipped rather than
/// failed.
fn resident_bytes() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kilobytes| kilobytes * 1024)
}

const MEGABYTE: u64 = 1024 * 1024;

#[test]
fn the_perf_fixture_is_the_size_the_budgets_are_stated_against() {
    let db = Db::open(database()).expect("open the perf fixture read-only");
    let counts = db.counts().expect("counts");
    assert_eq!(counts.messages, fixtures::PERF_MESSAGES);
    assert_eq!(counts.chats, fixtures::PERF_CHATS);
    let (unread, chats) = db.unread_totals().expect("unread totals");
    assert!(unread > 0 && chats > 0, "the fixture has unread to draw");
}

#[test]
fn startup_reads_the_chat_list_and_the_first_frame_inside_the_budget() {
    let path = database().to_path_buf();
    let mut terminal = terminal();

    // Exactly what `main` does before the first key can be pressed, minus the
    // things that are already off the UI thread: open the database, load the
    // chat list, open the newest conversation, draw.
    let started = Instant::now();
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(path);
    draw(&mut terminal, &mut app);
    let elapsed = started.elapsed();

    eprintln!("startup: {elapsed:?}");
    assert!(app.db_error.is_none(), "the fixture opened");
    assert_eq!(app.chat_rows.len(), fixtures::PERF_CHATS as usize);
    assert!(
        !app.message_rows.is_empty(),
        "the newest conversation loaded with the chat list"
    );
    assert!(
        elapsed < budget(Duration::from_millis(300)),
        "startup took {elapsed:?} over {} messages",
        fixtures::PERF_MESSAGES
    );
}

#[test]
fn scrolling_a_deep_thread_stays_inside_a_sixtieth_of_a_second() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(database().to_path_buf());
    app.focus = Focus::Conversation;
    let mut terminal = terminal();
    draw(&mut terminal, &mut app);

    // The deepest thread in the fixture, and the one the chat list opens on:
    // half of every message in the database is in it.
    let open = app.current_chat_rowid();
    assert_eq!(open, Some(fixtures::PERF_DEEP_CHAT));

    // Warm the first frame out of the measurement — the page it draws was
    // measured by the draw above.
    let mut slowest = Duration::ZERO;
    let mut total = Duration::ZERO;
    let frames = 240;
    for frame in 0..frames {
        let action = match frame % 8 {
            0..=2 => Action::SelectPrev,
            3 => Action::PageUp,
            4..=6 => Action::SelectNext,
            _ => Action::PageDown,
        };
        let started = Instant::now();
        app.update(action);
        draw(&mut terminal, &mut app);
        let elapsed = started.elapsed();
        slowest = slowest.max(elapsed);
        total += elapsed;
    }
    let average = total / frames;
    eprintln!("frames: average {average:?}, slowest {slowest:?}");

    // Sixty frames a second is 16.6ms a frame, and a keystroke is a frame.
    assert!(
        average < budget(Duration::from_micros(16_600)),
        "a frame averaged {average:?} (slowest {slowest:?})"
    );
    // One frame in the run pages upward, which is a query; it gets the same
    // budget four times over rather than the average's.
    assert!(
        slowest < budget(Duration::from_millis(66)),
        "the slowest frame took {slowest:?}"
    );
}

#[test]
fn a_page_costs_the_same_at_the_bottom_of_a_deep_thread_as_far_above_it() {
    let db = Db::open(database()).expect("open the perf fixture read-only");

    let started = Instant::now();
    let newest = db
        .messages_before(fixtures::PERF_DEEP_CHAT, None, PAGE)
        .expect("the newest page");
    let bottom = started.elapsed();
    assert_eq!(newest.len(), PAGE);

    // Fifty thousand messages back up the same thread.
    let deep_before = newest[0].rowid - 50_000;
    let started = Instant::now();
    let older = db
        .messages_before(fixtures::PERF_DEEP_CHAT, Some(deep_before), PAGE)
        .expect("a page far above the newest");
    let top = started.elapsed();
    assert_eq!(older.len(), PAGE);

    eprintln!("page at the newest end: {bottom:?}, page 50k back: {top:?}");
    for (which, elapsed) in [("newest", bottom), ("deep", top)] {
        assert!(
            elapsed < budget(Duration::from_millis(25)),
            "the {which} page took {elapsed:?}"
        );
    }
}

#[test]
fn the_loaded_app_stays_well_under_a_hundred_and_fifty_megabytes() {
    let Some(before) = resident_bytes() else {
        return;
    };

    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(database().to_path_buf());
    app.focus = Focus::Conversation;
    let mut terminal = terminal();
    draw(&mut terminal, &mut app);

    // Page upward through the deep thread the way a long read does, so what is
    // measured is an app that has been used rather than one that has just
    // started.
    for _ in 0..40 {
        app.update(Action::PageUp);
        draw(&mut terminal, &mut app);
    }
    assert!(app.message_rows.len() > PAGE, "pages were actually loaded");

    let after = resident_bytes().expect("ps answered once already");
    // The growth rather than the total: the test binary shares its process
    // with every other test in this file, and one of them built the fixture.
    let grew = after.saturating_sub(before);
    eprintln!(
        "resident: {} MB before, {} MB after, {} MB of growth",
        before / MEGABYTE,
        after / MEGABYTE,
        grew / MEGABYTE
    );
    assert!(
        grew < 150 * MEGABYTE,
        "loading and reading {} messages grew the process by {} MB",
        fixtures::PERF_MESSAGES,
        grew / MEGABYTE
    );
}
