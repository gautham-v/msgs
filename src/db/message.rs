//! The `message` table, plus the attachments and tapbacks hanging off it.
//!
//! Conversations are read a page at a time from the newest end backwards, so
//! opening a chat with 50,000 messages costs one bounded query. Each page then
//! costs two more: one for the attachments joined to the page, one for the
//! tapbacks aimed at it.
//!
//! Tapbacks are ordinary message rows with `associated_message_type` in the
//! 1000–3007 range pointing at another message's GUID, so the page query
//! excludes them and [`resolve_tapbacks`] folds them back onto their targets.
//!
//! Every query here windows and orders on `chat_message_join.message_id` rather
//! than on `message.ROWID`, even though the two hold the same number. The
//! join's primary key is `(chat_id, message_id)`, so SQLite can walk that index
//! straight to the newest page; asking for the same order by `message.ROWID`
//! reads every row of the conversation into a temporary b-tree first, which on
//! a 25,000-message thread is the difference between a third of a millisecond
//! and forty.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Local};
use imessage_database::util::streamtyped;
use rusqlite::types::Value;

use super::{Db, DbError, MAX_PAGE, local_time};

/// Placeholder standing in for an attachment inside a message body.
const ATTACHMENT_CHAR: char = '\u{FFFC}';
/// Placeholder standing in for an app payload inside a message body.
const APP_CHAR: char = '\u{FFFD}';

/// Columns the page query selects, in the order [`row_to_message`] reads them.
const COLUMNS: &str = "m.ROWID, m.guid, m.handle_id, h.id, h.service, m.service, \
     m.is_from_me, m.is_read, m.date, m.date_delivered, m.date_read, m.date_edited, \
     m.text, m.attributedBody, m.subject, m.reply_to_guid, m.thread_originator_guid, \
     m.item_type, m.group_action_type, m.group_title, m.other_handle";

/// Rows with an `associated_message_type` in this range react to another
/// message rather than being one.
const TAPBACK_RANGE: std::ops::RangeInclusive<i64> = 1000..=3007;

/// One row of `message`, joined to its chat, sender, attachments, and tapbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// `message.ROWID`. Monotonic, so it doubles as the paging cursor.
    pub rowid: i64,
    /// `message.guid`, the id replies and tapbacks point at.
    pub guid: String,
    /// The chat this row was read for.
    pub chat_rowid: i64,
    /// `handle.ROWID` of the sender, absent for messages you sent.
    pub handle_rowid: Option<i64>,
    /// `handle.id` of the sender: a phone number or email.
    pub handle: Option<String>,
    /// `iMessage`, `SMS`, or `RCS`.
    pub service: Option<String>,
    /// Whether you sent it.
    pub is_from_me: bool,
    /// Whether Messages considers it read.
    pub is_read: bool,
    /// Raw Messages timestamp; use [`Message::sent_at`].
    pub date: i64,
    /// Raw timestamp of delivery, `0` when never delivered.
    pub date_delivered: i64,
    /// Raw timestamp of the read receipt, `0` when unread or not reported.
    pub date_read: i64,
    /// Raw timestamp of the last edit, `0` when never edited.
    pub date_edited: i64,
    /// Whether the message has been edited since it was sent.
    pub is_edited: bool,
    /// Body text, from `text` or recovered from `attributedBody`.
    pub text: Option<String>,
    /// `message.subject`, which SMS threads occasionally carry.
    pub subject: Option<String>,
    /// Files sent with the message, in the order Messages stored them.
    pub attachments: Vec<AttachmentRef>,
    /// GUID of the message this one replies to, on the SMS path.
    pub reply_to_guid: Option<String>,
    /// GUID that opens the reply thread this message belongs to.
    pub thread_originator_guid: Option<String>,
    /// Reactions currently standing on this message.
    pub tapbacks: Vec<Tapback>,
    /// `message.item_type`: `0` for a normal message, other values for events.
    pub item_type: i64,
    /// `message.group_action_type`, read together with `item_type`.
    pub group_action_type: i64,
    /// New group name on a rename event.
    pub group_title: Option<String>,
    /// The other party on a join, leave, or number-change event.
    pub other_handle: Option<i64>,
    /// The group event this row announces, when it announces one.
    pub group_action: Option<GroupAction>,
}

impl Message {
    /// When it was sent, in local time.
    #[must_use]
    pub fn sent_at(&self) -> Option<DateTime<Local>> {
        local_time(self.date)
    }

