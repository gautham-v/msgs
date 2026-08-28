//! `msgs` — a terminal client for iMessage on macOS.
//!
//! This file owns the process: argument parsing, putting the terminal into and
//! out of raw alt-screen mode, and the event loop. Everything it does to the
//! terminal is undone on exit, on error, and on panic.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
};
use crossterm::{cursor, event, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use msgs::app::{App, WatcherStatus};
use msgs::config::Config;
use msgs::contacts::Contacts;
use msgs::db::{Db, Source};
use msgs::pins::Pins;
use msgs::seen::Seen;
use msgs::send::which;
use msgs::{config, contacts, default_db_path, keymap, media, search, send, theme, ui};

/// How long the loop waits for input before waking up to expire toasts.
const TICK: Duration = Duration::from_millis(250);

/// A terminal client for iMessage on macOS.
#[derive(Debug, Parser)]
#[command(name = "msgs", version, about, long_about = None)]
struct Cli {
    /// Read this database instead of ~/Library/Messages/chat.db.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Read this config file instead of ~/.config/msgs/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Do not capture the mouse.
    #[arg(long)]
    no_mouse: bool,

    /// Use this palette instead of the config's: dark, light, system, or terminal.
    #[arg(long, value_name = "NAME", value_parser = theme::BASES)]
    theme: Option<String>,

    /// Print a readiness report and exit without starting the UI.
    #[arg(long)]
    check: bool,

    /// Do not build or use the full-text message index.
    #[arg(long)]
    no_index: bool,

    /// Do not draw pictures inline; show every attachment as a chip.
    #[arg(long)]
    no_images: bool,

    /// Do not read Contacts; show phone numbers and addresses instead of names.
    #[arg(long)]
    no_contacts: bool,

    /// Do not read Messages.app's pinned conversations; list every chat by recency.
    #[arg(long)]
    no_pins: bool,

    /// Mask phone numbers and addresses on screen, for a demo or a screenshot.
    /// Names from Contacts and message bodies still show.
    #[arg(long)]
    redact: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (mut config, warnings) = Config::load(cli.config.as_deref());
    if let Some(name) = &cli.theme {
        config.theme.insert("base".to_string(), name.clone());
    }

    if cli.check {
        return check(&cli, &warnings);
    }

    if cli.redact {
        msgs::db::handle::set_redact(true);
    }

    let mut app = App::new(config, warnings);
    if cli.no_mouse {
        app.mouse_enabled = false;
    }
    // Read-only, and never fatal: a failure becomes the full-screen surface
    // that tells the reader how to grant Full Disk Access.
    app.open_db(cli.db.clone().unwrap_or_else(default_db_path));

    // Names for the handles the database just handed over. Read-only, cached,
    // and never fatal: a Mac that will not open its Contacts stores says so on
    // the status line and shows numbers.
    if !cli.no_contacts && app.config.contacts {
        app.enable_contacts_from_stores();
    }

    // The conversations pinned in Messages.app, out of its own preference file.
    // Read-only, never written, and never fatal: a Mac that has pinned nothing
    // simply has no such file, and the list is ordered by recency alone.
    if !cli.no_pins && app.config.pins {
        app.enable_pins_from_preferences();
    }

    // Whether Messages.app is up, asked on a timer on its own thread. The
    // status line says `unknown` until the first answer lands.
    app.enable_presence();

    // Whether macOS is in dark mode, asked the same way while the theme
    // follows the system.
    app.enable_appearance();

    // Which chats msgs itself has already put in front of you. Its own small
    // file beside the index; `chat.db` and Messages.app's badge are untouched.
    if let Some(path) = Seen::default_path() {
        app.enable_seen(&path);
    }

    // The message index is msgs's own file, never `chat.db`. Building it runs
    // on its own thread and reports onto the status line.
    if !cli.no_index {
        match search::default_index_path() {
            Ok(path) => app.enable_search(&path),
            Err(err) => app
                .status
                .warnings
                .push(format!("search: {}", err.summary())),
        }
    }

    install_panic_hook();
    let mut terminal = setup_terminal(app.mouse_enabled)?;
    // The terminal is asked what it draws with, and what it can draw, after
    // the alternate screen is up and before a single key is read, because the
    // answers come back on stdin and would otherwise land in the event loop
    // as keystrokes.
    app.set_terminal_colors(theme::query_terminal(Duration::from_millis(150)));
    if !cli.no_images && app.config.images {
        app.enable_images(media::Images::detect());
        let _ = terminal.clear();
    }
    let result = run(&mut terminal, &mut app);
    restore_terminal();
    result
}

/// The event loop: draw, wait for input, apply, repeat.
fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        // A synchronized update asks the terminal to hold the frame until it
        // is complete, so a resize's clear-and-redraw lands as one paint
        // instead of a blank screen followed by the new layout. Terminals
        // that do not understand the sequence ignore it.
        let _ = execute!(io::stdout(), BeginSynchronizedUpdate);
        let drawn = terminal.draw(|frame| ui::draw(frame, app));
        let _ = execute!(io::stdout(), EndSynchronizedUpdate);
        drawn?;

        // Drain everything queued before drawing again: a drag of the window
        // edge arrives as a burst of resize events, and one frame at the end
        // of the burst is the whole point of it.
        let mut waited = false;
        while event::poll(if waited { Duration::ZERO } else { TICK })? {
            waited = true;
            match event::read()? {
                // Terminals with the kitty keyboard protocol also report key
                // releases and repeats; only presses act.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = keymap::resolve(key, app.key_focus()) {
                        app.update(action);
                    }
                }
                Event::Mouse(mouse) => app.on_mouse(mouse),
                _ => {}
            }
            if app.should_quit {
                break;
            }
        }
        app.tick();

        if app.should_quit {
            return Ok(());
        }
    }
}

