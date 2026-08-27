//! The left pane: filter line on top, chat rows below, divider on the right.
//!
//! Each chat takes two rows — an unread dot, the name, the time and the unread
//! badge on the first; the preview of the last message on the second — and an
//! optional one-row heading opens each section. [`Shape`] is the pure geometry
//! behind that: it decides which chat is drawn where, which row a click lands
//! on, and how far to scroll to keep the selection visible, without a terminal
//! or a database in sight.

use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Focus, TextField};
use crate::db::Chat;
use crate::theme::Theme;
use crate::ui::format::{preview_line, relative_time, truncate, unread_badge, width};

/// Rows one chat occupies: the name line and the preview line.
pub const CHAT_ROWS: u16 = 2;

/// Columns before the name: the selection bar, the unread dot, and a space.
const GUTTER: usize = 3;

/// One column of air is kept on the right so nothing touches the divider.
const RIGHT_MARGIN: usize = 1;

/// The narrowest a name may be squeezed before the time is dropped instead.
const MIN_NAME: usize = 6;

/// One drawable entry in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A section heading, one row tall.
    Section(&'static str),
    /// A chat, [`CHAT_ROWS`] tall, by its index among the visible chats.
    Chat(usize),
}

/// The geometry of the rows area: what is in the list and where it is scrolled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shape {
    /// How many chats the filter leaves visible.
    pub total: usize,
    /// How many of the leading chats are pinned. `0` means the database does
    /// not record pinning, and the list is drawn without section headings.
    pub pinned: usize,
    /// Index of the first chat drawn.
    pub offset: usize,
    /// Rows available below the filter line.
    pub height: u16,
}

impl Shape {
    /// The list as it stands right now, for a rows area `height` tall.
    #[must_use]
    pub fn of(app: &App, height: u16) -> Self {
        Self {
            total: app.visible_chats.len(),
            pinned: app.pinned_visible,
            offset: app.chats.offset,
            height,
        }
    }

    /// Every entry that fits, as `(row within the area, entry)`.
    ///
    /// A chat that would be cut in half by the bottom edge is left out, so the
    /// list never shows a name without its preview.
    #[must_use]
    pub fn plan(&self) -> Vec<(u16, Entry)> {
        let mut rows = Vec::new();
        let mut y = 0u16;
        for index in self.offset..self.total {
            if let Some(section) = self.section_at(index) {
                if y >= self.height {
                    break;
                }
                rows.push((y, Entry::Section(section)));
                y += 1;
            }
            if y + CHAT_ROWS > self.height {
                break;
            }
            rows.push((y, Entry::Chat(index)));
            y += CHAT_ROWS;
        }
        rows
    }

    /// The heading that opens the section `index` belongs to, if it opens one.
    const fn section_at(&self, index: usize) -> Option<&'static str> {
        if self.pinned == 0 {
            return None;
        }
        if index == 0 {
            return Some("PINNED");
        }
        if index == self.pinned {
            return Some("RECENT");
        }
        None
    }

    /// The chat drawn at `row`, counted from the top of the rows area.
    #[must_use]
    pub fn chat_at(&self, row: u16) -> Option<usize> {
        self.plan().into_iter().find_map(|(y, entry)| match entry {
            Entry::Chat(index) if row >= y && row < y + CHAT_ROWS => Some(index),
            _ => None,
        })
    }

    /// The offset that brings `selected` into view, moving as little as it can.
    ///
    /// Scrolling with the wheel does not go through here — only a moved
    /// selection does — so the two can disagree until the next keypress, which
    /// is what makes a wheel scroll feel like a wheel scroll.
    #[must_use]
    pub fn offset_for(&self, selected: usize) -> usize {
        if self.height == 0 || self.total == 0 {
            return self.offset.min(selected);
        }
        let mut shape = Self {
            offset: self.offset.min(selected),
            ..*self
        };
        while shape.offset < selected && !shape.shows(selected) {
            shape.offset += 1;
        }
        shape.offset
    }

    fn shows(&self, index: usize) -> bool {
        self.plan()
            .iter()
            .any(|(_, entry)| *entry == Entry::Chat(index))
    }
}

