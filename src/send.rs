//! Outbound messages: the only path in msgs that leaves the machine.
//!
//! Everything here goes through `osascript` to Messages.app, because that is
//! the only supported way to put an iMessage on the wire. The script is built
//! once per send and handed to `osascript -e`; every value that reaches it goes
//! through [`escape`] first, so a body with quotes, backslashes, or newlines in
//! it cannot end the string literal it lives in.
//!
//! Messages.app is `launch`ed rather than `activate`d, so it starts hidden and
//! never steals focus from the terminal.
//!
//! Nothing here logs. Errors coming back from `osascript` are run
//! through [`sanitize`], which drops the quoted spans — the ones that would
//! otherwise carry a phone number or a body onto the status line.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use crate::db::Chat;

/// Longest error text kept from `osascript`.
const MAX_ERROR: usize = 120;

/// The tool that answers whether Messages.app is running.
const PGREP: &str = "/usr/bin/pgrep";

/// What Messages.app calls itself in the process table.
const MESSAGES_PROCESS: &str = "Messages";

/// Which of Messages' services a chat is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Service {
    /// Apple's own service.
    #[default]
    IMessage,
    /// A green-bubble thread: SMS, MMS, or RCS, all addressed the same way.
    Sms,
}

impl Service {
    /// Read `chat.service` / `handle.service`, which is `iMessage`, `SMS`, or
    /// `RCS`. Anything unrecognized is treated as iMessage, which is what
    /// Messages itself falls back to.
    #[must_use]
    pub fn parse(name: Option<&str>) -> Self {
        match name.map(str::trim) {
            Some(name) if name.eq_ignore_ascii_case("SMS") || name.eq_ignore_ascii_case("RCS") => {
                Self::Sms
            }
            _ => Self::IMessage,
        }
    }

    /// The AppleScript `service type` constant for this service.
    #[must_use]
    pub const fn applescript(self) -> &'static str {
        match self {
            Self::IMessage => "iMessage",
            Self::Sms => "SMS",
        }
    }
}

/// Where a message is being sent.
///
/// The `chat id` route is the exact one — it addresses the conversation the
/// user is looking at, group or not. The identifier is the fallback for the
/// case where Messages will not resolve the GUID: a one-to-one chat can still
/// be addressed by the handle on the other end of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `chat.guid`, e.g. `iMessage;-;+15550000000`.
    pub guid: Option<String>,
    /// `chat.chat_identifier`: a handle for a one-to-one chat, a group id for
    /// a group. Only used as a participant address when the chat is not a
    /// group.
    pub identifier: Option<String>,
    /// Which service to address it on.
    pub service: Service,
}

impl Target {
    /// The address of an open conversation.
    #[must_use]
    pub fn for_chat(chat: &Chat) -> Self {
        Self {
            guid: Some(chat.guid.clone()).filter(|guid| !guid.is_empty()),
            // Addressing a group by its identifier would start a new thread
            // with a chat id for a name, so only a one-to-one chat keeps one.
            identifier: if chat.is_group {
                None
            } else {
                chat.identifier
                    .clone()
                    .filter(|identifier| !identifier.is_empty())
            },
            service: Service::parse(chat.service.as_deref()),
        }
    }

    /// Whether there is any route to this conversation at all.
    #[must_use]
    pub fn is_addressable(&self) -> bool {
        self.guid.is_some() || self.identifier.is_some()
    }
}

