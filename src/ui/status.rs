//! The status line and the shortcuts bar under it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, DbStatus, WatcherStatus};

/// The rule between the panes and the status line.
pub fn render_rule(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(usize::from(area.width)),
            Style::new().fg(app.theme.border),
        ))),
        area,
    );
}

/// `Messages.app running │ 3 unread in 2 chats │ watching chat.db`, or a toast
/// while one is alive.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let theme = &app.theme;

    let line = if let Some((text, is_error)) = app.status.active_toast() {
        let color = if is_error {
            theme.error
        } else {
            theme.accent_me
        };
        Line::from(vec![
            Span::raw(" "),
            Span::styled(text.to_string(), Style::new().fg(color)),
        ])
    } else {
        let mut spans = vec![Span::raw(" ")];
        for (index, segment) in segments(app).into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled("  │  ", Style::new().fg(theme.gray_dim)));
            }
            spans.push(Span::styled(segment, Style::new().fg(theme.text_secondary)));
        }
        Line::from(spans)
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// The status segments, left to right.
#[must_use]
pub fn segments(app: &App) -> Vec<String> {
    let mut segments = Vec::with_capacity(4);

    segments.push(match app.status.messages_app_running {
        Some(true) => "Messages.app running".to_string(),
        Some(false) => "Messages.app not running".to_string(),
        None => "Messages.app unknown".to_string(),
    });

    segments.push(match &app.status.db {
        DbStatus::NotOpened => "chat.db not opened".to_string(),
        DbStatus::Ready => match app.status.unread_total {
            0 => "no unread".to_string(),
            1 => "1 unread".to_string(),
            n => format!("{n} unread in {} chats", app.status.unread_chats),
        },
        DbStatus::Unreadable(reason) => format!("chat.db unreadable: {reason}"),
    });

    // While the index is building it is the most interesting thing on the
    // line, so it goes before how fresh the screen is.
    if let Some(note) = app.search_state().note() {
        segments.push(note);
    }

    let how = match app.status.watcher {
        WatcherStatus::Off => "watcher off",
        WatcherStatus::Watching => "watching chat.db",
        WatcherStatus::Polling => "polling chat.db",
    };
    // How fresh the screen is, once there has been something to be fresh about.
    segments.push(match app.status.last_update {
        Some(when) => format!("{how} · {}", super::format::age(when.elapsed())),
        None => how.to_string(),
    });

    segments
}

/// The condensed key hints along the very bottom.
pub fn render_shortcuts(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(super::composer::shortcut_bar_line(app)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn segments_describe_a_fresh_app_without_a_database() {
        let app = App::new(Config::default(), Vec::new());
        let segments = segments(&app);
        assert_eq!(segments.len(), 3);
        assert!(segments[1].contains("not opened"));
        assert!(segments[2].contains("watcher off"));
    }

    #[test]
    fn unread_segment_pluralizes() {
        let mut app = App::new(Config::default(), Vec::new());
        app.status.db = DbStatus::Ready;
        assert_eq!(segments(&app)[1], "no unread");
        app.status.unread_total = 1;
        assert_eq!(segments(&app)[1], "1 unread");
        app.status.unread_total = 3;
        app.status.unread_chats = 2;
        assert_eq!(segments(&app)[1], "3 unread in 2 chats");
    }
}
