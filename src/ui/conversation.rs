//! The right pane: a header with the chat's identity, then message blocks.
//!
//! Messages are different heights, so the pane does not scroll by rows of a
//! flat list. [`Scroll`] anchors the viewport to a message and a number of rows
//! of it hidden above the top edge, which means moving the view only ever
//! touches the blocks around the edges rather than everything above them. The
//! heights it works from live in [`Measured`], recomputed only when the page or
//! the pane width changes, and only the blocks actually on screen are laid out
//! into styled lines each frame.

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as BlockWidget, Paragraph};

use std::collections::HashMap;

use crate::app::App;
use crate::db::{Chat, Message};
use crate::ui::format::{day_label, thousands, truncate};
use crate::ui::message::{self, CHROME, Ctx, GAP, MARGIN_LEFT, RAIL, RAIL_GLYPH};

/// Cached row heights of the loaded page, plus the index replies are resolved
/// through.
///
/// Both are derived from the same page at the same width, so they are stored
/// and replaced together and can never describe different states.
#[derive(Debug, Default)]
pub struct Measured {
    /// Pane width the heights were measured at.
    pub width: u16,
    /// `message.ROWID` of the first row measured.
    pub first: i64,
    /// `message.ROWID` of the last row measured.
    pub last: i64,
    /// Height of each loaded message, in rows.
    pub heights: Vec<u16>,
    /// `message.guid` to its index on the loaded page.
    pub by_guid: HashMap<String, usize>,
    /// Set when a loaded row changed without the page changing shape — an edit
    /// or a tapback — which nothing else about the page would reveal.
    pub stale: bool,
}

/// Where the conversation is scrolled to.
///
/// `top` is the message at the top edge of the pane and `skip` how many of its
/// rows are above that edge, so a block taller than the viewport can still be
/// read through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scroll {
    /// Index of the message at the top of the viewport.
    pub top: usize,
    /// Rows of that message hidden above the top of the viewport.
    pub skip: u16,
}

/// One message on screen, and which of its rows are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visible {
    /// Index of the message on the loaded page.
    pub index: usize,
    /// First row of the pane it occupies.
    pub y: u16,
    /// Rows of the block hidden above the pane.
    pub skip: u16,
    /// Rows of the block that are on screen.
    pub rows: u16,
}

impl Scroll {
    /// Whether the very first loaded message is fully in view, which is the
    /// moment to ask the database for an older page.
    #[must_use]
    pub const fn at_start(&self) -> bool {
        self.top == 0 && self.skip == 0
    }

    /// Every message with a row on screen, top to bottom.
    #[must_use]
    pub fn visible(&self, heights: &[u16], viewport: u16) -> Vec<Visible> {
        let mut out = Vec::new();
        let mut y = 0u16;
        let mut index = self.top;
        let mut skip = self.skip;
        while y < viewport && index < heights.len() {
            let rows = heights[index].saturating_sub(skip).min(viewport - y);
            if rows > 0 {
                out.push(Visible {
                    index,
                    y,
                    skip,
                    rows,
                });
                y += rows;
            }
            index += 1;
            skip = 0;
        }
        out
    }

    /// Move the viewport by `delta` rows, negative for up.
    pub fn by_rows(&mut self, heights: &[u16], viewport: u16, delta: i64) {
        if delta < 0 {
            self.up(heights, delta.unsigned_abs());
        } else {
            self.down(heights, delta.unsigned_abs());
        }
        self.clamp(heights, viewport);
    }

    fn up(&mut self, heights: &[u16], mut rows: u64) {
        while rows > 0 {
            if self.skip > 0 {
                let take = rows.min(u64::from(self.skip));
                self.skip -= u16::try_from(take).unwrap_or(self.skip);
                rows -= take;
            } else if self.top > 0 {
                self.top -= 1;
                self.skip = heights.get(self.top).copied().unwrap_or(1);
            } else {
                break;
            }
        }
    }

    fn down(&mut self, heights: &[u16], mut rows: u64) {
        while rows > 0 && self.top < heights.len() {
            let remaining = u64::from(heights[self.top].saturating_sub(self.skip));
            if rows < remaining {
                self.skip += u16::try_from(rows).unwrap_or(0);
                return;
            }
            rows -= remaining;
            self.top += 1;
            self.skip = 0;
        }
    }

