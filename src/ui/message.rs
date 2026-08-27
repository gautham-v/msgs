//! One message, laid out as a block of terminal rows.
//!
//! A block is the mockup's `.blk`: a two-column accent rail, a body wrapped to
//! the pane, and a meta line carrying the time, the delivery stamp, and any
//! tapback chips. [`block`] is a pure function of a message and the width it is
//! drawn at, so the whole grammar — who gets which color, where a line breaks,
//! how tall the block ends up — is testable without a terminal.
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
use crate::db::{AttachmentRef, Chat, GroupAction, Message, Tapback};
use crate::media::{Images, NOT_DOWNLOADED};
use crate::send::{Delivery, Pending, PendingTapback};
use crate::theme::Theme;
use crate::ui::format::{bytes, clock, day_label, find_links, single_line, truncate, width, wrap};

/// Blank column to the left of the rail.
pub const MARGIN_LEFT: u16 = 1;
/// Columns the accent rail occupies.
pub const RAIL: u16 = 2;
/// Columns between the rail and the body.
pub const GAP: u16 = 1;
/// Blank column to the right of the body.
pub const MARGIN_RIGHT: u16 = 1;
/// Every column a block spends on something other than words.
pub const CHROME: u16 = MARGIN_LEFT + RAIL + GAP + MARGIN_RIGHT;

/// The glyph drawn in the first column of the rail.
pub const RAIL_GLYPH: &str = "▌";
/// The left border of a quoted reply.
const QUOTE_GLYPH: &str = "▏";
/// The dashes that stand in for the mockup's dashed border on a file chip.
const CHIP_EDGE: &str = "┄";

/// Cells left for words at a pane `columns` wide.
#[must_use]
pub fn body_width(columns: u16) -> usize {
    usize::from(columns.saturating_sub(CHROME)).max(1)
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

/// A picture inside a laid-out block: the rows reserved for it, and which
/// attachment fills them.
///
/// [`block`] only reserves the space — the pixels are put there by
/// [`crate::media::Images::render`] once the row is actually on screen — so the
/// layout stays a pure function and the height can never drift from the
/// drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSpot {
    /// First row within [`Block::lines`] the picture covers.
    pub row: u16,
    /// Columns it covers, counted from the start of the body.
    pub columns: u16,
    /// Rows it covers.
    pub rows: u16,
    /// Its position in the message's `attachments`.
    pub attachment: usize,
}

/// One message as rows ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The day separator above this message, when the day changes here.
    pub day: Option<String>,
    /// The body rows, rail and margins excluded.
    pub lines: Vec<Line<'static>>,
    /// Rail color. `None` for a system line, which the mockup draws railless.
    pub rail: Option<Color>,
    /// Whether the body sits on the lighter band your own messages get.
    pub band: bool,
    /// Links in [`Block::lines`], for `Ctrl+L` and for clicks.
    pub links: Vec<Link>,
    /// Pictures drawn over [`Block::lines`], for the conversation to fill in.
    pub images: Vec<ImageSpot>,
}

