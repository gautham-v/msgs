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
//! Tapbacks are the one thing Messages will not take from AppleScript, so they
//! go out through [`imsg`](https://github.com/steipete/imsg) instead. `imsg`
//! has two routes and msgs tries both: `imsg tapback` reaches any message by
//! its GUID but needs the IMCore bridge, which will not load while System
//! Integrity Protection is on, and `imsg react` drives Messages' own UI, which
//! works with SIP on but can only reach the newest incoming message of a
//! conversation. Neither is required: without `imsg` on `$PATH` the picker
//! explains how to install it and nothing is sent.
//!
//! Nothing here logs. Errors coming back from `osascript` or `imsg` are run
//! through [`sanitize`], which drops the quoted spans — the ones that would
//! otherwise carry a phone number or a body onto the status line.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use crate::db::{Chat, Tapback, TapbackAction, TapbackKind};

/// Longest error text kept from `osascript`.
const MAX_ERROR: usize = 120;

/// The helper tapbacks are sent with, looked for on `$PATH`.
pub const IMSG: &str = "imsg";

/// What to tell somebody who does not have [`IMSG`].
pub const IMSG_INSTALL: &str = "brew install steipete/tap/imsg";

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
    /// `imsg`, which tapbacks go out through, is not on `$PATH`.
    NoHelper,
    /// Messages refused it; the string is a sanitized first line.
    Script(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTarget => f.write_str("no address for this conversation"),
            Self::NotAvailable => f.write_str("osascript is not available"),
            Self::NoFile => f.write_str("no such file"),
            Self::NoHelper => write!(f, "{IMSG} is not on PATH — {IMSG_INSTALL}"),
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

/// A tapback, as one of the six Messages has always had.
///
/// Messages can also carry an arbitrary emoji as a reaction, and msgs reads
/// those — [`TapbackKind::Emoji`] — but neither `imsg` route can put one on the
/// wire, so the picker offers the six that can be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Heart.
    Love,
    /// Thumbs up.
    Like,
    /// Thumbs down.
    Dislike,
    /// Laughing.
    Laugh,
    /// Exclamation marks.
    Emphasize,
    /// Question marks.
    Question,
}

/// The reactions the picker offers, in the order Messages shows them.
pub const REACTIONS: [Reaction; 6] = [
    Reaction::Love,
    Reaction::Like,
    Reaction::Dislike,
    Reaction::Laugh,
    Reaction::Emphasize,
    Reaction::Question,
];

impl Reaction {
    /// The emoji the picker draws.
    ///
    /// The same six glyphs [`TapbackKind::glyph`] draws for the rows that come
    /// back out of the database, which a test below holds them to.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Love => "❤️",
            Self::Like => "👍",
            Self::Dislike => "👎",
            Self::Laugh => "😂",
            Self::Emphasize => "‼️",
            Self::Question => "❓",
        }
    }

    /// The name `imsg tapback --kind` takes, which is also what the picker
    /// writes under the chosen glyph.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Love => "love",
            Self::Like => "like",
            Self::Dislike => "dislike",
            Self::Laugh => "laugh",
            Self::Emphasize => "emphasize",
            Self::Question => "question",
        }
    }

    /// The name `imsg react --reaction` takes, which is [`Reaction::label`]
    /// for five of the six and `emphasis` for the one that differs.
    #[must_use]
    pub const fn react_name(self) -> &'static str {
        match self {
            Self::Emphasize => "emphasis",
            other => other.label(),
        }
    }

    /// How the database records this reaction, so an optimistic chip and the
    /// row that later arrives for it are the same thing.
    #[must_use]
    pub const fn kind(self) -> TapbackKind {
        match self {
            Self::Love => TapbackKind::Loved,
            Self::Like => TapbackKind::Liked,
            Self::Dislike => TapbackKind::Disliked,
            Self::Laugh => TapbackKind::Laughed,
            Self::Emphasize => TapbackKind::Emphasized,
            Self::Question => TapbackKind::Questioned,
        }
    }

    /// Which reaction a database row carries, when it is one of the six.
    #[must_use]
    pub fn from_kind(kind: &TapbackKind) -> Option<Self> {
        REACTIONS
            .into_iter()
            .find(|reaction| &reaction.kind() == kind)
    }
}