/// Why a send did not happen. No variant carries a body or an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The conversation has no address msgs can use.
    NoTarget,
    /// `osascript` could not be run at all.
    NotAvailable,
    /// The file to attach is not there.
    NoFile,
    /// Messages refused it; the string is a sanitized first line.
    Script(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTarget => f.write_str("no address for this conversation"),
            Self::NotAvailable => f.write_str("osascript is not available"),
            Self::NoFile => f.write_str("no such file"),
            Self::Script(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for SendError {}

/// Escape `text` for an AppleScript string literal.
///
/// AppleScript literals understand `\"`, `\\`, `\n`, `\r`, and `\t` and nothing
/// else, so every other control character is dropped rather than smuggled
/// through raw — a stray `\r` inside a literal ends the line and the script
/// stops parsing.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Reduce an `osascript` error to something safe to show.
///
/// Messages quotes the thing it could not do — a chat id, a handle, sometimes
/// the body — so the quoted spans are replaced with an ellipsis before the text
/// goes anywhere near the status line.
#[must_use]
pub fn sanitize(stderr: &str) -> String {
    sanitize_or(stderr, "Messages refused the message")
}

/// [`sanitize`], with the words to fall back on when there is nothing to keep.
#[must_use]
pub fn sanitize_or(stderr: &str, fallback: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(fallback);
    let line = line
        .strip_prefix("execution error: ")
        .unwrap_or(line)
        .trim_start_matches("Messages got an error: ")
        .trim();

    let mut out = String::with_capacity(line.len());
    let mut inside = false;
    for c in line.chars() {
        match c {
            '"' | '\u{201c}' | '\u{201d}' => {
                if !inside {
                    out.push('…');
                }
                inside = !inside;
            }
            c if inside => {
                let _ = c;
            }
            c => out.push(c),
        }
    }
    let out = out.trim();
    if out.is_empty() {
        return fallback.to_string();
    }
    if out.chars().count() > MAX_ERROR {
        return out.chars().take(MAX_ERROR).collect::<String>() + "…";
    }
    out.to_string()
}

/// The script that sends `text`, or `None` when the target has no address.
///
/// Both routes are tried in order and the first one that works returns; if
/// neither does, the script raises so the caller hears about it rather than
/// silently dropping the message.
#[must_use]
pub fn text_script(target: &Target, text: &str) -> Option<String> {
    body_script(target, "payload", Some(text))
}

/// The script that sends the file at `path`.
#[must_use]
pub fn file_script(target: &Target, path: &Path) -> Option<String> {
    let literal = format!("POSIX file \"{}\"", escape(&path.to_string_lossy()));
    body_script(target, &literal, None)
}

/// Assemble a send script around whatever is being sent.
///
/// `payload` is the AppleScript expression handed to `send`, and `text` is the
/// body it needs bound first, if any.
fn body_script(target: &Target, payload: &str, text: Option<&str>) -> Option<String> {
    if !target.is_addressable() {
        return None;
    }
    let service = target.service.applescript();
    let mut script = String::from("tell application \"Messages\"\n\tlaunch\n");
    if let Some(text) = text {
        script.push_str(&format!("\tset payload to \"{}\"\n", escape(text)));
    }
    if let Some(guid) = target.guid.as_deref() {
        script.push_str(&format!(
            "\ttry\n\t\tsend {payload} to chat id \"{}\"\n\t\treturn \"sent\"\n\tend try\n",
            escape(guid)
        ));
    }
    if let Some(identifier) = target.identifier.as_deref() {
        script.push_str(&format!(
            "\ttry\n\t\tset msgsTarget to participant \"{}\" of (1st account whose service type = {service})\n\
             \t\tsend {payload} to msgsTarget\n\t\treturn \"sent\"\n\tend try\n",
            escape(identifier)
        ));
    }
    script.push_str("\terror \"msgs could not reach that conversation\"\nend tell\n");
    Some(script)
}

/// First match for `name` on `$PATH`.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Start Messages.app without bringing it to the front.
///
/// `launch` opens it hidden; `activate` — which msgs never runs — is what would
/// steal focus from the terminal.
///
/// # Errors
///
/// Returns [`SendError::NotAvailable`] when `osascript` cannot be run, and
/// [`SendError::Script`] when Messages will not start.
pub fn ensure_running() -> Result<(), SendError> {
    run("tell application \"Messages\" to launch")
}

/// Send `text` to `target`, blocking until Messages has taken it.
///
/// # Errors
///
/// Returns [`SendError::NoTarget`] when the conversation has no address,
/// [`SendError::NotAvailable`] when `osascript` is missing, and
/// [`SendError::Script`] when Messages refuses.
pub fn send_text(target: &Target, text: &str) -> Result<(), SendError> {
    let script = text_script(target, text).ok_or(SendError::NoTarget)?;
    run(&script)
}

/// Send the file at `path` to `target`, blocking until Messages has taken it.
///
/// # Errors
///
/// As [`send_text`], plus [`SendError::NoFile`] when the path is not a file.
pub fn send_file(target: &Target, path: &Path) -> Result<(), SendError> {
    if !path.is_file() {
        return Err(SendError::NoFile);
    }
    let script = file_script(target, path).ok_or(SendError::NoTarget)?;
    run(&script)
}

/// Hand one script to `osascript` and wait for it.
fn run(script: &str) -> Result<(), SendError> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| SendError::NotAvailable)?;
    if output.status.success() {
        return Ok(());
    }
    Err(SendError::Script(sanitize(&String::from_utf8_lossy(
        &output.stderr,
    ))))
}

