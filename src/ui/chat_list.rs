//! The left pane: filter line on top, chat rows below, divider on the right.
//!
//! Row rendering proper (names, previews, unread badges) arrives with the chat
//! list pass; for now the pane draws its chrome and an empty state.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Focus, TextField};

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

    if app.chats.len == 0 {
        let message = match app.chat_filter.as_ref().map(TextField::text) {
            Some(query) if !query.is_empty() => "no chats match",
            _ => "no chats loaded",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {message}"),
                Style::new().fg(theme.gray),
            ))),
            Rect { height: 1, ..rows },
        );
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