    /// When it was delivered, in local time.
    #[must_use]
    pub fn delivered_at(&self) -> Option<DateTime<Local>> {
        local_time(self.date_delivered)
    }

    /// When the read receipt arrived, in local time.
    #[must_use]
    pub fn read_at(&self) -> Option<DateTime<Local>> {
        local_time(self.date_read)
    }

    /// When it was last edited, in local time.
    #[must_use]
    pub fn edited_at(&self) -> Option<DateTime<Local>> {
        local_time(self.date_edited)
    }

    /// Whether the row is an event line (a rename, a join, a leave) rather than
    /// something somebody typed.
    #[must_use]
    pub const fn is_announcement(&self) -> bool {
        self.item_type != 0
    }

    /// Whether there is nothing to draw in a bubble: no text, no attachments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.attachments.is_empty()
    }
}

/// A file sent with a message. The bytes stay on disk; this is the row that
/// points at them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    /// `attachment.ROWID`.
    pub rowid: i64,
    /// `attachment.guid`.
    pub guid: String,
    /// The message it was sent with.
    pub message_rowid: i64,
    /// `attachment.filename`, usually starting with `~/Library/Messages/…`.
    pub filename: Option<String>,
    /// MIME type, when Messages recorded one.
    pub mime_type: Option<String>,
    /// Uniform type identifier, e.g. `public.jpeg`.
    pub uti: Option<String>,
    /// The name the sender's device used.
    pub transfer_name: Option<String>,
    /// Size on disk in bytes, `0` when unknown.
    pub total_bytes: i64,
    /// `attachment.transfer_state`; `5` means the transfer finished.
    pub transfer_state: i64,
    /// Whether it is a sticker rather than a file the user sent.
    pub is_sticker: bool,
    /// Whether Messages hides it from the transcript.
    pub hide_attachment: bool,
}

/// The broad sort of file an attachment is, which is all a one-line preview or
/// a file chip needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// A picture, shown inline where the terminal can draw one.
    Image,
    /// A video clip.
    Video,
    /// A voice message or other audio.
    Audio,
    /// Anything else.
    File,
}

impl AttachmentKind {
    /// The emoji the mockup puts in front of the file in a preview line.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Image => "📷",
            Self::Video => "🎬",
            Self::Audio => "🎤",
            Self::File => "📄",
        }
    }

    /// The word to show when there is no filename worth showing.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Image => "Photo",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::File => "File",
        }
    }
}

impl AttachmentRef {
    /// Which sort of file this is, from its MIME type and then its UTI.
    #[must_use]
    pub fn kind(&self) -> AttachmentKind {
        if let Some(mime) = self.mime_type.as_deref() {
            if mime.starts_with("image/") {
                return AttachmentKind::Image;
            }
            if mime.starts_with("video/") {
                return AttachmentKind::Video;
            }
            if mime.starts_with("audio/") {
                return AttachmentKind::Audio;
            }
        }
        match self.uti.as_deref() {
            Some(uti) if uti.contains("image") => AttachmentKind::Image,
            Some(uti) if uti.contains("movie") || uti.contains("video") => AttachmentKind::Video,
            Some(uti) if uti.contains("audio") => AttachmentKind::Audio,
            _ => AttachmentKind::File,
        }
    }

    /// The file's absolute path, with a leading `~` expanded.
    ///
    /// `None` when the row has no filename, which happens for attachments that
    /// were never downloaded to this Mac.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        let raw = self.filename.as_deref()?;
        let Some(rest) = raw.strip_prefix("~/") else {
            return Some(PathBuf::from(raw));
        };
        Some(dirs::home_dir().unwrap_or_default().join(rest))
    }

    /// Whether the bytes are present on this Mac.
    #[must_use]
    pub fn is_downloaded(&self) -> bool {
        self.path().is_some_and(|path| path.exists())
    }

    /// Whether it can be shown inline as a picture.
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.kind() == AttachmentKind::Image
    }

    /// The best name to show for the file.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.transfer_name.as_deref().or_else(|| {
            self.filename
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
        })
    }
}

/// Which reaction a tapback carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapbackKind {
    /// Heart.
    Loved,
    /// Thumbs up.
    Liked,
    /// Thumbs down.
    Disliked,
    /// Laughing.
    Laughed,
    /// Exclamation marks.
    Emphasized,
    /// Question marks.
    Questioned,
    /// Any emoji, on newer systems.
    Emoji(String),
    /// A sticker stuck onto the message.
    Sticker,
}