/// Draw the pane. `area` is the whole pane, `rows` the selectable rows below
/// the filter line and left of the divider.
pub fn render(frame: &mut Frame, app: &App, area: Rect, rows: Rect) {
    let theme = &app.theme;
    let focused = app.focus == Focus::ChatList;

    frame.render_widget(
        Block::new()
            .borders(Borders::RIGHT)
            .border_style(Style::new().fg(theme.border_for(focused)))
            .style(Style::new().bg(theme.bg_dark)),
        area,
    );

    render_filter_line(frame, app, Rect { height: 1, ..area });

    if app.visible_chats.is_empty() {
        let message = match app.chat_filter.as_ref().map(TextField::text) {
            Some(query) if !query.is_empty() => "no chats match",
            _ if app.chat_rows.is_empty() => "no chats loaded",
            _ => "no chats match",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {message}"),
                Style::new().fg(theme.gray),
            ))),
            Rect { height: 1, ..rows },
        );
        return;
    }

    let now = Local::now();
    let shape = Shape::of(app, rows.height);
    for (y, entry) in shape.plan() {
        let top = rows.y + y;
        match entry {
            Entry::Section(title) => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!(" {title}"),
                        Style::new().fg(theme.gray).add_modifier(Modifier::BOLD),
                    ))),
                    Rect {
                        y: top,
                        height: 1,
                        ..rows
                    },
                );
            }
            Entry::Chat(index) => {
                let Some(chat) = app.visible_chat(index) else {
                    continue;
                };
                let area = Rect {
                    y: top,
                    height: CHAT_ROWS,
                    ..rows
                };
                render_chat(frame, app, chat, index, area, now);
            }
        }
    }
}

/// One chat: the name line, then the preview line, over a background that says
/// whether it is selected or merely under the pointer.
fn render_chat(
    frame: &mut Frame,
    app: &App,
    chat: &Chat,
    index: usize,
    area: Rect,
    now: DateTime<Local>,
) {
    let theme = &app.theme;
    let selected = app.chats.selected == index;
    let hovered = !selected && app.hover.is_some_and(|point| area.contains(point));

    if selected || hovered {
        let background = if selected {
            theme.bg_highlight
        } else {
            theme.bg_hover
        };
        frame.render_widget(Block::new().style(Style::new().bg(background)), area);
    }

    let columns = usize::from(area.width);
    frame.render_widget(
        Paragraph::new(name_line(theme, chat, selected, columns, now)),
        Rect { height: 1, ..area },
    );
    if area.height >= CHAT_ROWS {
        frame.render_widget(
            Paragraph::new(preview_row(theme, chat, selected, columns)),
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
        );
    }
}

