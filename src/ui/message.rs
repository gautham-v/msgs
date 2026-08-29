//! One message, laid out as a block of terminal rows.
//!
//! A block is the mockup's `.msg`: the sender's name, a gray label, opening
//! the first line, the body wrapped to the pane, the clock right-aligned in a column of
//! its own on that first line, and — only when there is something to say — a
//! meta line under it with the delivery stamp, an edit mark, a picture's name,
//! or the tapback chips (gray text, no box). The name is a column: every row after the first —
//! a wrapped line, a chip, a picture, the meta line — is set in under the
//! words, not under the name. Consecutive messages from one person within a
//! few minutes form a run: the name is said once, and the rest sit in that
//! same column with just their clock. A blank row opens each run and each day.
//! [`block`] is a pure function of a message and the width it is drawn at, so
//! the whole grammar — where a name goes, where a line breaks, how tall the
//! block ends up — is testable without a terminal.
//!
//! The height of a block is the length of what [`block`] produces, so the
//! scroll arithmetic in [`super::conversation`] and the drawing can never
//! disagree about how many rows a message takes.

use std::collections::HashMap;

use chrono::{DateTime, Local};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::contacts::Contacts;
use crate::db::message::split_association;
use crate::db::{AttachmentKind, AttachmentRef, Chat, GroupAction, LinkPreview, Message, Tapback};
use crate::media::{Images, NOT_DOWNLOADED};
use crate::send::{Delivery, Pending};
use crate::theme::Theme;
use crate::ui::format::{bytes, clock, day_label, find_links, single_line, truncate, width, wrap};

/// Blank column to the left of the words.
pub const MARGIN_LEFT: u16 = 1;
/// Blank column to the right of the clock, where the scrollbar lives.
pub const MARGIN_RIGHT: u16 = 1;
/// Columns the clock takes: `12:15 PM`.
pub const TIME: u16 = 8;
/// Columns kept clear between the words and the clock.
pub const TIME_GAP: u16 = 2;
/// Every column a block spends on something other than words.
pub const CHROME: u16 = MARGIN_LEFT + TIME_GAP + TIME + MARGIN_RIGHT;
/// How long a pause ends a run: two messages further apart than this from the
/// same person each get their name.
pub const RUN_GAP_SECONDS: i64 = 5 * 60;
/// The left border of a quoted reply.
const QUOTE_GLYPH: &str = "▏";
/// The dashes that stand in for the mockup's dashed border on a file chip.
const CHIP_EDGE: &str = "┄";
/// What Messages says under a message it could not send.
pub const NOT_DELIVERED: &str = "Not delivered";

/// Cells left for words at a pane `columns` wide.
#[must_use]
pub fn body_width(columns: u16) -> usize {
    usize::from(columns.saturating_sub(CHROME)).max(1)
}

/// Cells a block's rows are drawn across: the words, the gap, and the clock.
#[must_use]
pub fn row_width(columns: u16) -> u16 {
    columns.saturating_sub(MARGIN_LEFT + MARGIN_RIGHT).max(1)
}

/// A link inside a laid-out block, so a click can find it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Row within [`Block::lines`].
    pub row: u16,
    /// First column of the link, counted from the start of the body.
    pub column: u16,
    /// How many cells it covers.
    pub cells: u16,
    /// Where it points, with a scheme added if the text lacked one.
    pub url: String,
}

/// Which picture of a message an [`ImageSpot`] holds room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picture {
    /// A file sent with the message, by its position in `attachments`.
    Attachment(usize),
    /// The picture on the message's link preview.
    Preview,
}

impl Picture {
    /// The attachment behind it, whichever sort of picture it is.
    ///
    /// One answer for both the measuring and the drawing, so the rows a block
    /// reserved and the pixels put into them can never come from two different
    /// files.
    #[must_use]
    pub fn of(self, message: &Message) -> Option<&AttachmentRef> {
        match self {
            Self::Attachment(index) => message.attachments.get(index),
            Self::Preview => message
                .link_preview
                .as_ref()
                .and_then(|preview| preview.image.as_ref()),
        }
    }

    /// Its position in `attachments`, for a click that wants to open the file.
    /// A preview's picture is not one of them.
    #[must_use]
    pub const fn attachment(self) -> Option<usize> {
        match self {
            Self::Attachment(index) => Some(index),
            Self::Preview => None,
        }
    }
}

/// A picture inside a laid-out block: the rows reserved for it, and which
/// picture fills them.
///
/// [`block`] only reserves the space — the pixels are put there by
/// [`crate::media::Images::render`] once the row is actually on screen — so the
/// layout stays a pure function and the height can never drift from the
/// drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSpot {
    /// First row within [`Block::lines`] the picture covers.
    pub row: u16,
    /// Column it starts at, counted from the start of the body: the name
    /// column the rest of the block is set in.
    pub column: u16,
    /// The width the picture was measured against, which is what the cache
    /// files it under; drawing asks with the same number.
    pub room: u16,
    /// Columns it covers, counted from [`ImageSpot::column`].
    pub columns: u16,
    /// Rows it covers.
    pub rows: u16,
    /// Which of the message's pictures it is.
    pub picture: Picture,
}

/// One message as rows ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Whether a blank row opens the block: the air above a new run or a new
    /// day. Never on the first message of the page.
    pub gap: bool,
    /// The day separator above this message, when the day changes here. It
    /// takes two rows: the label, and a blank one under it.
    pub day: Option<String>,
    /// The rows, margins excluded, the clock already on the end of the first.
    pub lines: Vec<Line<'static>>,
    /// Links in [`Block::lines`], for `Ctrl+L` and for clicks.
    pub links: Vec<Link>,
    /// Pictures drawn over [`Block::lines`], for the conversation to fill in.
    pub images: Vec<ImageSpot>,
}

impl Block {
    /// Rows above [`Block::lines`]: the blank row, and the day separator with
    /// the blank row under it.
    #[must_use]
    pub fn lead(&self) -> u16 {
        u16::from(self.gap) + 2 * u16::from(self.day.is_some())
    }

    /// Rows the block occupies, blank row and day separator included.
    #[must_use]
    pub fn height(&self) -> u16 {
        let lines = u16::try_from(self.lines.len()).unwrap_or(u16::MAX);
        lines.saturating_add(self.lead())
    }
}

/// Everything a block needs to know beyond the message itself.
pub struct Ctx<'a> {
    /// Colors.
    pub theme: &'a Theme,
    /// The open conversation, for participants and whether it is a group.
    pub chat: Option<&'a Chat>,
    /// The loaded page, for the day separators and for quoted replies.
    pub messages: &'a [Message],
    /// `message.guid` to index in [`Ctx::messages`], for quoted replies.
    pub by_guid: &'a HashMap<String, usize>,
    /// Messages sent but not yet read back out of `chat.db`, so their blocks
    /// can say so.
    pub pending: &'a [Pending],
    /// The clock, passed in so day labels are testable.
    pub now: DateTime<Local>,
    /// What the terminal can draw pictures with, and how big they come out.
    /// [`Images::off`] lays every attachment out as a chip.
    pub images: &'a Images,
    /// Names for handles, for the senders and reactors who are not in the
    /// participant list of the open chat.
    pub contacts: &'a Contacts,
}

