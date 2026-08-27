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

use crate::db::handle;
use crate::db::message::split_association;
use crate::db::{AttachmentRef, Chat, GroupAction, Message, Tapback};
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
    /// The clock, passed in so day labels are testable.
    pub now: DateTime<Local>,
}

impl Ctx<'_> {
    /// Whether the open conversation has more than two people in it.
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.chat.is_some_and(|chat| chat.is_group)
    }

    /// The rail color for a message.
    ///
    /// Yours is always the blue accent. In a one-to-one chat the other person
    /// is the green one; in a group everybody keeps the color their position in
    /// the participant list gives them, and that list is ordered by
    /// `handle.ROWID`, so a color follows a person across sessions.
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
        chat.participants
            .iter()
            .position(|handle| handle.rowid == rowid)
            .unwrap_or(0)
    }

    /// The name to show for whoever sent a message.
    #[must_use]
    pub fn sender(&self, message: &Message) -> String {
        if message.is_from_me {
            return "You".to_string();
        }
        self.person(message.handle_rowid)
            .or_else(|| message.handle.as_deref().map(handle::short_name))
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

    for attachment in &message.attachments {
        if attachment.hide_attachment {
            continue;
        }
        lines.push(Line::from(Span::styled(
            truncate(&chip(attachment), room),
            Style::new().fg(theme.text_secondary),
        )));
    }

    lines.extend(meta_lines(ctx, message, room));

    Block {
        day,
        lines,
        rail: Some(ctx.accent(message)),
        band: message.is_from_me,
        links,
    }
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

/// `📄 draft-order.pdf · 84 KB`.
fn chip(attachment: &AttachmentRef) -> String {
    let kind = attachment.kind();
    let name = attachment
        .display_name()
        .filter(|name| !name.is_empty())
        .map_or_else(|| kind.label().to_string(), ToString::to_string);
    if attachment.total_bytes > 0 {
        return format!(
            "{} {name} · {}",
            kind.glyph(),
            bytes(attachment.total_bytes)
        );
    }
    format!("{} {name}", kind.glyph())
}

/// The meta line, and the tapback chips that ride on the end of it.
fn meta_lines(ctx: &Ctx<'_>, message: &Message, room: usize) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let meta = meta_text(message);
    let chips = tapback_chips(ctx, message);

    let mut spans = vec![Span::styled(
        truncate(&meta, room),
        Style::new().fg(theme.gray),
    )];
    let mut used = width(&meta);
    let mut lines = Vec::new();

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
#[must_use]
pub fn meta_text(message: &Message) -> String {
    let sent = message.sent_at().map(clock).unwrap_or_default();
    if !message.is_from_me {
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
    let mut kinds: Vec<(String, Vec<&Tapback>)> = Vec::new();
    for tapback in &message.tapbacks {
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
                            .unwrap_or_else(|| handle::short_name(handle))
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
                Handle {
                    rowid: 1,
                    id: "alex@example.invalid".to_string(),
                    service: "iMessage".to_string(),
                },
                Handle {
                    rowid: 2,
                    id: "bailey@example.invalid".to_string(),
                    service: "iMessage".to_string(),
                },
            ],
            last_message_date: 0,
            last_message_rowid: 0,
            preview: None,
            message_count: 0,
            unread_count: 0,
            is_pinned: None,
        }
    }

    struct Fixture {
        theme: Theme,
        chat: Chat,
        messages: Vec<Message>,
        by_guid: HashMap<String, usize>,
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
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                theme: &self.theme,
                chat: Some(&self.chat),
                messages: &self.messages,
                by_guid: &self.by_guid,
                now: now(),
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
        assert_eq!(meta_text(&mine), "18:20", "no stamp before delivery");

        mine.date_delivered = stamp(9);
        assert_eq!(meta_text(&mine), "18:20 · Delivered");

        mine.date_read = stamp(8);
        assert_eq!(meta_text(&mine), "18:20 · Read 18:22");

        // Incoming messages never carry a stamp, even when the column is set.
        let mut theirs = message(2, false, "got it");
        theirs.date_delivered = stamp(9);
        theirs.date_read = stamp(8);
        assert_eq!(meta_text(&theirs), "18:20");
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
        let block = block(&fixture.ctx(), 0, 60);
        assert_eq!(block.lines.len(), 2, "chip and meta, no empty body row");
        assert_eq!(text_of(&block.lines[0]), "📄 draft-order.pdf · 84 KB");
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
