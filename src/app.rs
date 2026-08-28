//! Application state and the single `update` entry point that mutates it.
//!
//! Input handling is deliberately split in three: `keymap` turns a key into an
//! [`Action`], [`App::update`] applies the action, and `ui` draws the result.
//! Nothing else touches state, so behavior stays testable without a terminal.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::config::{Config, MIN_WIDTH_FOR_CHAT_LIST};
use crate::contacts::Contacts;
use crate::db::{AttachmentRef, Chat, Db, DbError, MAX_PAGE, Message, PAGE, Source};
use crate::jump::{self, Jump};
use crate::media::{self, Images};
use crate::search::{self, Search};
use crate::seen::Seen;
use crate::send::{
    self, Delivery, Outbox, Outgoing, Pending, PendingTapback, Presence, REACTIONS, ReactFallback,
    Reaction, SendError, Service, Target,
};
use crate::theme::{self, Base, Theme};
use crate::ui::Panes;
use crate::ui::conversation::{Hits, Measured, Scroll};
use crate::ui::message::{self, Ctx};
use crate::watch::Watcher;

/// How long a toast stays on the status line.
const TOAST_TTL: Duration = Duration::from_secs(2);
/// Rows moved per wheel notch.
const WHEEL_ROWS: i16 = 3;
/// Tallest the composer grows before it scrolls internally.
pub const COMPOSER_MAX_LINES: u16 = 6;
/// How often a sent message is looked for in `chat.db`.
const RECONCILE_EVERY: Duration = Duration::from_millis(400);
/// How long a sent message is looked for before the echo is left to stand on
/// its own. Messages has taken it by then; only the database is behind.
const RECONCILE_FOR: Duration = Duration::from_secs(20);
/// How far back of a sent message's own clock a database row may be and still
/// be that message. Messages timestamps what it sends, not what msgs typed.
const RECONCILE_SLACK: i64 = 120;
/// Longest quoted line `r` puts in the composer.
const QUOTE_LIMIT: usize = 80;
/// Pages the palette will load looking for the message it was asked to jump
/// to, before it gives up and says the message is further back than that.
const JUMP_PAGES: usize = 100;
/// `chat.ROWID` standing in for a conversation that does not exist yet: the
/// one `Ctrl+N` opens to an address nobody has written to.
pub const DRAFT_CHAT: i64 = -1;
/// How often a locked database's scratch copy is taken again.
///
/// A copy never changes on its own, so keeping up with a database that had to
/// be read that way means copying it again — which is far too expensive to do
/// on every write.
const SNAPSHOT_EVERY: Duration = Duration::from_secs(2);

/// Which pane or overlay currently receives keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Left pane, the list of chats.
    ChatList,
    /// Right pane, the message blocks.
    Conversation,
    /// The send box under the conversation.
    Composer,
    /// The `Ctrl+K` jump palette, floating over everything.
    Palette,
    /// The `?` help modal.
    Help,
    /// The `Ctrl+R` reaction picker, floating over the selected message.
    Reactions,
    /// The first-run surface, shown while `chat.db` cannot be read.
    ///
    /// Never assigned to [`App::focus`]: [`App::key_focus`] reports it while
    /// [`App::db_error`] is set, so the keys that screen offers are routed
    /// without disturbing whichever pane focus goes back to once the database
    /// opens.
    DbError,
}

impl Focus {
    /// Overlays float above the panes and take all keys while open.
    #[must_use]
    pub const fn is_overlay(self) -> bool {
        matches!(self, Self::Palette | Self::Help | Self::Reactions)
    }
}

/// Everything the UI can be asked to do. Produced by `keymap` from a key press
/// or by [`App::on_mouse`] from a click or wheel notch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Leave the app.
    Quit,
    /// Move focus to the next pane.
    FocusNext,
    /// Move focus to the previous pane.
    FocusPrev,
    /// Move focus to a specific pane (mouse click).
    FocusPane(Focus),
    /// Jump straight to the send box.
    FocusComposer,
    /// Show or hide the chat list.
    ToggleChatList,
    /// `Ctrl+T`: the next theme base — dark, light, then the system's.
    CycleTheme,
    /// Move the selection up one row.
    SelectPrev,
    /// Move the selection down one row.
    SelectNext,
    /// Move the selection up one page.
    PageUp,
    /// Move the selection down one page.
    PageDown,
    /// Jump to the first row.
    ToTop,
    /// Jump to the last row.
    ToBottom,
    /// Scroll the pane under the mouse by a signed number of rows.
    Scroll(i16),
    /// `Enter`: open the selected chat, or send the composed message.
    Activate,
    /// Open the jump palette.
    OpenPalette,
    /// `Tab` in the palette: cycle all / chats / messages / photos.
    PaletteFilter,
    /// `Ctrl+N` in the palette: start a chat to the typed address.
    NewChat,
    /// Open the help modal.
    OpenHelp,
    /// Try to open `chat.db` again, from the first-run surface.
    RetryDb,
    /// `Esc`: close an overlay, clear a filter, or leave the composer.
    Cancel,
    /// Start filtering the chat list by name.
    StartFilter,
    /// Open the attachment on the selected message.
    OpenAttachment,
    /// Save the attachment on the selected message.
    SaveAttachment,
    /// Quote the selected message in the composer.
    QuoteReply,
    /// Open the reaction picker on the selected message, or close it again.
    React,
    /// Copy the selected message to the clipboard.
    CopySelection,
    /// Open the first link in the selected message in the browser.
    OpenLink,
    /// `Ctrl+U`: mark every chat seen here, or give the unread back.
    ToggleAllSeen,
    /// Attach a file to the message being composed.
    Attach,
    /// Type a character into whatever text field has focus.
    Insert(char),
    /// Delete the character before the cursor.
    Backspace,
    /// Delete the character after the cursor.
    DeleteForward,
    /// Delete the word before the cursor.
    DeleteWordBack,
    /// Clear the whole text field.
    ClearLine,
    /// Insert a newline without sending.
    Newline,
    /// Move the text cursor left one character.
    CursorLeft,
    /// Move the text cursor right one character.
    CursorRight,
    /// Move the text cursor to the start of the field.
    CursorHome,
    /// Move the text cursor to the end of the field.
    CursorEnd,
}

/// Whether `chat.db` could be opened. Filled in by the database layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DbStatus {
    /// No database has been opened yet.
    #[default]
    NotOpened,
    /// Open and readable.
    Ready,
    /// Could not be opened; the string is a short, path-only reason.
    Unreadable(String),
}

/// How the app is learning about new messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatcherStatus {
    /// Not started.
    #[default]
    Off,
    /// Watching `chat.db-wal` for changes.
    Watching,
    /// The watcher failed; falling back to a timer.
    Polling,
}

/// A short-lived message on the status line.
#[derive(Debug, Clone)]
struct Toast {
    text: String,
    born: Instant,
    is_error: bool,
}

/// The segments of the bottom status line.
#[derive(Debug, Default)]
pub struct Status {
    /// State of `chat.db`.
    pub db: DbStatus,
    /// State of the live-update watcher.
    pub watcher: WatcherStatus,
    /// Whether Messages.app is running, once we have checked.
    pub messages_app_running: Option<bool>,
    /// Total unread messages across all chats.
    pub unread_total: usize,
    /// How many chats hold those unread messages.
    pub unread_chats: usize,
    /// Startup warnings (bad config keys, and so on).
    pub warnings: Vec<String>,
    /// When the database was last re-read because it changed.
    pub last_update: Option<Instant>,
    toast: Option<Toast>,
}

impl Status {
    /// Show a transient note on the status line.
    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            born: Instant::now(),
            is_error: false,
        });
    }

    /// Show a transient failure on the status line.
    pub fn error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            born: Instant::now(),
            is_error: true,
        });
    }

    /// The live toast, as `(text, is_error)`, if one has not expired.
    #[must_use]
    pub fn active_toast(&self) -> Option<(&str, bool)> {
        self.toast
            .as_ref()
            .filter(|toast| toast.born.elapsed() < TOAST_TTL)
            .map(|toast| (toast.text.as_str(), toast.is_error))
    }

    /// Drop an expired toast. Returns `true` if the screen needs a redraw.
    fn tick(&mut self) -> bool {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.born.elapsed() >= TOAST_TTL)
        {
            self.toast = None;
            return true;
        }
        false
    }
}

/// A vertically scrolling list of rows with one selected row.
///
/// Both panes use this: the chat list selects chats, the conversation selects
/// message blocks. `len` is `0` until a data layer fills it in, and every
/// method is a safe no-op at that size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListPane {
    /// Number of rows currently in the list.
    pub len: usize,
    /// Index of the selected row, always `< len` when `len > 0`.
    pub selected: usize,
    /// Index of the first visible row.
    pub offset: usize,
}

impl ListPane {
    /// Resize the list, keeping the selection in range.
    pub fn set_len(&mut self, len: usize) {
        self.len = len;
        self.selected = self.selected.min(len.saturating_sub(1));
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
        }
    }

    /// Move the selection by `delta` rows, clamped to the ends of the list.
    pub fn move_by(&mut self, delta: i64) {
        if self.len == 0 {
            return;
        }
        let last = (self.len - 1) as i64;
        let next = (self.selected as i64 + delta).clamp(0, last);
        self.selected = next as usize;
    }

    /// Select the first row.
    pub fn to_top(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    /// Select the last row.
    pub fn to_bottom(&mut self) {
        self.selected = self.len.saturating_sub(1);
    }

    /// Scroll the viewport without moving the selection.
    pub fn scroll_by(&mut self, delta: i64) {
        let max_offset = self.len.saturating_sub(1) as i64;
        self.offset = (self.offset as i64 + delta).clamp(0, max_offset) as usize;
    }

    /// Pull `offset` toward the selection so it stays visible in `height` rows.
    pub fn scroll_into_view(&mut self, height: u16) {
        let height = usize::from(height).max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected - height + 1;
        }
        let max_offset = self.len.saturating_sub(height);
        self.offset = self.offset.min(max_offset);
    }

    /// Select the row drawn at absolute terminal row `row` inside `area`.
    /// Returns `true` if a row was hit.
    pub fn select_at_row(&mut self, area: Rect, row: u16) -> bool {
        if self.len == 0 || row < area.y || row >= area.y + area.height {
            return false;
        }
        let index = self.offset + usize::from(row - area.y);
        if index >= self.len {
            return false;
        }
        self.selected = index;
        true
    }
}

/// A multi-line text field with a byte-offset cursor.
///
/// Used by the composer, the palette input, and the chat-list filter box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextField {
    text: String,
    cursor: usize,
}

impl TextField {
    /// Build a field with the cursor at the end of `text`.
    #[must_use]
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }

    /// The current contents.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte offset of the cursor within [`Self::text`].
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the field is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Insert a character at the cursor.
    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    /// Delete the character after the cursor.
    pub fn delete_forward(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    /// Delete back to the start of the current word.
    pub fn delete_word_back(&mut self) {
        let head = &self.text[..self.cursor];
        let trimmed = head.trim_end_matches(char::is_whitespace);
        let start = trimmed
            .rfind(char::is_whitespace)
            .map_or(0, |index| index + 1);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Empty the field.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Take the contents, leaving the field empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Move the cursor one character left.
    pub fn cursor_left(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.cursor = prev;
        }
    }

    /// Move the cursor one character right.
    pub fn cursor_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    /// Move the cursor to the start of the field.
    pub const fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end of the field.
    pub const fn cursor_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Number of newline-separated lines, at least one.
    #[must_use]
    pub fn line_count(&self) -> u16 {
        let lines = self.text.split('\n').count();
        u16::try_from(lines).unwrap_or(u16::MAX).max(1)
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }
}

/// The `Ctrl+R` reaction picker: what it is aimed at, and what is under the
/// cursor.
///
/// It is aimed at a message GUID rather than a row number, so a live update
/// arriving underneath it cannot move it onto a different message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionPicker {
    /// `message.guid` the reaction would land on.
    pub target_guid: String,
    /// Which part of that message it would land on.
    pub part: usize,
    /// `chat.ROWID` the target lives in.
    pub chat_rowid: i64,
    /// Index into [`REACTIONS`] under the cursor.
    pub selected: usize,
    /// The reactions of yours already standing on the target, so choosing one
    /// of them again takes it back rather than sending it twice.
    pub standing: Vec<Reaction>,
    /// Whether `imsg` is on `$PATH`. Without it the picker only says how to
    /// get it, and answering it sends nothing.
    pub available: bool,
    /// The `imsg react` route, filled in only when the target is the newest
    /// incoming message of its chat — the one message that route can reach.
    pub fallback: Option<ReactFallback>,
}

impl ReactionPicker {
    /// The reaction under the cursor.
    #[must_use]
    pub fn reaction(&self) -> Reaction {
        REACTIONS[self.selected.min(REACTIONS.len() - 1)]
    }

    /// Whether `reaction` is one you have already put on this message.
    #[must_use]
    pub fn holds(&self, reaction: Reaction) -> bool {
        self.standing.contains(&reaction)
    }

    /// Move the cursor, wrapping around the row the way a six-item menu should.
    pub fn move_by(&mut self, delta: i64) {
        let len = REACTIONS.len() as i64;
        let next = (self.selected as i64 + delta).rem_euclid(len);
        self.selected = usize::try_from(next).unwrap_or(0);
    }
}