// The terminal modes we turned on, so the restore path can undo exactly those.
static MOUSE_CAPTURED: AtomicBool = AtomicBool::new(false);
static KEYBOARD_FLAGS_PUSHED: AtomicBool = AtomicBool::new(false);

fn setup_terminal(mouse: bool) -> Result<Terminal<CrosstermBackend<Stdout>>> {
    // Ask before raw mode so the query and its reply do not race the UI. A
    // terminal that speaks the kitty protocol can tell Shift+Enter from Enter,
    // which the composer needs.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);

    enable_raw_mode().context("failed to put the terminal into raw mode")?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen).context("failed to enter the alternate screen")?;

    if mouse && execute!(out, EnableMouseCapture).is_ok() {
        MOUSE_CAPTURED.store(true, Ordering::SeqCst);
    }
    if enhanced
        && execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KEYBOARD_FLAGS_PUSHED.store(true, Ordering::SeqCst);
    }

    Terminal::new(CrosstermBackend::new(out)).context("failed to start the terminal backend")
}

/// Undo everything [`setup_terminal`] did. Safe to call more than once, and
/// safe to call when setup only partly succeeded.
fn restore_terminal() {
    let mut out = io::stdout();
    if KEYBOARD_FLAGS_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if MOUSE_CAPTURED.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
}

/// Restore the terminal before the default hook prints the panic message, so
/// the message lands on a usable screen instead of inside the alt screen.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// `--check`: a doctor-style report. Read-only, and it prints paths only —
/// never anything out of the database.
fn check(cli: &Cli, warnings: &[String]) -> Result<()> {
    println!("msgs {}", env!("CARGO_PKG_VERSION"));

    let path = cli.db.clone().unwrap_or_else(default_db_path);

    // The one answer everything else depends on, so it goes first. It is
    // asked of the real database even when `--db` points somewhere else,
    // because that is what Full Disk Access actually gates.
    row("full disk access", &full_disk_access());

    match Db::open(&path) {
        Ok(db) => {
            let where_from = match db.source() {
                Source::Live => "readable",
                Source::Copy => "readable via a scratch copy (the live file is locked)",
            };
            row("chat.db", &format!("{} — {where_from}", path.display()));
            // Row counts only. Nothing here reads a message body or a handle.
            match db.counts() {
                Ok(counts) => row(
                    "rows",
                    &format!(
                        "{} chats · {} messages · {} handles · {} attachments",
                        counts.chats, counts.messages, counts.handles, counts.attachments
                    ),
                ),
                Err(err) => row("rows", &format!("unavailable — {}", err.summary())),
            }
            match db.unread_totals() {
                Ok((total, chats)) => row("unread", &format!("{total} in {chats} chats")),
                Err(err) => row("unread", &format!("unavailable — {}", err.summary())),
            }
            // Counts only; the file holds chat row numbers and nothing else.
            match Seen::default_path() {
                Some(state) => {
                    let seen = Seen::load(&state, &path);
                    row(
                        "read state",
                        &format!(
                            "{} — {} chats marked seen here (Messages.app's own badge is its own)",
                            state.display(),
                            seen.marked()
                        ),
                    );
                }
                None => row("read state", "unavailable — no home directory"),
            }
        }
        Err(err) => {
            row(
                "chat.db",
                &format!("{} — {}", path.display(), err.summary()),
            );
            if let Some(hint) = err.hint() {
                for line in hint.lines() {
                    row("", line.trim());
                }
            }
        }
    }

    // Read-only: this starts a filesystem watcher on the directory and asks it
    // nothing about the contents.
    let watcher = msgs::watch::Watcher::start(&path);
    row(
        "live updates",
        match watcher.status() {
            WatcherStatus::Watching => "watching chat.db and its WAL",
            WatcherStatus::Polling => "no file watcher — polling every 2s instead",
            WatcherStatus::Off => "off",
        },
    );
    drop(watcher);

    let messages_app = [
        "/System/Applications/Messages.app",
        "/Applications/Messages.app",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).exists());
    row(
        "Messages.app",
        &match messages_app {
            Some(app) => format!("{app} — {}", running_label()),
            None => "not found — sending will not work".to_string(),
        },
    );

    row(
        "osascript",
        &which("osascript").map_or_else(
            || "not on PATH — sending will not work".to_string(),
            |path| path.display().to_string(),
        ),
    );

    row(
        "imsg",
        &send::imsg_path().map_or_else(
            || format!("not on PATH — no tapbacks; {}", send::IMSG_INSTALL),
            |path| path.display().to_string(),
        ),
    );

    let config_path = cli
        .config
        .clone()
        .or_else(config::default_path)
        .unwrap_or_default();
    let config_state = if config_path.exists() {
        "loaded"
    } else {
        "not present (defaults in use)"
    };
    row(
        "search index",
        &match search::default_index_path() {
            Ok(path) => {
                let state = if path.exists() {
                    "built"
                } else {
                    "not built yet — the first launch builds it"
                };
                format!("{} — {state}", path.display())
            }
            Err(err) => err.summary(),
        },
    );

    // Counts and paths only: no name and no number reaches this report.
    row(
        "contacts",
        &if cli.no_contacts {
            "off (--no-contacts)".to_string()
        } else {
            let stores = contacts::default_store_dir()
                .map(|dir| contacts::store_paths(&dir))
                .unwrap_or_default();
            let cache = contacts::default_cache_path()
                .map_or_else(|| "no cache".to_string(), |path| path.display().to_string());
            format!(
                "{} stores · {} · {}",
                stores.len(),
                Contacts::load().status().summary(),
                cache
            )
        },
    );

    // A path and a count. Which chats are pinned, and who they are with, stays
    // out of the report the way every other address does.
    row(
        "pins",
        &if cli.no_pins {
            "off (--no-pins)".to_string()
        } else {
            Pins::default_path().map_or_else(
                || "unavailable — no home directory".to_string(),
                |path| {
                    format!(
                        "{} — {}",
                        path.display(),
                        Pins::load(&path).status().summary()
                    )
                },
            )
        },
    );

    row(
        "config",
        &format!("{} — {config_state}", config_path.display()),
    );

    row(
        "terminal",
        &std::env::var("TERM_PROGRAM")
            .unwrap_or_else(|_| std::env::var("TERM").unwrap_or_else(|_| "unknown".to_string())),
    );

    // `--check` prints outside the alternate screen, so the capability query
    // would leave its reply on the shell's line. Guess from the environment
    // instead, and say that is what this is.
    row(
        "inline images",
        &if cli.no_images {
            "off (--no-images)".to_string()
        } else {
            format!("{} — guessed from the environment", guessed_images())
        },
    );

    row(
        "sips",
        &which("sips").map_or_else(
            || "not on PATH — HEIC photos will stay chips".to_string(),
            |path| path.display().to_string(),
        ),
    );

    row(
        "qlmanage",
        &which("qlmanage").map_or_else(
            || "not on PATH — videos stay chips".to_string(),
            |path| path.display().to_string(),
        ),
    );

    for warning in warnings {
        row("warning", warning);
    }
    Ok(())
}