impl Ctx<'_> {
    /// Whether the open conversation has more than two people in it.
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.chat.is_some_and(|chat| chat.is_group)
    }

    /// Whether the message at `index` is the newest one you sent.
    ///
    /// Only that one carries a `Delivered` / `Read` stamp, which is where
    /// Messages.app puts it: a receipt for the last thing you said, not a
    /// column of them down the thread. A page is only ever extended upwards, so
    /// the newest of yours is still the last of yours after scrolling back.
    #[must_use]
    pub fn is_latest_mine(&self, index: usize) -> bool {
        let mine = |message: &Message| message.is_from_me && !message.is_announcement();
        self.messages.get(index).is_some_and(mine) && !self.messages[index + 1..].iter().any(mine)
    }

    /// Whether the message at `index` continues the run above it: the same
    /// person, a few minutes apart at most, on the same day. A run says its
    /// name once; its later messages are set in under it.
    #[must_use]
    pub fn continues(&self, index: usize) -> bool {
        let Some(message) = self.messages.get(index) else {
            return false;
        };
        let Some(previous) = index
            .checked_sub(1)
            .and_then(|before| self.messages.get(before))
        else {
            return false;
        };
        if message.is_announcement() || previous.is_announcement() {
            return false;
        }
        let same_person = match (message.is_from_me, previous.is_from_me) {
            (true, true) => true,
            (false, false) => message.handle_rowid == previous.handle_rowid,
            _ => false,
        };
        if !same_person || opens_a_day(self.messages, index) {
            return false;
        }
        match (message.sent_at(), previous.sent_at()) {
            (Some(now), Some(then)) => (now - then).num_seconds().abs() <= RUN_GAP_SECONDS,
            _ => false,
        }
    }

    /// Whether the message at `index` would print the same clock as the one
    /// above it.
    #[must_use]
    pub fn same_minute(&self, index: usize) -> bool {
        let Some(now) = self.messages.get(index).and_then(Message::sent_at) else {
            return false;
        };
        index
            .checked_sub(1)
            .and_then(|before| self.messages.get(before))
            .and_then(Message::sent_at)
            .is_some_and(|then| clock(then) == clock(now))
    }

    /// The name to show for whoever sent a message.
    #[must_use]
    pub fn sender(&self, message: &Message) -> String {
        if message.is_from_me {
            return "You".to_string();
        }
        self.person(message.handle_rowid)
            .or_else(|| message.handle.as_deref().map(|id| self.contacts.short(id)))
            .unwrap_or_else(|| "Unknown".to_string())
    }

    /// A participant's short name, by `handle.ROWID`.
    fn person(&self, handle_rowid: Option<i64>) -> Option<String> {
        let (chat, rowid) = self.chat.zip(handle_rowid)?;
        chat.participants
            .iter()
            .find(|handle| handle.rowid == rowid)
            .map(crate::db::Handle::short_name)
    }

    /// What a block says about a message that has not reached `chat.db` yet:
    /// the `· Sending…` note, or the reason it failed.
    #[must_use]
    fn pending_note(&self, message: &Message) -> Option<(String, Color)> {
        let pending = self
            .pending
            .iter()
            .find(|pending| pending.guid == message.guid)?;
        match &pending.state {
            Delivery::Sending => Some(("· Sending…".to_string(), self.theme.gray_dim)),
            Delivery::Sent => None,
            Delivery::Failed(reason) => Some((format!("· Failed — {reason}"), self.theme.error)),
        }
    }

    /// The message a reply quotes, when it is on the loaded page.
    ///
    /// Only `thread_originator_guid` marks a real threaded reply. `reply_to_guid`
    /// is set by Messages on almost every message (it simply points at the one
    /// before it), so it must never be used as a reply marker.
    fn quoted(&self, message: &Message) -> Option<&Message> {
        let raw = message.thread_originator_guid.as_deref()?;
        let (_, guid) = split_association(raw)?;
        let index = *self.by_guid.get(guid)?;
        self.messages.get(index)
    }
}

/// Lay the `index`th message of the loaded page out for a pane `columns` wide.
///
/// # Panics
///
/// Never: an index past the end of the page comes back as an empty block.
#[must_use]
pub fn block(ctx: &Ctx<'_>, index: usize, columns: u16) -> Block {
    let Some(message) = ctx.messages.get(index) else {
        return Block {
            gap: false,
            day: None,
            lines: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
        };
    };
    let room = body_width(columns);
    let day = day_separator(ctx, index);
    let continues = ctx.continues(index);
    let gap = index > 0 && !continues;

    if message.is_announcement() {
        return system_block(ctx, message, gap, day, room);
    }

    let theme = ctx.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    // The name is a column. The head of a run puts the name in it on the
    // first row; every other row of the run — wrapped lines, chips, pictures,
    // the meta line, the later messages — is set in past it.
    let name = ctx.sender(message);
    let column = width(&name) + 2;
    let words = room.saturating_sub(column).max(1);
    let lead = || Span::raw(" ".repeat(column));

    // A quoted reply opens with the name too, on the quote's row.
    if let Some(target) = ctx.quoted(message) {
        let mut line = quote_line(ctx, target, words);
        line.spans.insert(0, lead());
        lines.push(line);
    }
    let first_row_names = !continues && lines.is_empty();

    let body = message.text.as_deref().unwrap_or_default();
    let wrapped = wrap(body, words, words);

    for (row, text) in wrapped.iter().enumerate() {
        let mut spans = Vec::new();
        if row == 0 && first_row_names {
            // A label, not a headline: regular weight, a step back from the
            // words. `You` is the one word on the screen in the accent.
            spans.push(Span::styled(
                name.clone(),
                Style::new().fg(if message.is_from_me {
                    theme.accent_me
                } else {
                    theme.text_secondary
                }),
            ));
            spans.push(Span::raw("  "));
        } else {
            spans.push(lead());
        }
        let row_index = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let (text_spans, found) = link_spans(
            text,
            theme,
            row_index,
            u16::try_from(column).unwrap_or(u16::MAX),
        );
        spans.extend(text_spans);
        links.extend(found);
        lines.push(Line::from(spans));
    }
    // A body of nothing but an attachment leaves an empty row behind; the chip
    // or the picture below says everything there is to say. A run head keeps
    // the row, because the sender's name is on it.
    if body.is_empty() && continues && !message.attachments.is_empty() {
        lines.pop();
    }

    let words_columns = u16::try_from(words).unwrap_or(u16::MAX);
    let mut images: Vec<ImageSpot> = Vec::new();
    let mut first_inline: Option<&AttachmentRef> = None;

    for (index, attachment) in message.attachments.iter().enumerate() {
        if attachment.hide_attachment {
            continue;
        }
        // A picture the terminal can draw takes rows of its own; the name and
        // the size then ride on the meta line, the way the mockup has them.
        if let Some((columns, rows)) = ctx.images.cells(attachment, words_columns) {
            images.push(ImageSpot {
                row: u16::try_from(lines.len()).unwrap_or(u16::MAX),
                column: u16::try_from(column).unwrap_or(u16::MAX),
                room: words_columns,
                columns,
                rows,
                picture: Picture::Attachment(index),
            });
            for _ in 0..rows {
                lines.push(Line::from(lead()));
            }
            if first_inline.is_none() {
                first_inline = Some(attachment);
            }
            continue;
        }
        let mut spans = chip_spans(attachment, theme, words);
        spans.insert(0, lead());
        lines.push(Line::from(spans));
    }
    let attachment_pictures = images.len();

    // The card for a link, under the line that holds the link itself. The
    // picture first, then what the page calls itself — the same shape the
    // rest of a block has, set in the name column like every other row.
    if let Some(preview) = message.link_preview.as_ref().filter(|p| !p.is_empty()) {
        if let Some((columns, rows)) = preview
            .image
            .as_ref()
            .and_then(|image| ctx.images.cells(image, words_columns))
        {
            images.push(ImageSpot {
                row: u16::try_from(lines.len()).unwrap_or(u16::MAX),
                column: u16::try_from(column).unwrap_or(u16::MAX),
                room: words_columns,
                columns,
                rows,
                picture: Picture::Preview,
            });
            for _ in 0..rows {
                lines.push(Line::from(lead()));
            }
        }
        for mut line in preview_lines(preview, theme, words) {
            line.spans.insert(0, lead());
            lines.push(line);
        }
    }

    let note = first_inline.map(|attachment| inline_note(attachment, attachment_pictures));
    for mut line in meta_lines(
        ctx,
        message,
        words,
        note.as_deref(),
        ctx.is_latest_mine(index),
    ) {
        line.spans.insert(0, lead());
        lines.push(line);
    }

    // The clock, right-aligned in its own column on the first row. Every row
    // is at most `room` wide, so the padding lands the clock on the same
    // column down the whole pane. Inside a run it is said once per minute:
    // a continuation on the same minute as the message above stays bare.
    if let Some(first) = lines.first_mut()
        && !(continues && ctx.same_minute(index))
    {
        add_clock(first, message, room, theme);
    }

    Block {
        gap,
        day,
        lines,
        links,
        images,
    }
}