/// First match for `name` on `$PATH`.
#[must_use]
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Where `imsg` is, if it is installed at all.
#[must_use]
pub fn imsg_path() -> Option<PathBuf> {
    which(IMSG)
}

/// The `--chat` value `imsg` wants: the conversation's own GUID, or one built
/// from the address of a one-to-one thread that has none.
#[must_use]
pub fn chat_argument(target: &Target) -> Option<String> {
    if let Some(guid) = target.guid.as_deref().filter(|guid| !guid.is_empty()) {
        return Some(guid.to_string());
    }
    let identifier = target.identifier.as_deref()?;
    Some(format!("{};-;{identifier}", target.service.applescript()))
}

/// The whole `imsg` invocation for one reaction, or `None` when the
/// conversation has no address `imsg` could take.
///
/// Split out from [`send_tapback`] so the arguments can be checked without
/// running anything.
#[must_use]
pub fn tapback_args(
    target: &Target,
    message_guid: &str,
    part: usize,
    reaction: Reaction,
    remove: bool,
) -> Option<Vec<String>> {
    let chat = chat_argument(target)?;
    let mut args = vec![
        "tapback".to_string(),
        "--chat".to_string(),
        chat,
        "--message".to_string(),
        message_guid.to_string(),
        "--kind".to_string(),
        reaction.label().to_string(),
        "--part".to_string(),
        part.to_string(),
    ];
    if remove {
        args.push("--remove".to_string());
    }
    Some(args)
}

/// How `imsg react` could reach a message, for the machines where the bridge
/// `imsg tapback` needs will not load.
///
/// That route addresses a conversation rather than a message and always lands
/// on its newest incoming one, so it is only ever offered for the message that
/// happens to be exactly that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactFallback {
    /// `chat.ROWID`, which is how `imsg react` names a conversation.
    pub chat_rowid: i64,
    /// The database msgs is reading, so a rowid means the same thing to
    /// `imsg` as it does here.
    pub db: PathBuf,
}

/// The `imsg react` invocation for one reaction.
#[must_use]
pub fn react_args(fallback: &ReactFallback, reaction: Reaction) -> Vec<String> {
    vec![
        "react".to_string(),
        "--db".to_string(),
        fallback.db.to_string_lossy().into_owned(),
        "--chat-id".to_string(),
        fallback.chat_rowid.to_string(),
        "--reaction".to_string(),
        reaction.react_name().to_string(),
    ]
}

/// Put one reaction on one message, blocking until `imsg` answers.
///
/// `imsg tapback` is tried first because it reaches the message by GUID. When
/// it will not run — the bridge it needs does not load with System Integrity
/// Protection on — and the target is the one message `imsg react` can address,
/// that route is tried too, so a reaction still goes out on a stock Mac.
///
/// # Errors
///
/// Returns [`SendError::NoHelper`] when `imsg` is not installed,
/// [`SendError::NoTarget`] when the conversation has no address, and
/// [`SendError::Script`] with a sanitized reason when `imsg` refuses.
pub fn send_tapback(
    target: &Target,
    message_guid: &str,
    part: usize,
    reaction: Reaction,
    remove: bool,
    fallback: Option<&ReactFallback>,
) -> Result<(), SendError> {
    let imsg = imsg_path().ok_or(SendError::NoHelper)?;
    let args =
        tapback_args(target, message_guid, part, reaction, remove).ok_or(SendError::NoTarget)?;
    let refused = match run_imsg(&imsg, &args) {
        Ok(()) => return Ok(()),
        Err(refused) => refused,
    };
    match fallback.filter(|_| !remove) {
        // Taking a reaction back is something only the bridge can do, so there
        // is nothing to fall back to for it.
        None => Err(refused),
        // The second route is the one that actually reached Messages, so its
        // complaint is the one worth showing.
        Some(fallback) => run_imsg(&imsg, &react_args(fallback, reaction)),
    }
}

/// Run one `imsg` subcommand and reduce a refusal to something safe to show.
fn run_imsg(imsg: &Path, args: &[String]) -> Result<(), SendError> {
    let output = Command::new(imsg)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| SendError::NoHelper)?;
    if output.status.success() {
        return Ok(());
    }
    // `imsg` writes its complaint to either stream depending on how far it
    // got; both are stripped of quoted spans before they go anywhere.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = if stderr.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        stderr.into_owned()
    };
    Err(SendError::Script(sanitize_or(
        &detail,
        "imsg could not send that reaction",
    )))
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