/// Whether Messages.app is running right now, when the answer can be had.
///
/// This asks `pgrep` for a process called `Messages` and reads nothing but its
/// exit status: `0` is running, `1` is not, and anything else — no `pgrep`, a
/// signal, a sandbox that will not let it run — is `None`, which the status
/// line writes as `Messages.app unknown` rather than guessing.
///
/// Deliberately not `osascript`: asking Messages whether it is running starts
/// it, which is the opposite of a question.
#[must_use]
pub fn messages_app_running() -> Option<bool> {
    let output = Command::new(PGREP)
        .arg("-x")
        .arg(MESSAGES_PROCESS)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// A background yes/no answer, refreshed on a timer: "is Messages.app
/// running?" by default, or any other `fn() -> Option<bool>` handed to
/// [`Presence::watching_with`] (the system appearance, for one).
///
/// The probe costs a process, so it is off by default — nothing under `tests/`
/// ever spawns one — and one launch's worth of it runs at a time: [`poll`]
/// starts a thread when the interval is up and picks the answer off a channel
/// whenever it lands, so the event loop never waits on `pgrep`.
///
/// [`poll`]: Presence::poll
#[derive(Debug)]
pub struct Presence {
    /// The question, asked on its own thread.
    ask: fn() -> Option<bool>,
    /// The answer to the probe that is in flight, if one is.
    rx: Option<Receiver<Option<bool>>>,
    /// When the next probe may start. `None` while one is in flight.
    due: Option<std::time::Instant>,
    /// How long between probes.
    interval: std::time::Duration,
}

/// How often [`Presence`] asks again.
pub const PRESENCE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

impl Presence {
    /// A probe that never asks anything. This is the default, and what the
    /// tests get.
    #[must_use]
    pub const fn off() -> Self {
        Self {
            ask: messages_app_running,
            rx: None,
            due: None,
            interval: PRESENCE_INTERVAL,
        }
    }

    /// A probe that asks now and then every [`PRESENCE_INTERVAL`].
    #[must_use]
    pub fn watching() -> Self {
        Self::watching_with(messages_app_running)
    }

    /// A probe that asks `ask` now and then every [`PRESENCE_INTERVAL`].
    #[must_use]
    pub fn watching_with(ask: fn() -> Option<bool>) -> Self {
        Self {
            ask,
            rx: None,
            due: Some(std::time::Instant::now()),
            interval: PRESENCE_INTERVAL,
        }
    }

    /// Whether this probe will ever ask anything.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.rx.is_some() || self.due.is_some()
    }

    /// Collect an answer if one has arrived, and start the next probe if it is
    /// time. Never blocks.
    ///
    /// The outer `Option` is "there is news"; the inner one is the answer,
    /// where `None` means the question could not be asked.
    pub fn poll(&mut self) -> Option<Option<bool>> {
        if let Some(rx) = self.rx.as_ref() {
            match rx.try_recv() {
                Ok(answer) => {
                    self.rx = None;
                    self.due = Some(std::time::Instant::now() + self.interval);
                    return Some(answer);
                }
                Err(TryRecvError::Empty) => return None,
                // The thread died without answering; ask again on the timer.
                Err(TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.due = Some(std::time::Instant::now() + self.interval);
                    return None;
                }
            }
        }
        if self.due.is_some_and(|due| due <= std::time::Instant::now()) {
            self.due = None;
            let (tx, rx) = channel();
            self.rx = Some(rx);
            let ask = self.ask;
            std::thread::spawn(move || {
                let _ = tx.send(ask());
            });
        }
        None
    }
}