/// The whole application.
pub struct App {
    /// User config, as loaded at startup.
    pub config: Config,
    /// Colors, after config overrides.
    pub theme: Theme,
    /// Which palette [`App::theme`] starts from; `Ctrl+T` cycles it.
    pub theme_base: Base,
    /// The system's answer to "dark mode?", for [`Base::System`]. `None`
    /// until the probe answers, which reads as dark.
    pub system_dark: Option<bool>,
    /// The probe behind [`App::system_dark`]. [`Presence::off`] until
    /// something asks for it, so no test spawns one.
    pub appearance: Presence,
    /// Which pane has keyboard focus.
    pub focus: Focus,
    /// Where focus returns to when an overlay closes.
    overlay_return: Focus,
    /// Whether the chat list is shown (`Ctrl+B`).
    pub show_chat_list: bool,
    /// Whether mouse capture is on.
    pub mouse_enabled: bool,
    /// Path of the database being read, once one is chosen.
    pub db_path: Option<PathBuf>,
    /// The open, read-only database, once one has been opened.
    pub db: Option<Db>,
    /// Why the database could not be opened. While this is set the UI shows a
    /// full-screen explanation instead of the panes.
    pub db_error: Option<DbError>,
    /// The chat list as read from the database, pinned first and newest first.
    pub chat_rows: Vec<Chat>,
    /// Indices into [`App::chat_rows`] that pass the filter, in display order.
    /// The chat-list selection indexes this, not `chat_rows`.
    pub visible_chats: Vec<usize>,
    /// How many leading entries of [`App::visible_chats`] are pinned, which is
    /// what puts the `Pinned` / `Recent` headings in the list. Always `0` while
    /// the database does not record pinning.
    pub pinned_visible: usize,
    /// `chat.ROWID` of the conversation currently loaded into
    /// [`App::message_rows`].
    pub open_chat: Option<i64>,
    /// The open conversation's loaded page of messages, oldest first.
    pub message_rows: Vec<Message>,
    /// Pictures in the open conversation, for the header.
    pub open_chat_photos: i64,
    /// Set once the oldest message of the open chat is loaded, so scrolling up
    /// stops asking the database for a page that is not there.
    pub conversation_start_loaded: bool,
    /// Where the conversation is scrolled to.
    pub convo: Scroll,
    /// Block heights and the reply index for the loaded page.
    pub measured: Measured,
    /// What the conversation put where on the last frame, for mouse clicks.
    pub hits: Hits,
    /// Set when a freshly opened conversation still has to be pinned to its
    /// newest message, which needs a pane height to do.
    pending_bottom: bool,
    /// Set to leave the event loop.
    pub should_quit: bool,
    /// Selection state of the chat list.
    pub chats: ListPane,
    /// Inline chat-list filter, active while `Some`.
    pub chat_filter: Option<TextField>,
    /// Selection state of the conversation.
    pub messages: ListPane,
    /// The send box.
    pub composer: TextField,
    /// The `Ctrl+A` path prompt, which takes the composer's place while it is
    /// open.
    pub attach_prompt: Option<TextField>,
    /// Messages sent but not yet read back out of `chat.db`, oldest first.
    /// They are drawn as ordinary blocks at the end of the open conversation.
    pub pending: Vec<Pending>,
    /// Reactions sent but not yet read back out of `chat.db`. They are drawn
    /// as chips on the message they were aimed at, exactly where the real ones
    /// will land.
    pub pending_tapbacks: Vec<PendingTapback>,
    /// The `Ctrl+R` picker, open while `Some`.
    pub reaction_picker: Option<ReactionPicker>,
    /// Sends currently out with `osascript`.
    ///
    /// Public so a test can put an [`Outbox::inert`] one in its place and drive
    /// the whole send path without anything leaving the machine.
    pub outbox: Outbox,
    /// Ids handed to [`App::outbox`], so every echo has a distinct one.
    next_send: u64,
    /// When the pending echoes were last looked for in the database.
    last_reconcile: Option<Instant>,
    /// When the oldest unreconciled echo was sent.
    reconcile_since: Option<Instant>,
    /// The jump palette query.
    pub palette: TextField,
    /// What the jump palette is showing: filter, rows, and selection.
    pub jump: Jump,
    /// The full-text message index, once one has been asked for.
    ///
    /// `None` means message search is off — which is how the tests run, so
    /// nothing under `tests/` ever builds an index in the user's home.
    pub search: Option<Search>,
    /// Where the composer sends while `Ctrl+N` has opened a conversation that
    /// does not exist in `chat.db` yet.
    pub draft_target: Option<Target>,
    /// Scroll offset of the help modal.
    pub help_scroll: u16,
    /// Status line state.
    pub status: Status,
    /// Rects from the last frame, for mouse hit-testing.
    pub panes: Panes,
    /// Last known mouse position, for the hover tint.
    pub hover: Option<Position>,
    /// Live updates: what tells the app that `chat.db` moved.
    ///
    /// Public so a test can leave it [`Watcher::off`] and drive
    /// [`App::on_db_change`] by hand instead of waiting on a filesystem.
    pub watcher: Watcher,
    /// Messages that arrived below the viewport since the reader last saw the
    /// bottom, which is what the `↓ N new` pill counts.
    pub new_below: usize,
    /// When the scratch copy of a locked database was last re-taken.
    last_snapshot: Option<Instant>,
    /// Inline pictures: what the terminal can draw, and everything measured or
    /// encoded so far. [`Images::off`] until the terminal has been asked, which
    /// is how the tests and `--no-images` leave it.
    pub images: Images,
    /// Names for handles. [`Contacts::empty`] until something reads the macOS
    /// Contacts stores, which is how the tests run.
    pub contacts: Contacts,
    /// The local read state: how much of each chat's unread has already been
    /// on screen here. [`Seen::off`] until something asks for it, which is how
    /// the tests run and what keeps them out of the user's home.
    pub seen: Seen,
    /// Whether Messages.app is running, asked on a timer.
    /// [`Presence::off`] until something asks for it, so no test spawns one.
    pub presence: Presence,
    /// Where the read state was asked to live, so a retry can pick it up again.
    seen_path: Option<PathBuf>,
    /// Where the message index was asked to live, for the same reason.
    index_path: Option<PathBuf>,
    /// Whether the names on screen came from the machine's own Contacts
    /// stores, so a retry after Full Disk Access is granted reads them again.
    /// False for the fixture contacts the tests hand over.
    contacts_from_stores: bool,
}

impl App {
    /// Build the app from a loaded config and any startup warnings.
    #[must_use]
    pub fn new(config: Config, mut warnings: Vec<String>) -> Self {
        let (theme_base, base_warning) = Theme::base_from(&config.theme);
        warnings.extend(base_warning);
        let mut theme = Theme::for_base(theme_base, None);
        warnings.extend(theme.apply_overrides(&config.theme));
        let show_chat_list = config.show_chat_list;
        let mouse_enabled = config.mouse;
        let mut status = Status::default();
        if let Some(first) = warnings.first() {
            status.error(first.clone());
        }
        status.warnings = warnings;

        Self {
            config,
            theme,
            theme_base,
            system_dark: None,
            appearance: Presence::off(),
            focus: Focus::ChatList,
            overlay_return: Focus::ChatList,
            show_chat_list,
            mouse_enabled,
            db_path: None,
            db: None,
            db_error: None,
            chat_rows: Vec::new(),
            visible_chats: Vec::new(),
            pinned_visible: 0,
            open_chat: None,
            message_rows: Vec::new(),
            open_chat_photos: 0,
            conversation_start_loaded: false,
            convo: Scroll::default(),
            measured: Measured::default(),
            hits: Hits::default(),
            pending_bottom: false,
            should_quit: false,
            chats: ListPane::default(),
            chat_filter: None,
            messages: ListPane::default(),
            composer: TextField::default(),
            attach_prompt: None,
            pending: Vec::new(),
            pending_tapbacks: Vec::new(),
            reaction_picker: None,
            outbox: Outbox::new(),
            next_send: 0,
            last_reconcile: None,
            reconcile_since: None,
            palette: TextField::default(),
            jump: Jump::default(),
            search: None,
            draft_target: None,
            help_scroll: 0,
            status,
            panes: Panes::default(),
            hover: None,
            watcher: Watcher::off(),
            new_below: 0,
            last_snapshot: None,
            images: Images::off(),
            contacts: Contacts::empty(),
            seen: Seen::off(),
            presence: Presence::off(),
            seen_path: None,
            index_path: None,
            contacts_from_stores: false,
        }
    }

    /// Open `path` read-only and load the chat list from it.
    ///
    /// Failure is not fatal: the error is kept on [`App::db_error`] and the UI
    /// renders it full-screen instead of the panes, so a terminal without Full
    /// Disk Access gets an explanation rather than an empty app.
    pub fn open_db(&mut self, path: PathBuf) {
        self.db_path = Some(path.clone());
        match Db::open(&path) {
            Ok(db) => {
                self.status.db = DbStatus::Ready;
                self.db_error = None;
                self.db = Some(db);
                self.reload_chats();
                self.start_watching();
            }
            Err(err) => {
                self.status.db = DbStatus::Unreadable(err.summary());
                self.db = None;
                self.db_error = Some(err);
                self.chat_rows.clear();
                self.message_rows.clear();
                self.watcher = Watcher::off();
                self.status.watcher = WatcherStatus::Off;
                self.refresh_chat_view();
            }
        }
    }

    /// Start live updates for the database that is open.
    ///
    /// Never fatal: a watcher that will not start is replaced by a two-second
    /// timer, said once in the startup warnings and then permanently on the
    /// status line as `polling chat.db`.
    pub fn start_watching(&mut self) {
        let Some(path) = self.db_path.clone() else {
            self.watcher = Watcher::off();
            self.status.watcher = WatcherStatus::Off;
            return;
        };
        self.watcher = Watcher::start(&path);
        self.status.watcher = self.watcher.status();
        if self.status.watcher == WatcherStatus::Polling {
            self.status.warnings.push(
                "live updates: the file watcher could not start — polling instead".to_string(),
            );
        }
    }

    /// `r` on the first-run surface: open `chat.db` again.
    ///
    /// This is what somebody presses after granting Full Disk Access, so it
    /// does the whole of what a launch does — the database, then the names,
    /// the read state, and the index that were asked for at startup — rather
    /// than leaving an open database with nothing hanging off it. A retry that
    /// fails leaves the same surface up and says so on it.
    fn retry_db(&mut self) {
        let Some(path) = self.db_path.clone() else {
            return;
        };
        self.open_db(path);
        if let Some(err) = self.db_error.as_ref() {
            let summary = err.summary();
            self.status
                .error(format!("still cannot read chat.db — {summary}"));
            return;
        }
        if self.contacts_from_stores && self.contacts.status().warning().is_some() {
            self.enable_contacts_from_stores();
        }
        if let Some(seen) = self.seen_path.clone() {
            self.enable_seen(&seen);
        }
        if self.search.is_none()
            && let Some(index) = self.index_path.clone()
        {
            self.enable_search(&index);
        }
        self.status.toast("chat.db opened");
    }

    /// Start building the full-text message index at `index_path`.
    ///
    /// The build runs on its own thread and reports onto the status line, so a
    /// first launch against a large database is readable while it happens. Off
    /// unless something asks for it, which is what keeps the tests from ever
    /// writing an index anywhere.
    pub fn enable_search(&mut self, index_path: &std::path::Path) {
        self.index_path = Some(index_path.to_path_buf());
        let Some(db_path) = self.db_path.clone() else {
            return;
        };
        if self.db_error.is_some() {
            return;
        }
        self.search = Some(Search::start(&db_path, index_path));
    }

    /// Start asking, on a timer, whether Messages.app is running.
    ///
    /// Off until something asks for it: the answer costs a process, and no
    /// test should spawn one.
    pub fn enable_presence(&mut self) {
        self.presence = Presence::watching();
    }

    /// Start asking, on a timer, whether macOS is in dark mode. Only asked
    /// while the base is [`Base::System`]; off in the tests like the rest.
    pub fn enable_appearance(&mut self) {
        self.appearance = Presence::watching_with(theme::system_is_dark);
    }

    /// `Ctrl+T`: move to the next base and say which one.
    fn cycle_theme(&mut self) {
        self.theme_base = self.theme_base.next();
        self.rebuild_theme();
        let name = self.theme_base.name();
        let toast = if self.theme_base == Base::System {
            let showing = if self.theme_base.is_dark(self.system_dark) {
                "dark"
            } else {
                "light"
            };
            format!("theme: {name} ({showing})")
        } else {
            format!("theme: {name}")
        };
        self.status.toast(toast);
    }

    /// Recompute [`App::theme`] from the base, the system's answer, and the
    /// config's slot overrides. The overrides were already reported at
    /// startup, so their warnings are not repeated here.
    fn rebuild_theme(&mut self) {
        let mut theme = Theme::for_base(self.theme_base, self.system_dark);
        let _ = theme.apply_overrides(&self.config.theme);
        self.theme = theme;
    }

    /// Take the names read out of the macOS Contacts stores.
    ///
    /// Everything already loaded is re-resolved, so this can be called before or
    /// after the database is opened. A store that could not be read arrives as
    /// [`crate::contacts::Status::Unavailable`] and leaves one line in the
    /// startup warnings; every handle then falls back to a pretty-printed
    /// address.
    pub fn enable_contacts(&mut self, contacts: Contacts) {
        if let Some(warning) = contacts.status().warning() {
            // A retry reads the stores again, so the same complaint can arrive
            // twice; the notes list keeps one of each.
            if !self.status.warnings.contains(&warning) {
                self.status.warnings.push(warning.clone());
            }
            if self.status.active_toast().is_none() {
                self.status.error(warning);
            }
        }
        self.contacts = contacts;
        self.contacts.apply(&mut self.chat_rows);
        self.measured.stale = true;
    }

    /// Read the machine's own Contacts stores and take the names from them.
    ///
    /// The only caller is the binary. Going through here rather than
    /// [`App::enable_contacts`] is what tells a retry that the names can be
    /// read again — the tests hand over a fixture and this is never reached.
    pub fn enable_contacts_from_stores(&mut self) {
        self.contacts_from_stores = true;
        self.enable_contacts(Contacts::load());
    }

    /// Start keeping local read state at `path`.
    ///
    /// Off until something asks for it, which is how the tests run: nothing
    /// under `tests/` writes a read state into the user's home. Enabling it
    /// after the database is open marks whatever conversation is already on
    /// screen as seen, because it is.
    pub fn enable_seen(&mut self, path: &std::path::Path) {
        self.seen_path = Some(path.to_path_buf());
        let Some(db_path) = self.db_path.clone() else {
            return;
        };
        self.seen = Seen::load(path, &db_path);
        self.apply_seen();
        self.mark_open_seen();
    }

    /// Lay the local read state over the chat rows and re-total the status
    /// line.
    ///
    /// The one place [`Chat::unread`] and the status-line totals are set, so
    /// the badge in the list, the dot beside it, and the count on the status
    /// line can never disagree.
    fn apply_seen(&mut self) {
        self.seen.apply(&mut self.chat_rows);
        self.seen.save();
        self.status.unread_total = self
            .chat_rows
            .iter()
            .map(|chat| usize::try_from(chat.unread).unwrap_or(0))
            .sum();
        self.status.unread_chats = self
            .chat_rows
            .iter()
            .filter(|chat| chat.is_unread())
            .count();
    }

    /// Record that the open conversation has been read here.
    ///
    /// Messages.app's own flags and its Dock badge are untouched: `chat.db` is
    /// read-only and there is no supported way to clear either from outside
    /// that app. Only msgs's own indicator moves.
    fn mark_open_seen(&mut self) {
        let Some(rowid) = self.open_chat else {
            return;
        };
        let Some(unread) = self
            .chat_rows
            .iter()
            .find(|chat| chat.rowid == rowid)
            .map(|chat| chat.unread_count)
        else {
            return;
        };
        if self.seen.mark(rowid, unread) {
            self.apply_seen();
        }
    }

    /// `Ctrl+U`: mark every chat seen here, or hand the unread back.
    ///
    /// Both halves are local. Messages.app keeps its own count either way, and
    /// its badge is not something msgs can clear.
    fn toggle_all_seen(&mut self) {
        if !self.seen.is_on() {
            self.status.toast("read state is off");
            return;
        }
        if self.chat_rows.iter().any(Chat::is_unread) {
            self.seen.mark_all(&self.chat_rows);
            self.apply_seen();
            self.status
                .toast("marked all seen here — Messages.app keeps its own badge");
        } else if self.seen.forget_all() {
            self.apply_seen();
            self.status.toast("unread restored from Messages");
        } else {
            self.status.toast("nothing unread");
        }
    }

