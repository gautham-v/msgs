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
pub mod filepick;
pub mod format;
pub mod help;
pub mod message;
pub mod notice;
pub mod palette;
pub mod status;

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Block;

use crate::app::{App, Focus};
use crate::config::MIN_WIDTH_FOR_CHAT_LIST;

/// Where each part of the screen was drawn on the last frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Panes {
    /// The whole terminal, which is how the app learns its size.
    pub screen: Rect,
    /// The whole chat list pane, `None` when it is hidden or the terminal is
    /// too narrow for it.
    pub chat_list: Option<Rect>,
    /// Just the selectable rows of the chat list, below its filter line.
    pub chat_list_rows: Option<Rect>,
    /// Chat name, service, and counts, above the messages.
    pub header: Rect,
    /// The one row the day of the topmost message is held on, between the
    /// header and the messages. `None` when there is nothing to label or no
    /// row to spare — the mockup's `.day.sticky` band.
    pub day: Option<Rect>,
    /// The scrolling message area.
    pub conversation: Rect,
    /// The one row `copied N chars to clipboard` is written on, between the
    /// messages and the composer. `None` whenever no notice is alive, which
    /// is nearly always — the row belongs to the conversation the rest of the
    /// time.
    pub notice: Option<Rect>,
    /// The send box.
    pub composer: Rect,
    /// The row the chrome shares with the header — or the filter line, on
    /// the list screen: `? help` and the unread count or a toast, right-aligned
    /// on it. Nothing of its own, so no row is spent on it.
    pub status: Rect,
}

/// Split `area` into panes according to the app's current state.
///
/// The only rule as the terminal shrinks is that the composer gives up its
/// extra lines before the conversation gives up its last row. Below
/// [`MIN_WIDTH_FOR_CHAT_LIST`] columns the chat list is not docked beside the
/// conversation: it is a screen of its own, drawn instead of the conversation
/// while it has focus, and nothing while it does not.
#[must_use]
pub fn compute(area: Rect, app: &App) -> Panes {
    if list_screen(area, app) {
        let (list, rows) = list_rows(area, false);
        return Panes {
            screen: area,
            chat_list: Some(list),
            chat_list_rows: Some(rows),
            header: empty_at(area),
            day: None,
            conversation: empty_at(area),
            notice: None,
            composer: empty_at(area),
            status: Rect {
                height: 1.min(area.height),
                ..list
            },
        };
    }

    let (chat_list, chat_list_rows, convo) = split_chat_list(area, app);
    let (header, day, conversation, notice, composer) = split_conversation(convo, app);
    let status = Rect {
        height: 1.min(header.height),
        ..header
    };

    Panes {
        screen: area,
        chat_list,
        chat_list_rows,
        header,
        day,
        conversation,
        notice,
        composer,
        status,
    }
}

/// Whether the chat list takes the whole terminal: too narrow to dock it,
/// and the list is what has focus.
#[must_use]
pub fn list_screen(area: Rect, app: &App) -> bool {
    area.width < MIN_WIDTH_FOR_CHAT_LIST && app.focus == Focus::ChatList && app.db_error.is_none()
}

/// The list pane and its selectable rows: under the filter line and the
/// blank row beneath it, and left of the divider when there is one.
fn list_rows(list: Rect, divider: bool) -> (Rect, Rect) {
    let rows = Rect::new(
        list.x,
        list.y.saturating_add(chat_list::HEAD_ROWS),
        list.width.saturating_sub(u16::from(divider)),
        list.height.saturating_sub(chat_list::HEAD_ROWS),
    );
    (list, rows)
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
    let (list, rows) = list_rows(list, true);
    (Some(list), Some(rows), convo)
}

