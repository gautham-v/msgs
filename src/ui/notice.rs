//! The copy notice: one right-aligned line directly above the composer,
//! saying how much just went to the clipboard.
//!
//! It is the one piece of chrome that is not on the header's row. It gets a
//! row of its own, taken off the bottom of the conversation by [`compute`]
//! only while the notice is alive, so it is never painted over a message.
//!
//! [`compute`]: super::compute

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

/// Draw `copied 27 chars to clipboard` on `area`, right-aligned and inset a
/// column so it ends where the composer's box does.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(text) = app.notice() else {
        return;
    };
    if area.height == 0 || area.width < 4 {
        return;
    }
    let inset = Rect {
        x: area.x + 1,
        width: area.width - 2,
        height: 1,
        ..area
    };
    let shown = super::format::truncate(text, usize::from(inset.width));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shown,
            Style::new().fg(app.theme.text_secondary),
        )))
        .alignment(Alignment::Right),
        inset,
    );
}
