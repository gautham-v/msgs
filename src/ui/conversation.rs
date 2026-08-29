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
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as BlockWidget, Paragraph};

use std::collections::HashMap;

use crate::app::App;
use crate::db::{Chat, Message};
use crate::ui::format::{day_label, truncate, width};
use crate::ui::message::{self, CHROME, Ctx, MARGIN_LEFT};

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

/// A picture as it was drawn on the last frame, in terminal coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageHit {
    /// The cells it covers on screen, clipped to the pane.
    pub rect: Rect,
    /// Index of the message it belongs to, in `App::message_rows`.
    pub message: usize,
    /// Index of the attachment on that message.
    pub attachment: usize,
}

/// What the last frame put where, so a click can act on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits {
    /// Message drawn on each row of the pane, from its top edge down.
    pub rows: Vec<Option<usize>>,
    /// Links drawn on the pane.
    pub links: Vec<LinkHit>,
    /// Pictures drawn on the pane.
    pub images: Vec<ImageHit>,
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

    /// The `(message, attachment)` of the picture under an absolute cell.
    #[must_use]
    pub fn image_at(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        self.images
            .iter()
            .find(|hit| hit.rect.contains(Position::new(column, row)))
            .map(|hit| (hit.message, hit.attachment))
    }
}

/// A drag of the mouse over the conversation, in absolute terminal cells.
///
/// It is a linear selection, the way a terminal's own is: everything between
/// the two ends in reading order, not the rectangle they corner. The scroll
/// and the pane it was made against are kept with it so a view that has moved
/// underneath drops it rather than tinting cells that now say something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The cell the drag started on.
    pub anchor: Position,
    /// The cell the pointer is on now.
    pub cursor: Position,
    /// Where the conversation was scrolled to when the drag started.
    pub scroll: Scroll,
    /// The pane it was drawn over.
    pub area: Rect,
}

impl Selection {
    /// A selection of the single cell `at`, which is what a click leaves
    /// until the pointer moves.
    #[must_use]
    pub const fn new(at: Position, scroll: Scroll, area: Rect) -> Self {
        Self {
            anchor: at,
            cursor: at,
            scroll,
            area,
        }
    }

    /// The two ends in reading order: row first, then column.
    #[must_use]
    pub fn span(&self) -> (Position, Position) {
        if (self.anchor.y, self.anchor.x) <= (self.cursor.y, self.cursor.x) {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Whether the drag never left the cell it started on, which is a click.
    #[must_use]
    pub fn is_click(&self) -> bool {
        self.anchor == self.cursor
    }
}

/// The half-open column range of `row` that `selection` covers.
///
/// Clipped to the pane's own text columns, so the scrollbar's column
/// ([`message::MARGIN_RIGHT`]) is never selected and never copied. The day
/// band is a pane of its own above this one, so it is out of reach already.
#[must_use]
pub fn selected_columns(selection: &Selection, area: Rect, row: u16) -> Option<(u16, u16)> {
    if area.width <= message::MARGIN_RIGHT || area.height == 0 {
        return None;
    }
    if row < area.y || row >= area.y.saturating_add(area.height) {
        return None;
    }
    let (start, end) = selection.span();
    if row < start.y || row > end.y {
        return None;
    }
    let text_end = area.x + area.width - message::MARGIN_RIGHT;
    let first = if row == start.y {
        start.x.max(area.x)
    } else {
        area.x
    };
    let last = if row == end.y {
        end.x.saturating_add(1)
    } else {
        text_end
    };
    let first = first.clamp(area.x, text_end);
    let last = last.clamp(area.x, text_end);
    (first < last).then_some((first, last))
}

/// What a selection covers, read back off the frame that drew it: the visible
/// cells of each row, trailing blanks trimmed, rows joined with newlines.
///
/// The cells are the words as the reader saw them — wrapping, the name column,
/// and the clock included — because that is what was pointed at.
#[must_use]
pub fn selection_text(buffer: &Buffer, area: Rect, selection: &Selection) -> String {
    let (start, end) = selection.span();
    let mut out = String::new();
    let mut first_row = true;
    for row in start.y..=end.y {
        let Some((first, last)) = selected_columns(selection, area, row) else {
            continue;
        };
        let mut line = String::new();
        for column in first..last {
            if let Some(cell) = buffer.cell((column, row)) {
                line.push_str(cell.symbol());
            }
        }
        if !first_row {
            out.push('\n');
        }
        first_row = false;
        out.push_str(line.trim_end());
    }
    out
}

/// Tint the selected cells with `bg_highlight`.
///
/// The rows are already drawn; only the background under them changes, which
/// is why this is done to the buffer rather than by a widget over the top —
/// nothing a message says is covered by it.
fn render_selection(frame: &mut Frame, app: &App, area: Rect) {
    let Some(selection) = app.selection else {
        return;
    };
    let highlight = app.theme.bg_highlight;
    let buffer = frame.buffer_mut();
    for row in area.y..area.y.saturating_add(area.height) {
        let Some((first, last)) = selected_columns(&selection, area, row) else {
            continue;
        };
        for column in first..last {
            if let Some(cell) = buffer.cell_mut((column, row)) {
                cell.set_bg(highlight);
            }
        }
    }
}

/// The chat's name and address, with a rule underneath when there is a row
/// to spare for it.
pub fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    let title = Rect { height: 1, ..area };

    // The chrome sits on the right of this row; the title stops short of
    // it, the address giving way first and the name last.
    let reserved = usize::from(super::status::reserved(app, title.width)) + 2;
    let room = usize::from(title.width).saturating_sub(reserved + 1);
    let (name, rest, name_color) = match app.selected_chat() {
        // A `Ctrl+N` draft has no row yet: the header is the address it will
        // go to, so the pane never claims to be the chat selected behind it.
        _ if app.draft_target.is_some() => {
            let address = app
                .draft_target
                .as_ref()
                .and_then(|target| target.identifier.as_deref())
                .map(crate::db::handle::display_name)
                .unwrap_or_default();
            (address, " · new message".to_string(), theme.text_primary)
        }
        Some(chat) => (chat.title(), subtitle(chat), theme.text_primary),
        None => (
            "no conversation".to_string(),
            " · pick a chat on the left".to_string(),
            theme.text_secondary,
        ),
    };
    let name = truncate(&name, room.min(title_room(area)));
    let rest = truncate(&rest, room.saturating_sub(width(&name)));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {name}"), Style::new().fg(name_color)),
            Span::styled(rest, Style::new().fg(theme.gray)),
        ])),
        title,
    );

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