    /// Put the newest message at the bottom of the pane.
    pub fn to_bottom(&mut self, heights: &[u16], viewport: u16) {
        let mut total = 0u32;
        let mut index = heights.len();
        while index > 0 && total < u32::from(viewport) {
            index -= 1;
            total += u32::from(heights[index]);
        }
        self.top = index;
        self.skip = u16::try_from(total.saturating_sub(u32::from(viewport))).unwrap_or(0);
    }

    /// Put the oldest loaded message at the top of the pane.
    pub const fn to_top(&mut self) {
        self.top = 0;
        self.skip = 0;
    }

    /// Pull the viewport back so it never hangs below the last message.
    pub fn clamp(&mut self, heights: &[u16], viewport: u16) {
        if heights.is_empty() {
            *self = Self::default();
            return;
        }
        if self.top >= heights.len() {
            self.to_bottom(heights, viewport);
            return;
        }
        let mut rows = 0u32;
        for height in &heights[self.top..] {
            rows += u32::from(*height);
            if rows >= u32::from(viewport) + u32::from(self.skip) {
                return;
            }
        }
        self.to_bottom(heights, viewport);
    }

    /// Scroll as little as is needed to show all of message `index`.
    pub fn reveal(&mut self, heights: &[u16], viewport: u16, index: usize) {
        if index >= heights.len() {
            return;
        }
        if index < self.top {
            self.top = index;
            self.skip = 0;
            self.clamp(heights, viewport);
            return;
        }
        let above = heights[self.top..index]
            .iter()
            .map(|height| i64::from(*height))
            .sum::<i64>()
            - i64::from(self.skip);
        let height = i64::from(heights[index]);
        if above < 0 || height >= i64::from(viewport) {
            self.top = index;
            self.skip = 0;
        } else if above + height > i64::from(viewport) {
            self.down(
                heights,
                (above + height - i64::from(viewport)).unsigned_abs(),
            );
        }
        self.clamp(heights, viewport);
    }
}

/// A link as it was drawn on the last frame, in terminal coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    /// The row it is on.
    pub y: u16,
    /// First column it covers.
    pub start: u16,
    /// One past the last column it covers.
    pub end: u16,
    /// Where it points.
    pub url: String,
}

/// What the last frame put where, so a click can act on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits {
    /// Message drawn on each row of the pane, from its top edge down.
    pub rows: Vec<Option<usize>>,
    /// Links drawn on the pane.
    pub links: Vec<LinkHit>,
    /// Where the `↓ N new` pill was drawn, when one was.
    pub pill: Option<Rect>,
}

impl Hits {
    /// The message drawn at absolute terminal row `row`.
    #[must_use]
    pub fn message_at(&self, area: Rect, row: u16) -> Option<usize> {
        if row < area.y {
            return None;
        }
        self.rows.get(usize::from(row - area.y)).copied().flatten()
    }

    /// Whether an absolute terminal cell is inside the `↓ N new` pill.
    #[must_use]
    pub fn pill_at(&self, column: u16, row: u16) -> bool {
        self.pill
            .is_some_and(|rect| rect.contains(Position::new(column, row)))
    }

    /// The link under an absolute terminal cell.
    #[must_use]
    pub fn link_at(&self, column: u16, row: u16) -> Option<&str> {
        self.links
            .iter()
            .find(|hit| hit.y == row && column >= hit.start && column < hit.end)
            .map(|hit| hit.url.as_str())
    }
}

/// Chat name on the left, counts on the right, with a rule underneath when
/// there is a row to spare for it.
pub fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    let title = Rect { height: 1, ..area };

    let left = match app.selected_chat() {
        Some(chat) => Line::from(vec![
            Span::styled(
                format!(" {}", truncate(&chat.title(), title_room(area))),
                Style::new()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(subtitle(chat), Style::new().fg(theme.gray)),
        ]),
        None => Line::from(vec![
            Span::styled(
                " no conversation",
                Style::new()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · pick a chat on the left", Style::new().fg(theme.gray)),
        ]),
    };
    frame.render_widget(Paragraph::new(left), title);

    if let Some(chat) = app.selected_chat()
        && area.width >= 40
    {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                counts(chat, app.open_chat_photos),
                Style::new().fg(theme.gray),
            )))
            .alignment(Alignment::Right),
            title,
        );
    }

    if area.height >= 2 {
        let rule = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(usize::from(area.width)),
                Style::new().fg(theme.border),
            ))),
            rule,
        );
    }
}

