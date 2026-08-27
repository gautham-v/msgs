//! The `Ctrl+K` jump palette.
//!
//! The frame is here — a floating input over a dimmed screen — so focus and
//! keys behave correctly from the start. Fuzzy chat matching and full-text
//! message search fill in the result rows later.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use crate::app::App;

/// Width of the palette in columns, before clamping to the screen.
const WIDTH: u16 = 62;
/// Input row, a result area, and the footer.
const HEIGHT: u16 = 9;

/// Draw the palette near the top of `area`.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let width = WIDTH.min(area.width);
    let height = HEIGHT.min(area.height);
    if width < 12 || height < 4 {
        return;
    }
    // Centered horizontally, a little below the top edge like the mockup.
    let y = area.y + (area.height.saturating_sub(height)).min(3);
    let modal = Rect::new(area.x + (area.width - width) / 2, y, width, height);

    frame.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_active))
        .style(Style::new().bg(theme.bg_light).fg(theme.text_primary))
        .padding(Padding::horizontal(1))
        .title_bottom(Span::styled(
            " Enter jump · Tab filter · Esc close ",
            Style::new().fg(theme.gray),
        ));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let query = app.palette.text();
    let input = Line::from(vec![
        Span::styled("› ", Style::new().fg(theme.accent_me)),
        if query.is_empty() {
            Span::styled(
                "jump to a chat, or search messages…",
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::styled(query.to_string(), Style::new().fg(theme.text_primary))
        },
    ]);
    frame.render_widget(Paragraph::new(input), Rect { height: 1, ..inner });

    if inner.height > 2 {
        let body = Rect {
            y: inner.y + 2,
            height: inner.height - 2,
            ..inner
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no results",
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            ))),
            body,
        );
    }

    if inner.width > 2 {
        let column = u16::try_from(query.chars().count()).unwrap_or(u16::MAX);
        let x = (inner.x + 2 + column).min(inner.x + inner.width - 1);
        frame.set_cursor_position((x, inner.y));
    }
}