impl TapbackKind {
    /// One grapheme standing for the reaction, for the chips under a message.
    #[must_use]
    pub fn glyph(&self) -> &str {
        match self {
            Self::Loved => "❤️",
            Self::Liked => "👍",
            Self::Disliked => "👎",
            Self::Laughed => "😂",
            Self::Emphasized => "‼️",
            Self::Questioned => "❓",
            Self::Emoji(emoji) => emoji,
            Self::Sticker => "🩹",
        }
    }
}

/// Whether a tapback row adds a reaction or takes one away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapbackAction {
    /// The reaction was applied.
    Added,
    /// A previously applied reaction was removed.
    Removed,
}

/// Classify `associated_message_type`, with the custom emoji when there is one.
///
/// Returns `None` for values outside the tapback range, which is how the page
/// query and this function agree on what a tapback is.
#[must_use]
pub fn classify_tapback(kind: i64, emoji: Option<&str>) -> Option<(TapbackAction, TapbackKind)> {
    let custom = || {
        emoji.map_or(TapbackKind::Sticker, |emoji| {
            TapbackKind::Emoji(emoji.to_string())
        })
    };
    let pair = match kind {
        1000 | 2007 => (TapbackAction::Added, TapbackKind::Sticker),
        2000 => (TapbackAction::Added, TapbackKind::Loved),
        2001 => (TapbackAction::Added, TapbackKind::Liked),
        2002 => (TapbackAction::Added, TapbackKind::Disliked),
        2003 => (TapbackAction::Added, TapbackKind::Laughed),
        2004 => (TapbackAction::Added, TapbackKind::Emphasized),
        2005 => (TapbackAction::Added, TapbackKind::Questioned),
        2006 => (TapbackAction::Added, custom()),
        3007 => (TapbackAction::Removed, TapbackKind::Sticker),
        3000 => (TapbackAction::Removed, TapbackKind::Loved),
        3001 => (TapbackAction::Removed, TapbackKind::Liked),
        3002 => (TapbackAction::Removed, TapbackKind::Disliked),
        3003 => (TapbackAction::Removed, TapbackKind::Laughed),
        3004 => (TapbackAction::Removed, TapbackKind::Emphasized),
        3005 => (TapbackAction::Removed, TapbackKind::Questioned),
        3006 => (TapbackAction::Removed, custom()),
        _ => return None,
    };
    Some(pair)
}

/// One reaction row, before or after it has been folded onto its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tapback {
    /// `message.ROWID` of the tapback row itself.
    pub rowid: i64,
    /// GUID of the message being reacted to, prefix already stripped.
    pub target_guid: String,
    /// Which part of a multi-part message it lands on; `0` for plain text.
    pub target_part: usize,
    /// Add or remove.
    pub action: TapbackAction,
    /// Which reaction.
    pub kind: TapbackKind,
    /// Whether you were the one reacting.
    pub is_from_me: bool,
    /// `handle.ROWID` of whoever reacted, absent when it was you.
    pub handle_rowid: Option<i64>,
    /// `handle.id` of whoever reacted.
    pub handle: Option<String>,
    /// Raw Messages timestamp of the reaction.
    pub date: i64,
}

impl Tapback {
    /// Whoever reacted, as a key that is stable inside one conversation.
    fn author(&self) -> (bool, i64) {
        (self.is_from_me, self.handle_rowid.unwrap_or(0))
    }
}

/// Fold a chat's tapback rows into the reactions that currently stand.
///
/// Messages keeps only the latest state per person per target, but a removal
/// can still be sitting in the table, so the last row wins per
/// `(target, part, author)` and removals drop out. The result is keyed by
/// target GUID with each target's reactions in the order they were added.
#[must_use]
pub fn resolve_tapbacks(mut rows: Vec<Tapback>) -> HashMap<String, Vec<Tapback>> {
    rows.sort_by_key(|tapback| tapback.rowid);

    let mut latest: HashMap<(String, usize, (bool, i64)), Tapback> = HashMap::new();
    for tapback in rows {
        let key = (
            tapback.target_guid.clone(),
            tapback.target_part,
            tapback.author(),
        );
        latest.insert(key, tapback);
    }

    let mut standing: Vec<Tapback> = latest
        .into_values()
        .filter(|tapback| tapback.action == TapbackAction::Added)
        .collect();
    standing.sort_by_key(|tapback| tapback.rowid);

    let mut by_target: HashMap<String, Vec<Tapback>> = HashMap::new();
    for tapback in standing {
        by_target
            .entry(tapback.target_guid.clone())
            .or_default()
            .push(tapback);
    }
    by_target
}

