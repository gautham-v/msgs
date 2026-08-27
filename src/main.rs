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
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, event, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use msgs::app::App;
use msgs::config::Config;
use msgs::db::{Db, Source};
use msgs::{config, default_db_path, keymap, ui};

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

    /// Print a readiness report and exit without starting the UI.
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (config, warnings) = Config::load(cli.config.as_deref());

    if cli.check {
        return check(&cli, &warnings);
    }

    let mut app = App::new(config, warnings);
    if cli.no_mouse {
        app.mouse_enabled = false;
    }
    // Read-only, and never fatal: a failure becomes the full-screen surface
    // that tells the reader how to grant Full Disk Access.
    app.open_db(cli.db.clone().unwrap_or_else(default_db_path));

    install_panic_hook();
    let mut terminal = setup_terminal(app.mouse_enabled)?;
    let result = run(&mut terminal, &mut app);
    restore_terminal();
    result
}

/// The event loop: draw, wait for input, apply, repeat.
fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(TICK)? {
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

    let messages_app = [
        "/System/Applications/Messages.app",
        "/Applications/Messages.app",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).exists());
    row(
        "Messages.app",
        messages_app.unwrap_or("not found — sending will not work"),
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
        &which("imsg").map_or_else(
            || "not on PATH — tapbacks will be unavailable".to_string(),
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
        "config",
        &format!("{} — {config_state}", config_path.display()),
    );

    row(
        "terminal",
        &std::env::var("TERM_PROGRAM")
            .unwrap_or_else(|_| std::env::var("TERM").unwrap_or_else(|_| "unknown".to_string())),
    );

    for warning in warnings {
        row("warning", warning);
    }
    Ok(())
}

fn row(label: &str, value: &str) {
    println!("  {label:<14} {value}");
}

/// First match for `name` on `$PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
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
        let cli = Cli::parse_from(["msgs", "--db", "/tmp/copy.db", "--no-mouse"]);
        assert_eq!(
            cli.db.as_deref(),
            Some(std::path::Path::new("/tmp/copy.db"))
        );
        assert!(cli.no_mouse);
        assert!(!cli.check);

        let cli = Cli::parse_from(["msgs"]);
        assert!(cli.db.is_none());
        assert!(!cli.no_mouse);
    }
}