/// Put the clock on the end of a row, [`TIME_GAP`] past the widest a row can be.
fn add_clock(line: &mut Line<'static>, message: &Message, room: usize, theme: &Theme) {
    let Some(when) = message.sent_at() else {
        return;
    };
    let used: usize = line.spans.iter().map(|span| width(&span.content)).sum();
    let pad = room.saturating_sub(used) + usize::from(TIME_GAP);
    line.spans.push(Span::raw(" ".repeat(pad)));
    line.spans.push(Span::styled(
        format!("{:>w$}", clock(when), w = usize::from(TIME)),
        Style::new().fg(theme.gray),
    ));
}

/// What the meta line says about the pictures drawn above it:
/// `IMG_4412.jpg · 2.1 MB`, or `3 photos` when a message carried several.
///
/// A video's still is marked with the video glyph, so a poster frame is not
/// read as a photo.
fn inline_note(attachment: &AttachmentRef, drawn: usize) -> String {
    if drawn > 1 {
        return format!("{drawn} photos");
    }
    let kind = attachment.kind();
    let name = attachment
        .display_name()
        .filter(|name| !name.is_empty())
        .map_or_else(|| kind.label().to_string(), ToString::to_string);
    let name = if kind == AttachmentKind::Video {
        format!("{} {name}", kind.glyph())
    } else {
        name
    };
    if attachment.total_bytes > 0 {
        return format!("{name} · {}", bytes(attachment.total_bytes));
    }
    name
}

/// A rename, a join, or a leave: dim, italic, and without a name or a clock.
fn system_block(
    ctx: &Ctx<'_>,
    message: &Message,
    gap: bool,
    day: Option<String>,
    room: usize,
) -> Block {
    let style = Style::new()
        .fg(ctx.theme.system)
        .add_modifier(Modifier::ITALIC);
    let lines = wrap(&system_text(ctx, message), room, room)
        .into_iter()
        .map(|text| Line::from(Span::styled(text, style)))
        .collect();
    Block {
        gap,
        day,
        lines,
        links: Vec::new(),
        images: Vec::new(),
    }
}

/// What a group event says, with the people in it named.
#[must_use]
pub fn system_text(ctx: &Ctx<'_>, message: &Message) -> String {
    let who = ctx.sender(message);
    let other = |rowid: i64| {
        ctx.person(Some(rowid))
            .unwrap_or_else(|| "someone".to_string())
    };
    match message.group_action.as_ref() {
        Some(GroupAction::NameChange(name)) if !name.is_empty() => {
            format!("{who} named the conversation “{}”", single_line(name))
        }
        Some(GroupAction::NameChange(_)) => format!("{who} renamed the conversation"),
        Some(GroupAction::ParticipantAdded(rowid)) => {
            format!("{who} added {} to the conversation", other(*rowid))
        }
        Some(GroupAction::ParticipantRemoved(rowid)) => {
            format!("{who} removed {} from the conversation", other(*rowid))
        }
        Some(GroupAction::ParticipantLeft) => format!("{who} left the conversation"),
        Some(GroupAction::IconChanged) => format!("{who} changed the group photo"),
        Some(GroupAction::IconRemoved) => format!("{who} removed the group photo"),
        Some(GroupAction::PhoneNumberChanged(_)) => format!("{who} changed their number"),
        None => format!("{who} updated the conversation"),
    }
}

/// The day separator above `index`, when the day changes there.
fn day_separator(ctx: &Ctx<'_>, index: usize) -> Option<String> {
    let when = ctx.messages.get(index)?.sent_at()?;
    opens_a_day(ctx.messages, index).then(|| day_label(ctx.now, when))
}

/// Whether the message at `index` is the first of its day on this page, which
/// is what earns it a separator row.
///
/// The day band above the pane asks the same question, so that a day the
/// separator is already announcing is not announced twice.
#[must_use]
pub fn opens_a_day(messages: &[Message], index: usize) -> bool {
    let Some(when) = messages.get(index).and_then(Message::sent_at) else {
        return false;
    };
    let previous = index
        .checked_sub(1)
        .and_then(|before| messages.get(before))
        .and_then(Message::sent_at);
    !matches!(previous, Some(before) if before.date_naive() == when.date_naive())
}

