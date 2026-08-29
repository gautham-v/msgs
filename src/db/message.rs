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
use std::io::Read as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use imessage_database::message_types::url::URLMessage;
use imessage_database::message_types::variants::BalloonProvider;
use imessage_database::util::plist::parse_ns_keyed_archiver;
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
     m.item_type, m.group_action_type, m.group_title, m.other_handle, m.error";

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
    /// Messages' own `error` code: nonzero when a message of yours could not
    /// be delivered — the red `Not Delivered` under a bubble in Messages.app.
    pub error: i64,
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
    /// The preview Messages.app already built for a link in this message.
    pub link_preview: Option<LinkPreview>,
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

/// `message.balloon_bundle_id` on a message Messages drew a link preview for.
pub const URL_BALLOON: &str = "com.apple.messages.URLBalloonProvider";

/// Where the link's picture is, counted in the message's own attachments.
const SUBSTITUTE_INDEX: &str = "richLinkImageAttachmentSubstituteIndex";

/// What Messages.app already knows about a link somebody sent.
///
/// When a message contains a URL, Messages fetches the page once, on the
/// sending or receiving device, and archives what it found in
/// `message.payload_data`: an `NSKeyedArchiver` plist holding the page's title,
/// summary, and site name, with the pictures written beside it as ordinary
/// `attachment` rows. msgs only reads that archive. **Nothing here opens a
/// socket** — a link whose preview Messages never built simply has none, and a
/// preview stays exactly as stale as Messages left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreview {
    /// The page the preview is of.
    pub url: Option<String>,
    /// The page's `<og:title>`.
    pub title: Option<String>,
    /// The site the page belongs to, when the page named one.
    pub site_name: Option<String>,
    /// The page's `<og:description>`, as one paragraph.
    pub summary: Option<String>,
    /// The picture Messages stored for it: one of the message's own
    /// attachments, typed by what its first bytes actually are, so it goes
    /// through the same [`crate::media::Images`] cache as any photo.
    pub image: Option<AttachmentRef>,
}

impl LinkPreview {
    /// The host of [`LinkPreview::url`], `www.` dropped.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        let raw = self.url.as_deref()?;
        let rest = raw.split_once("://").map_or(raw, |(_, rest)| rest);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
        let host = host.split_once(':').map_or(host, |(host, _)| host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        (!host.is_empty()).then_some(host)
    }

    /// What to call the site: the name the page gave, else its host.
    #[must_use]
    pub fn site(&self) -> Option<&str> {
        self.site_name.as_deref().or_else(|| self.host())
    }

    /// Whether the preview says nothing the URL line does not already say.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.title.is_none() && self.summary.is_none() && self.image.is_none()
    }
}

/// Read the preview out of one `payload_data` blob.
///
/// `attachments` are the message's own attachment rows, in the order Messages
/// stored them, because the archive points at its picture by position in that
/// list rather than by name. Anything unparseable — a payload from another
/// balloon, a truncated archive, an image type nothing here can decode — is
/// `None`, silently: a link preview is a nicety and never an error.
#[must_use]
pub fn parse_link_preview(payload: &[u8], attachments: &[AttachmentRef]) -> Option<LinkPreview> {
    let raw = plist::Value::from_reader(std::io::Cursor::new(payload)).ok()?;
    let resolved = parse_ns_keyed_archiver(&raw).ok()?;
    let balloon = URLMessage::from_map(&resolved).ok()?;
    // An unloaded placeholder is what Messages writes before it has fetched
    // anything. There is nothing in it to show.
    if balloon.placeholder {
        return None;
    }

    let text = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let preview = LinkPreview {
        url: text(balloon.url.or(balloon.original_url)),
        title: text(balloon.title),
        site_name: text(balloon.site_name),
        summary: text(balloon.summary),
        image: image_index(&raw)
            .and_then(|index| attachments.get(index))
            .and_then(preview_picture),
    };
    (preview.url.is_some() || !preview.is_empty()).then_some(preview)
}