/// Most of the header, so a long chat name cannot push the service off the row.
fn title_room(area: Rect) -> usize {
    usize::from(area.width).saturating_sub(2) * 2 / 3
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
        reactions: &app.pending_tapbacks,
        now,
        images: &app.images,
        contacts: &app.contacts,
    };
    let heights = &app.measured.heights;
    // The rows: the words, the gap, and the clock, between the two margins.
    let text_x = area.x + MARGIN_LEFT;
    let text_width = message::row_width(area.width);
    let visible = app.convo.visible(heights, area.height);
    for entry in &visible {
        let block = message::block(&ctx, entry.index, area.width);
        let selected = app.messages.selected == entry.index;
        let lead = block.lead();
        // The label's row: after the blank row, if there is one. The row
        // after the label is blank too.
        let day_row = u16::from(block.gap);

        for offset in 0..entry.rows {
            let row = entry.skip + offset;
            let y = area.y + entry.y + offset;
            hits.rows[usize::from(entry.y + offset)] = Some(entry.index);

            if row < lead {
                // The blank row is just that. The separator names the day —
                // unless the band above the pane already does, when it is the
                // very first thing on screen; then the row stays empty rather
                // than say it twice.
                if row != day_row || block.day.is_none() {
                    continue;
                }
                let named_by_band = app.panes.day.is_some() && entry.y + offset == 0;
                let label = if named_by_band {
                    String::new()
                } else {
                    block.day.clone().unwrap_or_default()
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(label, Style::new().fg(theme.gray)))),
                    Rect {
                        x: text_x,
                        y,
                        width: text_width,
                        height: 1,
                    },
                );
                continue;
            }

            let Some(line) = block.lines.get(usize::from(row - lead)) else {
                continue;
            };
            let strip = Rect {
                x: text_x,
                y,
                width: text_width,
                height: 1,
            };

            // The selected block is the one thing drawn on a background, and
            // it lives behind the words rather than around them.
            if selected {
                frame.render_widget(
                    BlockWidget::new().style(Style::new().bg(theme.bg_highlight)),
                    strip,
                );
            }

            frame.render_widget(Paragraph::new(line.clone()), strip);
        }

        // Pictures go on after the rows they sit over, so the selection tint
        // is underneath rather than over them. A block taller than the pane
        // hands the picture a negative offset and the protocol clips the rows
        // that scrolled past the top edge.
        for spot in &block.images {
            let top =
                i32::from(entry.y) + i32::from(spot.row) + i32::from(lead) - i32::from(entry.skip);
            let Some(attachment) = app
                .message_rows
                .get(entry.index)
                .and_then(|message| spot.picture.of(message))
            else {
                continue;
            };
            let x = text_x.saturating_add(spot.column);
            let strip = Rect {
                x,
                y: area.y,
                width: spot.columns.min(text_width.saturating_sub(spot.column)),
                height: area.height,
            };
            if strip.width == 0 {
                continue;
            }
            let offset = i16::try_from(top.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                .unwrap_or_default();
            // Asked at the width the block measured it at, which is the key
            // the cache holds it under.
            app.images
                .render(frame.buffer_mut(), strip, offset, attachment, spot.room);

            // Where it actually landed, so a click can open it. The rect comes
            // from the very numbers the block reserved and `render` drew into,
            // clipped to the pane rather than measured again.
            // A link preview's picture is not a file the reader can open, so
            // it registers no hit and a click on it does nothing.
            let Some(index) = spot.picture.attachment() else {
                continue;
            };
            let bottom = (top + i32::from(spot.rows)).min(i32::from(area.y + area.height));
            let visible_top = top.max(i32::from(area.y));
            if bottom > visible_top {
                let y = u16::try_from(visible_top).unwrap_or(area.y);
                let height = u16::try_from(bottom - visible_top).unwrap_or_default();
                hits.images.push(ImageHit {
                    rect: Rect {
                        x: strip.x,
                        y,
                        width: strip.width,
                        height,
                    },
                    message: entry.index,
                    attachment: index,
                });
            }
        }

        for link in &block.links {
            let Some(offset) = (link.row + lead).checked_sub(entry.skip) else {
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

    render_scrollbar(frame, app, area, heights);
    // The drag's tint goes on last of the words, and before the pill so the
    // pill stays legible over it.
    render_selection(frame, app, area);
    hits.pill = render_new_pill(frame, app, area);
    hits
}

/// The mockup's `.scroll .bar`: a one-column track down the right edge with a
/// thumb whose length and position say how much of the thread is on screen.
///
/// It lives in the column [`message::MARGIN_RIGHT`] keeps clear, so nothing
/// gives up a cell of words for it, and it is only drawn when there is more
/// thread than pane.
fn render_scrollbar(frame: &mut Frame, app: &App, area: Rect, heights: &[u16]) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let above: u32 = heights
        .iter()
        .take(app.convo.top)
        .map(|height| u32::from(*height))
        .sum::<u32>()
        + u32::from(app.convo.skip);
    let total: u32 = heights.iter().map(|height| u32::from(*height)).sum();
    let Some((start, length)) = thumb(total, area.height, above) else {
        return;
    };
    let x = area.x + area.width - 1;
    let theme = &app.theme;
    // One cell per row, written straight into the buffer: a widget apiece
    // would be a hundred allocations a frame for a hundred single characters.
    let buffer = frame.buffer_mut();
    for row in 0..area.height {
        let inside = row >= start && row < start + length;
        let (glyph, color) = if inside {
            (SCROLL_THUMB, theme.gray)
        } else {
            (SCROLL_TRACK, theme.border)
        };
        if let Some(cell) = buffer.cell_mut((x, area.y + row)) {
            cell.set_symbol(glyph).set_fg(color);
        }
    }
}

/// The glyph the scrollbar's empty track is drawn with.
const SCROLL_TRACK: &str = "│";
/// The glyph the scrollbar's thumb is drawn with.
const SCROLL_THUMB: &str = "┃";

/// Where the scrollbar thumb starts and how long it is, as `(row, rows)`.
///
/// `None` when the whole thread fits, because a bar that fills its own track
/// says nothing. Pure arithmetic, so the geometry is testable on its own.
#[must_use]
fn thumb(total: u32, viewport: u16, above: u32) -> Option<(u16, u16)> {
    let height = u32::from(viewport);
    if height == 0 || total <= height {
        return None;
    }
    let length = (height * height / total).clamp(1, height);
    let travel = height - length;
    let scrolled = above.min(total - height);
    let start = travel * scrolled / (total - height);
    Some((
        u16::try_from(start).unwrap_or(viewport),
        u16::try_from(length).unwrap_or(viewport),
    ))
}

/// The day of the topmost message on screen, held on a row of its own between
/// the header and the messages, set like the separators below it.
///
/// The band always names the day, even when the topmost block's own separator
/// is on screen: a blank band read as a rendering gap rather than a choice.
pub fn render_day_band(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    let Some(first) = app
        .convo
        .visible(&app.measured.heights, app.panes.conversation.height.max(1))
        .first()
        .copied()
    else {
        return;
    };
    let Some(when) = app.message_rows.get(first.index).and_then(Message::sent_at) else {
        return;
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(day_label(Local::now(), when), Style::new().fg(theme.gray)),
        ])),
        area,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_at_finds_the_picture_under_a_cell_and_nothing_beside_it() {
        let hits = Hits {
            images: vec![
                ImageHit {
                    rect: Rect::new(10, 4, 8, 5),
                    message: 2,
                    attachment: 0,
                },
                ImageHit {
                    rect: Rect::new(10, 9, 8, 5),
                    message: 2,
                    attachment: 1,
                },
            ],
            ..Hits::default()
        };

        assert_eq!(hits.image_at(10, 4), Some((2, 0)));
        assert_eq!(hits.image_at(17, 8), Some((2, 0)));
        assert_eq!(hits.image_at(12, 9), Some((2, 1)));
        // One cell past each edge is not the picture.
        assert_eq!(hits.image_at(18, 6), None);
        assert_eq!(hits.image_at(12, 3), None);
        assert_eq!(hits.image_at(12, 14), None);
    }

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
    fn the_scrollbar_thumb_says_where_in_the_thread_the_view_is() {
        // Everything fits: no bar at all.
        assert_eq!(thumb(10, 20, 0), None);
        assert_eq!(thumb(20, 20, 0), None);

        // Four screens of thread: a quarter-height thumb that walks the track.
        let (start, length) = thumb(80, 20, 0).expect("a bar");
        assert_eq!((start, length), (0, 5));
        assert_eq!(thumb(80, 20, 60), Some((15, 5)), "at the newest message");
        assert_eq!(thumb(80, 20, 30), Some((7, 5)), "and halfway up");

        // A very long thread still gets a thumb you can see, and it never
        // walks off the end of the track.
        let (start, length) = thumb(100_000, 20, 99_999).expect("a bar");
        assert_eq!(length, 1);
        assert_eq!(start, 19);
    }

    /// A pane four rows of ten columns, the last column the scrollbar's.
    fn pane() -> (Buffer, Rect) {
        let area = Rect::new(0, 0, 10, 4);
        let buffer = Buffer::with_lines([
            "abcdefgh \u{2502}",
            "ijkl     \u{2502}",
            "mnopqrst \u{2502}",
            "uvwxyz   \u{2502}",
        ]);
        (buffer, area)
    }

    fn drag(from: (u16, u16), to: (u16, u16), area: Rect) -> Selection {
        let mut selection = Selection::new(Position::new(from.0, from.1), Scroll::default(), area);
        selection.cursor = Position::new(to.0, to.1);
        selection
    }

    #[test]
    fn a_span_reads_the_same_dragged_either_way() {
        let area = Rect::new(0, 0, 10, 4);
        let down = drag((2, 1), (5, 3), area);
        let up = drag((5, 3), (2, 1), area);
        assert_eq!(down.span(), up.span());
        let (start, end) = down.span();
        assert_eq!((start.x, start.y), (2, 1));
        assert_eq!((end.x, end.y), (5, 3));

        // Backwards along one row orders by column.
        let (start, end) = drag((7, 2), (3, 2), area).span();
        assert_eq!((start.x, end.x), (3, 7));
        assert_eq!(start.y, end.y);

        assert!(drag((4, 1), (4, 1), area).is_click());
        assert!(!drag((4, 1), (5, 1), area).is_click());
    }

    #[test]
    fn a_selection_is_linear_and_stops_short_of_the_scrollbar() {
        let area = Rect::new(0, 0, 10, 4);
        let selection = drag((4, 1), (2, 3), area);
        assert_eq!(selected_columns(&selection, area, 0), None, "above it");
        assert_eq!(selected_columns(&selection, area, 1), Some((4, 9)));
        assert_eq!(
            selected_columns(&selection, area, 2),
            Some((0, 9)),
            "a whole row, but never the scrollbar's column"
        );
        assert_eq!(selected_columns(&selection, area, 3), Some((0, 3)));
        assert_eq!(selected_columns(&selection, area, 4), None, "below it");

        // A drag that ends on the scrollbar still stops at the words.
        let selection = drag((0, 0), (9, 0), area);
        assert_eq!(selected_columns(&selection, area, 0), Some((0, 9)));
    }

    #[test]
    fn the_text_is_what_the_rows_showed_with_the_blanks_trimmed() {
        let (buffer, area) = pane();

        let selection = drag((2, 0), (3, 2), area);
        assert_eq!(
            selection_text(&buffer, area, &selection),
            "cdefgh\nijkl\nmnop"
        );

        // One row, one word.
        let selection = drag((0, 1), (3, 1), area);
        assert_eq!(selection_text(&buffer, area, &selection), "ijkl");

        // The whole pane: every row, and no scrollbar on any of them.
        let selection = drag((0, 0), (9, 3), area);
        assert_eq!(
            selection_text(&buffer, area, &selection),
            "abcdefgh\nijkl\nmnopqrst\nuvwxyz"
        );

        // A single cell is a click, and says so little that nothing copies it.
        let selection = drag((1, 0), (1, 0), area);
        assert!(selection.is_click());
        assert_eq!(selection_text(&buffer, area, &selection), "b");
    }
}