/// Half the header, so a long chat name cannot push the service off the row.
fn title_room(area: Rect) -> usize {
    usize::from(area.width).saturating_sub(2) / 2
}

/// `· iMessage · +1 (650) 555-0198` for one person, `· iMessage · 6 people`
/// for a group — as much of it as is true.
///
/// This is the address half of the header, deliberately: the title beside it
/// already says the name, so repeating it here would waste the row. The
/// address is the thing a name hides and the header is where you go to see it.
#[must_use]
pub fn subtitle(chat: &Chat) -> String {
    let mut parts = Vec::new();
    if let Some(service) = chat.service.as_deref().filter(|s| !s.is_empty()) {
        parts.push(service.to_string());
    }
    if chat.is_group {
        parts.push(format!("{} people", chat.participants.len()));
    } else if let Some(handle) = chat.participants.first() {
        parts.push(crate::db::handle::display_name(&handle.id));
    } else if let Some(id) = chat.identifier.as_deref().filter(|id| !id.is_empty()) {
        parts.push(crate::db::handle::display_name(id));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" · {}", parts.join(" · "))
}

/// `1,204 msgs · 38 photos`, with the photo half left off when there are none.
#[must_use]
pub fn counts(chat: &Chat, photos: i64) -> String {
    let messages = chat.message_count;
    let mut out = format!(
        "{} msg{}",
        thousands(messages),
        if messages == 1 { "" } else { "s" }
    );
    if photos > 0 {
        out.push_str(&format!(
            " · {} photo{}",
            thousands(photos),
            if photos == 1 { "" } else { "s" }
        ));
    }
    out.push(' ');
    out
}

/// Draw the message blocks, and report where they landed.
pub fn render(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let mut hits = Hits::default();
    if area.width == 0 || area.height == 0 {
        return hits;
    }
    hits.rows = vec![None; usize::from(area.height)];
    let theme = &app.theme;

    if app.message_rows.is_empty() {
        let middle = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no messages yet",
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            )))
            .alignment(Alignment::Center),
            middle,
        );
        return hits;
    }
    if area.width <= CHROME {
        return hits;
    }

    let now = Local::now();
    let ctx = Ctx {
        theme,
        chat: app.selected_chat(),
        messages: &app.message_rows,
        by_guid: &app.measured.by_guid,
        pending: &app.pending,
        now,
        images: &app.images,
        contacts: &app.contacts,
    };
    let heights = &app.measured.heights;
    let body = Rect {
        x: area.x + MARGIN_LEFT + RAIL,
        width: area.width - MARGIN_LEFT - RAIL - message::MARGIN_RIGHT,
        y: area.y,
        height: 1,
    };
    let text_x = body.x + GAP;
    let text_width = body.width - GAP;
    // The width the blocks were laid out at, which is what a picture is filed
    // under in the cache.
    let room = u16::try_from(message::body_width(area.width)).unwrap_or(u16::MAX);

    let visible = app.convo.visible(heights, area.height);
    for entry in &visible {
        let block = message::block(&ctx, entry.index, area.width);
        let selected = app.messages.selected == entry.index;
        let day_rows = u16::from(block.day.is_some());

        for offset in 0..entry.rows {
            let row = entry.skip + offset;
            let y = area.y + entry.y + offset;
            hits.rows[usize::from(entry.y + offset)] = Some(entry.index);

            if row < day_rows {
                let label = block.day.clone().unwrap_or_default();
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(label, Style::new().fg(theme.gray))))
                        .alignment(Alignment::Center),
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                );
                continue;
            }

            let Some(line) = block.lines.get(usize::from(row - day_rows)) else {
                continue;
            };
            let strip = Rect { y, ..body };

            // The band behind your own messages, and the outline the selected
            // block gets, both live behind the words rather than around them.
            if selected {
                frame.render_widget(
                    BlockWidget::new().style(Style::new().bg(theme.bg_highlight)),
                    strip,
                );
            } else if block.band {
                frame.render_widget(
                    BlockWidget::new().style(Style::new().bg(theme.bg_light)),
                    strip,
                );
            }

            if let Some(rail) = block.rail {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(RAIL_GLYPH, Style::new().fg(rail)))),
                    Rect {
                        x: area.x + MARGIN_LEFT,
                        y,
                        width: RAIL,
                        height: 1,
                    },
                );
            }

            frame.render_widget(
                Paragraph::new(line.clone()),
                Rect {
                    x: text_x,
                    y,
                    width: text_width,
                    height: 1,
                },
            );
        }

        // Pictures go on after the rows they sit over, so the band and the
        // selection tint are underneath rather than over them. A block taller
        // than the pane hands the picture a negative offset and the protocol
        // clips the rows that scrolled past the top edge.
        for spot in &block.images {
            let top = i32::from(entry.y) + i32::from(spot.row) + i32::from(day_rows)
                - i32::from(entry.skip);
            let Some(attachment) = app
                .message_rows
                .get(entry.index)
                .and_then(|message| message.attachments.get(spot.attachment))
            else {
                continue;
            };
            let strip = Rect {
                x: text_x,
                y: area.y,
                width: spot.columns.min(text_width),
                height: area.height,
            };
            let offset = i16::try_from(top.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                .unwrap_or_default();
            app.images
                .render(frame.buffer_mut(), strip, offset, attachment, room);
        }

        for link in &block.links {
            let Some(offset) = (link.row + day_rows).checked_sub(entry.skip) else {
                continue;
            };
            if offset >= entry.rows {
                continue;
            }
            let start = text_x.saturating_add(link.column);
            hits.links.push(LinkHit {
                y: area.y + entry.y + offset,
                start,
                end: start.saturating_add(link.cells).min(text_x + text_width),
                url: link.url.clone(),
            });
        }
    }

    render_sticky_day(frame, app, area, &visible, now);
    hits.pill = render_new_pill(frame, app, area);
    hits
}