    /// Take the terminal's picture-drawing ability, once it has been asked.
    ///
    /// Until this is called nothing is drawn inline and every attachment is a
    /// chip, which is how the tests and `--no-images` run.
    pub fn enable_images(&mut self, images: Images) {
        self.images = images;
        self.measured.stale = true;
    }

    /// Where the message index lives, for `--check` and the status line.
    #[must_use]
    pub fn search_state(&self) -> search::State {
        self.search
            .as_ref()
            .map_or(search::State::Idle, |search| search.state().clone())
    }

    /// Re-read the chat list and the unread totals on the status line.
    ///
    /// The list is ordered by recency, so a message arriving anywhere moves
    /// rows around. The selection is kept on the conversation it was on rather
    /// than on the row number it was at.
    pub fn reload_chats(&mut self) {
        let anchor = self.selected_chat().map(|chat| chat.rowid);
        let Some(db) = self.db.as_ref() else {
            return;
        };
        match db.chats() {
            Ok(mut chats) => {
                // The one place names enter the app: every pane reads them off
                // the participants.
                self.contacts.apply(&mut chats);
                self.chat_rows = chats;
                // The local read state is laid over the database's counts the
                // same way names are laid over the handles: once, here, so
                // every pane and the status line read one number.
                self.apply_seen();
            }
            Err(err) => {
                self.status.error(format!("chat list: {}", err.summary()));
            }
        }
        self.adopt_draft();
        self.refresh_anchored(true, anchor);
    }

    /// A `Ctrl+N` draft stops being a draft the moment Messages has made the
    /// conversation real: the list gains a row with that address, and the
    /// selection moves onto it.
    fn adopt_draft(&mut self) {
        let Some(address) = self
            .draft_target
            .as_ref()
            .and_then(|target| target.identifier.clone())
        else {
            return;
        };
        let Some(rowid) = self
            .chat_rows
            .iter()
            .find(|chat| chat_has_address(chat, &address))
            .map(|chat| chat.rowid)
        else {
            return;
        };
        self.draft_target = None;
        if let Some(position) = self
            .chat_rows
            .iter()
            .position(|chat| chat.rowid == rowid)
            .and_then(|index| self.visible_chats.iter().position(|i| *i == index))
        {
            self.chats.selected = position;
        }
    }

    /// Re-apply the filter, keep the selection on the chat it was on, and open
    /// whatever it now points at.
    ///
    /// Cheap enough to run after every action: filtering a list of 500 chats is
    /// 500 substring tests, and the conversation is only re-read when the
    /// selected chat actually changed.
    pub fn refresh_chat_view(&mut self) {
        self.refresh(true);
    }

    /// [`App::refresh_chat_view`], but `follow_selection` is `false` after a
    /// wheel notch, which moves the viewport on purpose and must not be dragged
    /// back to the selection.
    fn refresh(&mut self, follow_selection: bool) {
        let anchor = self.selected_chat().map(|chat| chat.rowid);
        self.refresh_anchored(follow_selection, anchor);
    }

    /// [`App::refresh`] against a conversation the selection should land on,
    /// which is what a reordered list needs: the row the selection was at no
    /// longer holds the chat it was on.
    fn refresh_anchored(&mut self, follow_selection: bool, anchor: Option<i64>) {
        let needle = self
            .chat_filter
            .as_ref()
            .map(|field| field.text().trim().to_lowercase())
            .unwrap_or_default();

        let visible: Vec<usize> = self
            .chat_rows
            .iter()
            .enumerate()
            .filter(|(_, chat)| chat.matches(&needle))
            .map(|(index, _)| index)
            .collect();

        if visible != self.visible_chats {
            self.visible_chats = visible;
            self.chats.set_len(self.visible_chats.len());
        }
        // Narrowing or reordering the list must not drag the selection onto a
        // different conversation, so it follows the chat it was on where it can.
        if let Some(rowid) = anchor
            && let Some(position) = self
                .visible_chats
                .iter()
                .position(|index| self.chat_rows[*index].rowid == rowid)
        {
            self.chats.selected = position;
        }
        self.pinned_visible = self
            .visible_chats
            .iter()
            .take_while(|index| self.chat_rows[**index].is_pinned())
            .count();

        if follow_selection {
            self.sync_chat_scroll();
        }
        self.sync_open_chat();
    }

    /// Pull the chat list's scroll offset back to the selection, using the
    /// geometry of the last frame.
    fn sync_chat_scroll(&mut self) {
        let Some(rows) = self.panes.chat_list_rows else {
            // Nothing has been drawn yet, so all we can say is that the
            // selection must not be above the window.
            self.chats.offset = self.chats.offset.min(self.chats.selected);
            return;
        };
        self.chats.offset =
            crate::ui::chat_list::Shape::of(self, rows.height).offset_for(self.chats.selected);
    }

    /// Load the selected chat's conversation, if it is not the loaded one.
    fn sync_open_chat(&mut self) {
        // A `Ctrl+N` draft has no conversation to load: the pane stays empty
        // until the first message creates one.
        if self.draft_target.is_some() {
            if self.open_chat.take().is_some() || !self.message_rows.is_empty() {
                self.message_rows.clear();
                self.messages.set_len(0);
            }
            self.sync_pending_rows();
            return;
        }
        let Some(rowid) = self.selected_chat().map(|chat| chat.rowid) else {
            // Nothing selected: close whatever was open, and leave the pane
            // alone if nothing was.
            if self.open_chat.take().is_some() {
                self.message_rows.clear();
                self.messages.set_len(0);
            }
            return;
        };
        if self.open_chat == Some(rowid) {
            return;
        }
        self.load_conversation(rowid);
    }

    /// The chat under the chat-list selection.
    #[must_use]
    pub fn selected_chat(&self) -> Option<&Chat> {
        self.visible_chat(self.chats.selected)
    }

    /// The `n`th chat the filter leaves visible.
    #[must_use]
    pub fn visible_chat(&self, n: usize) -> Option<&Chat> {
        self.visible_chats
            .get(n)
            .and_then(|index| self.chat_rows.get(*index))
    }

    /// Load the newest page of `chat_rowid` into [`App::message_rows`], with
    /// the newest message selected.
    pub fn load_conversation(&mut self, chat_rowid: i64) {
        let Some(db) = self.db.as_ref() else {
            return;
        };
        let photos = db
            .attachment_counts(chat_rowid)
            .map_or(0, |(_, photos)| photos);
        let page = db.messages_before(chat_rowid, None, PAGE);

        self.open_chat = Some(chat_rowid);
        self.open_chat_photos = photos;
        match page {
            Ok(messages) => self.message_rows = messages,
            Err(err) => {
                self.message_rows.clear();
                self.status.error(format!("messages: {}", err.summary()));
            }
        }
        // A short conversation arrives whole, and nothing above it will ever
        // be asked for.
        self.conversation_start_loaded = self.message_rows.len() < PAGE;
        self.messages.set_len(self.message_rows.len());
        self.messages.to_bottom();
        self.measured = Measured::default();
        self.convo = Scroll::default();
        // A fresh conversation opens at its newest message, so nothing is
        // below the viewport for the pill to count.
        self.new_below = 0;
        // Echoes for this chat go back on the end of the page they belong to.
        self.sync_pending_rows();
        // The newest message goes to the bottom edge, which needs a pane
        // height; the next frame has one.
        self.messages.to_bottom();
        self.pending_bottom = true;
        // Reading a conversation here is what clears its badge here.
        self.mark_open_seen();
    }

    /// The message under the conversation selection.
    #[must_use]
    pub fn selected_message(&self) -> Option<&Message> {
        self.message_rows.get(self.messages.selected)
    }

    /// Load the page above the loaded one, keeping the selection and the
    /// viewport on the messages they were already on.
    ///
    /// Returns how many rows arrived, so a caller scrolling upward can tell the
    /// top of the conversation from a slow page.
    pub fn load_older(&mut self) -> usize {
        if self.conversation_start_loaded || self.message_rows.is_empty() {
            return 0;
        }
        let added = self.load_older_messages();
        if added < PAGE {
            self.conversation_start_loaded = true;
        }
        if added == 0 {
            return 0;
        }
        self.messages.set_len(self.message_rows.len());
        self.messages.selected += added;
        self.convo.top += added;
        // The heights the scroll arithmetic works from must describe the page
        // it is about to move through.
        self.measure(self.panes.conversation.width);
        added
    }

    /// Get the conversation ready to draw at `area`: measure what changed, and
    /// settle the viewport.
    pub fn prepare_conversation(&mut self, area: Rect) {
        self.measure(area.width);
        let viewport = area.height.max(1);
        if self.pending_bottom && !self.measured.heights.is_empty() {
            self.convo.to_bottom(&self.measured.heights, viewport);
            self.pending_bottom = false;
        } else {
            self.convo.clamp(&self.measured.heights, viewport);
        }
        // Back at the newest message: the pill has been read, so it goes away.
        if self.new_below > 0 && self.at_bottom() {
            self.new_below = 0;
        }
    }

    /// Whether the conversation is scrolled to its newest message.
    ///
    /// This is what decides between following a message that just arrived and
    /// offering the `↓ N new` pill instead.
    #[must_use]
    pub fn at_bottom(&self) -> bool {
        if self.pending_bottom {
            return true;
        }
        let mut end = self.convo;
        end.to_bottom(&self.measured.heights, self.conversation_height());
        self.convo == end
    }

    /// Re-measure the loaded page if the page or the pane width has changed.
    ///
    /// Laying every block out is what makes the scroll arithmetic exact, so it
    /// is done once per change rather than once per frame, and a page arriving
    /// on top of the loaded one only measures the rows it brought.
    fn measure(&mut self, width: u16) {
        let len = self.message_rows.len();
        let first = self.message_rows.first().map_or(0, |message| message.rowid);
        let last = self.message_rows.last().map_or(0, |message| message.rowid);
        if !self.measured.stale
            && self.measured.width == width
            && self.measured.first == first
            && self.measured.last == last
            && self.measured.heights.len() == len
        {
            return;
        }

        let prepended = self.measured.width == width
            && self.measured.last == last
            && !self.measured.heights.is_empty()
            && self.measured.heights.len() < len;
        let old = std::mem::take(&mut self.measured.heights);

        let by_guid: HashMap<String, usize> = self
            .message_rows
            .iter()
            .enumerate()
            .map(|(index, message)| (message.guid.clone(), index))
            .collect();
        let now = Local::now();
        let heights = {
            let ctx = Ctx {
                theme: &self.theme,
                chat: self.selected_chat(),
                messages: &self.message_rows,
                by_guid: &by_guid,
                pending: &self.pending,
                reactions: &self.pending_tapbacks,
                now,
                images: &self.images,
                contacts: &self.contacts,
            };
            let one = |index: usize| message::block(&ctx, index, width).height();
            if prepended {
                let added = len - old.len();
                // The message that used to open the page can lose its day
                // separator now that something sits above it, so it is
                // measured again along with the new rows.
                let mut rows: Vec<u16> = (0..=added).map(one).collect();
                rows.extend_from_slice(&old[1..]);
                rows
            } else {
                (0..len).map(one).collect()
            }
        };

        self.measured = Measured {
            width,
            first,
            last,
            heights,
            by_guid,
            stale: false,
        };
    }

    /// Prepend the page of messages above the ones already loaded.
    ///
    /// Returns how many rows were added, so the caller can stop asking once the
    /// top of the conversation is reached.
    pub fn load_older_messages(&mut self) -> usize {
        let Some(db) = self.db.as_ref() else {
            return 0;
        };
        let Some(oldest) = self.message_rows.first() else {
            return 0;
        };
        let (chat_rowid, before) = (oldest.chat_rowid, oldest.rowid);
        match db.messages_before(chat_rowid, Some(before), PAGE) {
            Ok(older) => {
                let added = older.len();
                self.message_rows.splice(0..0, older);
                added
            }
            Err(err) => {
                self.status.error(format!("messages: {}", err.summary()));
                0
            }
        }
    }

    /// `chat.ROWID` of the conversation the composer sends to.
    ///
    /// The open one, or — before a database has ever been read, which is how
    /// the render tests drive it — whatever the chat list has selected.
    #[must_use]
    pub fn current_chat_rowid(&self) -> Option<i64> {
        if self.draft_target.is_some() {
            return Some(DRAFT_CHAT);
        }
        self.open_chat
            .or_else(|| self.selected_chat().map(|chat| chat.rowid))
    }

    /// The conversation the composer sends to.
    #[must_use]
    pub fn current_chat(&self) -> Option<&Chat> {
        let rowid = self.current_chat_rowid()?;
        self.chat_rows.iter().find(|chat| chat.rowid == rowid)
    }

    /// `Enter` in the composer: hand the draft to Messages and echo it.
    ///
    /// The echo goes up first and the send runs on its own thread, because
    /// `osascript` takes long enough to answer that doing it here would freeze
    /// the UI mid-keystroke.
    fn send_composed(&mut self) {
        if self.composer.text().trim().is_empty() {
            return;
        }
        let Some(target) = self.outgoing_target() else {
            self.status.error("no conversation selected");
            return;
        };
        let text = self.composer.take();
        self.start_send(&target, text.clone(), Outgoing::Text(text), false);
    }

