//! The right pane: a header with the chat's identity, then message blocks.
//!
//! Message blocks arrive with the conversation pass; this module owns the
//! header, the rule under it, and the empty state.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

/// Chat name on the left, counts on the right, with a rule underneath when
/// there is a row to spare for it.
pub fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    let title = Rect { height: 1, ..area };

    let left = Line::from(vec![
        Span::styled(
            " no conversation",
            Style::new()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · pick a chat on the left", Style::new().fg(theme.gray)),
    ]);
    frame.render_widget(Paragraph::new(left), title);

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

/// The scrolling message area.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    if app.messages.len > 0 {
        return;
    }

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
}