impl Default for Presence {
    fn default() -> Self {
        Self::off()
    }
}

/// Take a lock, stepping over a panic in another thread rather than adding one.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The prefix every optimistic echo's GUID carries, so a row that is not in
/// `chat.db` yet can always be told from one that is.
pub const PENDING_PREFIX: &str = "msgs-pending:";

/// How far along an optimistic echo is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Handed to Messages, not answered for yet.
    Sending,
    /// Messages took it; `chat.db` has not shown it yet.
    Sent,
    /// Messages refused it; the string is a sanitized reason.
    Failed(String),
}

/// A message drawn in the transcript before `chat.db` knows about it.
///
/// The echo goes up the moment the user presses Enter, so the conversation
/// answers immediately rather than after the second `osascript` takes to
/// return. It comes back down when the real row is read out of the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// Ties the echo to the [`Reply`] that will answer for it.
    pub id: u64,
    /// `chat.ROWID` it was sent to.
    pub chat_rowid: i64,
    /// A synthetic GUID, prefixed with [`PENDING_PREFIX`].
    pub guid: String,
    /// What the block shows: the typed body, or the file's name.
    pub text: String,
    /// Whether it is a file rather than a typed message.
    pub is_file: bool,
    /// Raw Messages timestamp of when the user pressed Enter.
    pub date: i64,
    /// How far along it is.
    pub state: Delivery,
}

impl Pending {
    /// A fresh echo, in [`Delivery::Sending`].
    #[must_use]
    pub fn new(id: u64, chat_rowid: i64, text: String, is_file: bool, date: i64) -> Self {
        Self {
            id,
            chat_rowid,
            guid: format!("{PENDING_PREFIX}{id}"),
            text,
            is_file,
            date,
            state: Delivery::Sending,
        }
    }

    /// Whether `message`, freshly read out of `chat.db`, is this echo arriving
    /// for real.
    ///
    /// Matching is on what the database can be trusted to reproduce: the
    /// message is one of yours, it is not a group event, and either the body is
    /// the body that was typed or — for an attachment, whose body Messages
    /// leaves as a placeholder — it carries a file. The caller checks that it
    /// is recent enough.
    #[must_use]
    pub fn matches(&self, message: &crate::db::Message) -> bool {
        if !message.is_from_me || message.is_announcement() {
            return false;
        }
        if self.is_file {
            return !message.attachments.is_empty();
        }
        message.text.as_deref().unwrap_or_default().trim() == self.text.trim()
    }
}

/// What is being sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// A typed message.
    Text(String),
    /// A file from disk.
    File(PathBuf),
}

/// The answer to one send, matched to its optimistic echo by `id`.
#[derive(Debug)]
pub struct Reply {
    /// The id the caller gave [`Outbox::send`].
    pub id: u64,
    /// Whether Messages took it.
    pub result: Result<(), SendError>,
}

/// Sends in flight.
///
/// `osascript` takes the better part of a second to answer, which is far too
/// long to hold the event loop, so each send runs on its own thread and posts
/// its answer back down a channel that [`Outbox::drain`] picks up between
/// frames.
#[derive(Debug)]
pub struct Outbox {
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
    mode: Mode,
}