/// The group event a row announces, from `item_type` and `group_action_type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupAction {
    /// Somebody was added, by `handle.ROWID`.
    ParticipantAdded(i64),
    /// Somebody was removed, by `handle.ROWID`.
    ParticipantRemoved(i64),
    /// Somebody left on their own.
    ParticipantLeft,
    /// The group was renamed.
    NameChange(String),
    /// The group photo changed.
    IconChanged,
    /// The group photo was removed.
    IconRemoved,
    /// Somebody's number changed, by `handle.ROWID`.
    PhoneNumberChanged(i64),
}

impl GroupAction {
    /// Read the event out of a message row, or `None` if it is not one.
    ///
    /// The mapping follows `imessage-database`'s reading of the same columns.
    #[must_use]
    pub fn from_row(
        item_type: i64,
        action_type: i64,
        other_handle: Option<i64>,
        sender: Option<i64>,
        group_title: Option<&str>,
    ) -> Option<Self> {
        match (item_type, action_type, other_handle) {
            (1, 0, Some(who)) if sender == Some(who) => Some(Self::PhoneNumberChanged(who)),
            (1, 0, Some(who)) => Some(Self::ParticipantAdded(who)),
            (1, 1, Some(who)) => Some(Self::ParticipantRemoved(who)),
            (2, _, _) => Some(Self::NameChange(
                group_title.unwrap_or_default().to_string(),
            )),
            (3, 0, _) => Some(Self::ParticipantLeft),
            (3, 1, _) => Some(Self::IconChanged),
            (3, 2, _) => Some(Self::IconRemoved),
            _ => None,
        }
    }
}