    /// `Ctrl+A`, once a path has been typed: send that file.
    fn send_attachment(&mut self) {
        let raw = self
            .attach_prompt
            .as_ref()
            .map(|prompt| prompt.text().trim().to_string())
            .unwrap_or_default();
        if raw.is_empty() {
            self.attach_prompt = None;
            return;
        }
        let path = expand_path(&raw);
        if !path.is_file() {
            // The path is something the user typed, not message content, so
            // naming it back is what makes the error useful. The prompt keeps
            // what was typed, so a typo can be fixed rather than retyped.
            self.status
                .error(format!("no file at {}", crate::ui::home_relative(&path)));
            return;
        }
        let Some(target) = self.outgoing_target() else {
            self.status.error("no conversation selected");
            return;
        };
        let label = path.file_name().map_or_else(
            || "attachment".to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        self.attach_prompt = None;
        self.start_send(&target, format!("📎 {label}"), Outgoing::File(path), true);
    }

    /// Put an echo on screen and start the send behind it.
    fn start_send(&mut self, target: &Target, echo: String, what: Outgoing, is_file: bool) {
        if !target.is_addressable() {
            self.status.error(SendError::NoTarget.to_string());
            return;
        }
        let Some(chat_rowid) = self.current_chat_rowid() else {
            self.status.error("no conversation selected");
            return;
        };
        let id = self.next_send;
        self.next_send += 1;
        self.pending.push(Pending::new(
            id,
            chat_rowid,
            echo,
            is_file,
            crate::db::raw_time(Local::now()),
        ));
        self.outbox.send(id, target.clone(), what);
        self.reconcile_since.get_or_insert_with(Instant::now);
        self.last_reconcile = None;
        self.sync_pending_rows();
        // You just sent something: the transcript goes to the bottom to show it.
        self.messages.to_bottom();
        self.pending_bottom = true;
    }

    /// Rebuild the echo rows at the end of the loaded page.
    ///
    /// The echoes are ordinary [`Message`] rows carrying a synthetic GUID, so
    /// the measuring, the scrolling, and the drawing treat them exactly like
    /// anything read out of the database.
    fn sync_pending_rows(&mut self) {
        self.message_rows
            .retain(|message| !message.guid.starts_with(send::PENDING_PREFIX));
        let Some(chat_rowid) = self.current_chat_rowid() else {
            return;
        };
        let echoes: Vec<Message> = self
            .pending
            .iter()
            .filter(|pending| pending.chat_rowid == chat_rowid)
            .map(echo_row)
            .collect();
        self.message_rows.extend(echoes);
        self.messages.set_len(self.message_rows.len());
    }

    /// Take the answers to the sends that have come back.
    ///
    /// Returns `true` if anything changed on screen.
    fn absorb_replies(&mut self) -> bool {
        let replies = self.outbox.drain();
        if replies.is_empty() {
            return false;
        }
        for reply in replies {
            // A refused reaction takes its optimistic chip back down with it;
            // one that landed keeps the chip until the database's own row for
            // it arrives.
            if let Some(index) = self
                .pending_tapbacks
                .iter()
                .position(|pending| pending.id == reply.id)
            {
                if let Err(err) = reply.result {
                    self.pending_tapbacks.remove(index);
                    self.measured.stale = true;
                    self.status.error(format!("could not react — {err}"));
                }
                continue;
            }
            let Some(pending) = self
                .pending
                .iter_mut()
                .find(|pending| pending.id == reply.id)
            else {
                continue;
            };
            match reply.result {
                Ok(()) => {
                    pending.state = Delivery::Sent;
                    self.status.messages_app_running = Some(true);
                }
                Err(err) => {
                    let reason = err.to_string();
                    let text = pending.text.clone();
                    let is_file = pending.is_file;
                    pending.state = Delivery::Failed(reason.clone());
                    if matches!(err, SendError::NotAvailable) {
                        self.status.messages_app_running = Some(false);
                    }
                    // The draft is not lost: it goes back in the composer so
                    // the same keystroke sends it again.
                    if !is_file && self.composer.is_empty() {
                        self.composer = TextField::from_text(text);
                    }
                    self.status.error(format!("could not send — {reason}"));
                }
            }
        }
        self.sync_pending_rows();
        true
    }

    /// Look for the sent messages in `chat.db` and drop the echoes that have
    /// arrived for real.
    ///
    /// Returns `true` if anything changed on screen.
    fn reconcile_pending(&mut self) -> bool {
        if self.pending.is_empty() && self.pending_tapbacks.is_empty() {
            self.reconcile_since = None;
            return false;
        }
        // A failed send stands until the user does something about it; nothing
        // is coming for it.
        if self.pending_tapbacks.is_empty()
            && self
                .pending
                .iter()
                .all(|pending| matches!(pending.state, Delivery::Failed(_)))
        {
            self.reconcile_since = None;
            return false;
        }
        let waited = self.reconcile_since.map(|since| since.elapsed());
        if waited.is_some_and(|waited| waited > RECONCILE_FOR) {
            // Messages took it; the database is simply behind, or is being read
            // from a snapshot copy. The echo stays, without the note.
            for pending in &mut self.pending {
                if pending.state == Delivery::Sending {
                    pending.state = Delivery::Sent;
                }
            }
            // A chip the database has not confirmed by now is standing on a
            // guess. The database's own answer is the honest one to show.
            if !self.pending_tapbacks.is_empty() {
                self.pending_tapbacks.clear();
                self.measured.stale = true;
            }
            self.reconcile_since = None;
            self.sync_pending_rows();
            return true;
        }
        if self
            .last_reconcile
            .is_some_and(|last| last.elapsed() < RECONCILE_EVERY)
        {
            return false;
        }
        self.last_reconcile = Some(Instant::now());
        self.pull_new_messages()
    }

    /// Read whatever `chat.db` has gained at the end of the open conversation,
    /// retire the echoes it accounts for, and follow it if the reader was
    /// already at the newest message.
    ///
    /// Returns `true` if anything changed on screen.
    fn pull_new_messages(&mut self) -> bool {
        // Asked before the rows land, because appending to the page is what
        // moves the bottom out from under the viewport.
        let was_at_bottom = self.at_bottom();
        let Some(refreshed) = self.refresh_open_conversation() else {
            return false;
        };
        if refreshed.is_quiet() {
            return false;
        }
        if refreshed.appended > 0 {
            if was_at_bottom {
                self.messages.to_bottom();
                self.pending_bottom = true;
                self.new_below = 0;
            } else {
                // A row that replaced one of your own echoes is already on
                // screen; only what someone else added is news.
                self.new_below += refreshed.appended.saturating_sub(refreshed.claimed);
            }
        }
        // Anything the database did to the open thread — a row on the end, an
        // edit, an unsend, a tapback — also changed its row in the list.
        self.reload_chats();
        true
    }

    /// Re-read the loaded page of the open conversation.
    ///
    /// Rows that are already loaded are replaced where they stand, which is
    /// how an edit or a tapback lands in the block it belongs to instead of at
    /// the end of the thread; anything the database has beyond them goes on
    /// the end. Only the newest page of a long scrollback is re-read, so a
    /// thread scrolled a long way back does not re-query thousands of rows
    /// every time somebody types.
    ///
    /// `None` means there was nothing to read: no open chat, no database, or a
    /// query that failed.
    fn refresh_open_conversation(&mut self) -> Option<Refreshed> {
        let chat_rowid = self.open_chat?;
        // The echoes are ours, not the database's; they go back on at the end.
        self.message_rows
            .retain(|message| !message.guid.starts_with(send::PENDING_PREFIX));

        let loaded = self.message_rows.len();
        let window = loaded.min(PAGE);
        let after = self
            .message_rows
            .get(loaded - window)
            .map_or(0, |message| message.rowid - 1);
        let limit = (window + PAGE).min(MAX_PAGE);

        let db = self.db.as_ref()?;
        let fresh = match db.messages_after(chat_rowid, after, limit) {
            Ok(fresh) => fresh,
            Err(err) => {
                self.status.error(format!("messages: {}", err.summary()));
                self.sync_pending_rows();
                return None;
            }
        };

        let mut at: HashMap<String, usize> = self
            .message_rows
            .iter()
            .enumerate()
            .map(|(index, message)| (message.guid.clone(), index))
            .collect();
        let mut refreshed = Refreshed::default();
        let mut appended: Vec<Message> = Vec::new();
        for message in fresh {
            if let Some(index) = at.get(&message.guid).copied() {
                if self.message_rows[index] != message {
                    self.message_rows[index] = message;
                    refreshed.merged = true;
                }
            } else {
                at.insert(message.guid.clone(), loaded + appended.len());
                appended.push(message);
            }
        }

        // Echoes whose real row has now arrived have nothing left to stand for.
        let mut claimed: Vec<u64> = Vec::new();
        for message in &appended {
            let sent = message.date;
            let Some(pending) = self.pending.iter().find(|pending| {
                pending.chat_rowid == chat_rowid
                    && !claimed.contains(&pending.id)
                    && !matches!(pending.state, Delivery::Failed(_))
                    && recent_enough(pending.date, sent)
                    && pending.matches(message)
            }) else {
                continue;
            };
            claimed.push(pending.id);
        }
        self.pending
            .retain(|pending| !claimed.contains(&pending.id));

        refreshed.appended = appended.len();
        refreshed.claimed = claimed.len();
        self.message_rows.extend(appended);
        // The page is the database's own answer right now, which is the one
        // moment an optimistic chip can be held up against it.
        self.reconcile_tapbacks();
        if self.pending.is_empty() && self.pending_tapbacks.is_empty() {
            self.reconcile_since = None;
        }
        if refreshed.merged {
            // A block that changed in place keeps its `ROWID`, so nothing else
            // would tell the measuring pass that its height moved.
            self.measured.stale = true;
        }
        self.sync_pending_rows();
        Some(refreshed)
    }

    /// `chat.db` changed: re-read what is on screen.
    ///
    /// Driven by [`App::tick`] when the watcher fires, and called directly by
    /// tests, which is why it does not touch the watcher itself.
    pub fn on_db_change(&mut self) -> bool {
        if self.db.is_none() {
            return false;
        }
        self.refresh_snapshot();
        if let Some(search) = self.search.as_mut() {
            search.catch_up();
        }
        if !self.pull_new_messages() {
            // Nothing for the open thread, but another conversation may have
            // gained a message: its row moves to the top of the list and its
            // preview and unread badge change with it.
            self.reload_chats();
        }
        // Whatever arrived in the thread you are looking at is on screen, so
        // it does not come back as a badge on the chat you are already in.
        self.mark_open_seen();
        self.status.last_update = Some(Instant::now());
        true
    }

    /// Take the scratch copy of a locked database again.
    ///
    /// A copy is a still photograph: nothing new ever appears in it, so a
    /// database that had to be read that way only keeps up if the picture is
    /// taken again. That is expensive, so it is rate-limited, and a database
    /// being read in place — the normal case, with Messages.app closed — does
    /// none of this.
    fn refresh_snapshot(&mut self) {
        if self
            .db
            .as_ref()
            .is_none_or(|db| db.source() == Source::Live)
        {
            return;
        }
        if self
            .last_snapshot
            .is_some_and(|when| when.elapsed() < SNAPSHOT_EVERY)
        {
            return;
        }
        self.last_snapshot = Some(Instant::now());
        let Some(path) = self.db_path.clone() else {
            return;
        };
        if let Ok(db) = Db::open(&path) {
            self.db = Some(db);
        }
    }

    /// The focus `keymap` should resolve against.
    ///
    /// While the chat-list filter box is open the list pane behaves as a text
    /// field, so letters type instead of navigating.
    #[must_use]
    pub fn key_focus(&self) -> Focus {
        // An unreadable database has no panes to steer, so the keys belong to
        // the surface that is actually on screen — unless an overlay is over
        // it, which still takes its own keys.
        if self.db_error.is_some() && !self.focus.is_overlay() {
            return Focus::DbError;
        }
        // A terminal narrowed under a focused chat list leaves the focus on a
        // pane that is no longer drawn; the keys go to what is on screen.
        if self.focus == Focus::ChatList && !self.chat_list_visible() {
            return Focus::Conversation;
        }
        if self.focus == Focus::ChatList && self.chat_filter.is_some() {
            Focus::Composer
        } else {
            self.focus
        }
    }

    /// Panes that can hold focus right now, in `Tab` order.
    fn focus_cycle(&self) -> Vec<Focus> {
        let mut cycle = Vec::with_capacity(3);
        if self.chat_list_visible() {
            cycle.push(Focus::ChatList);
        }
        cycle.push(Focus::Conversation);
        cycle.push(Focus::Composer);
        cycle
    }

    fn cycle_focus(&mut self, forward: bool) {
        let cycle = self.focus_cycle();
        let current = cycle.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward {
            (current + 1) % cycle.len()
        } else {
            (current + cycle.len() - 1) % cycle.len()
        };
        self.focus = cycle[next];
    }

    fn open_overlay(&mut self, overlay: Focus) {
        if !self.focus.is_overlay() {
            self.overlay_return = self.focus;
        }
        self.focus = overlay;
    }

    fn close_overlay(&mut self) {
        self.help_scroll = 0;
        self.palette.clear();
        self.jump.clear();
        self.reaction_picker = None;
        self.focus = self.overlay_return;
    }

    /// The text field that currently has focus, if any.
    ///
    /// The attachment prompt sits in front of the composer while it is open, so
    /// the same keys type into it.
    fn active_field(&mut self) -> Option<&mut TextField> {
        match self.focus {
            Focus::Composer => Some(self.attach_prompt.as_mut().unwrap_or(&mut self.composer)),
            Focus::Palette => Some(&mut self.palette),
            Focus::ChatList => self.chat_filter.as_mut(),
            _ => None,
        }
    }

    /// The list that currently has focus, if any.
    fn active_list(&mut self) -> Option<&mut ListPane> {
        match self.focus {
            Focus::ChatList => Some(&mut self.chats),
            Focus::Conversation => Some(&mut self.messages),
            Focus::Palette => Some(&mut self.jump.list),
            Focus::Help | Focus::Composer | Focus::Reactions | Focus::DbError => None,
        }
    }

    /// Apply one action. This is the only place app state changes.
    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::FocusNext => self.cycle_focus(true),
            Action::FocusPrev => self.cycle_focus(false),
            Action::FocusPane(pane) => self.focus = pane,
            Action::FocusComposer => self.focus = Focus::Composer,
            Action::ToggleChatList => self.toggle_chat_list(),
            Action::CycleTheme => self.cycle_theme(),
            Action::SelectPrev => self.move_selection(-1),
            Action::SelectNext => self.move_selection(1),
            Action::PageUp => self.page(-1),
            Action::PageDown => self.page(1),
            Action::ToTop => self.jump(true),
            Action::ToBottom => self.jump(false),
            Action::Scroll(delta) => self.scroll(i64::from(delta)),
            Action::Activate => self.activate(),
            Action::OpenPalette => self.open_palette(),
            Action::PaletteFilter => self.jump.filter = self.jump.filter.next(),
            Action::NewChat => self.start_new_chat(),
            Action::OpenHelp => self.open_overlay(Focus::Help),
            Action::RetryDb => self.retry_db(),
            Action::Cancel => self.cancel(),
            Action::StartFilter => self.start_filter(),
            Action::Insert(c) => {
                // The picker is a menu, not a text field: a digit picks the
                // reaction at that position and sends it.
                if self.focus == Focus::Reactions {
                    self.pick_reaction(c);
                } else {
                    self.edit(|field| field.insert(c));
                }
            }
            Action::Backspace => self.backspace(),
            Action::DeleteForward => self.edit(TextField::delete_forward),
            Action::DeleteWordBack => self.edit(TextField::delete_word_back),
            Action::ClearLine => self.edit(TextField::clear),
            Action::Newline => self.edit(|field| field.insert('\n')),
            Action::CursorLeft => self.edit(TextField::cursor_left),
            Action::CursorRight => self.edit(TextField::cursor_right),
            Action::CursorHome => self.edit(TextField::cursor_home),
            Action::CursorEnd => self.edit(TextField::cursor_end),
            Action::Attach => self.start_attach(),
            Action::OpenAttachment => self.open_attachment(),
            Action::SaveAttachment => self.save_attachment(),
            Action::QuoteReply => self.quote_reply(),
            Action::React => self.toggle_reactions(),
            Action::CopySelection => self.copy_selection(),
            Action::OpenLink => self.open_selected_link(),
            Action::ToggleAllSeen => self.toggle_all_seen(),
        }
        // Every path out of an action ends here, so a filter keystroke, an
        // arrow key, and a click all leave the chat list in the same state.
        self.refresh(!matches!(action, Action::Scroll(_)));
        self.refresh_jump();
    }

    /// `Ctrl+K`: open the palette with the list you already have.
    fn open_palette(&mut self) {
        self.open_overlay(Focus::Palette);
        self.jump.clear();
    }

    /// Rebuild the palette rows when the query, the filter, or the index moved.
    ///
    /// Called after every action, and cheap when nothing changed: the rows
    /// remember what they were built from and only a difference rebuilds them.
    fn refresh_jump(&mut self) {
        if self.focus != Focus::Palette {
            return;
        }
        let query = self.palette.text().trim().to_string();
        let filter = self.jump.filter;
        let indexed = self.search.as_ref().is_some_and(Search::is_ready);
        if !self
            .jump
            .is_stale(&query, filter, indexed, self.chat_rows.len())
        {
            return;
        }

        // The index is only asked once the query is long enough to be worth a
        // query, which is what keeps one letter from matching half the store.
        let hits = if filter.wants_messages() && query.chars().count() >= search::MIN_QUERY {
            let kind = filter.kind();
            self.search
                .as_mut()
                .map(|search| search.query(&query, kind, search::QUERY_LIMIT))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let columns = self.palette_columns();
        let mut jump = std::mem::take(&mut self.jump);
        jump.rebuild(
            &query,
            filter,
            indexed,
            &self.chat_rows,
            &hits,
            Local::now(),
            columns,
        );
        self.jump = jump;
    }

    /// Columns a palette result row has for a matched line.
    fn palette_columns(&self) -> usize {
        let width = self.panes.status.width.max(40);
        crate::ui::palette::body_columns(Rect::new(0, 0, width, 1))
    }

    /// `Enter` in the palette: go where the selected row points.
    fn jump_to_selected(&mut self) {
        let Some(row) = self.jump.selected().cloned() else {
            self.close_overlay();
            return;
        };
        self.close_overlay();
        match row.message_rowid {
            Some(message_rowid) => self.open_message(row.chat_rowid, message_rowid),
            None => {
                self.open_chat_row(row.chat_rowid);
                self.focus = Focus::Conversation;
            }
        }
    }

    /// Select `chat_rowid` in the list and open it, lifting a filter that
    /// would otherwise be hiding it.
    pub fn open_chat_row(&mut self, chat_rowid: i64) {
        self.draft_target = None;
        self.chat_filter = None;
        self.refresh_anchored(true, Some(chat_rowid));
    }

    /// Open `chat_rowid` with `message_rowid` selected, loading pages upward
    /// until the message is on the loaded page.
    ///
    /// A conversation loads from its newest end, so a hit deep in the history
    /// costs one page query per hundred messages between here and there. That
    /// is bounded: past [`JUMP_PAGES`] pages the jump gives up and says so
    /// rather than reading the whole thread.
    pub fn open_message(&mut self, chat_rowid: i64, message_rowid: i64) {
        self.open_chat_row(chat_rowid);
        if self.open_chat != Some(chat_rowid) {
            self.status
                .error("that conversation is no longer in the list");
            return;
        }
        self.focus = Focus::Conversation;
        for _ in 0..JUMP_PAGES {
            if let Some(index) = self
                .message_rows
                .iter()
                .position(|message| message.rowid == message_rowid)
            {
                self.messages.selected = index;
                self.pending_bottom = false;
                let viewport = self.conversation_height();
                self.convo.reveal(&self.measured.heights, viewport, index);
                return;
            }
            if self.load_older() == 0 {
                break;
            }
        }
        self.status
            .toast("that message is further back than msgs will load");
    }

    /// `Ctrl+N` in the palette: address a message to what has been typed.
    ///
    /// An address you already have a thread with opens that thread; anything
    /// else opens an empty conversation the composer can send into, which is
    /// the only way to start one when the database is read-only.
    fn start_new_chat(&mut self) {
        if self.focus != Focus::Palette {
            return;
        }
        let Some(address) = jump::looks_like_address(self.palette.text()) else {
            self.status
                .toast("type a phone number or email to start a new chat");
            return;
        };
        self.close_overlay();

        if let Some(rowid) = self
            .chat_rows
            .iter()
            .find(|chat| chat_has_address(chat, &address))
            .map(|chat| chat.rowid)
        {
            self.open_chat_row(rowid);
            self.focus = Focus::Composer;
            return;
        }

        self.draft_target = Some(Target {
            guid: None,
            identifier: Some(address),
            service: Service::IMessage,
        });
        self.open_chat = None;
        self.message_rows.clear();
        self.messages.set_len(0);
        self.convo = Scroll::default();
        self.measured = Measured::default();
        self.focus = Focus::Composer;
        // The address is on screen in the header; it is not repeated here.
        self.status.toast("new message — type it and press Enter");
    }

    /// Where the composer sends: the `Ctrl+N` draft, else the open chat.
    fn outgoing_target(&self) -> Option<Target> {
        self.draft_target
            .clone()
            .or_else(|| self.current_chat().map(Target::for_chat))
    }

    /// Leave a `Ctrl+N` draft, because the reader went somewhere real.
    fn leave_draft(&mut self) {
        self.draft_target = None;
    }

    /// Whether the chat list is actually on screen: the toggle *and* enough
    /// room for it. `Panes::default()` is width 0, before the first frame, so
    /// the intent stands in until a real layout has been computed.
    pub fn chat_list_visible(&self) -> bool {
        if self.panes.status.width == 0 {
            return self.show_chat_list;
        }
        self.show_chat_list && self.panes.status.width >= MIN_WIDTH_FOR_CHAT_LIST
    }

    fn too_narrow_for_the_chat_list(&self) -> bool {
        self.panes.status.width > 0 && self.panes.status.width < MIN_WIDTH_FOR_CHAT_LIST
    }

    fn toggle_chat_list(&mut self) {
        if self.too_narrow_for_the_chat_list() {
            // Flipping a flag the layout would override reads as a dead key.
            self.status.toast(format!(
                "the chat list needs {MIN_WIDTH_FOR_CHAT_LIST} columns"
            ));
            return;
        }
        self.show_chat_list = !self.show_chat_list;
        if !self.show_chat_list {
            self.chat_filter = None;
            if self.focus == Focus::ChatList {
                self.focus = Focus::Conversation;
            }
            if self.overlay_return == Focus::ChatList {
                self.overlay_return = Focus::Conversation;
            }
        }
    }

    fn move_selection(&mut self, delta: i64) {
        if self.focus == Focus::ChatList {
            // Moving through real conversations is leaving the draft behind.
            self.leave_draft();
        }
        match self.focus {
            Focus::Help => {
                let next = i64::from(self.help_scroll) + delta;
                self.help_scroll = u16::try_from(next.max(0)).unwrap_or(u16::MAX);
            }
            Focus::Conversation => self.move_message(delta),
            Focus::Reactions => {
                if let Some(picker) = self.reaction_picker.as_mut() {
                    picker.move_by(delta);
                }
            }
            _ => {
                if let Some(list) = self.active_list() {
                    list.move_by(delta);
                }
            }
        }
    }

    /// `PageUp` / `PageDown`: a page of rows in the conversation, a page of
    /// entries anywhere else.
    fn page(&mut self, direction: i64) {
        if self.focus == Focus::Conversation {
            let rows = i64::from(self.conversation_height().max(2)) - 1;
            self.scroll_conversation(direction * rows);
            return;
        }
        self.move_selection(direction * i64::from(self.config.page_step));
    }

    fn jump(&mut self, to_top: bool) {
        if self.focus == Focus::Help {
            self.help_scroll = 0;
            return;
        }
        if self.focus == Focus::Conversation {
            if to_top {
                self.messages.to_top();
                self.convo.to_top();
            } else {
                self.messages.to_bottom();
                self.convo
                    .to_bottom(&self.measured.heights, self.conversation_height());
            }
            return;
        }
        if let Some(list) = self.active_list() {
            if to_top {
                list.to_top();
            } else {
                list.to_bottom();
            }
        }
    }

    fn scroll(&mut self, delta: i64) {
        match self.focus {
            Focus::Help => {
                let next = i64::from(self.help_scroll) + delta;
                self.help_scroll = u16::try_from(next.max(0)).unwrap_or(u16::MAX);
            }
            Focus::Conversation => self.scroll_conversation(delta),
            _ => {
                if let Some(list) = self.active_list() {
                    list.scroll_by(delta);
                }
            }
        }
    }

    /// Rows the message area had on the last frame, never zero.
    #[must_use]
    fn conversation_height(&self) -> u16 {
        self.panes.conversation.height.max(1)
    }

    /// Move the message selection, pulling the view along with it and reaching
    /// for an older page when it walks off the top.
    fn move_message(&mut self, delta: i64) {
        if self.messages.len == 0 {
            return;
        }
        if delta < 0 && self.messages.selected == 0 {
            self.load_older();
        }
        self.messages.move_by(delta);
        let viewport = self.conversation_height();
        self.convo
            .reveal(&self.measured.heights, viewport, self.messages.selected);
    }

    /// Scroll the message area by rows, loading older pages as the top is
    /// reached so scrolling up never stops at the edge of a page.
    fn scroll_conversation(&mut self, delta: i64) {
        let viewport = self.conversation_height();
        if delta < 0 && self.convo.at_start() {
            self.load_older();
        }
        self.convo.by_rows(&self.measured.heights, viewport, delta);
        // The move can land on the first loaded row; fetching what is above it
        // now means the next notch has somewhere to go. Both calls are no-ops
        // once the start of the conversation is loaded.
        if delta < 0 && self.convo.at_start() {
            self.load_older();
        }
    }

    fn activate(&mut self) {
        match self.focus {
            Focus::ChatList => {
                // Committing the filter keeps the narrowed list and hands keys
                // back to navigation.
                self.chat_filter = None;
                self.focus = Focus::Conversation;
            }
            Focus::Composer => {
                if self.attach_prompt.is_some() {
                    self.send_attachment();
                } else {
                    self.send_composed();
                }
            }
            Focus::Palette => self.jump_to_selected(),
            Focus::Conversation => self.focus = Focus::Composer,
            Focus::Reactions => self.send_reaction(),
            Focus::Help => self.close_overlay(),
            // Unreachable: `focus` is never set to it. Retrying is `r`.
            Focus::DbError => {}
        }
    }

    fn cancel(&mut self) {
        match self.focus {
            Focus::Help | Focus::Palette | Focus::Reactions => self.close_overlay(),
            Focus::ChatList => self.chat_filter = None,
            Focus::Composer => {
                // The attachment prompt is a layer in front of the composer, so
                // `Esc` closes it and leaves the draft where it was.
                if self.attach_prompt.take().is_none() {
                    self.focus = Focus::Conversation;
                }
            }
            Focus::Conversation => {
                if self.chat_list_visible() {
                    self.focus = Focus::ChatList;
                }
            }
            Focus::DbError => {}
        }
    }

    fn start_filter(&mut self) {
        if self.too_narrow_for_the_chat_list() {
            self.status.toast(format!(
                "the chat list needs {MIN_WIDTH_FOR_CHAT_LIST} columns"
            ));
            return;
        }
        if !self.show_chat_list {
            self.show_chat_list = true;
        }
        self.focus = Focus::ChatList;
        self.chat_filter = Some(TextField::default());
    }

    /// `Ctrl+A`: ask for a path, in the composer's place.
    fn start_attach(&mut self) {
        if self.focus.is_overlay() {
            return;
        }
        self.focus = Focus::Composer;
        self.attach_prompt.get_or_insert_with(TextField::default);
    }

    /// The attachment `o` and `s` act on: the first one on the selected
    /// message that Messages is not hiding.
    #[must_use]
    pub fn selected_attachment(&self) -> Option<&AttachmentRef> {
        self.selected_message()?
            .attachments
            .iter()
            .find(|attachment| !attachment.hide_attachment)
    }

    /// The selected attachment's path, once it is known to be on this Mac.
    ///
    /// Everything that can go wrong becomes a status line here rather than at
    /// each caller, and the path itself never reaches a log.
    /// `which` is `None` for the first non-hidden attachment (what `o` and `s`
    /// act on) or `Some(index)` for one the pointer landed on.
    fn openable_attachment(&mut self, which: Option<usize>) -> Option<PathBuf> {
        let found = match which {
            None => self.selected_attachment(),
            Some(index) => self.selected_message().and_then(|message| {
                message
                    .attachments
                    .get(index)
                    .filter(|attachment| !attachment.hide_attachment)
            }),
        };
        let Some(attachment) = found else {
            self.status.toast("no attachment on the selected message");
            return None;
        };
        let path = attachment.path();
        match path.filter(|path| path.is_file()) {
            Some(path) => Some(path),
            None => {
                self.status
                    .toast(format!("that file is {}", media::NOT_DOWNLOADED));
                None
            }
        }
    }

    /// `o`: hand the selected attachment to `open`.
    fn open_attachment(&mut self) {
        self.open_attachment_at(None);
    }

    /// `o`, or a click on a picture, which names the attachment it landed on.
    fn open_attachment_at(&mut self, which: Option<usize>) {
        let Some(path) = self.openable_attachment(which) else {
            return;
        };
        match crate::shell::open_path(&path) {
            Ok(()) => self.status.toast("opening the attachment"),
            Err(err) => self
                .status
                .error(format!("could not open the attachment: {err}")),
        }
    }

    /// `s`: copy the selected attachment into `~/Downloads`.
    ///
    /// A copy out, never a move, and never a write anywhere near `chat.db`.
    fn save_attachment(&mut self) {
        let Some(path) = self.openable_attachment(None) else {
            return;
        };
        match media::save_to_downloads(&path) {
            Ok(saved) => {
                let shown = crate::ui::home_relative(&saved);
                self.status.toast(format!("saved to {shown}"));
            }
            Err(err) => self.status.error(format!("could not save it: {err}")),
        }
    }

    /// `r`: open a reply by quoting what is selected.
    ///
    /// Messages has no in-thread reply that AppleScript can reach, so the quote
    /// is a `>` line in the draft — the same thing you would type by hand.
    fn quote_reply(&mut self) {
        let Some(message) = self.selected_message() else {
            self.status.error("no message selected");
            return;
        };
        let quoted = crate::ui::format::single_line(&copyable(message));
        if quoted.is_empty() {
            self.status.toast("nothing to quote in that message");
            return;
        }
        let quoted = crate::ui::format::truncate(&quoted, QUOTE_LIMIT);
        let draft = self.composer.take();
        self.composer = TextField::from_text(format!("> {quoted}\n{draft}"));
        self.attach_prompt = None;
        self.focus = Focus::Composer;
    }

    /// `Ctrl+R`: open the reaction picker, or close one that is already open.
    ///
    /// The key is global, so pressing it a second time is the same as `Esc`.
    fn toggle_reactions(&mut self) {
        if self.focus == Focus::Reactions {
            self.close_overlay();
            return;
        }
        if self.focus.is_overlay() {
            return;
        }
        self.open_reactions();
    }

    /// Aim the picker at the selected message.
    ///
    /// A reaction is addressed by the target's GUID, so anything without a real
    /// one — an echo still on its way to `chat.db` — and anything Messages does
    /// not let you react to is refused here rather than at `imsg`.
    fn open_reactions(&mut self) {
        let Some(message) = self.selected_message() else {
            self.status.error("no message selected");
            return;
        };
        if message.guid.starts_with(send::PENDING_PREFIX) {
            self.status
                .toast("that message has not reached Messages yet");
            return;
        }
        if message.is_announcement() {
            self.status.toast("there is nothing to react to there");
            return;
        }
        let target_guid = message.guid.clone();
        let part = message
            .tapbacks
            .first()
            .map_or(0, |tapback| tapback.target_part);
        let standing: Vec<Reaction> = message
            .tapbacks
            .iter()
            .filter(|tapback| tapback.is_from_me)
            .filter_map(|tapback| Reaction::from_kind(&tapback.kind))
            .collect();
        let Some(chat_rowid) = self.current_chat_rowid() else {
            self.status.error("no conversation selected");
            return;
        };
        // Start on a reaction you have already given, so the first `Enter`
        // takes it back rather than adding a second one.
        let selected = standing
            .first()
            .and_then(|first| REACTIONS.iter().position(|reaction| reaction == first))
            .unwrap_or(0);
        let fallback = self.react_fallback(&target_guid, chat_rowid);
        self.reaction_picker = Some(ReactionPicker {
            target_guid,
            part,
            chat_rowid,
            selected,
            standing,
            available: self.outbox.has_helper(),
            fallback,
        });
        self.open_overlay(Focus::Reactions);
    }

    /// The `imsg react` route for `target_guid`, when there is one.
    ///
    /// That route always lands on the newest incoming message of a
    /// conversation, so it is only offered when the target happens to be
    /// exactly that — and never for a conversation that has no row in
    /// `chat.db` for `imsg` to look up.
    fn react_fallback(&self, target_guid: &str, chat_rowid: i64) -> Option<ReactFallback> {
        if chat_rowid <= 0 {
            return None;
        }
        let newest = self
            .message_rows
            .iter()
            .rev()
            .find(|message| !message.is_from_me && !message.is_announcement())?;
        if newest.guid != target_guid {
            return None;
        }
        Some(ReactFallback {
            chat_rowid,
            db: self.db_path.clone()?,
        })
    }

    /// A digit in the picker: choose that reaction and send it.
    fn pick_reaction(&mut self, c: char) {
        let Some(index) = c.to_digit(10).and_then(|digit| usize::try_from(digit).ok()) else {
            return;
        };
        if index == 0 || index > REACTIONS.len() {
            return;
        }
        if let Some(picker) = self.reaction_picker.as_mut() {
            picker.selected = index - 1;
        }
        self.send_reaction();
    }

    /// `Enter` in the picker: hand the reaction to `imsg` and chip it on at
    /// once.
    ///
    /// Choosing a reaction you have already given takes it back, which is what
    /// tapping it again does in Messages.
    fn send_reaction(&mut self) {
        let Some(picker) = self.reaction_picker.clone() else {
            return;
        };
        if !picker.available {
            // Nothing is sent and nothing is chipped on: without the helper
            // there is no way to put a reaction on the wire at all.
            self.status.error(SendError::NoHelper.to_string());
            return;
        }
        let Some(target) = self.outgoing_target() else {
            self.status.error("no conversation selected");
            return;
        };
        if !target.is_addressable() {
            self.status.error(SendError::NoTarget.to_string());
            return;
        }
        let reaction = picker.reaction();
        let remove = picker.holds(reaction);
        let id = self.next_send;
        self.next_send += 1;
        self.pending_tapbacks.push(PendingTapback {
            id,
            chat_rowid: picker.chat_rowid,
            target_guid: picker.target_guid.clone(),
            part: picker.part,
            kind: reaction.kind(),
            remove,
            date: crate::db::raw_time(Local::now()),
        });
        self.outbox.send(
            id,
            target,
            Outgoing::Tapback {
                message_guid: picker.target_guid,
                part: picker.part,
                reaction,
                remove,
                fallback: picker.fallback,
            },
        );
        self.reconcile_since.get_or_insert_with(Instant::now);
        self.last_reconcile = None;
        // A chip changes how tall the block it rides on is.
        self.measured.stale = true;
        self.close_overlay();
        self.status.toast(if remove {
            format!("{} taken back", reaction.glyph())
        } else {
            format!("{} sent", reaction.glyph())
        });
    }

    /// Drop the optimistic chips that `chat.db` has caught up with.
    ///
    /// Called with the loaded page freshly read, which is the one moment
    /// [`App::message_rows`] is the database's own answer and nothing else.
    fn reconcile_tapbacks(&mut self) {
        if self.pending_tapbacks.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.pending_tapbacks);
        let before = waiting.len();
        let rows = &self.message_rows;
        self.pending_tapbacks = waiting
            .into_iter()
            .filter(|pending| {
                // A target that is not on the loaded page cannot be checked;
                // the reconcile window closing is what retires that one.
                rows.iter()
                    .find(|message| message.guid == pending.target_guid)
                    .is_none_or(|message| !pending.is_settled(message))
            })
            .collect();
        if self.pending_tapbacks.len() != before {
            self.measured.stale = true;
        }
    }

    fn edit(&mut self, apply: impl FnOnce(&mut TextField)) {
        if let Some(field) = self.active_field() {
            apply(field);
        }
    }

    /// Backspace on an empty filter box or path prompt closes it, which is what
    /// `Esc` would do.
    fn backspace(&mut self) {
        if self.focus == Focus::ChatList
            && self.chat_filter.as_ref().is_some_and(TextField::is_empty)
        {
            self.chat_filter = None;
            return;
        }
        if self.focus == Focus::Composer
            && self.attach_prompt.as_ref().is_some_and(TextField::is_empty)
        {
            self.attach_prompt = None;
            return;
        }
        self.edit(TextField::backspace);
    }

    /// `y`: put the selected message on the clipboard.
    ///
    /// The body goes to the pasteboard and nowhere else — not to the status
    /// line, not to a log.
    fn copy_selection(&mut self) {
        let Some(text) = self.selected_message().map(copyable) else {
            self.status.error("no message selected");
            return;
        };
        if text.is_empty() {
            self.status.toast("nothing to copy in that message");
            return;
        }
        match crate::shell::copy(&text) {
            Ok(()) => self.status.toast("copied to the clipboard"),
            Err(err) => self.status.error(format!("could not copy: {err}")),
        }
    }

    /// `Ctrl+L`: open the first link in the selected message.
    fn open_selected_link(&mut self) {
        let link = self
            .selected_message()
            .and_then(|message| message.text.as_deref())
            .and_then(crate::ui::format::first_link);
        let Some(url) = link else {
            self.status.toast("no link in the selected message");
            return;
        };
        self.open_link(&url);
    }

    /// Hand a link to the browser. The link is message content, so it is never
    /// echoed back onto the status line.
    fn open_link(&mut self, url: &str) {
        match crate::shell::open_url(url) {
            Ok(()) => self.status.toast("opening the link"),
            Err(err) => self.status.error(format!("could not open the link: {err}")),
        }
    }

    /// Height the composer wants, borders included.
    #[must_use]
    pub fn composer_height(&self) -> u16 {
        self.composer_lines().clamp(1, COMPOSER_MAX_LINES) + 2
    }

    /// Route a mouse event to the pane under the pointer.
    pub fn on_mouse(&mut self, event: MouseEvent) {
        let position = Position::new(event.column, event.row);
        self.hover = Some(position);
        let target = self.pane_at(position);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Clicks pass through to panes only while no overlay is up;
                // an overlay swallows them so a stray click cannot act on the
                // dimmed screen behind it.
                if self.focus.is_overlay() {
                    return;
                }
                if let Some(pane) = target {
                    self.focus = pane;
                    self.click(pane, position);
                }
            }
            MouseEventKind::ScrollUp => self.wheel(target, -WHEEL_ROWS),
            MouseEventKind::ScrollDown => self.wheel(target, WHEEL_ROWS),
            _ => {}
        }
    }

    /// Which pane covers `position`, per the last frame's layout.
    fn pane_at(&self, position: Position) -> Option<Focus> {
        if self
            .panes
            .chat_list
            .is_some_and(|rect| rect.contains(position))
        {
            return Some(Focus::ChatList);
        }
        if self.panes.conversation.contains(position) {
            return Some(Focus::Conversation);
        }
        if self.panes.composer.contains(position) {
            return Some(Focus::Composer);
        }
        None
    }

    fn click(&mut self, pane: Focus, position: Position) {
        if pane == Focus::ChatList {
            // Chats are two rows tall and section headings take one, so the
            // list's own geometry decides what was clicked.
            if let Some(rect) = self.panes.chat_list_rows
                && position.y >= rect.y
                && let Some(index) =
                    crate::ui::chat_list::Shape::of(self, rect.height).chat_at(position.y - rect.y)
            {
                self.leave_draft();
                self.chats.selected = index;
                self.refresh_chat_view();
            }
        } else if pane == Focus::Conversation {
            // The pill is a button: clicking it goes to what it is counting.
            if self.hits.pill_at(position.x, position.y) {
                self.messages.to_bottom();
                self.pending_bottom = true;
                self.new_below = 0;
                return;
            }
            // A click on a link opens it; anywhere else it selects the block
            // that was drawn on that row.
            if let Some(url) = self
                .hits
                .link_at(position.x, position.y)
                .map(ToString::to_string)
            {
                self.open_link(&url);
            } else if let Some((index, attachment)) = self.hits.image_at(position.x, position.y) {
                // A click on a drawn picture does what `o` does to it.
                self.messages.selected = index;
                self.open_attachment_at(Some(attachment));
            } else if let Some(index) = self.hits.message_at(self.panes.conversation, position.y) {
                self.messages.selected = index;
            }
        }
    }

    /// Scroll the pane under the pointer, or the focused one if the pointer is
    /// somewhere without a pane. The wheel never moves focus, so scrolling one
    /// pane in passing does not change where the next key goes.
    fn wheel(&mut self, target: Option<Focus>, delta: i16) {
        let previous = self.focus;
        if let Some(pane) = target
            && !previous.is_overlay()
        {
            self.focus = pane;
        }
        self.update(Action::Scroll(delta));
        self.focus = previous;
    }

    /// Housekeeping between frames. Returns `true` if a redraw is needed.
    pub fn tick(&mut self) -> bool {
        let mut dirty = self.status.tick();
        if let Some(search) = self.search.as_mut()
            && search.poll()
        {
            dirty = true;
            // Results that were built without an index are worth building
            // again now that there is one.
            self.refresh_jump();
        }
        dirty |= self.absorb_replies();
        dirty |= self.reconcile_pending();
        // A HEIC that finished converting can be drawn now, which changes how
        // tall its block is, so the page is measured again.
        if self.images.take_arrived() {
            self.images.reconsider();
            self.measured.stale = true;
            dirty = true;
        }
        // Only a changed answer is worth a frame; the probe itself runs on its
        // own thread and never holds the loop up.
        if let Some(answer) = self.presence.poll()
            && self.status.messages_app_running != answer
        {
            self.status.messages_app_running = answer;
            dirty = true;
        }
        // The system appearance is only worth asking about while it is what
        // the theme follows, and only a changed answer repaints.
        if self.theme_base == Base::System
            && let Some(answer) = self.appearance.poll()
            && self.system_dark != answer
        {
            self.system_dark = answer;
            self.rebuild_theme();
            dirty = true;
        }
        if self.watcher.ready() {
            dirty |= self.on_db_change();
        }
        // The watcher can lose its backend at any point and fall back to the
        // timer, so the status line reads it rather than remembering it.
        self.status.watcher = self.watcher.status();
        dirty
    }

    /// Height the composer's contents want, borders excluded.
    #[must_use]
    fn composer_lines(&self) -> u16 {
        self.attach_prompt
            .as_ref()
            .map_or_else(|| self.composer.line_count(), TextField::line_count)
    }
}