/// What an outbox does with a send.
#[derive(Debug)]
enum Mode {
    /// Run it, for real, through Messages.app.
    Osascript,
    /// Record it and send nothing.
    Inert(Mutex<Vec<(u64, Target, Outgoing)>>),
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Outbox {
    /// An outbox with nothing in flight.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            mode: Mode::Osascript,
        }
    }

    /// An outbox that records what it is asked to send and puts nothing on the
    /// wire.
    ///
    /// This is what the tests drive the send path with: the whole path — the
    /// echo, the reply, the reconciliation — runs, and no message leaves the
    /// machine. [`Outbox::recorded`] reads back what was asked for and
    /// [`Outbox::answer`] plays the reply that `osascript` would have given.
    #[must_use]
    pub fn inert() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            mode: Mode::Inert(Mutex::new(Vec::new())),
        }
    }

    /// Start one send in the background. The answer arrives from
    /// [`Outbox::drain`] carrying the same `id`.
    pub fn send(&self, id: u64, target: Target, what: Outgoing) {
        if let Mode::Inert(log) = &self.mode {
            lock(log).push((id, target, what));
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match what {
                Outgoing::Text(text) => send_text(&target, &text),
                Outgoing::File(path) => send_file(&target, &path),
            };
            // The receiver is gone only when the app is on its way out.
            let _ = tx.send(Reply { id, result });
        });
    }

    /// What an [`Outbox::inert`] outbox has been asked to send. Always empty
    /// for a real one.
    #[must_use]
    pub fn recorded(&self) -> Vec<(u64, Target, Outgoing)> {
        match &self.mode {
            Mode::Osascript => Vec::new(),
            Mode::Inert(log) => lock(log).clone(),
        }
    }

    /// Post an answer for `id`, as the send thread would have.
    pub fn answer(&self, id: u64, result: Result<(), SendError>) {
        let _ = self.tx.send(Reply { id, result });
    }

    /// Every answer that has arrived since the last call. Never blocks.
    pub fn drain(&self) -> Vec<Reply> {
        let mut replies = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(reply) => replies.push(reply),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return replies,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_presence_probe_that_is_off_never_asks_and_never_answers() {
        let mut off = Presence::off();
        assert!(!off.is_on());
        assert_eq!(off.poll(), None);
        assert_eq!(off.poll(), None);
        assert!(!off.is_on());
    }

    #[test]
    fn a_watching_probe_asks_once_and_answers_once() {
        let mut probe = Presence::watching();
        assert!(probe.is_on());
        // The first poll starts the thread; the answer lands on a later one.
        assert_eq!(probe.poll(), None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let answer = loop {
            if let Some(answer) = probe.poll() {
                break answer;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the probe never answered"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // Whether Messages is running is this machine's business; that an
        // answer came back at all is what is being tested.
        assert!(answer.is_some() || answer.is_none());
        // And it does not ask again straight away.
        assert_eq!(probe.poll(), None);
        assert!(probe.is_on());
    }

    use super::*;

    fn direct() -> Target {
        Target {
            guid: Some("iMessage;-;+15550000000".to_string()),
            identifier: Some("+15550000000".to_string()),
            service: Service::IMessage,
        }
    }

    #[test]
    fn escaping_closes_no_string_literal() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape("back\\slash"), "back\\\\slash");
        assert_eq!(escape("two\nlines"), "two\\nlines");
        assert_eq!(escape("tab\there"), "tab\\there");
        assert_eq!(escape("crlf\r\n"), "crlf\\r\\n");
        // A raw control character would end the literal in the middle of a
        // line, so it is dropped rather than passed on.
        assert_eq!(escape("bell\u{7}gone"), "bellgone");
        // Emoji and accents are ordinary characters to AppleScript.
        assert_eq!(escape("héllo 🌊"), "héllo 🌊");
    }

    #[test]
    fn an_escaped_body_leaves_no_unescaped_quote_in_the_script() {
        let hostile = "\"; do shell script \"rm -rf /\"; set x to \"";
        let script = text_script(&direct(), hostile).expect("script");
        let payload = script
            .lines()
            .find(|line| line.trim_start().starts_with("set payload to"))
            .expect("payload line");
        let literal = payload.trim_start().trim_start_matches("set payload to ");
        // Exactly two unescaped quotes: the ones opening and closing it.
        let mut bare = 0;
        let mut escaped = false;
        for c in literal.chars() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => bare += 1,
                _ => {}
            }
        }
        assert_eq!(bare, 2, "literal {literal}");
    }

    #[test]
    fn a_text_script_launches_hidden_and_tries_both_routes() {
        let script = text_script(&direct(), "on my way").expect("script");
        assert!(script.contains("tell application \"Messages\""));
        assert!(script.contains("launch"));
        assert!(
            !script.contains("activate"),
            "sending must never raise Messages"
        );
        assert!(script.contains("send payload to chat id"));
        assert!(script.contains("service type = iMessage"));
        assert!(script.trim_end().ends_with("end tell"));
    }

    #[test]
    fn a_group_is_addressed_by_guid_only() {
        let group = Target {
            guid: Some("iMessage;+;chat123".to_string()),
            identifier: None,
            service: Service::IMessage,
        };
        let script = text_script(&group, "hi").expect("script");
        assert!(script.contains("chat id \"iMessage;+;chat123\""));
        assert!(!script.contains("participant"));
    }

    #[test]
    fn an_sms_thread_asks_for_the_sms_service() {
        let sms = Target {
            guid: None,
            identifier: Some("+15550000000".to_string()),
            service: Service::Sms,
        };
        let script = text_script(&sms, "hi").expect("script");
        assert!(script.contains("service type = SMS"));
        assert!(!script.contains("chat id"));
    }

    #[test]
    fn a_target_with_no_address_produces_no_script() {
        let nowhere = Target {
            guid: None,
            identifier: None,
            service: Service::IMessage,
        };
        assert!(!nowhere.is_addressable());
        assert!(text_script(&nowhere, "hi").is_none());
        assert_eq!(send_text(&nowhere, "hi"), Err(SendError::NoTarget));
    }

    #[test]
    fn a_file_script_sends_a_posix_file() {
        let script = file_script(&direct(), Path::new("/tmp/a \"quoted\".png")).expect("script");
        assert!(script.contains("POSIX file \"/tmp/a \\\"quoted\\\".png\""));
        assert!(!script.contains("set payload to"));
    }

    #[test]
    fn missing_files_are_refused_before_any_script_runs() {
        let path = Path::new("/nonexistent/msgs-test-attachment.png");
        assert_eq!(send_file(&direct(), path), Err(SendError::NoFile));
    }

    #[test]
    fn service_names_map_onto_applescript_constants() {
        assert_eq!(Service::parse(Some("iMessage")), Service::IMessage);
        assert_eq!(Service::parse(Some("SMS")), Service::Sms);
        assert_eq!(Service::parse(Some("rcs")), Service::Sms);
        assert_eq!(Service::parse(None), Service::IMessage);
        assert_eq!(Service::IMessage.applescript(), "iMessage");
        assert_eq!(Service::Sms.applescript(), "SMS");
    }

    #[test]
    fn errors_keep_the_reason_and_drop_the_quoted_parts() {
        let reason = sanitize(
            "execution error: Messages got an error: Can’t get chat id \"iMessage;-;+15550000000\". (-1728)",
        );
        assert!(reason.contains("Can’t get chat id"));
        assert!(!reason.contains("5550000000"), "address must not survive");
        assert!(reason.contains('…'));

        assert_eq!(sanitize(""), "Messages refused the message");
        assert_eq!(sanitize("   \n  "), "Messages refused the message");
        assert!(sanitize(&"x".repeat(400)).chars().count() <= MAX_ERROR + 1);
    }

    #[test]
    fn an_outbox_hands_answers_back_by_id() {
        let outbox = Outbox::new();
        assert!(outbox.drain().is_empty());
        // A target with no address fails without running anything.
        let nowhere = Target {
            guid: None,
            identifier: None,
            service: Service::IMessage,
        };
        outbox.send(7, nowhere, Outgoing::Text("hi".to_string()));
        let reply = loop {
            if let Some(reply) = outbox.drain().pop() {
                break reply;
            }
            std::thread::yield_now();
        };
        assert_eq!(reply.id, 7);
        assert_eq!(reply.result, Err(SendError::NoTarget));
    }
}
