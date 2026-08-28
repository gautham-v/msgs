//! The send box: a rounded hairline box with the `❯` prompt and the draft, a
//! column in from either edge of the pane.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{App, Focus};

/// Draw the composer, and place the terminal cursor in it when it has focus.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width < 5 || area.height < 3 {
        return;
    }
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
    let inner = block.inner(boxed);
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
/// the border.
const PROMPT: &str = " ❯ ";
const PROMPT_WIDTH: u16 = 3;

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
    use super::cursor_cell;

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
}