/// What one re-read of the open conversation did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Refreshed {
    /// Messages added at the end of the loaded page.
    appended: usize,
    /// How many of those were the real rows behind echoes on screen.
    claimed: usize,
    /// Whether a row that was already loaded changed — an edit, or a tapback.
    merged: bool,
}

impl Refreshed {
    /// Whether the database had nothing new to say.
    const fn is_quiet(self) -> bool {
        self.appended == 0 && !self.merged
    }
}

/// The optimistic echo of `pending`, as a message row.
///
/// Its `ROWID` is above anything `chat.db` can hold, so it sorts to the end of
/// the page, and its GUID carries [`send::PENDING_PREFIX`], so it can always be
/// told apart from a real row.
fn echo_row(pending: &Pending) -> Message {
    Message {
        rowid: i64::MAX - i64::try_from(pending.id).unwrap_or(0),
        guid: pending.guid.clone(),
        chat_rowid: pending.chat_rowid,
        handle_rowid: None,
        handle: None,
        service: None,
        is_from_me: true,
        is_read: false,
        date: pending.date,
        date_delivered: 0,
        date_read: 0,
        date_edited: 0,
        is_edited: false,
        text: Some(pending.text.clone()),
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
    }
}

/// Whether `chat` is a conversation with exactly `address` on the other end.
fn chat_has_address(chat: &Chat, address: &str) -> bool {
    if chat.is_group {
        return false;
    }
    if chat
        .identifier
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case(address))
    {
        return true;
    }
    chat.participants
        .iter()
        .any(|handle| handle.id.eq_ignore_ascii_case(address))
}

