//! The bordered send box and the hint row beneath it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{App, Focus};
use crate::keymap;

/// Draw the composer, and place the terminal cursor in it when it has focus.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let theme = &app.theme;
    let focused = app.focus == Focus::Composer;

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_for(focused)))
        .style(Style::new().bg(theme.bg_base));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = app.composer.text();
    let (cursor_row, cursor_column) = cursor_cell(text, app.composer.cursor());
    // Keep the cursor line visible once the draft is taller than the box.
    let scroll = cursor_row.saturating_sub(usize::from(inner.height).saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    if text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(PROMPT, Style::new().fg(theme.accent_me)),
            Span::styled(
                "message…",
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            ),
        ]));
    } else {
        for (index, raw) in text.split('\n').enumerate() {
            let marker = if index == 0 { PROMPT } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::new().fg(theme.accent_me)),
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

/// The `› ` marker in front of the first line of the draft.
const PROMPT: &str = "› ";
const PROMPT_WIDTH: u16 = 2;

/// Translate a byte offset into `(line, column)` in characters.
fn cursor_cell(text: &str, cursor: usize) -> (usize, usize) {
    let head = &text[..cursor.min(text.len())];
    let row = head.matches('\n').count();
    let column = head.rsplit('\n').next().unwrap_or("").chars().count();
    (row, column)
}

/// The context hints from the mockup, one line under the composer.
pub fn render_info_row(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let hints: &[(&str, &str)] = match app.focus {
        Focus::Composer => &[
            ("Enter", "send"),
            ("Alt+Enter", "newline"),
            ("Ctrl+A", "attach"),
            ("Esc", "back"),
        ],
        Focus::ChatList => &[
            ("Enter", "open"),
            ("/", "filter"),
            ("Ctrl+B", "hide list"),
            ("Ctrl+K", "jump"),
        ],
        _ => &[
            ("o", "open"),
            ("s", "save"),
            ("y", "copy"),
            ("Ctrl+R", "react"),
        ],
    };
    frame.render_widget(Paragraph::new(hint_line(app, hints)), area);
}

/// `key label · key label` with the keys picked out.
pub(crate) fn hint_line<'a>(app: &App, hints: &[(&'a str, &'a str)]) -> Line<'a> {
    let theme = &app.theme;
    let mut spans = vec![Span::raw(" ")];
    for (index, (keys, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(theme.gray_dim)));
        }
        spans.push(Span::styled(
            *keys,
            Style::new()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*label, Style::new().fg(theme.gray)));
    }
    Line::from(spans)
}

/// The condensed shortcuts bar along the bottom of the screen.
pub fn shortcut_bar_line(app: &App) -> Line<'static> {
    hint_line(app, keymap::SHORTCUT_BAR)
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
