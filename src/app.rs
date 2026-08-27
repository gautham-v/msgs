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
use crate::db::{Chat, Db, DbError, Message, PAGE};
use crate::theme::Theme;
use crate::ui::Panes;
use crate::ui::conversation::{Hits, Measured, Scroll};
use crate::ui::message::{self, Ctx};

/// How long a toast stays on the status line.
const TOAST_TTL: Duration = Duration::from_secs(2);
/// Rows moved per wheel notch.
const WHEEL_ROWS: i16 = 3;
/// Tallest the composer grows before it scrolls internally.
pub const COMPOSER_MAX_LINES: u16 = 6;

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
            palette: TextField::default(),
            help_scroll: 0,
            status,
            panes: Panes::default(),
            hover: None,
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
            }
            Err(err) => {
                self.status.db = DbStatus::Unreadable(err.summary());
                self.db = None;
                self.db_error = Some(err);
                self.chat_rows.clear();
                self.message_rows.clear();
                self.refresh_chat_view();
            }
        }
    }

    /// Re-read the chat list and the unread totals on the status line.
    pub fn reload_chats(&mut self) {
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
        self.refresh_chat_view();
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
            // Narrowing the list must not drag the selection onto a different
            // conversation, so it follows the chat it was on where it can.
            let anchor = self.selected_chat().map(|chat| chat.rowid);
            self.visible_chats = visible;
            self.chats.set_len(self.visible_chats.len());
            if let Some(rowid) = anchor
                && let Some(position) = self
                    .visible_chats
                    .iter()
                    .position(|index| self.chat_rows[*index].rowid == rowid)
            {
                self.chats.selected = position;
            }
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
        // The newest message goes to the bottom edge, which needs a pane
        // height; the next frame has one.
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
        if self.measured.width == width
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
    fn active_field(&mut self) -> Option<&mut TextField> {
        match self.focus {
            Focus::Composer => Some(&mut self.composer),
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
            Action::Attach => self.not_yet("Attaching files arrives with the composer"),
            Action::OpenAttachment => self.not_yet("Attachments arrive with the attachment pass"),
            Action::SaveAttachment => self.not_yet("Attachments arrive with the attachment pass"),
            Action::QuoteReply => self.not_yet("Reply quoting arrives with the composer"),
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
                if self.composer.is_empty() {
                    return;
                }
                self.not_yet("Sending arrives with the Messages.app bridge");
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
            Focus::Composer => self.focus = Focus::Conversation,
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

    fn edit(&mut self, apply: impl FnOnce(&mut TextField)) {
        if let Some(field) = self.active_field() {
            apply(field);
        }
    }

    /// Backspace on an empty filter box closes it, which is what `Esc` would do.
    fn backspace(&mut self) {
        if self.focus == Focus::ChatList
            && self.chat_filter.as_ref().is_some_and(TextField::is_empty)
        {
            self.chat_filter = None;
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
        self.composer.line_count().clamp(1, COMPOSER_MAX_LINES) + 2
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
        self.status.tick()
    }
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

    #[test]
    fn quit_stops_the_loop() {
        let mut app = app();
        assert!(!app.should_quit);
        app.update(Action::Quit);
        assert!(app.should_quit);
    }
}
