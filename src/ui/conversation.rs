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
use crate::db::Chat;
use crate::ui::format::truncate;

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
                format!(" {}", truncate(&chat.fallback_title(), title_room(area))),
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
        let count = chat.message_count;
        let counts = format!("{count} msg{} ", if count == 1 { "" } else { "s" });
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                counts,
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

/// `· iMessage · 6 people`, as much of it as is true.
fn subtitle(chat: &Chat) -> String {
    let mut parts = Vec::new();
    if let Some(service) = chat.service.as_deref().filter(|s| !s.is_empty()) {
        parts.push(service.to_string());
    }
    if chat.is_group {
        let people = chat.participants.len();
        parts.push(format!("{people} people"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" · {}", parts.join(" · "))
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