/// The label on the pill: `↓ 3 new`, padded so it reads as a chip.
#[must_use]
pub fn new_pill_label(count: usize) -> String {
    format!(" ↓ {count} new ")
}

/// The `↓ N new` pill, sitting on the bottom edge of the pane while messages
/// have arrived below what the reader is looking at.
///
/// It carries a count and nothing else — never a name and never a line of the
/// message it is counting.
fn render_new_pill(frame: &mut Frame, app: &App, area: Rect) -> Option<Rect> {
    if app.new_below == 0 || area.width == 0 || area.height == 0 {
        return None;
    }
    let label = new_pill_label(app.new_below);
    let width = u16::try_from(crate::ui::format::width(&label))
        .unwrap_or(u16::MAX)
        .min(area.width);
    if width == 0 {
        return None;
    }
    let rect = Rect {
        x: area.x + area.width - width,
        y: area.y + area.height - 1,
        width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::new()
                .bg(app.theme.accent_me)
                .fg(app.theme.bg_base)
                .add_modifier(Modifier::BOLD),
        ))),
        rect,
    );
    Some(rect)
}

/// The day of the topmost message, held at the top edge while it scrolls.
fn render_sticky_day(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    visible: &[Visible],
    now: chrono::DateTime<Local>,
) {
    let Some(first) = visible.first() else {
        return;
    };
    let Some(when) = app.message_rows.get(first.index).and_then(Message::sent_at) else {
        return;
    };
    let theme = &app.theme;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(day_label(now, when), Style::new().fg(theme.text_secondary)),
        ]))
        .style(Style::new().bg(theme.bg_light)),
        Rect { height: 1, ..area },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six messages: two rows, then three, then two, and so on.
    fn heights() -> Vec<u16> {
        vec![2, 3, 2, 4, 2, 3]
    }

    #[test]
    fn the_visible_walk_starts_at_the_anchor_and_stops_at_the_edge() {
        let scroll = Scroll { top: 1, skip: 1 };
        let visible = scroll.visible(&heights(), 5);
        assert_eq!(
            visible
                .iter()
                .map(|entry| (entry.index, entry.y, entry.skip, entry.rows))
                .collect::<Vec<_>>(),
            vec![(1, 0, 1, 2), (2, 2, 0, 2), (3, 4, 0, 1)]
        );
    }

    #[test]
    fn scrolling_down_moves_through_blocks_a_row_at_a_time() {
        let heights = heights();
        let mut scroll = Scroll::default();
        scroll.by_rows(&heights, 6, 1);
        assert_eq!(scroll, Scroll { top: 0, skip: 1 });
        scroll.by_rows(&heights, 6, 1);
        assert_eq!(scroll, Scroll { top: 1, skip: 0 });
        scroll.by_rows(&heights, 6, 4);
        assert_eq!(scroll, Scroll { top: 2, skip: 1 });
    }

    #[test]
    fn scrolling_up_walks_back_into_the_block_above() {
        let heights = heights();
        let mut scroll = Scroll { top: 3, skip: 1 };
        scroll.by_rows(&heights, 6, -1);
        assert_eq!(scroll, Scroll { top: 3, skip: 0 });
        scroll.by_rows(&heights, 6, -1);
        assert_eq!(scroll, Scroll { top: 2, skip: 1 });
        scroll.by_rows(&heights, 6, -99);
        assert_eq!(scroll, Scroll::default(), "and stops at the very top");
    }

    #[test]
    fn the_view_never_hangs_below_the_last_message() {
        let heights = heights();
        let mut scroll = Scroll::default();
        scroll.by_rows(&heights, 6, 500);
        // Sixteen rows of messages, six of viewport: the last six are shown,
        // which cuts into the fourth block rather than past the sixth.
        assert_eq!(scroll, Scroll { top: 3, skip: 3 });
        let visible = scroll.visible(&heights, 6);
        assert_eq!(visible.last().map(|entry| entry.index), Some(5));
    }

    #[test]
    fn a_conversation_shorter_than_the_pane_sits_at_the_top() {
        let heights = vec![2u16, 2];
        let mut scroll = Scroll { top: 1, skip: 1 };
        scroll.clamp(&heights, 40);
        assert_eq!(scroll, Scroll::default());
        scroll.to_bottom(&heights, 40);
        assert_eq!(scroll, Scroll::default());
    }

    #[test]
    fn revealing_a_message_moves_as_little_as_it_can() {
        let heights = heights();
        let mut scroll = Scroll::default();

        scroll.reveal(&heights, 6, 1);
        assert_eq!(scroll, Scroll::default(), "already on screen");

        scroll.reveal(&heights, 6, 3);
        assert_eq!(scroll, Scroll { top: 2, skip: 0 }, "scrolled down to it");

        scroll.reveal(&heights, 6, 0);
        assert_eq!(scroll, Scroll::default(), "and back up to it");
    }

    #[test]
    fn a_block_taller_than_the_pane_is_shown_from_its_top() {
        let heights = vec![2u16, 20, 2];
        let mut scroll = Scroll::default();
        scroll.reveal(&heights, 6, 1);
        assert_eq!(scroll, Scroll { top: 1, skip: 0 });
    }

    #[test]
    fn an_empty_conversation_scrolls_nowhere() {
        let mut scroll = Scroll { top: 4, skip: 2 };
        scroll.by_rows(&[], 10, 3);
        assert_eq!(scroll, Scroll::default());
        assert!(scroll.visible(&[], 10).is_empty());
        assert!(scroll.at_start());
    }

    #[test]
    fn counts_read_as_a_sentence_and_drop_what_is_not_there() {
        let mut chat = crate::db::Chat {
            rowid: 1,
            guid: "g".to_string(),
            identifier: None,
            display_name: None,
            service: None,
            style: 45,
            is_group: false,
            participants: Vec::new(),
            last_message_date: 0,
            last_message_rowid: 0,
            preview: None,
            message_count: 1204,
            unread_count: 0,
            is_pinned: None,
        };
        assert_eq!(counts(&chat, 38), "1,204 msgs · 38 photos ");
        assert_eq!(counts(&chat, 0), "1,204 msgs ");
        chat.message_count = 1;
        assert_eq!(counts(&chat, 1), "1 msg · 1 photo ");
    }
}