/// Whether a row that just arrived is close enough in time to be the message
/// that was sent. Unreadable timestamps do not disqualify anything.
fn recent_enough(echoed: i64, arrived: i64) -> bool {
    let (Some(echoed), Some(arrived)) = (
        crate::db::unix_seconds(echoed),
        crate::db::unix_seconds(arrived),
    ) else {
        return true;
    };
    (arrived - echoed).abs() <= RECONCILE_SLACK
}

/// Expand a leading `~` and make the path absolute, the way a shell would.
#[must_use]
fn expand_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
    let expanded = if trimmed == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(trimmed))
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        dirs::home_dir().map_or_else(|| PathBuf::from(trimmed), |home| home.join(rest))
    } else {
        PathBuf::from(trimmed)
    };
    if expanded.is_absolute() {
        return expanded;
    }
    std::env::current_dir().map_or(expanded.clone(), |cwd| cwd.join(expanded))
}

/// What `y` puts on the clipboard: the body, or the names of what was sent when
/// there is no body.
fn copyable(message: &Message) -> String {
    if let Some(text) = message.text.as_deref().filter(|text| !text.is_empty()) {
        return text.to_string();
    }
    message
        .attachments
        .iter()
        .filter_map(|attachment| attachment.display_name())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn app() -> App {
        App::new(Config::default(), Vec::new())
    }

    #[test]
    fn tab_cycles_panes_and_skips_a_hidden_chat_list() {
        let mut app = app();
        assert_eq!(app.focus, Focus::ChatList);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::Conversation);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::Composer);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::ChatList);

        app.update(Action::ToggleChatList);
        assert_eq!(app.focus, Focus::Conversation);
        app.update(Action::FocusNext);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::Conversation);
    }

    #[test]
    fn tab_skips_a_chat_list_the_terminal_is_too_narrow_to_draw() {
        // The flag says the reader wants it, but nothing is drawn there.
        let mut app = app();
        app.panes.status.width = MIN_WIDTH_FOR_CHAT_LIST - 1;
        app.focus = Focus::Conversation;
        assert!(app.show_chat_list);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::Composer);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::Conversation);
    }

    #[test]
    fn keys_leave_a_chat_list_the_terminal_narrowed_out_from_under() {
        // Focused while there was room, then the terminal shrank.
        let mut app = app();
        app.focus = Focus::ChatList;
        app.panes.status.width = MIN_WIDTH_FOR_CHAT_LIST - 1;
        assert_eq!(app.key_focus(), Focus::Conversation);
        app.panes.status.width = MIN_WIDTH_FOR_CHAT_LIST;
        assert_eq!(app.key_focus(), Focus::ChatList);
    }

    #[test]
    fn a_narrow_terminal_explains_the_toggle_instead_of_flipping_it() {
        let mut app = app();
        app.panes.status.width = MIN_WIDTH_FOR_CHAT_LIST - 1;
        assert!(app.show_chat_list);

        app.update(Action::ToggleChatList);
        assert!(app.show_chat_list);
        assert!(!app.chat_list_visible());
        let (text, is_error) = app.status.active_toast().expect("a toast explains it");
        assert!(text.contains(&MIN_WIDTH_FOR_CHAT_LIST.to_string()));
        assert!(!is_error);
    }

    #[test]
    fn overlays_return_focus_where_it_was() {
        let mut app = app();
        app.update(Action::FocusNext);
        app.update(Action::FocusNext);
        assert_eq!(app.focus, Focus::Composer);

        app.update(Action::OpenHelp);
        assert_eq!(app.focus, Focus::Help);
        app.update(Action::OpenPalette);
        assert_eq!(app.focus, Focus::Palette);
        app.update(Action::Cancel);
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn hiding_the_chat_list_moves_the_overlay_return_off_it() {
        let mut app = app();
        app.update(Action::OpenHelp);
        app.update(Action::ToggleChatList);
        app.update(Action::Cancel);
        assert_eq!(app.focus, Focus::Conversation);
    }

    #[test]
    fn selection_is_clamped_and_safe_on_an_empty_list() {
        let mut app = app();
        app.update(Action::SelectNext);
        app.update(Action::ToBottom);
        assert_eq!(app.chats.selected, 0);

        app.chats.set_len(4);
        app.update(Action::PageDown);
        assert_eq!(app.chats.selected, 3);
        app.update(Action::SelectPrev);
        assert_eq!(app.chats.selected, 2);
        app.update(Action::ToTop);
        assert_eq!(app.chats.selected, 0);
        app.update(Action::SelectPrev);
        assert_eq!(app.chats.selected, 0);
    }

    #[test]
    fn shrinking_a_list_keeps_the_selection_in_range() {
        let mut list = ListPane::default();
        list.set_len(10);
        list.to_bottom();
        assert_eq!(list.selected, 9);
        list.set_len(3);
        assert_eq!(list.selected, 2);
        list.set_len(0);
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn scroll_into_view_follows_the_selection_both_ways() {
        let mut list = ListPane::default();
        list.set_len(50);
        list.selected = 20;
        list.scroll_into_view(10);
        assert_eq!(list.offset, 11);
        list.selected = 5;
        list.scroll_into_view(10);
        assert_eq!(list.offset, 5);
    }

    #[test]
    fn filter_box_swallows_letters_then_gives_them_back() {
        let mut app = app();
        assert_eq!(app.key_focus(), Focus::ChatList);
        app.update(Action::StartFilter);
        assert_eq!(app.key_focus(), Focus::Composer);

        app.update(Action::Insert('p'));
        app.update(Action::Insert('r'));
        assert_eq!(app.chat_filter.as_ref().unwrap().text(), "pr");

        app.update(Action::Backspace);
        assert_eq!(app.chat_filter.as_ref().unwrap().text(), "p");
        app.update(Action::Cancel);
        assert!(app.chat_filter.is_none());
        assert_eq!(app.key_focus(), Focus::ChatList);
    }

    #[test]
    fn backspace_on_an_empty_filter_closes_it() {
        let mut app = app();
        app.update(Action::StartFilter);
        app.update(Action::Backspace);
        assert!(app.chat_filter.is_none());
    }

    #[test]
    fn composer_grows_to_a_ceiling() {
        let mut app = app();
        app.update(Action::FocusPane(Focus::Composer));
        assert_eq!(app.composer_height(), 3);
        for _ in 0..3 {
            app.update(Action::Newline);
        }
        assert_eq!(app.composer_height(), 6);
        for _ in 0..20 {
            app.update(Action::Newline);
        }
        assert_eq!(app.composer_height(), COMPOSER_MAX_LINES + 2);
    }

    #[test]
    fn text_field_edits_respect_char_boundaries() {
        let mut field = TextField::from_text("héllo 🌊");
        field.backspace();
        assert_eq!(field.text(), "héllo ");
        field.cursor_home();
        field.cursor_right();
        field.delete_forward();
        assert_eq!(field.text(), "hllo ");
        field.cursor_end();
        field.delete_word_back();
        assert_eq!(field.text(), "");
    }

    #[test]
    fn text_field_take_empties_it() {
        let mut field = TextField::from_text("hi");
        assert_eq!(field.take(), "hi");
        assert!(field.is_empty());
        assert_eq!(field.cursor(), 0);
    }

    /// An app whose conversation is `count` blocks of two rows each, as if a
    /// frame had already measured them.
    fn with_measured_conversation(count: usize) -> App {
        let mut app = app();
        app.messages.set_len(count);
        app.panes.conversation = Rect::new(30, 1, 50, 20);
        app.measured = Measured {
            width: 50,
            first: 1,
            last: count as i64,
            heights: vec![2; count],
            by_guid: HashMap::new(),
            stale: false,
        };
        app.conversation_start_loaded = true;
        app
    }

    #[test]
    fn wheel_scrolls_the_pane_under_the_pointer_without_stealing_focus() {
        let mut app = with_measured_conversation(100);
        app.panes.chat_list = Some(Rect::new(0, 0, 30, 22));

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });

        assert_eq!(
            app.focus,
            Focus::ChatList,
            "focus must not follow the wheel"
        );
        // Three rows down through two-row blocks: one whole block and a half.
        assert_eq!(app.convo, Scroll { top: 1, skip: 1 });
        assert_eq!(app.messages.selected, 0, "the wheel does not select");
        assert_eq!(WHEEL_ROWS, 3);
    }

    #[test]
    fn clicking_a_pane_focuses_it_and_selects_the_row() {
        let mut app = with_measured_conversation(100);
        // As the last frame drew it: two rows per message, from the pane's top.
        app.hits = Hits {
            rows: (0..20).map(|row| Some(row / 2)).collect(),
            links: Vec::new(),
            images: Vec::new(),
            pill: None,
        };

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 8,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });

        assert_eq!(app.focus, Focus::Conversation);
        assert_eq!(app.messages.selected, 3);
    }

    #[test]
    fn clicking_a_picture_selects_its_message_and_opens_it() {
        let mut app = with_measured_conversation(2);
        let mut row = message_row("IMG-0001");
        row.attachments = vec![AttachmentRef {
            rowid: 9,
            guid: "ATT-0001".to_string(),
            message_rowid: 5,
            // A path that is not on this Mac, so the click stops at the toast
            // instead of handing anything to `open`.
            filename: Some("~/Library/Messages/Attachments/invented.heic".to_string()),
            mime_type: Some("image/heic".to_string()),
            uti: Some("public.heic".to_string()),
            transfer_name: Some("invented.heic".to_string()),
            total_bytes: 0,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        }];
        app.message_rows = vec![message_row("TXT-0001"), row];
        app.hits = Hits {
            rows: (0..20).map(|row| Some(row / 2)).collect(),
            links: Vec::new(),
            images: vec![crate::ui::conversation::ImageHit {
                rect: Rect::new(34, 5, 10, 6),
                message: 1,
                attachment: 0,
            }],
            pill: None,
        };

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 36,
            row: 7,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });

        assert_eq!(app.focus, Focus::Conversation);
        assert_eq!(app.messages.selected, 1, "the picture's own message");
        let (text, _) = app.status.active_toast().expect("a toast explains it");
        assert!(text.contains(media::NOT_DOWNLOADED), "{text}");
    }

    #[test]
    fn keys_move_the_message_selection_and_pull_the_view_after_it() {
        let mut app = with_measured_conversation(40);
        app.focus = Focus::Conversation;

        app.update(Action::ToBottom);
        assert_eq!(app.messages.selected, 39);
        // Eighty rows of blocks in twenty rows of pane: the last ten are shown.
        assert_eq!(app.convo, Scroll { top: 30, skip: 0 });

        app.update(Action::SelectPrev);
        assert_eq!(app.messages.selected, 38);
        assert_eq!(app.convo, Scroll { top: 30, skip: 0 }, "already on screen");

        app.update(Action::ToTop);
        assert_eq!(app.messages.selected, 0);
        assert_eq!(app.convo, Scroll::default());

        // A page moves the view without touching the selection.
        app.update(Action::PageDown);
        assert_eq!(app.messages.selected, 0);
        assert_eq!(app.convo, Scroll { top: 9, skip: 1 });
    }

    #[test]
    fn an_open_overlay_swallows_clicks() {
        let mut app = app();
        app.panes.conversation = Rect::new(30, 1, 50, 20);
        app.update(Action::OpenHelp);

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 4,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });

        assert_eq!(app.focus, Focus::Help);
    }

    #[test]
    fn ctrl_t_cycles_the_base_and_keeps_the_slot_overrides() {
        let (config, warnings) = Config::parse("[theme]\naccent_me = \"#123456\"");
        let mut app = App::new(config, warnings);
        let accent = ratatui::style::Color::Rgb(0x12, 0x34, 0x56);
        assert_eq!(app.theme_base, Base::Dark);

        app.update(Action::CycleTheme);
        assert_eq!(app.theme_base, Base::Light);
        assert_eq!(app.theme.bg_base, Theme::light().bg_base);
        assert_eq!(app.theme.accent_me, accent, "the override survives");
        assert!(app.status.active_toast().is_some());

        // System with no answer yet reads as dark; an answer of light repaints.
        app.update(Action::CycleTheme);
        assert_eq!(app.theme_base, Base::System);
        assert_eq!(app.theme.bg_base, Theme::default().bg_base);
        app.system_dark = Some(false);
        app.rebuild_theme();
        assert_eq!(app.theme.bg_base, Theme::light().bg_base);

        app.update(Action::CycleTheme);
        assert_eq!(app.theme_base, Base::Dark);
        assert_eq!(app.theme.bg_base, Theme::default().bg_base);
    }

    #[test]
    fn config_base_system_starts_dark_without_spawning_anything() {
        let (config, warnings) = Config::parse("[theme]\nbase = \"system\"");
        let mut app = App::new(config, warnings);
        assert_eq!(app.theme_base, Base::System);
        assert!(app.status.warnings.is_empty());
        assert_eq!(app.theme, Theme::default());
        // The probe is off, so a tick asks nothing and changes nothing.
        app.tick();
        assert_eq!(app.system_dark, None);
    }

    #[test]
    fn config_theme_overrides_reach_the_app_and_bad_ones_warn() {
        let (config, warnings) =
            Config::parse("[theme]\naccent_me = \"#123456\"\nbogus = \"#fff\"");
        let app = App::new(config, warnings);
        assert_eq!(
            app.theme.accent_me,
            ratatui::style::Color::Rgb(0x12, 0x34, 0x56)
        );
        assert_eq!(app.status.warnings.len(), 1);
        assert!(app.status.active_toast().is_some());
    }

    /// A one-to-one chat with an invented address, and an outbox that records
    /// sends instead of running them. Nothing in these tests reaches
    /// `osascript`, so nothing leaves the machine.
    fn app_with_chat() -> App {
        let mut app = app();
        app.outbox = Outbox::inert();
        app.chat_rows = vec![Chat {
            rowid: 1,
            guid: "iMessage;-;+15550000000".to_string(),
            identifier: Some("+15550000000".to_string()),
            display_name: None,
            service: Some("iMessage".to_string()),
            style: 45,
            is_group: false,
            participants: Vec::new(),
            last_message_date: 0,
            last_message_rowid: 0,
            preview: None,
            message_count: 0,
            unread_count: 0,
            unread: 0,
            is_pinned: None,
        }];
        app.refresh_chat_view();
        app.focus = Focus::Composer;
        app
    }

    fn compose(app: &mut App, text: &str) {
        for c in text.chars() {
            app.update(Action::Insert(c));
        }
    }

    #[test]
    fn enter_hands_the_draft_to_messages_and_echoes_it_at_once() {
        let mut app = app_with_chat();
        compose(&mut app, "on my way");
        app.update(Action::Activate);

        assert!(app.composer.is_empty(), "the draft leaves the box");
        assert_eq!(app.pending.len(), 1);
        assert_eq!(app.pending[0].state, Delivery::Sending);

        let echo = app.message_rows.last().expect("an echo block");
        assert!(echo.is_from_me);
        assert!(echo.guid.starts_with(send::PENDING_PREFIX));
        assert_eq!(app.messages.len, 1);

        // Addressed by chat guid, as text, on the chat's own service.
        let recorded = app.outbox.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, app.pending[0].id);
        assert_eq!(
            recorded[0].1.guid.as_deref(),
            Some("iMessage;-;+15550000000")
        );
        assert_eq!(recorded[0].2, Outgoing::Text("on my way".to_string()));
    }

    /// One invented message in the open conversation, selected.
    fn message_row(guid: &str) -> Message {
        Message {
            rowid: 5,
            guid: guid.to_string(),
            chat_rowid: 1,
            handle_rowid: Some(1),
            handle: None,
            service: None,
            is_from_me: false,
            is_read: true,
            date: 0,
            date_delivered: 0,
            date_read: 0,
            date_edited: 0,
            is_edited: false,
            text: Some("invented".to_string()),
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
        }
    }

    fn app_with_message() -> App {
        let mut app = app_with_chat();
        app.open_chat = Some(1);
        app.message_rows = vec![message_row("ABCD-1234")];
        app.messages.set_len(1);
        app.focus = Focus::Conversation;
        app
    }

    /// Your own reaction, as the database would hand it back.
    fn mine(kind: crate::db::TapbackKind) -> crate::db::Tapback {
        crate::db::Tapback {
            rowid: 9,
            target_guid: "ABCD-1234".to_string(),
            target_part: 0,
            action: crate::db::TapbackAction::Added,
            kind,
            is_from_me: true,
            handle_rowid: None,
            handle: None,
            date: 0,
        }
    }

    #[test]
    fn ctrl_r_aims_the_picker_at_the_selected_message_and_ctrl_r_closes_it() {
        let mut app = app_with_message();
        app.update(Action::React);
        assert_eq!(app.focus, Focus::Reactions);
        let picker = app.reaction_picker.as_ref().expect("a picker");
        assert_eq!(picker.target_guid, "ABCD-1234");
        assert_eq!(picker.chat_rowid, 1);
        assert_eq!(picker.reaction(), REACTIONS[0]);

        app.update(Action::React);
        assert_eq!(
            app.focus,
            Focus::Conversation,
            "focus goes back where it was"
        );
        assert!(app.reaction_picker.is_none());
    }

    #[test]
    fn the_react_fallback_is_offered_only_for_the_one_message_it_can_reach() {
        let mut app = app_with_message();
        app.db_path = Some(PathBuf::from("/tmp/msgs-test.db"));
        app.update(Action::React);
        let fallback = app
            .reaction_picker
            .as_ref()
            .expect("picker")
            .fallback
            .clone()
            .expect("the newest incoming message can be reached both ways");
        assert_eq!(fallback.chat_rowid, 1);
        assert_eq!(fallback.db, PathBuf::from("/tmp/msgs-test.db"));
        app.update(Action::Cancel);

        // Somebody else writes after it: only the GUID route can reach the
        // older message now.
        let mut newer = message_row("EFGH-5678");
        newer.rowid = 6;
        app.message_rows.push(newer);
        app.messages.set_len(2);
        app.messages.selected = 0;
        app.update(Action::React);
        assert!(
            app.reaction_picker
                .as_ref()
                .expect("picker")
                .fallback
                .is_none()
        );
    }

    #[test]
    fn the_cursor_wraps_around_the_row_of_reactions() {
        let mut app = app_with_message();
        app.update(Action::React);
        app.update(Action::SelectPrev);
        assert_eq!(
            app.reaction_picker.as_ref().expect("picker").reaction(),
            REACTIONS[REACTIONS.len() - 1]
        );
        app.update(Action::SelectNext);
        assert_eq!(
            app.reaction_picker.as_ref().expect("picker").reaction(),
            REACTIONS[0]
        );
    }

    #[test]
    fn enter_hands_the_reaction_to_imsg_and_chips_it_on_at_once() {
        let mut app = app_with_message();
        app.update(Action::React);
        app.update(Action::SelectNext);
        app.update(Action::Activate);

        assert!(app.reaction_picker.is_none(), "the picker closes behind it");
        assert_eq!(app.pending_tapbacks.len(), 1);
        assert_eq!(app.pending_tapbacks[0].target_guid, "ABCD-1234");
        assert!(!app.pending_tapbacks[0].remove);

        let recorded = app.outbox.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].1.guid.as_deref(),
            Some("iMessage;-;+15550000000")
        );
        assert_eq!(
            recorded[0].2,
            Outgoing::Tapback {
                message_guid: "ABCD-1234".to_string(),
                part: 0,
                reaction: REACTIONS[1],
                remove: false,
                fallback: None,
            }
        );
        // The loaded page is still exactly what the database said.
        assert!(app.message_rows[0].tapbacks.is_empty());
    }

    #[test]
    fn a_digit_picks_a_reaction_and_sends_it_in_one_keystroke() {
        let mut app = app_with_message();
        app.update(Action::React);
        app.update(Action::Insert('4'));
        assert!(app.reaction_picker.is_none());
        let recorded = app.outbox.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].2,
            Outgoing::Tapback {
                message_guid: "ABCD-1234".to_string(),
                part: 0,
                reaction: REACTIONS[3],
                remove: false,
                fallback: None,
            }
        );

        // A digit outside the row does nothing at all.
        let mut app = app_with_message();
        app.update(Action::React);
        app.update(Action::Insert('9'));
        assert_eq!(app.focus, Focus::Reactions);
        assert!(app.outbox.recorded().is_empty());
    }

    #[test]
    fn choosing_a_reaction_you_already_gave_takes_it_back() {
        let mut app = app_with_message();
        app.message_rows[0].tapbacks = vec![mine(REACTIONS[2].kind())];
        app.update(Action::React);
        let picker = app.reaction_picker.as_ref().expect("picker");
        assert_eq!(
            picker.reaction(),
            REACTIONS[2],
            "the cursor starts on the reaction already standing"
        );
        app.update(Action::Activate);
        assert!(app.pending_tapbacks[0].remove);
        assert_eq!(
            app.outbox.recorded()[0].2,
            Outgoing::Tapback {
                message_guid: "ABCD-1234".to_string(),
                part: 0,
                reaction: REACTIONS[2],
                remove: true,
                fallback: None,
            }
        );
    }

    #[test]
    fn a_refused_reaction_takes_its_chip_back_down() {
        let mut app = app_with_message();
        app.update(Action::React);
        app.update(Action::Activate);
        let id = app.pending_tapbacks[0].id;

        app.outbox
            .answer(id, Err(SendError::Script("nope".to_string())));
        assert!(app.absorb_replies());
        assert!(app.pending_tapbacks.is_empty());
        let (text, is_error) = app.status.active_toast().expect("a toast");
        assert!(is_error);
        assert!(text.contains("could not react"));
    }

    #[test]
    fn a_reaction_that_landed_keeps_its_chip_until_the_database_agrees() {
        let mut app = app_with_message();
        app.update(Action::React);
        app.update(Action::Activate);
        let id = app.pending_tapbacks[0].id;
        app.outbox.answer(id, Ok(()));
        assert!(app.absorb_replies());
        assert_eq!(app.pending_tapbacks.len(), 1, "the chip stands");

        // The database catches up: the optimistic chip has nothing left to do.
        app.message_rows[0].tapbacks = vec![mine(REACTIONS[0].kind())];
        app.reconcile_tapbacks();
        assert!(app.pending_tapbacks.is_empty());
    }

    #[test]
    fn without_imsg_the_picker_explains_and_sends_nothing() {
        let mut app = app_with_message();
        app.update(Action::React);
        app.reaction_picker.as_mut().expect("picker").available = false;
        app.update(Action::Activate);
        assert!(app.outbox.recorded().is_empty());
        assert!(app.pending_tapbacks.is_empty());
        assert_eq!(app.focus, Focus::Reactions, "the picker stays up");
        let (text, is_error) = app.status.active_toast().expect("a toast");
        assert!(is_error);
        assert!(text.contains(send::IMSG_INSTALL));
    }

    #[test]
    fn an_echo_that_has_not_reached_messages_cannot_be_reacted_to() {
        let mut app = app_with_chat();
        compose(&mut app, "on my way");
        app.update(Action::Activate);
        app.focus = Focus::Conversation;
        app.update(Action::React);
        assert_eq!(app.focus, Focus::Conversation);
        assert!(app.reaction_picker.is_none());
        assert!(app.status.active_toast().is_some());
    }

    #[test]
    fn an_empty_or_blank_draft_sends_nothing() {
        let mut app = app_with_chat();
        app.update(Action::Activate);
        compose(&mut app, "   ");
        app.update(Action::Activate);
        assert!(app.pending.is_empty());
        assert!(app.outbox.recorded().is_empty());
    }

    #[test]
    fn a_send_with_no_conversation_open_says_so_instead_of_sending() {
        let mut app = app();
        app.outbox = Outbox::inert();
        app.focus = Focus::Composer;
        compose(&mut app, "hello?");
        app.update(Action::Activate);
        assert!(app.pending.is_empty());
        assert!(app.outbox.recorded().is_empty());
        assert_eq!(
            app.composer.text(),
            "hello?",
            "the draft is not thrown away"
        );
    }

    #[test]
    fn a_send_that_lands_keeps_the_echo_and_drops_the_note() {
        let mut app = app_with_chat();
        compose(&mut app, "on my way");
        app.update(Action::Activate);

        app.outbox.answer(app.pending[0].id, Ok(()));
        assert!(app.tick(), "the answer changes the screen");
        assert_eq!(app.pending[0].state, Delivery::Sent);
        assert_eq!(app.status.messages_app_running, Some(true));
        assert!(app.composer.is_empty());
    }

    #[test]
    fn a_refused_send_is_marked_failed_and_gives_the_draft_back() {
        let mut app = app_with_chat();
        compose(&mut app, "on my way");
        app.update(Action::Activate);

        app.outbox.answer(
            app.pending[0].id,
            Err(SendError::Script("Messages refused it".to_string())),
        );
        assert!(app.tick());

        assert_eq!(
            app.pending[0].state,
            Delivery::Failed("Messages refused it".to_string())
        );
        assert_eq!(app.composer.text(), "on my way", "the draft comes back");
        let (_, is_error) = app.status.active_toast().expect("a toast");
        assert!(is_error);
        // Nothing is coming for a failed send, so nothing keeps polling.
        assert!(!app.tick());
    }

    #[test]
    fn echoes_follow_their_own_conversation() {
        let mut app = app_with_chat();
        compose(&mut app, "on my way");
        app.update(Action::Activate);
        assert_eq!(app.message_rows.len(), 1);

        // A different chat's page has no echo on it.
        app.chats.selected = 0;
        app.pending[0].chat_rowid = 99;
        app.sync_pending_rows();
        assert!(app.message_rows.is_empty());
    }

    #[test]
    fn ctrl_a_asks_for_a_path_and_escape_leaves_the_draft_alone() {
        let mut app = app_with_chat();
        compose(&mut app, "look at this");
        app.update(Action::Attach);
        assert!(app.attach_prompt.is_some());
        assert_eq!(app.focus, Focus::Composer);

        compose(&mut app, "/tmp/x");
        assert_eq!(app.attach_prompt.as_ref().expect("prompt").text(), "/tmp/x");
        assert_eq!(
            app.composer.text(),
            "look at this",
            "the draft is untouched"
        );

        app.update(Action::Cancel);
        assert!(app.attach_prompt.is_none());
        assert_eq!(app.focus, Focus::Composer, "Esc closes the prompt only");
        assert_eq!(app.composer.text(), "look at this");
    }

    #[test]
    fn a_path_that_is_not_a_file_is_refused_and_kept_for_fixing() {
        let mut app = app_with_chat();
        app.update(Action::Attach);
        compose(&mut app, "/nonexistent/msgs-test/nothing.png");
        app.update(Action::Activate);

        assert!(app.pending.is_empty());
        assert!(app.outbox.recorded().is_empty());
        assert!(app.attach_prompt.is_some(), "the typed path survives");
        let (_, is_error) = app.status.active_toast().expect("a toast");
        assert!(is_error);
    }

    #[test]
    fn attaching_a_real_file_sends_it_and_echoes_its_name() {
        let path = std::env::temp_dir().join(format!("msgs-attach-{}.txt", std::process::id()));
        std::fs::write(&path, b"fixture").expect("write a scratch file");

        let mut app = app_with_chat();
        app.update(Action::Attach);
        compose(&mut app, &path.to_string_lossy());
        app.update(Action::Activate);

        assert!(app.attach_prompt.is_none());
        assert_eq!(app.pending.len(), 1);
        assert!(app.pending[0].is_file);
        let echo = app.message_rows.last().expect("an echo block");
        assert_eq!(
            echo.text.as_deref(),
            Some(format!("📎 {}", path.file_name().unwrap().to_string_lossy()).as_str())
        );
        assert_eq!(
            app.outbox.recorded()[0].2,
            Outgoing::File(path.clone()),
            "the file goes out as a file"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn r_quotes_the_selected_message_into_the_composer() {
        let mut app = app_with_chat();
        app.message_rows = vec![echo_row(&Pending::new(
            0,
            1,
            "dinner tonight?".to_string(),
            false,
            0,
        ))];
        app.messages.set_len(1);
        app.pending.clear();
        app.focus = Focus::Conversation;

        app.update(Action::QuoteReply);
        assert_eq!(app.focus, Focus::Composer);
        assert_eq!(app.composer.text(), "> dinner tonight?\n");
        // The cursor is on the empty line under the quote, ready to type.
        assert_eq!(app.composer.cursor(), app.composer.text().len());
    }

    /// A message carrying one attachment at `filename`, selected.
    fn app_with_attachment(filename: Option<String>) -> App {
        let mut app = app_with_chat();
        let mut row = echo_row(&Pending::new(0, 1, String::new(), false, 0));
        row.text = None;
        row.attachments = vec![AttachmentRef {
            rowid: 1,
            guid: "A1".to_string(),
            message_rowid: row.rowid,
            filename,
            mime_type: Some("image/png".to_string()),
            uti: None,
            transfer_name: Some("shot.png".to_string()),
            total_bytes: 2048,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        }];
        app.message_rows = vec![row];
        app.messages.set_len(1);
        app.pending.clear();
        app.focus = Focus::Conversation;
        app
    }

    #[test]
    fn o_and_s_say_so_when_there_is_no_attachment_to_act_on() {
        let mut app = app_with_chat();
        app.message_rows = vec![echo_row(&Pending::new(
            0,
            1,
            "just words".to_string(),
            false,
            0,
        ))];
        app.messages.set_len(1);
        app.pending.clear();
        app.focus = Focus::Conversation;

        app.update(Action::OpenAttachment);
        let (text, is_error) = app.status.active_toast().expect("a toast");
        assert!(text.contains("no attachment"), "{text}");
        assert!(!is_error, "a missing attachment is not an error");

        app.update(Action::SaveAttachment);
        let (text, _) = app.status.active_toast().expect("a toast");
        assert!(text.contains("no attachment"), "{text}");
    }

    #[test]
    fn an_undownloaded_attachment_is_neither_opened_nor_saved() {
        // No filename is what `chat.db` holds for bytes that never arrived.
        let mut app = app_with_attachment(None);
        assert!(app.selected_attachment().is_some());

        app.update(Action::OpenAttachment);
        let (text, _) = app.status.active_toast().expect("a toast");
        assert!(text.contains(media::NOT_DOWNLOADED), "{text}");

        app.update(Action::SaveAttachment);
        let (text, _) = app.status.active_toast().expect("a toast");
        assert!(text.contains(media::NOT_DOWNLOADED), "{text}");
    }

    #[test]
    fn a_path_that_points_nowhere_is_treated_as_undownloaded() {
        let missing = std::env::temp_dir().join("msgs-not-here.png");
        let mut app = app_with_attachment(Some(missing.display().to_string()));
        app.update(Action::OpenAttachment);
        let (text, _) = app.status.active_toast().expect("a toast");
        assert!(text.contains(media::NOT_DOWNLOADED), "{text}");
    }

    #[test]
    fn a_tilde_path_expands_and_a_relative_one_is_made_absolute() {
        let home = dirs::home_dir().expect("a home directory");
        assert_eq!(expand_path("~"), home);
        assert_eq!(expand_path("~/Pictures/x.png"), home.join("Pictures/x.png"));
        assert_eq!(
            expand_path("\"/tmp/quoted.png\""),
            PathBuf::from("/tmp/quoted.png")
        );
        assert!(expand_path("relative.png").is_absolute());
    }

    #[test]
    fn only_a_row_from_about_the_same_moment_can_be_the_echo() {
        let now = crate::db::raw_time(Local::now());
        assert!(recent_enough(now, now));
        assert!(recent_enough(now, now + 60_000_000_000));
        assert!(!recent_enough(now, now - 600_000_000_000));
        // A timestamp of zero means "never", which disqualifies nothing.
        assert!(recent_enough(0, now));
    }

    #[test]
    fn quit_stops_the_loop() {
        let mut app = app();
        assert!(!app.should_quit);
        app.update(Action::Quit);
        assert!(app.should_quit);
    }
}