/// Recover the plain body of a message.
///
/// `text` is authoritative when it holds anything; when it is null or holds
/// only attachment placeholders, the body is dug out of the `attributedBody`
/// typedstream blob instead.
#[must_use]
pub fn body_text(text: Option<&str>, attributed_body: Option<&[u8]>) -> Option<String> {
    if let Some(text) = text {
        let cleaned = clean_body(text);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    let blob = attributed_body?;
    let parsed = streamtyped::parse(blob.to_vec()).ok()?;
    let cleaned = clean_body(&parsed);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Drop the placeholders that stand in for attachments and app payloads.
fn clean_body(raw: &str) -> String {
    raw.replace([ATTACHMENT_CHAR, APP_CHAR], "")
        .trim()
        .to_string()
}

/// Split `associated_message_guid` into the body part index and the target GUID.
///
/// The column is written as `p:<part>/<guid>` for ordinary messages, `bp:<guid>`
/// for app and link bubbles, and occasionally as a bare GUID.
#[must_use]
pub fn split_association(raw: &str) -> Option<(usize, &str)> {
    if let Some(rest) = raw.strip_prefix("p:") {
        let (part, guid) = rest.split_once('/')?;
        return Some((part.parse().unwrap_or(0), guid.get(0..36).unwrap_or(guid)));
    }
    if let Some(rest) = raw.strip_prefix("bp:") {
        return Some((0, rest.get(0..36).unwrap_or(rest)));
    }
    (!raw.is_empty()).then(|| (0, raw.get(0..36).unwrap_or(raw)))
}

impl Db {
    /// The page of messages in `chat_rowid` ending just before `before_rowid`.
    ///
    /// `before_rowid` is `None` for the newest page and the smallest `rowid` of
    /// the page you already have for every page above it, which is what the
    /// conversation pane scrolls upward with. The result is oldest first.
    ///
    /// # Errors
    ///
    /// Fails if any of the three queries behind a page fails.
    pub fn messages_before(
        &self,
        chat_rowid: i64,
        before_rowid: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Message>, DbError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM message m \
             JOIN chat_message_join j ON j.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id = ?1 AND (?2 IS NULL OR j.message_id < ?2) \
               AND COALESCE(m.associated_message_type, 0) = 0 \
             ORDER BY j.message_id DESC LIMIT ?3"
        );
        let limit = clamp_limit(limit);
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement
            .query_map(rusqlite::params![chat_rowid, before_rowid, limit], |row| {
                row_to_message(row, chat_rowid)
            })?;
        let mut page = rows.collect::<Result<Vec<_>, _>>()?;
        page.reverse();
        self.decorate(chat_rowid, &mut page)?;
        Ok(page)
    }

    /// The messages in `chat_rowid` newer than `after_rowid`, oldest first.
    ///
    /// This is what the live-update pass re-queries after `chat.db` changes.
    ///
    /// # Errors
    ///
    /// Fails if any of the three queries behind a page fails.
    pub fn messages_after(
        &self,
        chat_rowid: i64,
        after_rowid: i64,
        limit: usize,
    ) -> Result<Vec<Message>, DbError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM message m \
             JOIN chat_message_join j ON j.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id = ?1 AND j.message_id > ?2 \
               AND COALESCE(m.associated_message_type, 0) = 0 \
             ORDER BY j.message_id ASC LIMIT ?3"
        );
        let limit = clamp_limit(limit);
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement
            .query_map(rusqlite::params![chat_rowid, after_rowid, limit], |row| {
                row_to_message(row, chat_rowid)
            })?;
        let mut page = rows.collect::<Result<Vec<_>, _>>()?;
        self.decorate(chat_rowid, &mut page)?;
        Ok(page)
    }

    /// The highest `message.ROWID` in the database, or `0` when it is empty.
    ///
    /// The live-update pass keeps this as its watermark.
    ///
    /// # Errors
    ///
    /// Fails if `message` cannot be read.
    pub fn max_message_rowid(&self) -> Result<i64, DbError> {
        Ok(self
            .conn()
            .query_row("SELECT COALESCE(MAX(ROWID), 0) FROM message", [], |row| {
                row.get(0)
            })?)
    }

    /// How many files, and how many of those are pictures, a chat holds.
    ///
    /// One aggregate query, so the conversation header can say `38 photos`
    /// without walking the thread. Sticker and hidden rows are left out, the
    /// same way the transcript leaves them out.
    ///
    /// # Errors
    ///
    /// Fails if the attachment tables cannot be read.
    pub fn attachment_counts(&self, chat_rowid: i64) -> Result<(i64, i64), DbError> {
        let sql = "SELECT COUNT(*), \
                          COALESCE(SUM(CASE WHEN a.mime_type LIKE 'image/%' \
                                              OR a.uti LIKE '%image%' \
                                            THEN 1 ELSE 0 END), 0) \
                   FROM chat_message_join j \
                   JOIN message_attachment_join k ON k.message_id = j.message_id \
                   JOIN attachment a ON a.ROWID = k.attachment_id \
                   WHERE j.chat_id = ?1 \
                     AND COALESCE(a.is_sticker, 0) = 0 \
                     AND COALESCE(a.hide_attachment, 0) = 0";
        Ok(self
            .conn()
            .query_row(sql, [chat_rowid], |row| Ok((row.get(0)?, row.get(1)?)))?)
    }

    /// Hang attachments and tapbacks off a page of messages.
    fn decorate(&self, chat_rowid: i64, page: &mut [Message]) -> Result<(), DbError> {
        if page.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = page.iter().map(|message| message.rowid).collect();
        let mut attachments = self.attachments_by_message(&ids)?;
        let mut tapbacks = self.tapbacks_for(chat_rowid, page)?;
        for message in page.iter_mut() {
            message.attachments = attachments.remove(&message.rowid).unwrap_or_default();
            message.tapbacks = tapbacks.remove(&message.guid).unwrap_or_default();
        }
        Ok(())
    }

    /// Attachments hanging off the given messages, keyed by message `ROWID`.
    ///
    /// The chat list uses this too, for the one message it previews per chat,
    /// so both callers share a single query shape. At most [`MAX_PAGE`] ids are
    /// read in one call, which is what keeps the `IN (…)` list bounded; callers
    /// with more than that chunk their ids.
    ///
    /// # Errors
    ///
    /// Fails if `attachment` or `message_attachment_join` cannot be read.
    pub fn attachments_by_message(
        &self,
        message_rowids: &[i64],
    ) -> Result<HashMap<i64, Vec<AttachmentRef>>, DbError> {
        if message_rowids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<Value> = message_rowids
            .iter()
            .take(MAX_PAGE)
            .map(|rowid| Value::Integer(*rowid))
            .collect();
        let sql = format!(
            "SELECT k.message_id, a.ROWID, a.guid, a.filename, a.mime_type, a.uti, \
                    a.transfer_name, a.total_bytes, a.transfer_state, a.is_sticker, \
                    a.hide_attachment \
             FROM message_attachment_join k \
             JOIN attachment a ON a.ROWID = k.attachment_id \
             WHERE k.message_id IN ({}) \
             ORDER BY k.message_id, k.attachment_id",
            placeholders(ids.len())
        );

        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(ids), |row| {
            let message_rowid: i64 = row.get(0)?;
            Ok(AttachmentRef {
                message_rowid,
                rowid: row.get(1)?,
                guid: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                filename: row.get(3)?,
                mime_type: row.get(4)?,
                uti: row.get(5)?,
                transfer_name: row.get(6)?,
                total_bytes: row.get::<_, Option<i64>>(7)?.unwrap_or_default(),
                transfer_state: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                is_sticker: row.get::<_, Option<i64>>(9)?.unwrap_or_default() != 0,
                hide_attachment: row.get::<_, Option<i64>>(10)?.unwrap_or_default() != 0,
            })
        })?;

        let mut by_message: HashMap<i64, Vec<AttachmentRef>> = HashMap::new();
        for attachment in rows {
            let attachment = attachment?;
            by_message
                .entry(attachment.message_rowid)
                .or_default()
                .push(attachment);
        }
        Ok(by_message)
    }

    /// Reactions standing on a page, keyed by target GUID.
    fn tapbacks_for(
        &self,
        chat_rowid: i64,
        page: &[Message],
    ) -> Result<HashMap<String, Vec<Tapback>>, DbError> {
        // Nobody can react to a message that does not exist yet, so every
        // tapback aimed at this page has a higher `ROWID` than the page's
        // oldest message. That bound is what keeps this query off the rest of
        // a long conversation.
        let floor = page.iter().map(|message| message.rowid).min().unwrap_or(0);
        let mut params: Vec<Value> = Vec::with_capacity(page.len() + 2);
        params.push(Value::Integer(chat_rowid));
        params.push(Value::Integer(floor));
        params.extend(page.iter().map(|message| Value::Text(message.guid.clone())));

        // The stored GUID is prefixed (`p:0/…`), so match on its tail rather
        // than on equality, and split the prefix off in Rust afterwards.
        let emoji = if self.schema().tapback_emoji {
            "m.associated_message_emoji"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT m.ROWID, m.is_from_me, m.handle_id, h.id, m.date, \
                    m.associated_message_guid, m.associated_message_type, \
                    {emoji} \
             FROM message m \
             JOIN chat_message_join j ON j.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id = ?1 AND j.message_id > ?2 \
               AND m.associated_message_guid IS NOT NULL \
               AND COALESCE(m.associated_message_type, 0) BETWEEN {} AND {} \
               AND SUBSTR(m.associated_message_guid, -36) IN ({}) \
             ORDER BY j.message_id",
            TAPBACK_RANGE.start(),
            TAPBACK_RANGE.end(),
            placeholders_from(page.len(), 3)
        );

        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
            let raw_guid: String = row.get::<_, Option<String>>(5)?.unwrap_or_default();
            let kind: i64 = row.get::<_, Option<i64>>(6)?.unwrap_or_default();
            let emoji: Option<String> = row.get(7).unwrap_or(None);
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?.unwrap_or_default() != 0,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
                raw_guid,
                kind,
                emoji,
            ))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (rowid, is_from_me, handle_rowid, handle, date, raw_guid, kind, emoji) = row?;
            let Some((target_part, target_guid)) = split_association(&raw_guid) else {
                continue;
            };
            let Some((action, kind)) = classify_tapback(kind, emoji.as_deref()) else {
                continue;
            };
            events.push(Tapback {
                rowid,
                target_guid: target_guid.to_string(),
                target_part,
                action,
                kind,
                is_from_me,
                handle_rowid: (!is_from_me).then_some(handle_rowid).flatten(),
                handle: if is_from_me { None } else { handle },
                date,
            });
        }
        Ok(resolve_tapbacks(events))
    }
}