fn split_conversation(area: Rect, app: &App) -> (Rect, Option<Rect>, Rect, Option<Rect>, Rect) {
    let mut rest = area;
    // Header text plus the rule under it.
    let header_height = if area.height >= 6 { 2 } else { 1 };
    let header = take_top(&mut rest, header_height, true).unwrap_or(empty_at(area));

    // The composer grows with the draft but always leaves one message row.
    let wanted = app.composer_height(area.width);
    let composer_height = wanted.min(rest.height.saturating_sub(1)).max(1);
    let composer = take_bottom(&mut rest, composer_height, true).unwrap_or(empty_at(area));

    // The copy notice is the one bit of chrome off the header's row, and it
    // only exists while it has something to say. Like the day band it is a
    // row of its own rather than a line painted over the bottom message, and
    // it never takes the conversation's last row.
    let room_for_a_notice = app.notice().is_some() && rest.height >= 2;
    let notice = take_bottom(&mut rest, 1, room_for_a_notice);

    // The day band is a row of its own rather than a label painted over the
    // top message, so nothing a message says is ever hidden under it. It is
    // only worth a row once there are messages to label and a row to spare.
    let room_for_a_band = !app.message_rows.is_empty() && rest.height >= 3;
    let day = take_top(&mut rest, 1, room_for_a_band);

    (header, day, rest, notice, composer)
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
    // What was on screen last time is not what is on screen now; the pictures
    // fill the list back in as they are drawn, and only those are animated.
    app.images.begin_frame();

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
        app.panes = Panes {
            screen: area,
            ..Panes::default()
        };
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

    // Measuring and settling the viewport happens before anything is drawn, so
    // the blocks, the scrollback, and the mouse map all describe one state.
    app.prepare_conversation(panes.conversation);

    if let (Some(list), Some(rows)) = (panes.chat_list, panes.chat_list_rows) {
        chat_list::render(frame, app, list, rows, panes.conversation.width > 0);
    }
    conversation::render_header(frame, app, panes.header);
    if let Some(day) = panes.day {
        conversation::render_day_band(frame, app, day);
    }
    let hits = conversation::render(frame, app, panes.conversation);
    app.hits = hits;
    // A released drag is copied here rather than in `on_mouse`: the words it
    // covered are cells of the frame that just drew them, so they are read
    // back off this buffer instead of a copy of it kept on the app. The toast
    // still lands on this frame, because the status line is drawn below.
    if app.copy_selection_pending
        && let Some(selection) = app.selection
    {
        let text = conversation::selection_text(frame.buffer_mut(), panes.conversation, &selection);
        let (start, end) = selection.span();
        // One row is a phrase and comes across as the cells said it; more
        // than one is a transcript of the messages under them.
        let indices = if start.y == end.y {
            Vec::new()
        } else {
            conversation::selected_messages(&selection, panes.conversation, &app.hits)
        };
        app.copy_dragged(&text, &indices);
    }
    if let Some(row) = panes.notice {
        notice::render(frame, app, row);
    }
    composer::render(frame, app, panes.composer);
    // The `@` picker stands on top of the composer, so it goes on after it
    // and before the chrome that shares the header's row.
    if app.focus == Focus::FilePicker {
        filepick::render(frame, app, area);
    }
    // Last on its row, so it sits over the tail of a long title or filter.
    status::render(frame, app, panes.status);

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
        // `status` shares the header's row on purpose, so it is not here.
        let mut rects = vec![panes.header, panes.conversation, panes.composer];
        rects.extend(panes.chat_list);
        rects.extend(panes.day);
        rects.extend(panes.notice);
        rects.retain(|rect| rect.width > 0 && rect.height > 0);
        rects
    }

    #[test]
    fn wide_terminal_shows_every_pane() {
        let panes = compute(Rect::new(0, 0, 120, 40), &app());
        assert!(panes.chat_list.is_some());
        assert!(panes.conversation.height > 20);
    }

    #[test]
    fn narrow_terminal_makes_the_chat_list_a_screen_of_its_own() {
        let mut app = app();
        assert!(app.show_chat_list);
        // The list has focus at startup, so a narrow terminal opens on it.
        let narrow = Rect::new(0, 0, MIN_WIDTH_FOR_CHAT_LIST - 1, 40);
        let panes = compute(narrow, &app);
        let list = panes.chat_list.expect("the list screen");
        assert_eq!(list.width, narrow.width);
        assert_eq!(panes.conversation.width, 0);
        assert_eq!(panes.composer.width, 0);

        // Focus on the conversation, and the list is gone entirely.
        app.focus = Focus::Conversation;
        let panes = compute(narrow, &app);
        assert!(panes.chat_list.is_none());
        assert_eq!(panes.conversation.width, MIN_WIDTH_FOR_CHAT_LIST - 1);

        let panes = compute(Rect::new(0, 0, MIN_WIDTH_FOR_CHAT_LIST, 40), &app);
        assert!(panes.chat_list.is_some());

        app.show_chat_list = false;
        let panes = compute(Rect::new(0, 0, 200, 40), &app);
        assert!(panes.chat_list.is_none());
    }

    #[test]
    fn a_toggle_on_a_narrow_terminal_swaps_the_list_screen_in_and_out() {
        use crate::app::Action;

        let narrow = Rect::new(0, 0, MIN_WIDTH_FOR_CHAT_LIST - 1, 40);
        let mut app = app();
        app.focus = Focus::Conversation;
        app.panes = compute(narrow, &app);
        assert!(app.panes.chat_list.is_none());

        app.update(Action::ToggleChatList);
        let panes = compute(narrow, &app);
        assert!(panes.chat_list.is_some(), "Ctrl+B brings the list screen");
        assert_eq!(panes.conversation.width, 0);

        app.panes = panes;
        app.update(Action::ToggleChatList);
        let panes = compute(narrow, &app);
        assert!(panes.chat_list.is_none(), "and takes it away again");
        assert_eq!(panes.conversation.width, MIN_WIDTH_FOR_CHAT_LIST - 1);
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
        assert_eq!(tall.status.y, tall.header.y, "on the header's row");
        assert_eq!(tall.status.height, 1);
        assert_eq!(tall.screen.width, 120);

        let short = compute(Rect::new(0, 0, 120, 7), &app);
        assert!(short.status.height == 1);
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
    fn the_copy_notice_takes_a_row_only_while_it_is_alive() {
        let mut app = app();
        let area = Rect::new(0, 0, 120, 40);

        let quiet = compute(area, &app);
        assert!(quiet.notice.is_none(), "no notice, no row");

        app.notify_copied(27, 0);
        let loud = compute(area, &app);
        let notice = loud.notice.expect("a row for the notice");
        assert_eq!(notice.height, 1);
        assert_eq!(notice.y + 1, loud.composer.y, "directly above the composer");
        assert_eq!(
            notice.y,
            loud.conversation.y + loud.conversation.height,
            "and the row came off the bottom of the conversation"
        );
        assert_eq!(loud.conversation.height, quiet.conversation.height - 1);
        assert_eq!(loud.composer, quiet.composer, "the composer does not move");

        // Never at the cost of the conversation's last row.
        for height in [4u16, 5, 6, 7] {
            let tight = compute(Rect::new(0, 0, 120, height), &app);
            assert!(
                tight.conversation.height >= 1,
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