/// A background answer to "is Messages.app running?", refreshed on a timer.
///
/// The probe costs a process, so it is off by default — nothing under `tests/`
/// ever spawns one — and one launch's worth of it runs at a time: [`poll`]
/// starts a thread when the interval is up and picks the answer off a channel
/// whenever it lands, so the event loop never waits on `pgrep`.
///
/// [`poll`]: Presence::poll
#[derive(Debug)]
pub struct Presence {
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
            rx: None,
            due: None,
            interval: PRESENCE_INTERVAL,
        }
    }

    /// A probe that asks now and then every [`PRESENCE_INTERVAL`].
    #[must_use]
    pub fn watching() -> Self {
        Self {
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
            std::thread::spawn(move || {
                let _ = tx.send(messages_app_running());
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

/// A reaction shown under a message before `chat.db` knows about it.
///
/// The counterpart of [`Pending`] for tapbacks: it goes up the moment the
/// picker is answered, and it comes down when the database's own row for it
/// arrives — or, if it never does, when the reconcile window closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTapback {
    /// Ties it to the [`Reply`] that will answer for it.
    pub id: u64,
    /// `chat.ROWID` the target message lives in.
    pub chat_rowid: i64,
    /// `message.guid` the reaction lands on.
    pub target_guid: String,
    /// Which part of the target it lands on.
    pub part: usize,
    /// Which reaction, in the form the database records.
    pub kind: TapbackKind,
    /// Whether it takes a reaction of yours away rather than adding one.
    pub remove: bool,
    /// Raw Messages timestamp of when the picker was answered.
    pub date: i64,
}

impl PendingTapback {
    /// The chip this stands for, as the row the database will eventually hold.
    #[must_use]
    pub fn as_tapback(&self) -> Tapback {
        Tapback {
            // Above anything `chat.db` can hold, so it sorts after the real
            // reactions already on the message.
            rowid: i64::MAX - i64::try_from(self.id).unwrap_or(0),
            target_guid: self.target_guid.clone(),
            target_part: self.part,
            action: TapbackAction::Added,
            kind: self.kind.clone(),
            is_from_me: true,
            handle_rowid: None,
            handle: None,
            date: self.date,
        }
    }

    /// Whether `message` already carries the reaction this stands for.
    #[must_use]
    pub fn stands_on(&self, message: &crate::db::Message) -> bool {
        message
            .tapbacks
            .iter()
            .any(|tapback| tapback.is_from_me && tapback.kind == self.kind)
    }

    /// Whether the database now says what this was sent to say, which is when
    /// the optimistic chip has nothing left to do.
    #[must_use]
    pub fn is_settled(&self, message: &crate::db::Message) -> bool {
        self.stands_on(message) != self.remove
    }
}

/// What is being sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// A typed message.
    Text(String),
    /// A file from disk.
    File(PathBuf),
    /// A reaction on the message with this GUID, through `imsg`.
    Tapback {
        /// `message.guid` of the message being reacted to.
        message_guid: String,
        /// Which part of it the reaction lands on.
        part: usize,
        /// Which of the six reactions.
        reaction: Reaction,
        /// Whether it takes one of yours back instead of adding one.
        remove: bool,
        /// The `imsg react` route, when this message is the one it can reach.
        fallback: Option<ReactFallback>,
    },
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

    /// Whether tapbacks have a way out: `imsg` is on `$PATH`, or the outbox is
    /// inert and would not run it anyway. Asked when the picker opens, so an
    /// `imsg` installed while msgs is running is found the next time.
    #[must_use]
    pub fn has_helper(&self) -> bool {
        matches!(self.mode, Mode::Inert(_)) || imsg_path().is_some()
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
                Outgoing::Tapback {
                    message_guid,
                    part,
                    reaction,
                    remove,
                    fallback,
                } => send_tapback(
                    &target,
                    &message_guid,
                    part,
                    reaction,
                    remove,
                    fallback.as_ref(),
                ),
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
    fn every_reaction_reads_the_same_as_the_row_it_becomes() {
        for reaction in REACTIONS {
            assert_eq!(
                reaction.glyph(),
                reaction.kind().glyph(),
                "{} draws differently once it comes back from the database",
                reaction.label()
            );
            assert_eq!(Reaction::from_kind(&reaction.kind()), Some(reaction));
        }
        // A custom emoji is readable but not sendable, so it maps to nothing.
        assert_eq!(
            Reaction::from_kind(&TapbackKind::Emoji("🦀".to_string())),
            None
        );
        assert_eq!(Reaction::from_kind(&TapbackKind::Sticker), None);
    }

    #[test]
    fn a_tapback_is_addressed_by_chat_and_message_guid() {
        let args = tapback_args(&direct(), "ABCD-1234", 0, Reaction::Love, false).expect("args");
        assert_eq!(args[0], "tapback");
        let pairs: Vec<(&str, &str)> = args[1..]
            .chunks(2)
            .filter(|chunk| chunk.len() == 2)
            .map(|chunk| (chunk[0].as_str(), chunk[1].as_str()))
            .collect();
        assert!(pairs.contains(&("--chat", "iMessage;-;+15550000000")));
        assert!(pairs.contains(&("--message", "ABCD-1234")));
        assert!(pairs.contains(&("--kind", "love")));
        assert!(pairs.contains(&("--part", "0")));
        assert!(!args.iter().any(|arg| arg == "--remove"));

        let taken =
            tapback_args(&direct(), "ABCD-1234", 2, Reaction::Question, true).expect("args");
        assert!(taken.iter().any(|arg| arg == "--remove"));
        assert!(taken.iter().any(|arg| arg == "question"));
        assert!(taken.iter().any(|arg| arg == "2"));
    }

    #[test]
    fn a_chat_without_a_guid_is_addressed_by_its_service_and_handle() {
        let sms = Target {
            guid: None,
            identifier: Some("+15550000000".to_string()),
            service: Service::Sms,
        };
        assert_eq!(
            chat_argument(&sms).as_deref(),
            Some("SMS;-;+15550000000"),
            "a thread with no stored guid still has an address"
        );
        let nowhere = Target {
            guid: None,
            identifier: None,
            service: Service::IMessage,
        };
        assert!(chat_argument(&nowhere).is_none());
        assert!(tapback_args(&nowhere, "ABCD", 0, Reaction::Like, false).is_none());
        assert_eq!(
            send_tapback(&nowhere, "ABCD", 0, Reaction::Like, false, None),
            Err(if imsg_path().is_some() {
                SendError::NoTarget
            } else {
                SendError::NoHelper
            })
        );
    }

    #[test]
    fn a_missing_helper_says_how_to_get_it_and_names_nothing_else() {
        let reason = SendError::NoHelper.to_string();
        assert!(reason.contains(IMSG_INSTALL));
        assert!(reason.contains("PATH"));
    }

    #[test]
    fn the_fallback_route_addresses_a_chat_rowid_in_the_database_msgs_reads() {
        let fallback = ReactFallback {
            chat_rowid: 7,
            db: PathBuf::from("/tmp/copy.db"),
        };
        let args = react_args(&fallback, Reaction::Emphasize);
        assert_eq!(args[0], "react");
        let pairs: Vec<(&str, &str)> = args[1..]
            .chunks(2)
            .filter(|chunk| chunk.len() == 2)
            .map(|chunk| (chunk[0].as_str(), chunk[1].as_str()))
            .collect();
        assert!(pairs.contains(&("--chat-id", "7")));
        assert!(pairs.contains(&("--db", "/tmp/copy.db")));
        // The two routes spell this one differently, and each gets its own.
        assert!(pairs.contains(&("--reaction", "emphasis")));
        assert_eq!(Reaction::Emphasize.label(), "emphasize");
        for reaction in REACTIONS {
            if reaction != Reaction::Emphasize {
                assert_eq!(reaction.label(), reaction.react_name());
            }
        }
    }

    #[test]
    fn an_optimistic_chip_becomes_the_row_the_database_will_hold() {
        let pending = PendingTapback {
            id: 3,
            chat_rowid: 1,
            target_guid: "ABCD-1234".to_string(),
            part: 0,
            kind: Reaction::Love.kind(),
            remove: false,
            date: 0,
        };
        let chip = pending.as_tapback();
        assert!(chip.is_from_me);
        assert!(chip.handle.is_none(), "your own reaction names nobody");
        assert_eq!(chip.action, TapbackAction::Added);
        assert_eq!(chip.kind, TapbackKind::Loved);
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