/// Which of the message's attachments holds the preview picture.
///
/// The index sits on the metadata's `image` dictionary, which
/// `parse_ns_keyed_archiver` folds away, so this walks the raw archive:
/// `$top.root` → `richLinkMetadata` → `image` → the index.
fn image_index(raw: &plist::Value) -> Option<usize> {
    let body = raw.as_dictionary()?;
    let objects = body.get("$objects")?.as_array()?;
    let follow = |value: Option<&plist::Value>| -> Option<&plist::Dictionary> {
        let index = usize::try_from(value?.as_uid()?.get()).ok()?;
        objects.get(index)?.as_dictionary()
    };
    let root = follow(body.get("$top")?.as_dictionary()?.get("root"))?;
    let metadata = follow(
        root.get("richLinkMetadata")
            .or_else(|| root.get("metadata")),
    )?;
    let image = follow(
        metadata
            .get("image")
            .or_else(|| metadata.get("imageMetadata")),
    )?;
    usize::try_from(image.get(SUBSTITUTE_INDEX)?.as_signed_integer()?).ok()
}

/// The attachment row a preview picture is drawn from.
///
/// Messages files these under a `.pluginPayloadAttachment` name with a
/// generated UTI and no MIME type, and marks them hidden so the transcript does
/// not list them as files. The bytes underneath are an ordinary PNG or JPEG, so
/// this reads the first of them and hands back a row typed by what is really
/// there — and `None` for a favicon or anything else msgs cannot decode.
fn preview_picture(attachment: &AttachmentRef) -> Option<AttachmentRef> {
    if attachment.is_sticker {
        return None;
    }
    let mime = sniff_image(&attachment.path()?)?;
    Some(AttachmentRef {
        mime_type: Some(mime.to_string()),
        uti: None,
        // The row it was cloned from stays hidden; this one is the picture the
        // card draws, so it is not.
        hide_attachment: false,
        ..attachment.clone()
    })
}