/// `▏↳ Dev: who's in for the draft?`, the first line of what is being answered.
fn quote_line(ctx: &Ctx<'_>, target: &Message, room: usize) -> Line<'static> {
    let theme = ctx.theme;
    let body = target.text.as_deref().unwrap_or_default();
    let said = if body.is_empty() {
        target
            .attachments
            .first()
            .map_or_else(|| "an earlier message".to_string(), chip)
    } else {
        single_line(body)
    };
    let text = format!("↳ {}: {said}", ctx.sender(target));
    Line::from(vec![
        Span::styled(QUOTE_GLYPH, Style::new().fg(theme.gray_dim)),
        Span::styled(
            truncate(&text, room.saturating_sub(1)),
            Style::new().fg(theme.gray),
        ),
    ])
}

/// `📄 draft-order.pdf · 84 KB`, with
/// `· (not downloaded on this Mac)` on the end when the bytes are not here.
fn chip(attachment: &AttachmentRef) -> String {
    let kind = attachment.kind();
    let name = attachment
        .display_name()
        .filter(|name| !name.is_empty())
        .map_or_else(|| kind.label().to_string(), ToString::to_string);
    let mut out = format!("{} {name}", kind.glyph());
    if attachment.total_bytes > 0 {
        out.push_str(" · ");
        out.push_str(&bytes(attachment.total_bytes));
    }
    if !attachment.is_downloaded() {
        out.push_str(" · ");
        out.push_str(NOT_DOWNLOADED);
    }
    out
}

/// The chip with the mockup's dashed border around it, drawn as the two dashes
/// a terminal has room for: `┄ 📄 draft-order.pdf · 84 KB ┄`.
fn chip_spans(attachment: &AttachmentRef, theme: &Theme, room: usize) -> Vec<Span<'static>> {
    let text = chip(attachment);
    let edges = width(CHIP_EDGE) * 2 + 2;
    let color = if attachment.is_downloaded() {
        theme.text_secondary
    } else {
        theme.gray_dim
    };
    if room <= edges {
        return vec![Span::styled(truncate(&text, room), Style::new().fg(color))];
    }
    let dash = Style::new().fg(theme.border);
    vec![
        Span::styled(format!("{CHIP_EDGE} "), dash),
        Span::styled(truncate(&text, room - edges), Style::new().fg(color)),
        Span::styled(format!(" {CHIP_EDGE}"), dash),
    ]
}

/// The card under a link: the page's title, the site it belongs to, and one
/// line of its own summary.
///
/// The URL keeps its own line in the body above — the card is what Messages
/// already knew about the page, added under it, never in place of it. Every row
/// is truncated rather than wrapped, so a page with a paragraph for a title
/// cannot push the rest of the transcript around and the card is as tall as the
/// number of things the preview actually knows. No box, no rule, no colour: the
/// title is the body's own [`Theme::text_primary`], the site a step back, the
/// summary the gray the meta line uses, and the one accent on the whole block
/// stays on the link itself.
fn preview_lines(preview: &LinkPreview, theme: &Theme, room: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row = |text: Option<&str>, color: Color| {
        let Some(text) = text.map(single_line).filter(|text| !text.is_empty()) else {
            return;
        };
        lines.push(Line::from(Span::styled(
            truncate(&text, room),
            Style::new().fg(color),
        )));
    };
    let title = preview.title.as_deref().map(single_line);
    row(title.as_deref(), theme.text_primary);
    // A site row that only repeats the title says nothing twice.
    let site = preview
        .site()
        .filter(|site| !title.as_deref().is_some_and(|title| title == *site));
    row(site, theme.text_secondary);
    row(preview.summary.as_deref(), theme.gray);
    lines
}

/// The meta line, and the tapback chips that ride on the end of it. Nothing
/// at all when there is no stamp, no note, no send in flight, and no chip:
/// the clock is already on the first row, so most messages end there.
fn meta_lines(
    ctx: &Ctx<'_>,
    message: &Message,
    room: usize,
    note: Option<&str>,
    stamp: bool,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let meta = match (meta_text(message, stamp), note) {
        (meta, Some(note)) if meta.is_empty() => note.to_string(),
        (meta, Some(note)) => format!("{meta} · {note}"),
        (meta, None) => meta,
    };
    let chips = tapback_chips(ctx, message);
    let pending = ctx.pending_note(message);
    // Messages' own verdict on a message of yours, in the error color: the
    // one thing on the meta line that is not gray, because it is the one
    // thing that needs doing something about.
    let failed = message.is_from_me && message.error != 0;
    if meta.is_empty() && chips.is_empty() && pending.is_none() && !failed {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let mut used = 0usize;
    if failed {
        spans.push(Span::styled(NOT_DELIVERED, Style::new().fg(theme.error)));
        used += width(NOT_DELIVERED);
        if !meta.is_empty() {
            spans.push(Span::styled(" · ", Style::new().fg(theme.gray)));
            used += 3;
        }
    }
    if !meta.is_empty() {
        let meta = truncate(&meta, room.saturating_sub(used));
        used += width(&meta);
        spans.push(Span::styled(meta, Style::new().fg(theme.gray)));
    }
    let mut lines = Vec::new();

    // `· Sending…` rides on the end of the meta line rather than taking a row
    // of its own, so a block does not change height when the send lands.
    if let Some((note, color)) = pending {
        let sep = usize::from(used > 0);
        let room_left = room.saturating_sub(used + sep);
        if room_left > 0 {
            let note = truncate(&note, room_left);
            used += width(&note) + sep;
            if sep > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(note, Style::new().fg(color)));
        }
    }

    for chip in chips {
        let cells = width(&chip) + 3;
        if used + cells > room && used > 0 {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {chip} "),
            Style::new().fg(theme.text_secondary),
        ));
        used += cells;
    }
    lines.push(Line::from(spans));
    lines
}

/// What goes under a message besides its clock: `Delivered`, `Read 18:05`,
/// `Edited`, or `Read 18:05 · Edited` — and nothing, for most messages.
///
/// `stamp` says whether this message is the one the receipt belongs under —
/// [`Ctx::is_latest_mine`] — so an older message of yours carries none, the
/// way Messages.app draws it. The edit mark is not a receipt and goes on
/// every edited message, whoever sent it: msgs cannot edit, but `chat.db`
/// records what another device did and saying so is cheaper than surprising
/// somebody with a body that changed under them.
#[must_use]
pub fn meta_text(message: &Message, stamp: bool) -> String {
    let mut parts = Vec::new();
    if message.is_from_me && stamp {
        if let Some(read) = message.read_at() {
            parts.push(format!("Read {}", clock(read)));
        } else if message.delivered_at().is_some() {
            parts.push("Delivered".to_string());
        }
    }
    if message.is_edited {
        parts.push("Edited".to_string());
    }
    parts.join(" · ")
}

/// The reactions standing on a message, as chip labels.
///
/// A one-to-one chat has room to say who reacted; a group says how many did.
#[must_use]
pub fn tapback_chips(ctx: &Ctx<'_>, message: &Message) -> Vec<String> {
    let standing = &message.tapbacks;
    let mut kinds: Vec<(String, Vec<&Tapback>)> = Vec::new();
    for tapback in standing {
        let glyph = tapback.kind.glyph().to_string();
        match kinds.iter_mut().find(|(seen, _)| *seen == glyph) {
            Some((_, group)) => group.push(tapback),
            None => kinds.push((glyph, vec![tapback])),
        }
    }

    kinds
        .into_iter()
        .map(|(glyph, group)| {
            if !ctx.is_group() && group.len() == 1 {
                let who = group[0].handle.as_deref().map_or_else(
                    || "You".to_string(),
                    |handle| {
                        ctx.person(group[0].handle_rowid)
                            .unwrap_or_else(|| ctx.contacts.short(handle))
                    },
                );
                return format!("{glyph} {who}");
            }
            format!("{glyph} {}", group.len())
        })
        .collect()
}

