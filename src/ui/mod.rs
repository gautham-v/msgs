//! Screen layout and drawing.
//!
//! [`compute`] is a pure function from a terminal [`Rect`] to the rectangle of
//! every pane, so the responsive rules can be tested without a terminal.
//! [`draw`] then paints each pane and records the rectangles on the app for
//! mouse hit-testing on the next frame.

pub mod chat_list;
pub mod composer;
pub mod conversation;
pub mod db_error;
pub mod format;
pub mod help;
pub mod palette;
pub mod status;

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Block;

use crate::app::{App, Focus};
use crate::config::MIN_WIDTH_FOR_CHAT_LIST;

/// Below this many rows the shortcuts bar is dropped to keep message rows.
pub const MIN_HEIGHT_FOR_SHORTCUTS: u16 = 12;
/// Below this many rows the composer hint row goes too.
pub const MIN_HEIGHT_FOR_INFO_ROW: u16 = 10;
/// Below this many rows the rule above the status line goes.
pub const MIN_HEIGHT_FOR_RULE: u16 = 8;

/// Where each part of the screen was drawn on the last frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Panes {
    /// The whole chat list pane, `None` when it is hidden or the terminal is
    /// too narrow for it.
    pub chat_list: Option<Rect>,
    /// Just the selectable rows of the chat list, below its filter line.
    pub chat_list_rows: Option<Rect>,
    /// Chat name, service, and counts, above the messages.
    pub header: Rect,
    /// The scrolling message area.
    pub conversation: Rect,
    /// The bordered send box.
    pub composer: Rect,
    /// The hint row under the composer.
    pub info_row: Option<Rect>,
    /// The horizontal rule between the panes and the status line.
    pub rule: Option<Rect>,
    /// The status line.
    pub status: Rect,
    /// The shortcuts bar under the status line.
    pub shortcuts: Option<Rect>,
}

/// Split `area` into panes according to the app's current state.
///
/// Rules, in the order they give way as the terminal shrinks: the shortcuts
/// bar, the composer hint row, the rule above the status line, then the
/// composer's extra lines. The chat list is hidden below
/// [`MIN_WIDTH_FOR_CHAT_LIST`] columns regardless of the toggle.
#[must_use]
pub fn compute(area: Rect, app: &App) -> Panes {
    let mut rest = area;

    let shortcuts = take_bottom(&mut rest, 1, area.height >= MIN_HEIGHT_FOR_SHORTCUTS);
    let status = take_bottom(&mut rest, 1, true).unwrap_or(empty_at(area));
    // A one-row rule separating the panes from the status line.
    let rule = take_bottom(&mut rest, 1, area.height >= MIN_HEIGHT_FOR_RULE);

    let (chat_list, chat_list_rows, convo) = split_chat_list(rest, app);
    let (header, conversation, composer, info_row) =
        split_conversation(convo, app, area.height >= MIN_HEIGHT_FOR_INFO_ROW);

    Panes {
        chat_list,
        chat_list_rows,
        header,
        conversation,
        composer,
        info_row,
        rule,
        status,
        shortcuts,
    }
}

fn split_chat_list(area: Rect, app: &App) -> (Option<Rect>, Option<Rect>, Rect) {
    let visible = app.show_chat_list && area.width >= MIN_WIDTH_FOR_CHAT_LIST;
    if !visible {
        return (None, None, area);
    }
    // Never let the list eat more than half the screen, and always leave the
    // conversation something to draw in.
    let width = app.config.chat_list_width.min(area.width / 2).max(1);
    let list = Rect::new(area.x, area.y, width, area.height);
    let convo = Rect::new(area.x + width, area.y, area.width - width, area.height);
    // Row 0 is the filter line; the last column is the divider.
    let rows = Rect::new(
        list.x,
        list.y.saturating_add(1),
        list.width.saturating_sub(1),
        list.height.saturating_sub(1),
    );
    (Some(list), Some(rows), convo)
}