/// The MIME type a file's first bytes say it is, for the types msgs can decode.
fn sniff_image(path: &Path) -> Option<&'static str> {
    let mut head = [0u8; 8];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
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
    /// The page of messages in `chat_rowids` ending just before `before`.
    ///
    /// `chat_rowids` is every `chat` row the conversation was merged from —
    /// one per service for the same address — so a thread is the union of them,
    /// in date order. The messages are stamped with the first rowid, which is
    /// the conversation's own.
    ///
    /// `before` is `None` for the newest page and the `(date, rowid)` of the
    /// oldest message you already have for every page above it, which is what
    /// the conversation pane scrolls upward with. Ordering by date rather than
    /// by rowid is what interleaves two services' rows, and the date read is
    /// `chat_message_join.message_date` because that is the column the join is
    /// indexed by; the rowid is only the tie-break, so the cursor still lands
    /// on exactly one message. The result is oldest first.
    ///
    /// # Errors
    ///
    /// Fails if any of the three queries behind a page fails.
    pub fn messages_before(
        &self,
        chat_rowids: &[i64],
        before: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<Message>, DbError> {
        let Some(&chat_rowid) = chat_rowids.first() else {
            return Ok(Vec::new());
        };
        let mut params: Vec<Value> = chat_rowids.iter().map(|id| Value::Integer(*id)).collect();
        let date = chat_rowids.len() + 1;
        let rowid = date + 1;
        params.push(before.map_or(Value::Null, |(date, _)| Value::Integer(date)));
        params.push(before.map_or(Value::Null, |(_, rowid)| Value::Integer(rowid)));
        let limit = clamp_limit(limit);
        params.push(Value::Integer(limit));
        let sql = format!(
            "SELECT {COLUMNS} FROM message m \
             JOIN chat_message_join j ON j.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id IN ({}) \
               AND (?{date} IS NULL \
                    OR j.message_date < ?{date} \
                    OR (j.message_date = ?{date} AND j.message_id < ?{rowid})) \
               AND COALESCE(m.associated_message_type, 0) = 0 \
             ORDER BY j.message_date DESC, j.message_id DESC LIMIT ?{}",
            placeholders(chat_rowids.len()),
            rowid + 1
        );
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
            row_to_message(row, chat_rowid)
        })?;
        let mut page = rows.collect::<Result<Vec<_>, _>>()?;
        page.reverse();
        self.decorate(chat_rowids, &mut page)?;
        Ok(page)
    }

    /// The messages in `chat_rowids` newer than `after_rowid`, by rowid.
    ///
    /// Rowid order, not date order: this is the live-update pass, and a row
    /// `chat.db` has only just written always has the higher rowid whichever
    /// service it came in on.
    ///
    /// This is what the live-update pass re-queries after `chat.db` changes.
    ///
    /// # Errors
    ///
    /// Fails if any of the three queries behind a page fails.
    pub fn messages_after(
        &self,
        chat_rowids: &[i64],
        after_rowid: i64,
        limit: usize,
    ) -> Result<Vec<Message>, DbError> {
        let Some(&chat_rowid) = chat_rowids.first() else {
            return Ok(Vec::new());
        };
        let mut params: Vec<Value> = chat_rowids.iter().map(|id| Value::Integer(*id)).collect();
        let next = chat_rowids.len() + 1;
        params.push(Value::Integer(after_rowid));
        let limit = clamp_limit(limit);
        params.push(Value::Integer(limit));
        let sql = format!(
            "SELECT {COLUMNS} FROM message m \
             JOIN chat_message_join j ON j.message_id = m.ROWID \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE j.chat_id IN ({}) AND j.message_id > ?{next} \
               AND COALESCE(m.associated_message_type, 0) = 0 \
             ORDER BY j.message_id ASC LIMIT ?{}",
            placeholders(chat_rowids.len()),
            next + 1
        );
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
            row_to_message(row, chat_rowid)
        })?;
        let mut page = rows.collect::<Result<Vec<_>, _>>()?;
        self.decorate(chat_rowids, &mut page)?;
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
    pub fn attachment_counts(&self, chat_rowids: &[i64]) -> Result<(i64, i64), DbError> {
        if chat_rowids.is_empty() {
            return Ok((0, 0));
        }
        let sql = format!(
            "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN a.mime_type LIKE 'image/%' \
                                        OR a.uti LIKE '%image%' \
                                      THEN 1 ELSE 0 END), 0) \
             FROM chat_message_join j \
             JOIN message_attachment_join k ON k.message_id = j.message_id \
             JOIN attachment a ON a.ROWID = k.attachment_id \
             WHERE j.chat_id IN ({}) \
               AND COALESCE(a.is_sticker, 0) = 0 \
               AND COALESCE(a.hide_attachment, 0) = 0",
            placeholders(chat_rowids.len())
        );
        Ok(self.conn().query_row(
            &sql,
            rusqlite::params_from_iter(chat_rowids.iter().copied()),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    /// Hang attachments and tapbacks off a page of messages.
    fn decorate(&self, chat_rowids: &[i64], page: &mut [Message]) -> Result<(), DbError> {
        if page.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = page.iter().map(|message| message.rowid).collect();
        let mut attachments = self.attachments_by_message(&ids)?;
        let mut tapbacks = self.tapbacks_for(chat_rowids, page)?;
        let mut payloads = self.link_payloads(&ids)?;
        for message in page.iter_mut() {
            message.attachments = attachments.remove(&message.rowid).unwrap_or_default();
            message.tapbacks = tapbacks.remove(&message.guid).unwrap_or_default();
            // After the attachments, because the archive points at its picture
            // by position in that list.
            message.link_preview = payloads
                .remove(&message.rowid)
                .and_then(|payload| parse_link_preview(&payload, &message.attachments));
        }
        Ok(())
    }

    /// The rich-link payloads on the given messages, keyed by message `ROWID`.
    ///
    /// Empty when link previews are switched off, and empty on a database old
    /// enough not to have the two columns — the blob is only read for the rows
    /// Messages marked as a link balloon, which is a handful per page.
    fn link_payloads(&self, message_rowids: &[i64]) -> Result<HashMap<i64, Vec<u8>>, DbError> {
        if !self.link_previews() || !self.schema().link_preview || message_rowids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<Value> = message_rowids
            .iter()
            .take(MAX_PAGE)
            .map(|rowid| Value::Integer(*rowid))
            .collect();
        let sql = format!(
            "SELECT ROWID, payload_data FROM message \
             WHERE ROWID IN ({}) AND balloon_bundle_id = '{URL_BALLOON}' \
               AND payload_data IS NOT NULL",
            placeholders(ids.len())
        );
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(ids), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(DbError::from)
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
        chat_rowids: &[i64],
        page: &[Message],
    ) -> Result<HashMap<String, Vec<Tapback>>, DbError> {
        // Nobody can react to a message that does not exist yet, so every
        // tapback aimed at this page has a higher `ROWID` than the page's
        // oldest message. That bound is what keeps this query off the rest of
        // a long conversation.
        let floor = page.iter().map(|message| message.rowid).min().unwrap_or(0);
        let mut params: Vec<Value> = Vec::with_capacity(page.len() + chat_rowids.len() + 1);
        params.extend(chat_rowids.iter().map(|id| Value::Integer(*id)));
        params.push(Value::Integer(floor));
        let after = chat_rowids.len() + 1;
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
             WHERE j.chat_id IN ({}) AND j.message_id > ?{after} \
               AND m.associated_message_guid IS NOT NULL \
               AND COALESCE(m.associated_message_type, 0) BETWEEN {} AND {} \
               AND SUBSTR(m.associated_message_guid, -36) IN ({}) \
             ORDER BY j.message_id",
            placeholders(chat_rowids.len()),
            TAPBACK_RANGE.start(),
            TAPBACK_RANGE.end(),
            placeholders_from(page.len(), after + 1)
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
        error: row.get::<_, Option<i64>>(21)?.unwrap_or_default(),
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
        link_preview: None,
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

    fn preview(url: &str) -> LinkPreview {
        LinkPreview {
            url: Some(url.to_string()),
            title: None,
            site_name: None,
            summary: None,
            image: None,
        }
    }

    #[test]
    fn a_preview_names_its_site_by_the_url_when_the_page_did_not() {
        let mut link = preview("https://www.example.invalid:8443/a/b?c=d#e");
        assert_eq!(link.host(), Some("example.invalid"));
        assert_eq!(link.site(), Some("example.invalid"));

        link.site_name = Some("Example".to_string());
        assert_eq!(link.site(), Some("Example"), "the page's own name wins");

        assert!(preview("").host().is_none());
        assert!(preview("not a url at all").host().is_some());
    }

    #[test]
    fn a_preview_with_nothing_but_a_url_has_nothing_to_draw() {
        let mut link = preview("https://example.invalid/x");
        assert!(link.is_empty());
        link.title = Some("Something".to_string());
        assert!(!link.is_empty());
    }

    #[test]
    fn a_payload_that_is_not_an_archive_is_no_preview_at_all() {
        assert!(parse_link_preview(b"", &[]).is_none());
        assert!(parse_link_preview(b"not a property list", &[]).is_none());
        // A well-formed plist that is not a keyed archive either.
        let mut plain = Vec::new();
        plist::Value::String("hello".to_string())
            .to_writer_binary(&mut plain)
            .expect("a plist");
        assert!(parse_link_preview(&plain, &[]).is_none());
    }

    #[test]
    fn a_preview_picture_is_typed_by_its_bytes_not_by_the_row() {
        let dir = std::env::temp_dir().join(format!("msgs-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let png = dir.join("shot.pluginPayloadAttachment");
        let pixels = image::ImageBuffer::from_pixel(8, 8, image::Rgba::<u8>([1, 2, 3, 255]));
        image::DynamicImage::from(pixels)
            .save_with_format(&png, image::ImageFormat::Png)
            .expect("a written png");
        let text = dir.join("notes.pluginPayloadAttachment");
        std::fs::write(&text, b"not a picture at all").expect("a written file");

        // The row Messages writes: a generated UTI, no MIME type, and hidden.
        let row = |path: &std::path::Path| AttachmentRef {
            rowid: 1,
            guid: "A".to_string(),
            message_rowid: 1,
            filename: Some(path.display().to_string()),
            mime_type: None,
            uti: Some("dyn.age81a5dzq7y066dbtf0g82peqf4hk2".to_string()),
            transfer_name: None,
            total_bytes: 64,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: true,
        };

        let picture = preview_picture(&row(&png)).expect("a picture");
        assert_eq!(picture.mime_type.as_deref(), Some("image/png"));
        assert!(picture.is_image(), "the Images cache will take it");
        assert!(
            !picture.hide_attachment,
            "the card draws it, so it is not hidden"
        );
        assert!(preview_picture(&row(&text)).is_none());
        assert!(preview_picture(&row(&dir.join("missing"))).is_none());

        let _ = std::fs::remove_dir_all(&dir);
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