impl Block {
    /// Rows the block occupies, day separator included.
    #[must_use]
    pub fn height(&self) -> u16 {
        let lines = u16::try_from(self.lines.len()).unwrap_or(u16::MAX);
        lines.saturating_add(u16::from(self.day.is_some()))
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
    /// Reactions sent but not yet read back out of `chat.db`. They are laid
    /// over the database's own reactions here rather than written into the
    /// loaded page, so the page stays exactly what was read.
    pub reactions: &'a [PendingTapback],
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

    /// The reactions standing on `message`: what the database holds, plus the
    /// ones just sent that it has not caught up with, minus the ones just
    /// taken back.
    ///
    /// This is the only place the two are combined, so the height a block is
    /// measured at and the chips it is drawn with come from one answer.
    #[must_use]
    pub fn tapbacks(&self, message: &Message) -> Vec<Tapback> {
        let mut standing = message.tapbacks.clone();
        for pending in self
            .reactions
            .iter()
            .filter(|pending| pending.target_guid == message.guid)
        {
            let mine = standing
                .iter()
                .position(|tapback| tapback.is_from_me && tapback.kind == pending.kind);
            match (pending.remove, mine) {
                (true, Some(index)) => {
                    standing.remove(index);
                }
                (false, None) => standing.push(pending.as_tapback()),
                _ => {}
            }
        }
        standing
    }

    /// The rail color for a message.
    ///
    /// Yours is always the blue accent. In a one-to-one chat the other person
    /// is the green one; in a group everybody keeps the color their position in
    /// the participant list gives them, and that list is ordered by
    /// `handle.ROWID`, so a color follows a person across sessions.
    ///
    /// The position is taken by person, not by row: somebody who is in a group
    /// twice — an Apple ID and a phone number, which Contacts calls one name —
    /// gets one color for both.
    #[must_use]
    pub fn accent(&self, message: &Message) -> Color {
        if message.is_from_me {
            return self.theme.accent_me;
        }
        let Some(chat) = self.chat else {
            return self.theme.accent_them;
        };
        if !chat.is_group {
            return self.theme.accent_them;
        }
        self.theme.participant(self.slot(message.handle_rowid))
    }

    /// Which participant color a sender gets.
    fn slot(&self, handle_rowid: Option<i64>) -> usize {
        let Some((chat, rowid)) = self.chat.zip(handle_rowid) else {
            return 0;
        };
        let Some(sender) = chat
            .participants
            .iter()
            .find(|handle| handle.rowid == rowid)
        else {
            return 0;
        };
        // The first row that is the same person, which is that row itself
        // unless Contacts has joined two addresses under one name.
        chat.participants
            .iter()
            .position(|handle| same_person(handle, sender))
            .unwrap_or(0)
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
    fn quoted(&self, message: &Message) -> Option<&Message> {
        let raw = message
            .thread_originator_guid
            .as_deref()
            .or(message.reply_to_guid.as_deref())?;
        let (_, guid) = split_association(raw)?;
        let index = *self.by_guid.get(guid)?;
        self.messages.get(index)
    }
}

/// Whether two participant rows are the same person: the same name where
/// Contacts knows one, and otherwise the same address.
fn same_person(left: &crate::db::Handle, right: &crate::db::Handle) -> bool {
    match (left.name.as_ref(), right.name.as_ref()) {
        (Some(left), Some(right)) if !left.is_empty() && !right.is_empty() => left == right,
        _ => left.id.eq_ignore_ascii_case(&right.id),
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
            day: None,
            lines: Vec::new(),
            rail: None,
            band: false,
            links: Vec::new(),
            images: Vec::new(),
        };
    };
    let room = body_width(columns);
    let day = day_separator(ctx, index);

    if message.is_announcement() {
        return system_block(ctx, message, day, room);
    }

    let theme = ctx.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut links: Vec<Link> = Vec::new();

    if let Some(target) = ctx.quoted(message) {
        lines.push(quote_line(ctx, target, room));
    }

    // In a group the sender's name opens the first body line, which is why the
    // first line is wrapped to a narrower column than the rest.
    let name = if ctx.is_group() {
        Some(ctx.sender(message))
    } else {
        None
    };
    let prefix = name.as_ref().map_or(0, |name| width(name) + 2);
    let body = message.text.as_deref().unwrap_or_default();
    let wrapped = wrap(body, room.saturating_sub(prefix), room);

    for (row, text) in wrapped.iter().enumerate() {
        let mut spans = Vec::new();
        let mut column = 0u16;
        if row == 0
            && let Some(name) = name.as_ref()
        {
            spans.push(Span::styled(
                name.clone(),
                Style::new()
                    .fg(ctx.accent(message))
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw("  "));
            column = u16::try_from(prefix).unwrap_or(u16::MAX);
        }
        let row_index = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let (text_spans, found) = link_spans(text, theme, row_index, column);
        spans.extend(text_spans);
        links.extend(found);
        lines.push(Line::from(spans));
    }
    // A body of nothing but an attachment leaves an empty row behind; the chip
    // below says everything there is to say. A group keeps the row, because the
    // sender's name is on it.
    if body.is_empty() && name.is_none() && !message.attachments.is_empty() {
        lines.pop();
    }

    let room_columns = u16::try_from(room).unwrap_or(u16::MAX);
    let mut images: Vec<ImageSpot> = Vec::new();
    let mut first_inline: Option<&AttachmentRef> = None;

    for (index, attachment) in message.attachments.iter().enumerate() {
        if attachment.hide_attachment {
            continue;
        }
        // A picture the terminal can draw takes rows of its own; the name and
        // the size then ride on the meta line, the way the mockup has them.
        if let Some((columns, rows)) = ctx.images.cells(attachment, room_columns) {
            images.push(ImageSpot {
                row: u16::try_from(lines.len()).unwrap_or(u16::MAX),
                columns,
                rows,
                attachment: index,
            });
            for _ in 0..rows {
                lines.push(Line::default());
            }
            if first_inline.is_none() {
                first_inline = Some(attachment);
            }
            continue;
        }
        lines.push(Line::from(chip_spans(attachment, theme, room)));
    }

    let note = first_inline.map(|attachment| inline_note(attachment, images.len()));
    lines.extend(meta_lines(
        ctx,
        message,
        room,
        note.as_deref(),
        ctx.is_latest_mine(index),
    ));

    Block {
        day,
        lines,
        rail: Some(ctx.accent(message)),
        band: message.is_from_me,
        links,
        images,
    }
}

/// What the meta line says about the pictures drawn above it:
/// `IMG_4412.jpg · 2.1 MB`, or `3 photos` when a message carried several.
fn inline_note(attachment: &AttachmentRef, drawn: usize) -> String {
    if drawn > 1 {
        return format!("{drawn} photos");
    }
    let name = attachment
        .display_name()
        .filter(|name| !name.is_empty())
        .map_or_else(|| "Photo".to_string(), ToString::to_string);
    if attachment.total_bytes > 0 {
        return format!("{name} · {}", bytes(attachment.total_bytes));
    }
    name
}

/// A rename, a join, or a leave: dim, italic, and without a rail.
fn system_block(ctx: &Ctx<'_>, message: &Message, day: Option<String>, room: usize) -> Block {
    let style = Style::new()
        .fg(ctx.theme.system)
        .add_modifier(Modifier::ITALIC);
    let lines = wrap(&system_text(ctx, message), room, room)
        .into_iter()
        .map(|text| Line::from(Span::styled(text, style)))
        .collect();
    Block {
        day,
        lines,
        rail: None,
        band: false,
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
    let previous = index
        .checked_sub(1)
        .and_then(|before| ctx.messages.get(before))
        .and_then(Message::sent_at);
    match previous {
        Some(before) if before.date_naive() == when.date_naive() => None,
        _ => Some(day_label(ctx.now, when)),
    }
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

/// The meta line, and the tapback chips that ride on the end of it.
fn meta_lines(
    ctx: &Ctx<'_>,
    message: &Message,
    room: usize,
    note: Option<&str>,
    stamp: bool,
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let meta = match note {
        Some(note) => format!("{} · {note}", meta_text(message, stamp)),
        None => meta_text(message, stamp),
    };
    let chips = tapback_chips(ctx, message);

    let mut spans = vec![Span::styled(
        truncate(&meta, room),
        Style::new().fg(theme.gray),
    )];
    let mut used = width(&meta).min(room);
    let mut lines = Vec::new();

    // `· Sending…` rides on the end of the meta line rather than taking a row
    // of its own, so a block does not change height when the send lands.
    if let Some((note, color)) = ctx.pending_note(message) {
        let room_left = room.saturating_sub(used + 1);
        if room_left > 0 {
            let note = truncate(&note, room_left);
            used += width(&note) + 1;
            spans.push(Span::raw(" "));
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
            Style::new().bg(theme.bg_highlight).fg(theme.text_secondary),
        ));
        used += cells;
    }
    lines.push(Line::from(spans));
    lines
}

/// `18:02`, `18:06 · Delivered`, `18:02 · Read 18:05`.
///
/// `stamp` says whether this message is the one the receipt belongs under —
/// [`Ctx::is_latest_mine`] — so an older message of yours is just its clock,
/// the way Messages.app draws it.
#[must_use]
pub fn meta_text(message: &Message, stamp: bool) -> String {
    let sent = message.sent_at().map(clock).unwrap_or_default();
    if !message.is_from_me || !stamp {
        return sent;
    }
    if let Some(read) = message.read_at() {
        return format!("{sent} · Read {}", clock(read));
    }
    if message.delivered_at().is_some() {
        return format!("{sent} · Delivered");
    }
    sent
}

/// The reactions standing on a message, as chip labels.
///
/// A one-to-one chat has room to say who reacted; a group says how many did.
#[must_use]
pub fn tapback_chips(ctx: &Ctx<'_>, message: &Message) -> Vec<String> {
    let standing = ctx.tapbacks(message);
    let mut kinds: Vec<(String, Vec<&Tapback>)> = Vec::new();
    for tapback in &standing {
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
        }
    }

    fn chat(is_group: bool) -> Chat {
        Chat {
            rowid: 1,
            guid: "iMessage;-;chat1".to_string(),
            identifier: Some("chat1".to_string()),
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
                reactions: &[],
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
    fn a_block_is_a_body_and_a_meta_line_under_a_day_separator() {
        let fixture = Fixture::new(false, vec![message(1, false, "yes!! 7?")]);
        let block = block(&fixture.ctx(), 0, 60);

        assert_eq!(block.day.as_deref(), Some("Today"));
        assert_eq!(block.lines.len(), 2);
        assert_eq!(text_of(&block.lines[0]), "yes!! 7?");
        assert_eq!(block.height(), 3);
        assert_eq!(block.rail, Some(Theme::default().accent_them));
        assert!(!block.band);
    }

    #[test]
    fn your_own_message_gets_the_band_and_the_blue_rail() {
        let fixture = Fixture::new(false, vec![message(1, true, "on my way")]);
        let block = block(&fixture.ctx(), 0, 60);
        assert_eq!(block.rail, Some(Theme::default().accent_me));
        assert!(block.band);
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
    fn a_group_names_the_sender_and_colors_them_by_participant_order() {
        let mut second = message(2, false, "in. keeping my keeper");
        second.handle_rowid = Some(2);
        let fixture = Fixture::new(true, vec![message(1, false, "who's in?"), second]);
        let ctx = fixture.ctx();

        let first = block(&ctx, 0, 60);
        assert!(text_of(&first.lines[0]).starts_with("alex  "));
        assert_eq!(first.rail, Some(Theme::default().participant(0)));

        let second = block(&ctx, 1, 60);
        assert!(text_of(&second.lines[0]).starts_with("bailey  "));
        assert_eq!(second.rail, Some(Theme::default().participant(1)));
    }

    #[test]
    fn a_one_to_one_chat_shows_no_names() {
        let fixture = Fixture::new(false, vec![message(1, false, "hello")]);
        assert_eq!(text_of(&block(&fixture.ctx(), 0, 60).lines[0]), "hello");
    }

    #[test]
    fn a_long_body_wraps_to_the_pane_and_grows_the_block() {
        let long = "wrap ".repeat(40);
        let fixture = Fixture::new(false, vec![message(1, false, long.trim())]);
        let narrow = block(&fixture.ctx(), 0, 30);
        let wide = block(&fixture.ctx(), 0, 100);
        assert!(narrow.height() > wide.height());
        for line in &narrow.lines {
            assert!(width(&text_of(line)) <= body_width(30));
        }
    }

    #[test]
    fn delivery_stamps_only_appear_on_your_own_messages() {
        let mut mine = message(1, true, "sent");
        assert_eq!(meta_text(&mine, true), "18:20", "no stamp before delivery");

        mine.date_delivered = stamp(9);
        assert_eq!(meta_text(&mine, true), "18:20 · Delivered");

        mine.date_read = stamp(8);
        assert_eq!(meta_text(&mine, true), "18:20 · Read 18:22");

        // Incoming messages never carry a stamp, even when the column is set.
        let mut theirs = message(2, false, "got it");
        theirs.date_delivered = stamp(9);
        theirs.date_read = stamp(8);
        assert_eq!(meta_text(&theirs, true), "18:20");
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
            text_of(block.lines.last().expect("a meta line"))
        };
        assert!(!meta(0).contains("Read"), "the older one is just its clock");
        assert!(meta(1).contains("· Read"), "the newest of yours is stamped");
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
    fn a_reply_to_a_message_off_the_page_is_not_quoted() {
        let mut reply = message(2, false, "can we do 4?");
        reply.thread_originator_guid = Some("p:0/MISSING".to_string());
        let fixture = Fixture::new(true, vec![reply]);
        let block = block(&fixture.ctx(), 0, 60);
        assert!(!text_of(&block.lines[0]).contains('↳'));
    }

    #[test]
    fn a_group_event_is_a_railless_italic_line() {
        let mut event = message(3, false, "");
        event.item_type = 2;
        event.group_action = Some(GroupAction::NameChange("Sunday Football".to_string()));
        let fixture = Fixture::new(true, vec![event]);
        let block = block(&fixture.ctx(), 0, 80);

        assert!(block.rail.is_none());
        assert!(!block.band);
        assert_eq!(block.lines.len(), 1);
        assert!(text_of(&block.lines[0]).contains("named the conversation"));
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
        assert_eq!(block.lines.len(), 2, "chip and meta, no empty body row");
        let chip = text_of(&block.lines[0]);
        assert!(chip.contains("📄 draft-order.pdf · 84 KB"), "{chip}");
        assert!(chip.starts_with(CHIP_EDGE), "the dashed edge: {chip}");
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
        assert_eq!(spot.row, 0, "the picture opens the block");
        assert_eq!(spot.attachment, 0);
        // Twenty by ten cells at a ten-by-twenty font: five rows for the rows.
        assert_eq!((spot.columns, spot.rows), (20, 5));
        assert_eq!(block.lines.len(), usize::from(spot.rows) + 1);
        for row in 0..usize::from(spot.rows) {
            assert_eq!(text_of(&block.lines[row]), "", "row {row} is left blank");
        }
        let meta = text_of(&block.lines[usize::from(spot.rows)]);
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
        let chip = text_of(&block.lines[0]);
        assert!(chip.contains(NOT_DOWNLOADED), "{chip}");
        assert!(block.images.is_empty(), "nothing to draw");
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
        assert_eq!(link.column, 10);
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
                let expected =
                    u16::try_from(block.lines.len()).unwrap() + u16::from(block.day.is_some());
                assert_eq!(block.height(), expected);
                assert!(block.height() >= 1);
            }
        }
    }
}
