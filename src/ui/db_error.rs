//! The full-screen surface shown when `chat.db` cannot be read.
//!
//! This is the first thing most people see if their terminal has not been given
//! Full Disk Access, so it says what happened, which file it happened to, and
//! exactly where in System Settings to fix it. It never shows anything that
//! came out of the database, because nothing came out of the database.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::db::DbError;

/// Widest the panel grows, in columns.
const MAX_WIDTH: u16 = 72;

/// Draw the explanation centered in `area`.
pub fn render(frame: &mut Frame, app: &App, area: Rect, err: &DbError) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let theme = &app.theme;

    let mut lines = vec![
        Line::from(Span::styled(
            err.headline(),
            Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if let Some(path) = err.path() {
        lines.push(Line::from(Span::styled(
            super::home_relative(path),
            Style::new().fg(theme.text_secondary),
        )));
    }
    lines.push(Line::from(Span::styled(
        err.detail(),
        Style::new().fg(theme.gray),
    )));

    if let Some(hint) = err.hint() {
        lines.push(Line::from(""));
        for line in hint.lines() {
            lines.push(Line::from(Span::styled(
                line.trim(),
                Style::new().fg(theme.text_primary),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("?", Style::new().fg(theme.accent_me)),
        Span::styled(" help    ", Style::new().fg(theme.gray)),
        Span::styled("q", Style::new().fg(theme.accent_me)),
        Span::styled(" quit", Style::new().fg(theme.gray)),
    ]));

    let width = MAX_WIDTH.min(area.width);
    // Two rows of border plus the text, or as much of it as fits.
    let height = (u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2))
    .min(area.height);
    let panel = super::centered(area, width, height);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.error))
        .style(Style::new().bg(theme.bg_light));

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        panel,
    );
}