/// Whether this process can read the real `chat.db`, which is what Full Disk
/// Access grants. Nothing is read out of the file — only whether it opens.
fn full_disk_access() -> String {
    let real = default_db_path();
    if !real.exists() {
        return format!(
            "cannot tell — no database at {} yet",
            msgs::ui::home_relative(&real)
        );
    }
    match std::fs::File::open(&real) {
        Ok(_) => "granted".to_string(),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            "NOT granted — System Settings → Privacy & Security → Full Disk Access".to_string()
        }
        Err(err) => format!("cannot tell — {}", err.kind()),
    }
}

/// Whether Messages.app is running, in the words the status line uses.
fn running_label() -> &'static str {
    match send::messages_app_running() {
        Some(true) => "running",
        Some(false) => "not running",
        None => "cannot tell whether it is running",
    }
}

/// What `--check` says about inline images, without querying the terminal.
///
/// The real answer comes from a control sequence the terminal replies to, and
/// asking for it here would print the reply onto the user's shell.
fn guessed_images() -> &'static str {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    let kitty = matches!(program.as_str(), "ghostty" | "WezTerm")
        || term.contains("kitty")
        || term.contains("ghostty")
        || std::env::var_os("KITTY_WINDOW_ID").is_some();
    if kitty {
        media::Backend::Kitty.label()
    } else if program == "iTerm.app" {
        media::Backend::Iterm2.label()
    } else {
        media::Backend::Halfblocks.label()
    }
}

fn row(label: &str, value: &str) {
    println!("  {label:<16} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn flags_parse() {
        let cli = Cli::parse_from([
            "msgs",
            "--db",
            "/tmp/copy.db",
            "--no-mouse",
            "--no-index",
            "--no-images",
        ]);
        assert_eq!(
            cli.db.as_deref(),
            Some(std::path::Path::new("/tmp/copy.db"))
        );
        assert!(cli.no_mouse);
        assert!(cli.no_index);
        assert!(cli.no_images);
        assert!(!cli.no_contacts);
        assert!(!cli.no_pins);
        assert!(!cli.check);

        let cli = Cli::parse_from(["msgs"]);
        assert!(cli.db.is_none());
        assert!(!cli.no_mouse);
        assert!(!cli.no_index);
        assert!(!cli.no_images);
        assert!(!cli.no_contacts);
        assert!(!cli.no_pins);

        let cli = Cli::parse_from(["msgs", "--no-pins"]);
        assert!(cli.no_pins);
    }
}