/// `▌● Name              2m  3`, with whatever fits.
fn name_line<'a>(
    theme: &Theme,
    chat: &Chat,
    selected: bool,
    columns: usize,
    now: DateTime<Local>,
) -> Line<'a> {
    let unread = chat.is_unread();
    let time = chat
        .last_message_at()
        .map(|when| relative_time(now, when))
        .unwrap_or_default();
    let badge = unread_badge(chat.unread_count);

    // The name gives up columns to the time and the badge, but never all of
    // them: below a floor the badge goes first, then the time.
    let mut name_width = columns.saturating_sub(GUTTER + RIGHT_MARGIN);
    let badge_width = width(&badge) + 3;
    let show_badge = !badge.is_empty() && name_width >= badge_width + MIN_NAME;
    if show_badge {
        name_width -= badge_width;
    }
    let time_width = width(&time) + 1;
    let show_time = !time.is_empty() && name_width >= time_width + MIN_NAME;
    if show_time {
        name_width -= time_width;
    }

    let name = truncate(&chat.title(), name_width);
    let padding = name_width.saturating_sub(width(&name));

    let mut spans = vec![
        marker(theme, selected),
        Span::styled(
            if unread { "●" } else { " " },
            Style::new().fg(theme.accent_me),
        ),
        Span::raw(" "),
        Span::styled(name, name_style(theme, unread)),
        Span::raw(" ".repeat(padding)),
    ];
    if show_time {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(time, Style::new().fg(theme.gray)));
    }
    if show_badge {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {badge} "),
            Style::new()
                .bg(theme.accent_me)
                .fg(theme.bg_base)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// `▌  You: pushed the fix, check CI`.
fn preview_row<'a>(theme: &Theme, chat: &Chat, selected: bool, columns: usize) -> Line<'a> {
    let unread = chat.is_unread();
    let (prefix, body) = preview_line(chat);
    let room = columns.saturating_sub(GUTTER + RIGHT_MARGIN);
    let prefix = truncate(&prefix, room);
    let body = truncate(&body, room.saturating_sub(width(&prefix)));

    let body_color = if unread {
        theme.text_secondary
    } else {
        theme.gray
    };
    Line::from(vec![
        marker(theme, selected),
        Span::raw("  "),
        Span::styled(prefix, Style::new().fg(theme.gray_dim)),
        Span::styled(body, Style::new().fg(body_color)),
    ])
}

/// The left bar that stands in for the mockup's selection outline.
fn marker<'a>(theme: &Theme, selected: bool) -> Span<'a> {
    if selected {
        Span::styled("▌", Style::new().fg(theme.border_active))
    } else {
        Span::raw(" ")
    }
}

fn name_style(theme: &Theme, unread: bool) -> Style {
    let style = Style::new().fg(theme.text_primary);
    if unread {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// The `/ search chats…` line, which becomes an input while filtering.
fn render_filter_line(frame: &mut Frame, app: &App, area: Rect) {
    if area.width < 3 {
        return;
    }
    let theme = &app.theme;
    let line = match app.chat_filter.as_ref() {
        Some(field) => Line::from(vec![
            Span::styled(" / ", Style::new().fg(theme.accent_me)),
            Span::styled(
                field.text().to_string(),
                Style::new().fg(theme.text_primary),
            ),
            Span::styled("▏", Style::new().fg(theme.accent_me)),
        ]),
        None => Line::from(Span::styled(
            " / search chats…",
            Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(total: usize, pinned: usize, offset: usize, height: u16) -> Shape {
        Shape {
            total,
            pinned,
            offset,
            height,
        }
    }

    #[test]
    fn a_list_without_pinning_is_two_rows_per_chat() {
        let plan = shape(5, 0, 0, 6).plan();
        assert_eq!(
            plan,
            vec![
                (0, Entry::Chat(0)),
                (2, Entry::Chat(1)),
                (4, Entry::Chat(2))
            ]
        );
    }

    #[test]
    fn pinned_chats_get_headings_and_the_rest_get_one_too() {
        let plan = shape(4, 1, 0, 20).plan();
        assert_eq!(
            plan,
            vec![
                (0, Entry::Section("PINNED")),
                (1, Entry::Chat(0)),
                (3, Entry::Section("RECENT")),
                (4, Entry::Chat(1)),
                (6, Entry::Chat(2)),
                (8, Entry::Chat(3)),
            ]
        );
    }

    #[test]
    fn a_chat_that_would_be_cut_in_half_is_left_out() {
        let plan = shape(5, 0, 0, 5).plan();
        assert_eq!(plan.len(), 2, "only whole chats are drawn");
        assert!(shape(5, 0, 0, 1).plan().is_empty());
        assert!(shape(5, 0, 0, 0).plan().is_empty());
    }

    #[test]
    fn a_click_finds_the_chat_on_either_of_its_two_rows() {
        let shape = shape(5, 0, 1, 6);
        assert_eq!(shape.chat_at(0), Some(1));
        assert_eq!(shape.chat_at(1), Some(1));
        assert_eq!(shape.chat_at(2), Some(2));
        assert_eq!(shape.chat_at(5), Some(3));
        assert_eq!(shape.chat_at(6), None);
    }

    #[test]
    fn a_click_on_a_section_heading_selects_nothing() {
        let shape = shape(4, 1, 0, 20);
        assert_eq!(shape.chat_at(0), None);
        assert_eq!(shape.chat_at(1), Some(0));
        assert_eq!(shape.chat_at(3), None, "the RECENT heading");
        assert_eq!(shape.chat_at(4), Some(1));
    }

    #[test]
    fn scrolling_follows_the_selection_both_ways() {
        // Six rows hold three chats.
        let down = shape(20, 0, 0, 6);
        assert_eq!(down.offset_for(2), 0, "already visible");
        assert_eq!(down.offset_for(3), 1, "one chat further down");
        assert_eq!(down.offset_for(19), 17, "the very bottom");

        let up = shape(20, 0, 10, 6);
        assert_eq!(up.offset_for(4), 4, "jumping above the window");
        assert_eq!(up.offset_for(11), 10, "still visible");
    }

    #[test]
    fn scrolling_accounts_for_the_row_a_heading_takes() {
        let shape = shape(10, 2, 0, 6);
        // PINNED, two pinned chats, RECENT, then the first recent chat is cut.
        assert_eq!(shape.plan().len(), 4);
        assert_eq!(shape.offset_for(2), 1);
    }

    #[test]
    fn an_empty_list_scrolls_nowhere() {
        assert_eq!(shape(0, 0, 0, 10).offset_for(0), 0);
        assert_eq!(shape(3, 0, 0, 0).offset_for(2), 0);
    }
}
