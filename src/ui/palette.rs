//! The `Ctrl+K` jump palette: a floating input over a dimmed screen, with
//! chats, people, and full-text message hits under it.
//!
//! The matching happens in [`crate::jump`]; this file only draws what it
//! produced. Every row arrives carrying the character ranges that matched, so
//! the highlight is exactly what the matcher saw.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use crate::app::App;
use crate::jump::{Kind, Row};
use crate::theme::Theme;
use crate::ui::format::{truncate, width};

/// Width of the palette in columns, before clamping to the screen.
const WIDTH: u16 = 72;
/// Most result rows shown at once.
const MAX_VISIBLE: u16 = 8;
/// Columns the chat name takes on a message row, so the bodies line up.
const NAME_COLUMN: usize = 12;
/// The input row, the rule under it, and the two borders.
const CHROME: u16 = 4;

/// Columns of the palette body that result rows can use, for the caller that
/// has to decide how much of a matched line to keep.
#[must_use]
pub const fn body_columns(area: Rect) -> usize {
    let width = if area.width < WIDTH {
        area.width
    } else {
        WIDTH
    };
    // Two borders, two columns of padding, the marker, and the name column.
    (width as usize).saturating_sub(6 + NAME_COLUMN + 12)
}

/// Draw the palette near the top of `area`.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let width = WIDTH.min(area.width);
    let rows = u16::try_from(app.jump.rows.len()).unwrap_or(MAX_VISIBLE);
    let visible = rows.clamp(1, MAX_VISIBLE);
    let height = (visible + CHROME).min(area.height);
    if width < 20 || height < 4 {
        return;
    }
    // Centered horizontally, a little below the top edge like the mockup.
    let y = area.y + (area.height.saturating_sub(height)).min(3);
    let modal = Rect::new(area.x + (area.width - width) / 2, y, width, height);

    // The screen behind the palette is dimmed, so the eye goes to the input
    // rather than to the conversation it is floating over.
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    frame.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_active))
        .style(Style::new().bg(theme.bg_light).fg(theme.text_primary))
        .padding(Padding::horizontal(1))
        .title_bottom(Span::styled(footer(app), Style::new().fg(theme.gray)));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let query = app.palette.text();
    frame.render_widget(
        Paragraph::new(input_line(theme, query)),
        Rect { height: 1, ..inner },
    );

    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(usize::from(inner.width)),
                Style::new().fg(theme.border),
            ))),
            Rect {
                y: inner.y + 1,
                height: 1,
                ..inner
            },
        );
    }
    if inner.height < 3 {
        return;
    }

    let body = Rect {
        y: inner.y + 2,
        height: inner.height - 2,
        ..inner
    };
    render_rows(frame, app, body);

    // The cursor sits after the prompt glyph and whatever has been typed.
    let column = u16::try_from(width_of(query)).unwrap_or(u16::MAX);
    let x = (inner.x + 2 + column).min(inner.x + inner.width.saturating_sub(1));
    frame.set_cursor_position((x, inner.y));
}

/// `› thai`, or the placeholder when nothing has been typed.
fn input_line(theme: &Theme, query: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("❯ ", Style::new().fg(theme.text_secondary)),
        if query.is_empty() {
            Span::styled(
                "jump to a chat, or search messages…",
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::styled(query.to_string(), Style::new().fg(theme.text_primary))
        },
    ])
}

/// The result rows, or the one line that says why there are none.
fn render_rows(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    if app.jump.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                empty_note(app),
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            ))),
            area,
        );
        return;
    }

    let height = usize::from(area.height);
    let selected = app.jump.list.selected;
    // The window follows the selection without any stored scroll offset: the
    // list is short and rebuilt on every keystroke, so there is nothing worth
    // remembering between frames.
    let offset = selected.saturating_sub(height.saturating_sub(1));

    for (index, row) in app.jump.rows.iter().enumerate().skip(offset).take(height) {
        let y = area.y + u16::try_from(index - offset).unwrap_or(0);
        let line = Rect {
            y,
            height: 1,
            ..area
        };
        if index == selected {
            frame.render_widget(
                Block::new().style(Style::new().bg(theme.bg_highlight)),
                line,
            );
        }
        frame.render_widget(
            Paragraph::new(row_line(theme, row, index == selected, area.width)),
            line,
        );
        let meta = truncate(&row.meta, usize::from(area.width));
        if !meta.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(meta, Style::new().fg(theme.gray))))
                    .alignment(Alignment::Right),
                line,
            );
        }
    }
}

/// One result row, without its right-aligned date.
#[must_use]
pub fn row_line(theme: &Theme, row: &Row, selected: bool, columns: u16) -> Line<'static> {
    let room = usize::from(columns).saturating_sub(width_of(&row.meta) + 1);
    let mut spans = vec![Span::styled(
        if selected { "▸ " } else { "  " },
        Style::new().fg(theme.text_secondary),
    )];

    let name = Style::new()
        .fg(theme.text_primary)
        .add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(theme.text_secondary);
    let hit = Style::new().fg(theme.fuzzy).add_modifier(Modifier::BOLD);

    if row.kind == Kind::Chat {
        spans.extend(highlight(&row.label, &row.label_hits, name, hit));
    } else {
        // The chat name gets a fixed column so the matched lines line up under
        // each other, the way the mockup has them.
        let label = truncate(&row.label, NAME_COLUMN - 1);
        let pad = NAME_COLUMN.saturating_sub(width_of(&label));
        spans.push(Span::styled(label, name));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.extend(highlight(&row.body, &row.body_hits, dim, hit));
    }

    Line::from(fit(spans, room))
}