/// Read one page row into a [`Message`], leaving attachments and tapbacks empty.
fn row_to_message(row: &rusqlite::Row<'_>, chat_rowid: i64) -> rusqlite::Result<Message> {
    let handle_rowid: Option<i64> = row.get::<_, Option<i64>>(2)?.filter(|id| *id != 0);
    let is_from_me = row.get::<_, Option<i64>>(6)?.unwrap_or_default() != 0;
    let date_edited = row.get::<_, Option<i64>>(11)?.unwrap_or_default();
    let text: Option<String> = row.get(12)?;
    let attributed: Option<Vec<u8>> = row.get(13)?;
    let item_type = row.get::<_, Option<i64>>(17)?.unwrap_or_default();
    let group_action_type = row.get::<_, Option<i64>>(18)?.unwrap_or_default();
    let group_title: Option<String> = row.get(19)?;
    let other_handle: Option<i64> = row.get::<_, Option<i64>>(20)?.filter(|id| *id != 0);

    Ok(Message {
        rowid: row.get(0)?,
        guid: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        chat_rowid,
        handle_rowid,
        handle: row.get(3)?,
        service: row.get::<_, Option<String>>(5)?.or(row.get(4)?),
        is_from_me,
        is_read: row.get::<_, Option<i64>>(7)?.unwrap_or_default() != 0,
        date: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
        date_delivered: row.get::<_, Option<i64>>(9)?.unwrap_or_default(),
        date_read: row.get::<_, Option<i64>>(10)?.unwrap_or_default(),
        date_edited,
        is_edited: date_edited != 0,
        text: body_text(text.as_deref(), attributed.as_deref()),
        subject: row.get(14)?,
        attachments: Vec::new(),
        reply_to_guid: row.get(15)?,
        thread_originator_guid: row.get(16)?,
        tapbacks: Vec::new(),
        item_type,
        group_action_type,
        group_action: GroupAction::from_row(
            item_type,
            group_action_type,
            other_handle,
            handle_rowid,
            group_title.as_deref(),
        ),
        group_title,
        other_handle,
    })
}

