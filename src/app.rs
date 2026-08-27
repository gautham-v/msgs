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

use crate::config::Config;
use crate::db::{Chat, Db, DbError, MAX_PAGE, Message, PAGE, Source};
use crate::send::{self, Delivery, Outbox, Outgoing, Pending, SendError, Target};
use crate::theme::Theme;
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
}

impl Focus {
    /// Overlays float above the panes and take all keys while open.
    #[must_use]
    pub const fn is_overlay(self) -> bool {
        matches!(self, Self::Palette | Self::Help)
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
    /// Open the help modal.
    OpenHelp,
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
    /// React to the selected message.
    React,
    /// Copy the selected message to the clipboard.
    CopySelection,
    /// Open the first link in the selected message in the browser.
    OpenLink,
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

/// The whole application.
pub struct App {
    /// User config, as loaded at startup.
    pub config: Config,
    /// Colors, after config overrides.
    pub theme: Theme,
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
}

impl App {
    /// Build the app from a loaded config and any startup warnings.
    #[must_use]
    pub fn new(config: Config, mut warnings: Vec<String>) -> Self {
        let mut theme = Theme::default();
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
            outbox: Outbox::new(),
            next_send: 0,
            last_reconcile: None,
            reconcile_since: None,
            palette: TextField::default(),
            help_scroll: 0,
            status,
            panes: Panes::default(),
            hover: None,
            watcher: Watcher::off(),
            new_below: 0,
            last_snapshot: None,
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
            Ok(chats) => {
                self.status.unread_total = chats
                    .iter()
                    .map(|chat| usize::try_from(chat.unread_count).unwrap_or(0))
                    .sum();
                self.status.unread_chats = chats.iter().filter(|chat| chat.is_unread()).count();
                self.chat_rows = chats;
            }
            Err(err) => {
                self.status.error(format!("chat list: {}", err.summary()));
            }
        }
        self.refresh_anchored(true, anchor);
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
                now,
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
        let Some(target) = self.current_chat().map(Target::for_chat) else {
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
        let Some(target) = self.current_chat().map(Target::for_chat) else {
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
        if self.pending.is_empty() {
            self.reconcile_since = None;
            return false;
        }
        // A failed send stands until the user does something about it; nothing
        // is coming for it.
        if self
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
            // The chat list's preview line and ordering moved with it.
            self.reload_chats();
        }
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
        if self.pending.is_empty() {
            self.reconcile_since = None;
        }

        refreshed.appended = appended.len();
        refreshed.claimed = claimed.len();
        self.message_rows.extend(appended);
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
        if !self.pull_new_messages() {
            // Nothing for the open thread, but another conversation may have
            // gained a message: its row moves to the top of the list and its
            // preview and unread badge change with it.
            self.reload_chats();
        }
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
        if self.focus == Focus::ChatList && self.chat_filter.is_some() {
            Focus::Composer
        } else {
            self.focus
        }
    }

    /// Panes that can hold focus right now, in `Tab` order.
    fn focus_cycle(&self) -> Vec<Focus> {
        let mut cycle = Vec::with_capacity(3);
        if self.show_chat_list {
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
            _ => None,
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
            Action::SelectPrev => self.move_selection(-1),
            Action::SelectNext => self.move_selection(1),
            Action::PageUp => self.page(-1),
            Action::PageDown => self.page(1),
            Action::ToTop => self.jump(true),
            Action::ToBottom => self.jump(false),
            Action::Scroll(delta) => self.scroll(i64::from(delta)),
            Action::Activate => self.activate(),
            Action::OpenPalette => self.open_overlay(Focus::Palette),
            Action::OpenHelp => self.open_overlay(Focus::Help),
            Action::Cancel => self.cancel(),
            Action::StartFilter => self.start_filter(),
            Action::Insert(c) => self.edit(|field| field.insert(c)),
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
            Action::OpenAttachment => self.not_yet("Attachments arrive with the attachment pass"),
            Action::SaveAttachment => self.not_yet("Attachments arrive with the attachment pass"),
            Action::QuoteReply => self.quote_reply(),
            Action::React => self.not_yet("Tapbacks arrive with the imsg integration"),
            Action::CopySelection => self.copy_selection(),
            Action::OpenLink => self.open_selected_link(),
        }
        // Every path out of an action ends here, so a filter keystroke, an
        // arrow key, and a click all leave the chat list in the same state.
        self.refresh(!matches!(action, Action::Scroll(_)));
    }

    fn toggle_chat_list(&mut self) {
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
        match self.focus {
            Focus::Help => {
                let next = i64::from(self.help_scroll) + delta;
                self.help_scroll = u16::try_from(next.max(0)).unwrap_or(u16::MAX);
            }
            Focus::Conversation => self.move_message(delta),
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
            Focus::Palette => self.not_yet("The jump palette arrives with search"),
            Focus::Conversation => self.focus = Focus::Composer,
            Focus::Help => self.close_overlay(),
        }
    }

    fn cancel(&mut self) {
        match self.focus {
            Focus::Help | Focus::Palette => self.close_overlay(),
            Focus::ChatList => self.chat_filter = None,
            Focus::Composer => {
                // The attachment prompt is a layer in front of the composer, so
                // `Esc` closes it and leaves the draft where it was.
                if self.attach_prompt.take().is_none() {
                    self.focus = Focus::Conversation;
                }
            }
            Focus::Conversation => {
                if self.show_chat_list {
                    self.focus = Focus::ChatList;
                }
            }
        }
    }

    fn start_filter(&mut self) {
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

    fn not_yet(&mut self, what: &str) {
        self.status.toast(format!("{what} — not wired up yet"));
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
        dirty |= self.absorb_replies();
        dirty |= self.reconcile_pending();
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