fn split_conversation(
    area: Rect,
    app: &App,
    show_info_row: bool,
) -> (Rect, Rect, Rect, Option<Rect>) {
    let mut rest = area;
    // Header text plus the rule under it.
    let header_height = if area.height >= 6 { 2 } else { 1 };
    let header = take_top(&mut rest, header_height, true).unwrap_or(empty_at(area));

    let info_row = take_bottom(&mut rest, 1, show_info_row);
    // The composer grows with the draft but always leaves one message row.
    let wanted = app.composer_height();
    let composer_height = wanted.min(rest.height.saturating_sub(1)).max(1);
    let composer = take_bottom(&mut rest, composer_height, true).unwrap_or(empty_at(area));

    (header, rest, composer, info_row)
}

fn take_top(rest: &mut Rect, rows: u16, when: bool) -> Option<Rect> {
    if !when || rest.height < rows || rows == 0 {
        return None;
    }
    let taken = Rect::new(rest.x, rest.y, rest.width, rows);
    *rest = Rect::new(rest.x, rest.y + rows, rest.width, rest.height - rows);
    Some(taken)
}

fn take_bottom(rest: &mut Rect, rows: u16, when: bool) -> Option<Rect> {
    if !when || rest.height < rows || rows == 0 {
        return None;
    }
    let taken = Rect::new(rest.x, rest.y + rest.height - rows, rest.width, rows);
    *rest = Rect::new(rest.x, rest.y, rest.width, rest.height - rows);
    Some(taken)
}

const fn empty_at(area: Rect) -> Rect {
    Rect::new(area.x, area.y, 0, 0)
}

/// Draw one frame and record the layout for the next mouse event.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let panes = compute(area, app);
    app.panes = panes;

    frame.render_widget(
        Block::new().style(
            Style::new()
                .bg(app.theme.bg_base)
                .fg(app.theme.text_primary),
        ),
        area,
    );

    // An unreadable database means there are no panes worth drawing; say what
    // went wrong instead, and leave the layout empty so stray clicks hit
    // nothing.
    if app.db_error.is_some() {
        app.panes = Panes::default();
        let app: &App = app;
        if let Some(err) = app.db_error.as_ref() {
            db_error::render(frame, app, area, err);
        }
        // Overlays still open over it, so `?` and `Ctrl+K` are not dead keys
        // that leave focus somewhere invisible.
        match app.focus {
            Focus::Palette => palette::render(frame, app, area),
            Focus::Help => help::render(frame, app, area),
            _ => {}
        }
        return;
    }

    if let (Some(list), Some(rows)) = (panes.chat_list, panes.chat_list_rows) {
        chat_list::render(frame, app, list, rows);
    }
    conversation::render_header(frame, app, panes.header);
    conversation::render(frame, app, panes.conversation);
    composer::render(frame, app, panes.composer);
    if let Some(info_row) = panes.info_row {
        composer::render_info_row(frame, app, info_row);
    }
    if let Some(rule) = panes.rule {
        status::render_rule(frame, app, rule);
    }
    status::render(frame, app, panes.status);
    if let Some(shortcuts) = panes.shortcuts {
        status::render_shortcuts(frame, app, shortcuts);
    }

    match app.focus {
        Focus::Palette => palette::render(frame, app, area),
        Focus::Help => help::render(frame, app, area),
        _ => {}
    }
}

/// A path with the home directory written as `~`, for showing on screen.
#[must_use]
pub fn home_relative(path: &Path) -> String {
    let shown = path.display().to_string();
    let Some(home) = dirs::home_dir() else {
        return shown;
    };
    let home = home.display().to_string();
    if home.is_empty() {
        return shown;
    }
    match shown.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => shown,
    }
}