/// `text` as spans, with `ranges` of characters in `accent` and the rest in
/// `base`.
#[must_use]
pub fn highlight(
    text: &str,
    ranges: &[(usize, usize)],
    base: Style,
    accent: Style,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut at = 0usize;
    for (start, end) in ranges {
        let (start, end) = ((*start).min(chars.len()), (*end).min(chars.len()));
        if start < at {
            continue;
        }
        if start > at {
            spans.push(Span::styled(
                chars[at..start].iter().collect::<String>(),
                base,
            ));
        }
        if end > start {
            spans.push(Span::styled(
                chars[start..end].iter().collect::<String>(),
                accent,
            ));
        }
        at = end;
    }
    if at < chars.len() {
        spans.push(Span::styled(chars[at..].iter().collect::<String>(), base));
    }
    spans
}

/// Cut `spans` to `columns` and pad them out to it, so the right-aligned date
/// never lands on top of the text.
fn fit(spans: Vec<Span<'static>>, columns: usize) -> Vec<Span<'static>> {
    let mut used = 0usize;
    let mut out = Vec::with_capacity(spans.len() + 1);
    for span in spans {
        if used >= columns {
            break;
        }
        let span_width = width_of(&span.content);
        if used + span_width <= columns {
            used += span_width;
            out.push(span);
        } else {
            let cut = truncate(&span.content, columns - used);
            used += width_of(&cut);
            out.push(Span::styled(cut, span.style));
            break;
        }
    }
    if used < columns {
        out.push(Span::raw(" ".repeat(columns - used)));
    }
    out
}

/// `1 chat · 4 messages · Enter jump · Tab filter: all`.
#[must_use]
pub fn footer(app: &App) -> String {
    let mut parts = Vec::with_capacity(5);
    if app.jump.chats > 0 {
        parts.push(plural(app.jump.chats, "chat"));
    }
    if app.jump.messages > 0 {
        let noun = if app.jump.filter == crate::jump::Filter::Photos {
            "photo"
        } else {
            "message"
        };
        parts.push(plural(app.jump.messages, noun));
    }
    parts.push("Enter jump".to_string());
    if crate::jump::looks_like_address(app.palette.text()).is_some() {
        parts.push("Ctrl+N new message".to_string());
    }
    parts.push(format!("Tab filter: {}", app.jump.filter.label()));
    format!(" {} ", parts.join(" · "))
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Why the list is empty, which is different before and after the index exists.
fn empty_note(app: &App) -> String {
    let typed = app.palette.text().trim().chars().count();
    if typed == 0 {
        return "start typing".to_string();
    }
    if typed < crate::search::MIN_QUERY && app.jump.filter.wants_messages() {
        return format!(
            "no chats · {} letters searches messages",
            crate::search::MIN_QUERY
        );
    }
    match app.search.as_ref().map(crate::search::Search::state) {
        Some(state) if !state.is_ready() => {
            state.note().unwrap_or_else(|| "no results".to_string())
        }
        Some(_) => "no results".to_string(),
        None => "no results — message search is off".to_string(),
    }
}

fn width_of(text: &str) -> usize {
    width(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jump::Kind;
    use crate::theme::Theme;

    fn row(kind: Kind) -> Row {
        Row {
            kind,
            chat_rowid: 1,
            message_rowid: Some(2),
            label: "Fixture".to_string(),
            label_hits: vec![(0, 3)],
            body: "a matched line".to_string(),
            body_hits: vec![(2, 9)],
            meta: "Tue".to_string(),
        }
    }

    #[test]
    fn highlighting_splits_the_text_at_the_matched_runs() {
        let base = Style::new();
        let accent = Style::new().add_modifier(Modifier::BOLD);
        let spans = highlight("thailand", &[(0, 4)], base, accent);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "thai");
        assert_eq!(spans[1].content, "land");
        assert_eq!(spans[0].style, accent);

        // No ranges is one plain span, and a range past the end is ignored.
        assert_eq!(highlight("abc", &[], base, accent).len(), 1);
        let spans = highlight("abc", &[(1, 99)], base, accent);
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "abc"
        );
    }

    #[test]
    fn a_row_is_padded_to_leave_room_for_the_date() {
        let theme = Theme::default();
        for kind in [Kind::Chat, Kind::Message, Kind::Photo] {
            let line = row_line(&theme, &row(kind), true, 60);
            let drawn: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(width(&drawn), 60 - 4, "{kind:?}: {drawn:?}");
        }
    }

    #[test]
    fn a_narrow_palette_still_produces_a_row() {
        let theme = Theme::default();
        let line = row_line(&theme, &row(Kind::Message), false, 12);
        let drawn: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(width(&drawn) <= 12, "{drawn:?}");
    }
}
