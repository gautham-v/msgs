//! The chrome, right-aligned on the header's row (or the filter line, on
//! the list screen): the unread count while there is one, or a toast while
//! one is alive, then `? help`. Nothing else, and no row of its own.

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, DbStatus, WatcherStatus};

/// `2 unread · ? help`, right-aligned on `area`'s row, with a toast in the
/// unread's place while one is alive.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    if area.width < 8 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    let mut spans = Vec::new();

    let left = match app.status.active_toast() {
        Some((text, is_error)) => Some((
            text.to_string(),
            if is_error {
                theme.error
            } else {
                theme.text_secondary
            },
        )),
        None => unread_label(app).map(|label| (label, theme.gray)),
    };
    if let Some((text, color)) = left {
        // Never wider than half the row: the title on the left keeps its
        // half, and a toast is only ever a few words.
        let room = usize::from(area.width / 2).saturating_sub(10);
        spans.push(Span::styled(
            super::format::truncate(&text, room),
            Style::new().fg(color),
        ));
        spans.push(Span::styled(" · ", Style::new().fg(theme.gray)));
    }
    spans.push(Span::styled(
        "?",
        Style::new()
            .fg(theme.text_secondary)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" help ", Style::new().fg(theme.gray)));

    let cells = cells_of(&spans).min(area.width);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Right),
        Rect {
            x: area.x + area.width - cells,
            width: cells,
            height: 1,
            ..area
        },
    );
}

fn cells_of(spans: &[Span<'_>]) -> u16 {
    let width: usize = spans
        .iter()
        .map(|span| super::format::width(&span.content))
        .sum();
    u16::try_from(width).unwrap_or(u16::MAX)
}

/// How many cells [`render`] will take on the right of a row `columns` wide,
/// so what shares the row can stop short of it.
#[must_use]
pub fn reserved(app: &App, columns: u16) -> u16 {
    let left = match app.status.active_toast() {
        Some((text, _)) => Some(text.to_string()),
        None => unread_label(app),
    };
    let left = left.map_or(0, |text| {
        let room = usize::from(columns / 2).saturating_sub(10);
        super::format::width(&super::format::truncate(&text, room)) + 3
    });
    u16::try_from(left + 7).unwrap_or(u16::MAX)
}

/// `1 unread` or `3 unread in 2 chats` while there is any; nothing when
/// there is not, because a footer that says "no unread" all day is noise.
#[must_use]
pub fn unread_label(app: &App) -> Option<String> {
    if app.status.db != DbStatus::Ready {
        return None;
    }
    match app.status.unread_total {
        0 => None,
        1 => Some("1 unread".to_string()),
        n if app.status.unread_chats > 1 => {
            Some(format!("{n} unread in {} chats", app.status.unread_chats))
        }
        n => Some(format!("{n} unread")),
    }
}

/// What the app is doing, as segments: how fresh the screen is, the unread
/// total, the index while it is building, and Messages.app. The panes do not
/// draw these any more — `--check` and the tests are where they surface —
/// but the answers stay in one place.
#[must_use]
pub fn segments(app: &App) -> Vec<String> {
    let mut segments = Vec::with_capacity(4);

    let how = match app.status.watcher {
        WatcherStatus::Off => "watcher off",
        WatcherStatus::Watching => "watching chat.db",
        WatcherStatus::Polling => "polling chat.db",
    };
    segments.push(match app.status.last_update {
        Some(when) => format!("{how} · {}", super::format::age(when.elapsed())),
        None => how.to_string(),
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

    if let Some(note) = app.search_state().note() {
        segments.push(note);
    }

    segments.push(match app.status.messages_app_running {
        Some(true) => "Messages.app running".to_string(),
        Some(false) => "Messages.app not running".to_string(),
        None => "Messages.app unknown".to_string(),
    });

    segments
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
        assert!(segments[0].contains("watcher off"));
        assert!(segments[1].contains("not opened"));
        assert!(segments[2].contains("Messages.app"));
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

    #[test]
    fn the_footer_says_unread_only_when_there_is_some() {
        let mut app = App::new(Config::default(), Vec::new());
        assert_eq!(unread_label(&app), None, "no database yet");
        app.status.db = DbStatus::Ready;
        assert_eq!(unread_label(&app), None, "nothing unread says nothing");
        app.status.unread_total = 1;
        app.status.unread_chats = 1;
        assert_eq!(unread_label(&app).as_deref(), Some("1 unread"));
        app.status.unread_total = 3;
        assert_eq!(unread_label(&app).as_deref(), Some("3 unread"));
        app.status.unread_chats = 2;
        assert_eq!(unread_label(&app).as_deref(), Some("3 unread in 2 chats"));
    }
}
