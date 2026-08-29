//! The send box: a rounded hairline box with the `❯` prompt and the draft, a
//! column in from either edge of the pane.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{App, Focus};

/// Draw the composer, and place the terminal cursor in it when it has focus.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    // One rectangle for the draft, so a click lands on the character the
    // cursor was drawn under.
    let Some(inner) = text_area(area) else {
        return;
    };
    let theme = &app.theme;
    let focused = app.focus == Focus::Composer;
    let attaching = app.attach_prompt.as_ref();
    let field = attaching.unwrap_or(&app.composer);

    // A column of air either side, the way the mockup insets the box. The
    // border is gray, and a step brighter while the box has focus: that is
    // the whole focus signal.
    let boxed = Rect {
        x: area.x + 1,
        width: area.width - 2,
        ..area
    };
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_for(focused)))
        .style(Style::new().bg(theme.bg_base));
    // The path prompt names itself on the border, so the box is never
    // mistaken for the draft it is standing in front of.
    if attaching.is_some() {
        block = block.title_top(Line::from(Span::styled(
            " attach ",
            Style::new().fg(theme.text_secondary),
        )));
    }
    debug_assert_eq!(inner, block.inner(boxed));
    frame.render_widget(block, boxed);

    let text = field.text();
    let (cursor_row, cursor_column) = cursor_cell(text, field.cursor());
    // Keep the cursor line visible once the draft is taller than the box.
    let scroll = cursor_row.saturating_sub(usize::from(inner.height).saturating_sub(1));

    let prompt = Style::new().fg(if focused {
        theme.text_secondary
    } else {
        theme.gray
    });
    let mut lines: Vec<Line> = Vec::new();
    if text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(PROMPT, prompt),
            Span::styled(
                placeholder(app, attaching.is_some(), inner.width),
                Style::new().fg(theme.gray),
            ),
        ]));
    } else {
        for (index, raw) in text.split('\n').enumerate() {
            let marker = if index == 0 { PROMPT } else { "   " };
            lines.push(Line::from(vec![
                Span::styled(marker, prompt),
                Span::styled(raw.to_string(), Style::new().fg(theme.text_primary)),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        inner,
    );

    if focused {
        let x = inner
            .x
            .saturating_add(PROMPT_WIDTH)
            .saturating_add(u16::try_from(cursor_column).unwrap_or(u16::MAX))
            .min(inner.x + inner.width.saturating_sub(1));
        let y = inner
            .y
            .saturating_add(u16::try_from(cursor_row - scroll).unwrap_or(0))
            .min(inner.y + inner.height.saturating_sub(1));
        frame.set_cursor_position((x, y));
    }
}

/// The `❯ ` marker in front of the first line of the draft, a cell in from
/// the border. Continuation lines are set in past it, so one width serves
/// every row.
const PROMPT: &str = " ❯ ";
const PROMPT_WIDTH: u16 = 3;

/// The rows the draft itself is drawn on: a column of air either side of the
/// pane, then the border. `None` when the pane is too small to draw at all,
/// which is the same test [`render`] leaves on.
///
/// One function, so a click lands where the cursor was drawn.
#[must_use]
pub fn text_area(area: Rect) -> Option<Rect> {
    if area.width < 5 || area.height < 3 {
        return None;
    }
    Some(Rect::new(
        area.x + 2,
        area.y + 1,
        area.width - 4,
        area.height - 2,
    ))
}

/// Where a click at `position` puts the text cursor: a byte offset into
/// `text`, or `None` when the click missed the drawn draft.
///
/// `cursor` is where the cursor is now, because that is what decides how far
/// the box has scrolled. Past the end of what is written — a click in the
/// blank right of the last character, or on a row below the draft — the
/// answer is the end of the draft, where `End` goes.
#[must_use]
pub fn offset_at(text: &str, cursor: usize, area: Rect, position: Position) -> Option<usize> {
    let inner = text_area(area)?;
    if !inner.contains(position) {
        return None;
    }
    let (cursor_row, _) = cursor_cell(text, cursor);
    let scroll = cursor_row.saturating_sub(usize::from(inner.height).saturating_sub(1));
    let row = usize::from(position.y - inner.y) + scroll;

    let lines: Vec<&str> = text.split('\n').collect();
    let Some(line) = lines.get(row) else {
        return Some(text.len());
    };
    // Every line is set in past the prompt column, so a click left of it is
    // the start of that line.
    let start: usize = lines[..row].iter().map(|line| line.len() + 1).sum();
    let text_x = inner.x + PROMPT_WIDTH;
    if position.x < text_x {
        return Some(start);
    }
    let column = usize::from(position.x - text_x);
    match line.char_indices().nth(column) {
        Some((index, _)) => Some(start + index),
        None => Some(text.len()),
    }
}

/// The dim line shown in an empty box: `Message Priya`, or what the path
/// prompt is waiting for.
fn placeholder(app: &App, attaching: bool, width: u16) -> String {
    if attaching {
        return "Path to a file — ~ works".to_string();
    }
    let Some(chat) = app.current_chat() else {
        return "Message".to_string();
    };
    // The name is content, so it is truncated to the box rather than allowed
    // to push the layout around.
    let room = usize::from(width.saturating_sub(PROMPT_WIDTH + 10)).max(8);
    format!("Message {}", super::format::truncate(&chat.title(), room))
}

/// Translate a byte offset into `(line, column)` in characters.
fn cursor_cell(text: &str, cursor: usize) -> (usize, usize) {
    let head = &text[..cursor.min(text.len())];
    let row = head.matches('\n').count();
    let column = head.rsplit('\n').next().unwrap_or("").chars().count();
    (row, column)
}

/// `key label   key label` with the keys picked out, fitted to `columns`.
/// `sep` goes between pairs and is always three cells wide, which is what
/// the fitting counts on.
pub(crate) fn hint_line<'a>(
    app: &App,
    hints: &[(&'a str, &'a str)],
    sep: &'static str,
    columns: u16,
) -> Line<'a> {
    let theme = &app.theme;
    let fitted = super::format::fit_hints(hints, usize::from(columns));
    let mut spans = vec![Span::raw(" ")];
    for (index, (keys, label)) in fitted.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(sep, Style::new().fg(theme.gray_dim)));
        }
        spans.push(Span::styled(
            keys.into_owned(),
            Style::new()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(label) = label.filter(|label| !label.is_empty()) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(label, Style::new().fg(theme.gray)));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::{PROMPT_WIDTH, cursor_cell, offset_at, text_area};
    use ratatui::layout::{Position, Rect};

    #[test]
    fn cursor_cell_counts_lines_and_characters() {
        assert_eq!(cursor_cell("", 0), (0, 0));
        assert_eq!(cursor_cell("hello", 5), (0, 5));
        assert_eq!(cursor_cell("one\ntwo", 7), (1, 3));
        assert_eq!(cursor_cell("a\nb\nc", 2), (1, 0));
    }

    #[test]
    fn cursor_cell_counts_characters_not_bytes() {
        let text = "héllo";
        assert_eq!(cursor_cell(text, text.len()), (0, 5));
    }

    #[test]
    fn a_click_lands_on_the_character_under_it() {
        let pane = Rect::new(30, 20, 50, 3);
        let inner = text_area(pane).expect("room for the box");
        assert_eq!(inner, Rect::new(32, 21, 46, 1));
        let text_x = inner.x + PROMPT_WIDTH;

        let text = "one two";
        let at = |x| offset_at(text, 0, pane, Position::new(x, inner.y));
        assert_eq!(at(text_x), Some(0));
        assert_eq!(at(text_x + 4), Some(4));
        // Left of the prompt is the start of the line; past the last
        // character is the end of the draft.
        assert_eq!(at(inner.x), Some(0));
        assert_eq!(at(text_x + 7), Some(text.len()));
        assert_eq!(at(text_x + 30), Some(text.len()));
        // Outside the box entirely.
        assert_eq!(offset_at(text, 0, pane, Position::new(31, inner.y)), None);
        assert_eq!(offset_at(text, 0, pane, Position::new(text_x, 20)), None);
    }

    #[test]
    fn a_click_on_a_wrapped_draft_counts_the_lines_above_it() {
        let pane = Rect::new(0, 0, 50, 4);
        let inner = text_area(pane).expect("room for the box");
        assert_eq!(inner.height, 2);
        let text = "one\ntwo";
        let cursor = text.len();
        let at = |x, y| offset_at(text, cursor, pane, Position::new(x, y));
        let text_x = inner.x + PROMPT_WIDTH;
        assert_eq!(at(text_x + 1, inner.y), Some(1));
        assert_eq!(at(text_x + 1, inner.y + 1), Some(5));
        // A row with nothing written on it is the end of the draft.
        let short = "one";
        assert_eq!(
            offset_at(short, 0, pane, Position::new(text_x, inner.y + 1)),
            Some(short.len())
        );
    }

    #[test]
    fn a_pane_too_small_for_the_box_has_no_text_area() {
        assert_eq!(text_area(Rect::new(0, 0, 4, 3)), None);
        assert_eq!(text_area(Rect::new(0, 0, 50, 2)), None);
    }

    #[test]
    fn a_click_on_a_scrolled_draft_follows_the_cursor_line() {
        // Four lines in a two-row box: the box is showing the last two.
        let pane = Rect::new(0, 0, 50, 4);
        let text = "one\ntwo\nthree\nfour";
        let inner = text_area(pane).expect("room for the box");
        let text_x = inner.x + PROMPT_WIDTH;
        assert_eq!(
            offset_at(text, text.len(), pane, Position::new(text_x, inner.y)),
            Some(8),
            "the top row of the box is the third line"
        );
    }
}