/// A rectangle `width` × `height` centered in `area`, clamped to fit.
#[must_use]
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn app() -> App {
        App::new(Config::default(), Vec::new())
    }

    fn all_rects(panes: &Panes) -> Vec<Rect> {
        let mut rects = vec![
            panes.header,
            panes.conversation,
            panes.composer,
            panes.status,
        ];
        rects.extend(panes.chat_list);
        rects.extend(panes.info_row);
        rects.extend(panes.shortcuts);
        rects.retain(|rect| rect.width > 0 && rect.height > 0);
        rects
    }

    #[test]
    fn wide_terminal_shows_every_pane() {
        let panes = compute(Rect::new(0, 0, 120, 40), &app());
        assert!(panes.chat_list.is_some());
        assert!(panes.info_row.is_some());
        assert!(panes.shortcuts.is_some());
        assert!(panes.conversation.height > 20);
    }

    #[test]
    fn narrow_terminal_hides_the_chat_list() {
        let mut app = app();
        assert!(app.show_chat_list);
        let panes = compute(Rect::new(0, 0, MIN_WIDTH_FOR_CHAT_LIST - 1, 40), &app);
        assert!(panes.chat_list.is_none());
        assert_eq!(panes.conversation.width, MIN_WIDTH_FOR_CHAT_LIST - 1);

        let panes = compute(Rect::new(0, 0, MIN_WIDTH_FOR_CHAT_LIST, 40), &app);
        assert!(panes.chat_list.is_some());

        app.show_chat_list = false;
        let panes = compute(Rect::new(0, 0, 200, 40), &app);
        assert!(panes.chat_list.is_none());
    }

    #[test]
    fn chat_list_never_takes_more_than_half_the_width() {
        let mut app = app();
        app.config.chat_list_width = 60;
        let panes = compute(Rect::new(0, 0, 90, 40), &app);
        let list = panes.chat_list.expect("chat list");
        assert!(list.width <= 45, "list width {}", list.width);
        assert!(panes.conversation.width >= 45);
    }

    #[test]
    fn short_terminal_drops_chrome_before_message_rows() {
        let app = app();
        let tall = compute(Rect::new(0, 0, 120, 40), &app);
        assert!(tall.shortcuts.is_some() && tall.info_row.is_some());

        let medium = compute(Rect::new(0, 0, 120, MIN_HEIGHT_FOR_SHORTCUTS - 1), &app);
        assert!(medium.shortcuts.is_none());
        assert!(medium.info_row.is_some());

        let short = compute(Rect::new(0, 0, 120, MIN_HEIGHT_FOR_INFO_ROW - 1), &app);
        assert!(short.info_row.is_none());
        assert!(short.conversation.height >= 1);
    }

    #[test]
    fn panes_stay_inside_the_screen_and_never_overlap() {
        let app = app();
        for width in [40u16, 60, 90, 120, 200] {
            for height in [5u16, 8, 10, 12, 24, 60] {
                let area = Rect::new(0, 0, width, height);
                let panes = compute(area, &app);
                let rects = all_rects(&panes);
                for rect in &rects {
                    assert!(
                        rect.x + rect.width <= area.x + area.width
                            && rect.y + rect.height <= area.y + area.height,
                        "{rect:?} escapes {area:?}"
                    );
                }
                for (i, a) in rects.iter().enumerate() {
                    for b in &rects[i + 1..] {
                        let overlap = a.intersection(*b);
                        assert!(
                            overlap.width == 0 || overlap.height == 0,
                            "{a:?} overlaps {b:?} at {area:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_growing_composer_never_starves_the_conversation() {
        let mut app = app();
        for _ in 0..30 {
            app.update(crate::app::Action::FocusPane(crate::app::Focus::Composer));
            app.update(crate::app::Action::Newline);
        }
        for height in [6u16, 8, 10, 14, 30] {
            let panes = compute(Rect::new(0, 0, 120, height), &app);
            assert!(
                panes.conversation.height >= 1,
                "no message rows at height {height}"
            );
        }
    }

    #[test]
    fn centered_clamps_to_the_area() {
        let area = Rect::new(0, 0, 20, 10);
        assert_eq!(centered(area, 10, 4), Rect::new(5, 3, 10, 4));
        assert_eq!(centered(area, 100, 100), area);
    }
}