/// Split one already-wrapped line into spans, underlining any links.
fn link_spans(
    text: &str,
    theme: &Theme,
    row: u16,
    start_column: u16,
) -> (Vec<Span<'static>>, Vec<Link>) {
    let ranges = find_links(text);
    if ranges.is_empty() {
        return (
            vec![Span::styled(
                text.to_string(),
                Style::new().fg(theme.text_primary),
            )],
            Vec::new(),
        );
    }

    let link_style = Style::new()
        .fg(theme.accent_me)
        .add_modifier(Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut links = Vec::new();
    let mut cursor = 0usize;
    let mut column = start_column;

    for (start, end) in ranges {
        if start > cursor {
            let plain = &text[cursor..start];
            spans.push(Span::styled(
                plain.to_string(),
                Style::new().fg(theme.text_primary),
            ));
            column = column.saturating_add(u16::try_from(width(plain)).unwrap_or(0));
        }
        let raw = &text[start..end];
        let cells = u16::try_from(width(raw)).unwrap_or(0);
        spans.push(Span::styled(raw.to_string(), link_style));
        links.push(Link {
            row,
            column,
            cells,
            url: crate::ui::format::first_link(raw).unwrap_or_else(|| raw.to_string()),
        });
        column = column.saturating_add(cells);
        cursor = end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(
            text[cursor..].to_string(),
            Style::new().fg(theme.text_primary),
        ));
    }
    (spans, links)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Handle, TapbackAction, TapbackKind};
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2025, 8, 27, 18, 30, 0)
            .single()
            .expect("an unambiguous local time")
    }

    /// A raw Messages timestamp `minutes` before [`now`].
    fn stamp(minutes: i64) -> i64 {
        let when = now() - chrono::Duration::minutes(minutes);
        (when.timestamp() - 978_307_200) * 1_000_000_000
    }

    fn message(rowid: i64, from_me: bool, text: &str) -> Message {
        Message {
            rowid,
            guid: format!("G{rowid}"),
            chat_rowid: 1,
            handle_rowid: (!from_me).then_some(1),
            handle: (!from_me).then(|| "+15550000001".to_string()),
            service: Some("iMessage".to_string()),
            is_from_me: from_me,
            is_read: true,
            date: stamp(10),
            date_delivered: 0,
            date_read: 0,
            date_edited: 0,
            is_edited: false,
            error: 0,
            text: (!text.is_empty()).then(|| text.to_string()),
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
            link_preview: None,
        }
    }

    fn chat(is_group: bool) -> Chat {
        Chat {
            rowid: 1,
            rowids: vec![1],
            guid: "iMessage;-;chat1".to_string(),
            identifier: Some("chat1".to_string()),
            group_id: None,
            original_group_id: None,
            display_name: None,
            service: Some("iMessage".to_string()),
            style: if is_group { 43 } else { 45 },
            is_group,
            participants: vec![
                Handle::new(
                    1,
                    "alex@example.invalid".to_string(),
                    "iMessage".to_string(),
                ),
                Handle::new(
                    2,
                    "bailey@example.invalid".to_string(),
                    "iMessage".to_string(),
                ),
            ],
            last_message_date: 0,
            last_message_rowid: 0,
            preview: None,
            message_count: 0,
            unread_count: 0,
            unread: 0,
            is_pinned: None,
        }
    }

    struct Fixture {
        theme: Theme,
        chat: Chat,
        messages: Vec<Message>,
        by_guid: HashMap<String, usize>,
        images: crate::media::Images,
        contacts: Contacts,
    }

    impl Fixture {
        fn new(is_group: bool, messages: Vec<Message>) -> Self {
            let by_guid = messages
                .iter()
                .enumerate()
                .map(|(index, message)| (message.guid.clone(), index))
                .collect();
            Self {
                theme: Theme::default(),
                chat: chat(is_group),
                messages,
                by_guid,
                images: crate::media::Images::off(),
                contacts: Contacts::empty(),
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                theme: &self.theme,
                chat: Some(&self.chat),
                messages: &self.messages,
                by_guid: &self.by_guid,
                pending: &[],
                now: now(),
                images: &self.images,
                contacts: &self.contacts,
            }
        }
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_block_is_one_row_with_the_name_and_the_clock_under_a_day_separator() {
        let fixture = Fixture::new(false, vec![message(1, false, "yes!! 7?")]);
        let block = block(&fixture.ctx(), 0, 60);

        assert_eq!(block.day.as_deref(), Some("Today"));
        assert!(!block.gap, "the first message of the page has no air above");
        assert_eq!(block.lines.len(), 1, "no meta line without a stamp");
        let row = text_of(&block.lines[0]);
        assert!(row.starts_with("alex  yes!! 7?"), "{row}");
        assert!(row.ends_with("6:20 PM"), "{row}");
        assert_eq!(width(&row), usize::from(row_width(60)));
        assert_eq!(
            block.height(),
            3,
            "the day label, its blank row, the message"
        );
    }

    #[test]
    fn your_own_name_is_the_accent_and_theirs_is_gray() {
        let fixture = Fixture::new(
            false,
            vec![message(1, true, "on my way"), {
                let mut theirs = message(2, false, "ok");
                theirs.date = stamp(9);
                theirs
            }],
        );
        let ctx = fixture.ctx();
        let mine = block(&ctx, 0, 60);
        assert_eq!(mine.lines[0].spans[0].content, "You");
        assert_eq!(
            mine.lines[0].spans[0].style.fg,
            Some(Theme::default().accent_me)
        );

        let theirs = block(&ctx, 1, 60);
        assert!(theirs.gap, "a new run opens with a blank row");
        assert_eq!(theirs.lines[0].spans[0].content, "alex");
        assert_eq!(
            theirs.lines[0].spans[0].style.fg,
            Some(Theme::default().text_secondary)
        );
        assert!(
            !theirs.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "a label, not a headline"
        );
    }

    #[test]
    fn a_run_says_the_name_once_and_sets_the_rest_in() {
        let mut second = message(2, false, "or different saturday?");
        second.date = stamp(9);
        let mut later = message(3, false, "sunday for me");
        later.date = stamp(2);
        let fixture = Fixture::new(false, vec![message(1, false, "saturday?"), second, later]);
        let ctx = fixture.ctx();

        assert!(!ctx.continues(0));
        assert!(ctx.continues(1), "a minute later, same person");
        assert!(!ctx.continues(2), "seven minutes is a pause");

        let head = block(&ctx, 0, 60);
        let tail = block(&ctx, 1, 60);
        assert!(text_of(&head.lines[0]).starts_with("alex  saturday?"));
        assert!(!tail.gap, "no air inside a run");
        let row = text_of(&tail.lines[0]);
        assert!(
            row.starts_with("      or different"),
            "under the words, not the name: {row}"
        );
        assert!(row.ends_with("6:21 PM"), "{row}");
        assert_eq!(width(&row), usize::from(row_width(60)));
        assert!(block(&ctx, 2, 60).gap);
    }

    #[test]
    fn only_the_first_message_of_a_day_carries_a_separator() {
        let mut older = message(1, false, "yesterday");
        older.date = stamp(60 * 30);
        let fixture = Fixture::new(false, vec![older, message(2, false, "today")]);
        let ctx = fixture.ctx();

        assert_eq!(block(&ctx, 0, 60).day.as_deref(), Some("Yesterday"));
        assert_eq!(block(&ctx, 1, 60).day.as_deref(), Some("Today"));

        let same_day = Fixture::new(
            false,
            vec![message(1, false, "one"), message(2, false, "two")],
        );
        assert!(block(&same_day.ctx(), 1, 60).day.is_none());
    }

    #[test]
    fn a_group_names_each_sender_and_another_person_ends_the_run() {
        let mut second = message(2, false, "in. keeping my keeper");
        second.handle_rowid = Some(2);
        let fixture = Fixture::new(true, vec![message(1, false, "who's in?"), second]);
        let ctx = fixture.ctx();

        let first = block(&ctx, 0, 60);
        assert!(text_of(&first.lines[0]).starts_with("alex  "));

        assert!(!ctx.continues(1), "a different person");
        let second = block(&ctx, 1, 60);
        assert!(second.gap);
        assert!(text_of(&second.lines[0]).starts_with("bailey  "));
    }

    #[test]
    fn a_long_body_wraps_to_the_pane_and_grows_the_block() {
        let long = "wrap ".repeat(40);
        let fixture = Fixture::new(false, vec![message(1, false, long.trim())]);
        let narrow = block(&fixture.ctx(), 0, 30);
        let wide = block(&fixture.ctx(), 0, 100);
        assert!(narrow.height() > wide.height());
        assert!(
            text_of(&narrow.lines[1]).starts_with("      wrap"),
            "wrapped lines sit under the words"
        );
        for line in &narrow.lines {
            assert!(width(&text_of(line)) <= usize::from(row_width(30)));
        }
    }

    #[test]
    fn delivery_stamps_only_appear_on_your_own_messages() {
        let mut mine = message(1, true, "sent");
        assert_eq!(meta_text(&mine, true), "", "no stamp before delivery");

        mine.date_delivered = stamp(9);
        assert_eq!(meta_text(&mine, true), "Delivered");

        mine.date_read = stamp(8);
        assert_eq!(meta_text(&mine, true), "Read 6:22 PM");

        // Incoming messages never carry a stamp, even when the column is set.
        let mut theirs = message(2, false, "got it");
        theirs.date_delivered = stamp(9);
        theirs.date_read = stamp(8);
        assert_eq!(meta_text(&theirs, true), "");
    }

    #[test]
    fn a_message_messages_could_not_send_says_so_in_red() {
        let mut mine = message(1, true, "hi there");
        mine.error = 22;
        let fixture = Fixture::new(false, vec![mine]);
        let laid = block(&fixture.ctx(), 0, 60);
        assert_eq!(laid.lines.len(), 2, "a meta line for the verdict");
        let meta = &laid.lines[1];
        let verdict = meta
            .spans
            .iter()
            .find(|span| span.content.contains(NOT_DELIVERED))
            .expect("the verdict");
        assert_eq!(verdict.style.fg, Some(Theme::default().error));

        // Somebody else's message never carries one, whatever the column says.
        let mut theirs = message(2, false, "got it");
        theirs.error = 22;
        let fixture = Fixture::new(false, vec![theirs]);
        assert_eq!(block(&fixture.ctx(), 0, 60).lines.len(), 1);
    }

    #[test]
    fn an_edited_message_says_so_whoever_sent_it() {
        let mut mine = message(1, true, "sent");
        mine.date_edited = stamp(5);
        mine.is_edited = true;
        assert_eq!(meta_text(&mine, true), "Edited");
        mine.date_delivered = stamp(9);
        assert_eq!(meta_text(&mine, true), "Delivered · Edited");
        mine.date_read = stamp(8);
        assert_eq!(meta_text(&mine, true), "Read 6:22 PM · Edited");

        let mut theirs = message(2, false, "got it");
        theirs.is_edited = true;
        assert_eq!(meta_text(&theirs, true), "Edited");
    }

    #[test]
    fn only_the_newest_message_you_sent_carries_the_stamp() {
        let stamped = |rowid: i64, text: &str| {
            let mut message = message(rowid, true, text);
            message.date_delivered = stamp(9);
            message.date_read = stamp(8);
            message
        };
        let fixture = Fixture::new(
            false,
            vec![stamped(1, "first"), stamped(2, "second"), {
                let mut theirs = message(3, false, "got it");
                theirs.date_read = stamp(7);
                theirs
            }],
        );
        let ctx = fixture.ctx();

        assert!(!ctx.is_latest_mine(0));
        assert!(ctx.is_latest_mine(1), "the last one you sent");
        assert!(!ctx.is_latest_mine(2), "not one of yours");

        let meta = |index: usize| {
            let block = block(&ctx, index, 60);
            text_of(block.lines.last().expect("a row"))
        };
        assert!(!meta(0).contains("Read"), "the older one is just its clock");
        assert_eq!(block(&ctx, 0, 60).lines.len(), 1, "and no meta line");
        assert!(meta(1).contains("Read"), "the newest of yours is stamped");
        assert_eq!(block(&ctx, 1, 60).lines.len(), 2);
        assert!(!meta(2).contains("Read"));
    }

    #[test]
    fn tapbacks_name_the_reactor_in_a_one_to_one_and_count_them_in_a_group() {
        let reaction = |handle: Option<&str>, rowid: Option<i64>| Tapback {
            rowid: 9,
            target_guid: "G1".to_string(),
            target_part: 0,
            action: TapbackAction::Added,
            kind: TapbackKind::Loved,
            is_from_me: handle.is_none(),
            handle_rowid: rowid,
            handle: handle.map(ToString::to_string),
            date: 0,
        };

        let mut direct = message(1, true, "here's the menu");
        direct.tapbacks = vec![reaction(Some("alex@example.invalid"), Some(1))];
        let fixture = Fixture::new(false, vec![direct]);
        assert_eq!(
            tapback_chips(&fixture.ctx(), &fixture.messages[0]),
            vec!["❤️ alex".to_string()]
        );

        let mut group = message(1, false, "who's in?");
        group.tapbacks = vec![
            reaction(Some("alex@example.invalid"), Some(1)),
            reaction(Some("bailey@example.invalid"), Some(2)),
        ];
        let fixture = Fixture::new(true, vec![group]);
        assert_eq!(
            tapback_chips(&fixture.ctx(), &fixture.messages[0]),
            vec!["❤️ 2".to_string()]
        );
    }

    #[test]
    fn a_reply_quotes_the_first_line_of_what_it_answers() {
        let mut reply = message(2, false, "can we do 4?");
        reply.thread_originator_guid = Some("p:0/G1".to_string());
        let fixture = Fixture::new(true, vec![message(1, false, "who's in?\nsunday"), reply]);
        let block = block(&fixture.ctx(), 1, 60);
        let quote = text_of(&block.lines[0]);
        assert!(quote.contains('↳'), "{quote}");
        assert!(quote.contains("who's in? sunday"), "{quote}");
    }

    #[test]
    fn reply_to_guid_alone_is_not_a_reply() {
        let mut plain = message(2, false, "just the next message");
        plain.reply_to_guid = Some("G1".to_string());
        let fixture = Fixture::new(true, vec![message(1, false, "who's in?"), plain]);
        let block = block(&fixture.ctx(), 1, 60);
        assert!(
            !text_of(&block.lines[0]).contains('↳'),
            "{}",
            text_of(&block.lines[0])
        );
    }

    #[test]
    fn a_reply_to_a_message_off_the_page_is_not_quoted() {
        let mut reply = message(2, false, "can we do 4?");
        reply.thread_originator_guid = Some("p:0/MISSING".to_string());
        let fixture = Fixture::new(true, vec![reply]);
        let block = block(&fixture.ctx(), 0, 60);
        assert!(!text_of(&block.lines[0]).contains('↳'));
    }

    #[test]
    fn a_group_event_is_an_italic_line_without_a_clock() {
        let mut event = message(3, false, "");
        event.item_type = 2;
        event.group_action = Some(GroupAction::NameChange("Sunday Football".to_string()));
        let fixture = Fixture::new(true, vec![event]);
        let block = block(&fixture.ctx(), 0, 80);

        assert_eq!(block.lines.len(), 1);
        let row = text_of(&block.lines[0]);
        assert!(row.contains("named the conversation"));
        assert!(!row.ends_with("PM"), "{row}");
    }

    #[test]
    fn an_attachment_becomes_a_chip_instead_of_an_empty_row() {
        let mut photo = message(1, false, "");
        photo.attachments = vec![AttachmentRef {
            rowid: 1,
            guid: "A".to_string(),
            message_rowid: 1,
            filename: None,
            mime_type: Some("application/pdf".to_string()),
            uti: None,
            transfer_name: Some("draft-order.pdf".to_string()),
            total_bytes: 86_016,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        }];
        let fixture = Fixture::new(false, vec![photo]);
        let block = block(&fixture.ctx(), 0, 80);
        assert_eq!(block.lines.len(), 2, "the name row and the chip, no meta");
        let chip = text_of(&block.lines[1]);
        assert!(chip.contains("📄 draft-order.pdf · 84 KB"), "{chip}");
        assert!(
            chip.trim_start().starts_with(CHIP_EDGE),
            "the dashed edge: {chip}"
        );
        assert!(chip.starts_with("      ┄"), "set in past the name: {chip}");
        assert!(
            text_of(&block.lines[0]).ends_with("6:20 PM"),
            "the clock stays on the first row"
        );
        assert!(block.images.is_empty(), "a pdf is never drawn inline");
    }

    #[test]
    fn a_picture_takes_rows_of_its_own_and_names_itself_on_the_meta_line() {
        let dir = std::env::temp_dir().join(format!("msgs-block-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("IMG_4412.jpg");
        let pixels = image::ImageBuffer::from_pixel(200, 100, image::Rgba::<u8>([1, 2, 3, 255]));
        image::DynamicImage::from(pixels)
            .save(&path)
            .expect("a written jpeg");

        let mut photo = message(1, false, "");
        photo.attachments = vec![AttachmentRef {
            rowid: 7,
            guid: "A7".to_string(),
            message_rowid: 1,
            filename: Some(path.display().to_string()),
            mime_type: Some("image/jpeg".to_string()),
            uti: None,
            transfer_name: Some("IMG_4412.jpg".to_string()),
            total_bytes: 2_202_009,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        }];
        let mut fixture = Fixture::new(false, vec![photo]);
        fixture.images = crate::media::Images::halfblocks();
        let block = block(&fixture.ctx(), 0, 60);

        let spot = *block.images.first().expect("a reserved picture");
        assert_eq!(spot.row, 1, "the picture sits under the name row");
        assert_eq!(spot.column, 6, "set in past `alex  `");
        assert_eq!(spot.room, u16::try_from(body_width(60) - 6).unwrap());
        assert_eq!(spot.picture, Picture::Attachment(0));
        // Twenty by ten cells at a ten-by-twenty font: five rows for the rows.
        assert_eq!((spot.columns, spot.rows), (20, 5));
        assert_eq!(block.lines.len(), usize::from(spot.rows) + 2);
        for row in 1..=usize::from(spot.rows) {
            assert_eq!(
                text_of(&block.lines[row]).trim(),
                "",
                "row {row} is left blank"
            );
        }
        let meta = text_of(&block.lines[usize::from(spot.rows) + 1]);
        assert!(meta.contains("IMG_4412.jpg · 2.1 MB"), "{meta}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_never_reached_this_mac_says_so() {
        let mut photo = message(1, false, "");
        photo.attachments = vec![AttachmentRef {
            rowid: 1,
            guid: "A".to_string(),
            message_rowid: 1,
            // No filename is what an undownloaded attachment looks like.
            filename: None,
            mime_type: Some("image/jpeg".to_string()),
            uti: None,
            transfer_name: Some("IMG_4412.jpg".to_string()),
            total_bytes: 2_202_009,
            transfer_state: 1,
            is_sticker: false,
            hide_attachment: false,
        }];
        let fixture = Fixture::new(false, vec![photo]);
        let block = block(&fixture.ctx(), 0, 120);
        let chip = text_of(&block.lines[1]);
        assert!(chip.contains(NOT_DOWNLOADED), "{chip}");
        assert!(block.images.is_empty(), "nothing to draw");
    }

    /// The preview Messages would have stored for a link, with every string in
    /// it invented here.
    fn link_preview(image: Option<AttachmentRef>) -> LinkPreview {
        LinkPreview {
            url: Some("https://example.invalid/menu".to_string()),
            title: Some("The Tuesday Menu".to_string()),
            site_name: Some("Example Kitchen".to_string()),
            summary: Some("Six things to cook, and one of them is soup.".to_string()),
            image,
        }
    }

    #[test]
    fn a_link_preview_is_a_card_under_the_url_it_is_of() {
        let mut linked = message(1, false, "https://example.invalid/menu");
        linked.link_preview = Some(link_preview(None));
        let fixture = Fixture::new(false, vec![linked]);
        let block = block(&fixture.ctx(), 0, 80);

        assert_eq!(block.lines.len(), 4, "the URL and the card's three rows");
        let url = text_of(&block.lines[0]);
        assert!(
            url.starts_with("alex  https://example.invalid/menu"),
            "the URL keeps its own line: {url}"
        );
        assert_eq!(block.links.len(), 1, "and stays the link `o` opens");

        let rows: Vec<String> = block.lines[1..].iter().map(text_of).collect();
        for row in &rows {
            assert!(row.starts_with("      "), "set in past the name: {row}");
        }
        assert_eq!(rows[0].trim(), "The Tuesday Menu");
        assert_eq!(rows[1].trim(), "Example Kitchen");
        assert_eq!(
            rows[2].trim(),
            "Six things to cook, and one of them is soup."
        );

        let theme = Theme::default();
        assert_eq!(block.lines[1].spans[1].style.fg, Some(theme.text_primary));
        assert_eq!(block.lines[2].spans[1].style.fg, Some(theme.text_secondary));
        assert_eq!(block.lines[3].spans[1].style.fg, Some(theme.gray));
        for line in &block.lines[1..] {
            assert!(
                line.spans
                    .iter()
                    .all(|span| !span.style.add_modifier.contains(Modifier::BOLD)),
                "the card is as calm as the rest of the block"
            );
        }
        assert!(block.images.is_empty(), "no picture on this preview");
    }

    #[test]
    fn a_preview_row_is_truncated_rather_than_wrapped() {
        let mut linked = message(1, false, "look");
        let mut preview = link_preview(None);
        preview.title = Some("a very long title ".repeat(20));
        preview.summary = Some("a very long summary ".repeat(20));
        linked.link_preview = Some(preview);
        let fixture = Fixture::new(false, vec![linked]);
        let block = block(&fixture.ctx(), 0, 50);

        assert_eq!(block.lines.len(), 4, "one row each, however long they are");
        for line in &block.lines {
            assert!(width(&text_of(line)) <= usize::from(row_width(50)));
        }
    }

    #[test]
    fn a_preview_with_nothing_to_say_draws_no_card() {
        let mut linked = message(1, false, "https://example.invalid/menu");
        linked.link_preview = Some(LinkPreview {
            url: Some("https://example.invalid/menu".to_string()),
            title: None,
            site_name: Some("Example Kitchen".to_string()),
            summary: None,
            image: None,
        });
        let fixture = Fixture::new(false, vec![linked]);
        assert_eq!(block(&fixture.ctx(), 0, 60).lines.len(), 1);
    }

    #[test]
    fn a_site_row_that_only_repeats_the_title_is_left_out() {
        let mut linked = message(1, false, "look");
        let mut preview = link_preview(None);
        preview.site_name.clone_from(&preview.title);
        preview.summary = None;
        linked.link_preview = Some(preview);
        let fixture = Fixture::new(false, vec![linked]);
        let block = block(&fixture.ctx(), 0, 60);
        assert_eq!(block.lines.len(), 2, "the body and the title, said once");
    }

    #[test]
    fn a_previews_picture_takes_rows_of_its_own_above_the_card() {
        let dir = std::env::temp_dir().join(format!("msgs-preview-block-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("preview.pluginPayloadAttachment");
        let pixels = image::ImageBuffer::from_pixel(200, 100, image::Rgba::<u8>([4, 5, 6, 255]));
        image::DynamicImage::from(pixels)
            .save_with_format(&path, image::ImageFormat::Png)
            .expect("a written png");

        let picture = AttachmentRef {
            rowid: 42,
            guid: "LINK42".to_string(),
            message_rowid: 1,
            filename: Some(path.display().to_string()),
            mime_type: Some("image/png".to_string()),
            uti: None,
            transfer_name: None,
            total_bytes: 4096,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        };
        let mut linked = message(1, false, "https://example.invalid/menu");
        linked.link_preview = Some(link_preview(Some(picture)));
        let mut fixture = Fixture::new(false, vec![linked]);

        // Pictures off: the card is its three rows and nothing is reserved.
        let flat = block(&fixture.ctx(), 0, 60);
        assert!(flat.images.is_empty());
        assert_eq!(flat.lines.len(), 4);

        fixture.images = crate::media::Images::halfblocks();
        let block = block(&fixture.ctx(), 0, 60);
        let spot = *block.images.first().expect("a reserved picture");
        assert_eq!(spot.picture, Picture::Preview);
        assert_eq!(spot.row, 1, "under the URL, above the title");
        assert_eq!(spot.column, 6, "set in past `alex  `");
        assert_eq!(spot.room, u16::try_from(body_width(60) - 6).unwrap());
        assert_eq!((spot.columns, spot.rows), (20, 5));
        // The one number: the rows reserved are the rows the block grew by.
        assert_eq!(block.lines.len(), flat.lines.len() + usize::from(spot.rows));
        for row in 1..=usize::from(spot.rows) {
            assert_eq!(text_of(&block.lines[row]).trim(), "");
        }
        assert_eq!(
            text_of(&block.lines[usize::from(spot.rows) + 1]).trim(),
            "The Tuesday Menu"
        );
        assert!(
            spot.picture.of(&fixture.messages[0]).is_some(),
            "the drawing finds the same file the measuring did"
        );
        assert!(
            spot.picture.attachment().is_none(),
            "it is not a file `o` opens"
        );
        assert_eq!(
            block.height(),
            u16::try_from(block.lines.len()).unwrap() + 2
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_photo_and_a_link_preview_each_keep_their_own_rows() {
        let dir = std::env::temp_dir().join(format!("msgs-preview-both-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let photo_path = dir.join("IMG_0001.jpg");
        let pixels = image::ImageBuffer::from_pixel(200, 100, image::Rgba::<u8>([7, 7, 7, 255]));
        image::DynamicImage::from(pixels)
            .save(&photo_path)
            .expect("a written jpeg");
        let card_path = dir.join("card.pluginPayloadAttachment");
        image::DynamicImage::from(image::ImageBuffer::from_pixel(
            200,
            100,
            image::Rgba::<u8>([8, 8, 8, 255]),
        ))
        .save_with_format(&card_path, image::ImageFormat::Png)
        .expect("a written png");

        let file = |rowid: i64, path: &std::path::Path, mime: &str| AttachmentRef {
            rowid,
            guid: format!("A{rowid}"),
            message_rowid: 1,
            filename: Some(path.display().to_string()),
            mime_type: Some(mime.to_string()),
            uti: None,
            transfer_name: Some("IMG_0001.jpg".to_string()),
            total_bytes: 2_202_009,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        };
        let mut linked = message(1, false, "https://example.invalid/menu");
        linked.attachments = vec![file(1, &photo_path, "image/jpeg")];
        linked.link_preview = Some(link_preview(Some(file(2, &card_path, "image/png"))));
        let mut fixture = Fixture::new(false, vec![linked]);
        fixture.images = crate::media::Images::halfblocks();
        let block = block(&fixture.ctx(), 0, 60);

        assert_eq!(block.images.len(), 2);
        assert_eq!(block.images[0].picture, Picture::Attachment(0));
        assert_eq!(block.images[1].picture, Picture::Preview);
        assert!(block.images[0].row < block.images[1].row);
        // The meta line counts the photo, not the card's picture.
        let meta = text_of(block.lines.last().expect("a row"));
        assert!(meta.contains("IMG_0001.jpg"), "{meta}");
        assert!(!meta.contains("photos"), "{meta}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_is_underlined_and_reported_with_its_column() {
        let fixture = Fixture::new(
            false,
            vec![message(1, true, "menu here https://example.invalid/menu")],
        );
        let block = block(&fixture.ctx(), 0, 80);
        assert_eq!(block.links.len(), 1);
        let link = &block.links[0];
        assert_eq!(link.row, 0);
        assert_eq!(link.column, 15, "past `You  menu here `");
        assert_eq!(link.url, "https://example.invalid/menu");
        assert!(
            block.lines[0]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
    }

    #[test]
    fn every_block_is_as_tall_as_it_says_it_is() {
        let mut quoted = message(2, false, "a reply with a rather long body that will wrap");
        quoted.thread_originator_guid = Some("G1".to_string());
        quoted.tapbacks = Vec::new();
        let mut event = message(3, false, "");
        event.item_type = 1;
        let fixture = Fixture::new(true, vec![message(1, false, "who's in?"), quoted, event]);
        let ctx = fixture.ctx();
        for columns in [20u16, 40, 80] {
            for index in 0..fixture.messages.len() {
                let block = block(&ctx, index, columns);
                let expected = u16::try_from(block.lines.len()).unwrap() + block.lead();
                assert_eq!(block.height(), expected);
                assert!(block.height() >= 1);
                for line in &block.lines {
                    assert!(
                        width(&text_of(line)) <= usize::from(row_width(columns)),
                        "{columns}: {:?}",
                        text_of(line)
                    );
                }
            }
        }
    }
}