/// Keep a page bounded so the `IN (…)` lists stay small.
fn clamp_limit(limit: usize) -> i64 {
    i64::try_from(limit.clamp(1, MAX_PAGE)).unwrap_or(1)
}

/// `?1, ?2, …` for `count` values.
fn placeholders(count: usize) -> String {
    placeholders_from(count, 1)
}

/// `?first, ?first+1, …` for `count` values.
fn placeholders_from(count: usize, first: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tapback(rowid: i64, action: TapbackAction, kind: TapbackKind, author: i64) -> Tapback {
        Tapback {
            rowid,
            target_guid: "TARGET".to_string(),
            target_part: 0,
            action,
            kind,
            is_from_me: author == 0,
            handle_rowid: (author != 0).then_some(author),
            handle: None,
            date: rowid,
        }
    }

    #[test]
    fn association_guids_split_into_part_and_target() {
        let guid = "11111111-2222-3333-4444-555555555555";
        assert_eq!(split_association(guid), Some((0, guid)));
        assert_eq!(split_association(&format!("p:0/{guid}")), Some((0, guid)));
        assert_eq!(split_association(&format!("p:2/{guid}")), Some((2, guid)));
        assert_eq!(split_association(&format!("bp:{guid}")), Some((0, guid)));
        assert_eq!(split_association(""), None);
    }

    #[test]
    fn tapback_types_map_to_add_and_remove_pairs() {
        assert_eq!(
            classify_tapback(2000, None),
            Some((TapbackAction::Added, TapbackKind::Loved))
        );
        assert_eq!(
            classify_tapback(3005, None),
            Some((TapbackAction::Removed, TapbackKind::Questioned))
        );
        assert_eq!(
            classify_tapback(2006, Some("🐙")),
            Some((TapbackAction::Added, TapbackKind::Emoji("🐙".to_string())))
        );
        assert!(classify_tapback(0, None).is_none());
        assert!(classify_tapback(4000, None).is_none());
    }

    #[test]
    fn a_removal_cancels_the_reaction_it_follows() {
        let resolved = resolve_tapbacks(vec![
            tapback(1, TapbackAction::Added, TapbackKind::Liked, 7),
            tapback(2, TapbackAction::Removed, TapbackKind::Liked, 7),
        ]);
        assert!(!resolved.contains_key("TARGET"));
    }

    #[test]
    fn re_adding_after_a_removal_stands_again() {
        let resolved = resolve_tapbacks(vec![
            tapback(1, TapbackAction::Added, TapbackKind::Liked, 7),
            tapback(2, TapbackAction::Removed, TapbackKind::Liked, 7),
            tapback(3, TapbackAction::Added, TapbackKind::Loved, 7),
        ]);
        let standing = resolved.get("TARGET").expect("one reaction stands");
        assert_eq!(standing.len(), 1);
        assert_eq!(standing[0].kind, TapbackKind::Loved);
    }

    #[test]
    fn each_person_holds_one_reaction_and_they_keep_their_order() {
        let resolved = resolve_tapbacks(vec![
            tapback(1, TapbackAction::Added, TapbackKind::Liked, 7),
            tapback(2, TapbackAction::Added, TapbackKind::Laughed, 8),
            tapback(3, TapbackAction::Added, TapbackKind::Loved, 7),
            tapback(4, TapbackAction::Added, TapbackKind::Liked, 0),
        ]);
        let standing = resolved.get("TARGET").expect("three reactions stand");
        assert_eq!(standing.len(), 3);
        assert_eq!(standing[0].kind, TapbackKind::Laughed);
        assert_eq!(standing[1].kind, TapbackKind::Loved);
        assert!(standing[2].is_from_me);
    }

    #[test]
    fn reactions_on_different_parts_do_not_replace_each_other() {
        let mut second = tapback(2, TapbackAction::Added, TapbackKind::Loved, 7);
        second.target_part = 1;
        let resolved = resolve_tapbacks(vec![
            tapback(1, TapbackAction::Added, TapbackKind::Liked, 7),
            second,
        ]);
        assert_eq!(resolved.get("TARGET").map(Vec::len), Some(2));
    }

    #[test]
    fn placeholder_lists_are_one_based_and_can_start_late() {
        assert_eq!(placeholders(3), "?1, ?2, ?3");
        assert_eq!(placeholders_from(2, 5), "?5, ?6");
    }

    #[test]
    fn group_actions_read_out_of_the_two_type_columns() {
        assert_eq!(
            GroupAction::from_row(1, 0, Some(4), Some(9), None),
            Some(GroupAction::ParticipantAdded(4))
        );
        assert_eq!(
            GroupAction::from_row(1, 0, Some(4), Some(4), None),
            Some(GroupAction::PhoneNumberChanged(4))
        );
        assert_eq!(
            GroupAction::from_row(1, 1, Some(4), Some(9), None),
            Some(GroupAction::ParticipantRemoved(4))
        );
        assert_eq!(
            GroupAction::from_row(2, 0, None, None, Some("Trip")),
            Some(GroupAction::NameChange("Trip".to_string()))
        );
        assert_eq!(
            GroupAction::from_row(3, 0, None, None, None),
            Some(GroupAction::ParticipantLeft)
        );
        assert_eq!(GroupAction::from_row(0, 0, None, None, None), None);
    }

    #[test]
    fn body_text_prefers_the_text_column() {
        assert_eq!(
            body_text(Some("plain"), Some(b"ignored")),
            Some("plain".to_string())
        );
    }

    #[test]
    fn a_body_of_only_placeholders_is_no_body_at_all() {
        assert_eq!(body_text(Some("\u{FFFC}"), None), None);
        assert_eq!(body_text(Some("   "), None), None);
        assert_eq!(body_text(None, None), None);
    }

    #[test]
    fn placeholders_are_stripped_from_a_body_that_also_has_words() {
        assert_eq!(
            body_text(Some("\u{FFFC}look"), None),
            Some("look".to_string())
        );
    }

    #[test]
    fn an_undownloaded_attachment_has_no_path() {
        let attachment = AttachmentRef {
            rowid: 1,
            guid: "A".to_string(),
            message_rowid: 1,
            filename: None,
            mime_type: Some("image/png".to_string()),
            uti: None,
            transfer_name: Some("shot.png".to_string()),
            total_bytes: 10,
            transfer_state: 0,
            is_sticker: false,
            hide_attachment: false,
        };
        assert!(attachment.path().is_none());
        assert!(!attachment.is_downloaded());
        assert!(attachment.is_image());
        assert_eq!(attachment.display_name(), Some("shot.png"));
    }

    #[test]
    fn a_tilde_path_expands_to_the_home_directory() {
        let attachment = AttachmentRef {
            rowid: 1,
            guid: "A".to_string(),
            message_rowid: 1,
            filename: Some("~/Library/Messages/Attachments/x/file.pdf".to_string()),
            mime_type: Some("application/pdf".to_string()),
            uti: None,
            transfer_name: None,
            total_bytes: 10,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        };
        let path = attachment.path().expect("a path");
        assert!(path.is_absolute());
        assert!(!path.to_string_lossy().starts_with('~'));
        assert!(!attachment.is_image());
        assert_eq!(attachment.display_name(), Some("file.pdf"));
    }

    #[test]
    fn every_tapback_kind_has_a_glyph() {
        for kind in [
            TapbackKind::Loved,
            TapbackKind::Liked,
            TapbackKind::Disliked,
            TapbackKind::Laughed,
            TapbackKind::Emphasized,
            TapbackKind::Questioned,
            TapbackKind::Sticker,
            TapbackKind::Emoji("🎉".to_string()),
        ] {
            assert!(!kind.glyph().is_empty());
        }
    }
}
