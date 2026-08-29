//! The `Ctrl+R` reaction picker: six glyphs in a row over a dimmed screen.
//!
//! Nothing here decides anything. [`crate::app::ReactionPicker`] holds what the
//! picker is aimed at and which glyph is under the cursor; this file draws it,
//! and marks the reactions you have already given so it is clear that choosing
//! one again takes it back.
//!
//! Without `imsg` on `$PATH` there is no way to put a reaction on the wire at
//! all, so the picker says how to install it and the row is drawn dim. The
//! same goes for the message a stock Mac cannot reach: with System Integrity
//! Protection on, `imsg` gets at the newest incoming message of a chat and no
//! other, and the caption says so before `Enter` rather than after.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use crate::app::{App, ReactionPicker};
use crate::send::{IMSG, IMSG_INSTALL, REACTIONS, SIP_REACH};
use crate::theme::Theme;

/// Width of the picker in columns, before clamping to the screen.
const WIDTH: u16 = 52;
/// Two borders plus the glyph row, a blank row, and the caption.
const HEIGHT: u16 = 5;
/// Blank columns kept either side of the text, inside the borders.
const PAD: u16 = 1;

/// Draw the picker centered over `area`.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = app.reaction_picker.as_ref() else {
        return;
    };
    let theme = &app.theme;
    let width = WIDTH.min(area.width);
    let height = HEIGHT.min(area.height);
    if width < 24 || height < HEIGHT {
        return;
    }
    let modal = super::centered(area, width, height);

    // The same treatment the palette gets: the screen behind goes dim so the
    // eye lands on the row of glyphs.
    frame
        .buffer_mut()
        .set_style(area, Style::new().add_modifier(Modifier::DIM));
    frame.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_active))
        .style(Style::new().bg(theme.bg_light).fg(theme.text_primary))
        .padding(Padding::horizontal(PAD))
        .title_top(Span::styled(" react ", Style::new().fg(theme.gray)))
        .title_bottom(Span::styled(footer(picker), Style::new().fg(theme.gray)));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    if inner.width == 0 || inner.height < 3 {
        return;
    }

    frame.render_widget(
        Paragraph::new(glyph_line(theme, picker)).alignment(Alignment::Center),
        Rect { height: 1, ..inner },
    );
    frame.render_widget(
        Paragraph::new(caption(theme, picker)).alignment(Alignment::Center),
        Rect {
            y: inner.y + 2,
            height: 1,
            ..inner
        },
    );
}

/// The row of glyphs: the cursor on a lighter band, the ones you have already
/// given in the accent color.
fn glyph_line(theme: &Theme, picker: &ReactionPicker) -> Line<'static> {
    let mut spans = Vec::with_capacity(REACTIONS.len());
    for (index, reaction) in REACTIONS.into_iter().enumerate() {
        let mut style = if picker.holds(reaction) {
            Style::new().fg(theme.accent_me)
        } else {
            Style::new().fg(theme.text_primary)
        };
        if index == picker.selected {
            style = style.bg(theme.bg_highlight).add_modifier(Modifier::BOLD);
        }
        if !picker.reaches() {
            style = style.add_modifier(Modifier::DIM);
        }
        spans.push(Span::styled(format!(" {} ", reaction.glyph()), style));
        spans.push(Span::raw(" "));
    }
    spans.pop();
    Line::from(spans)
}

/// The line under the glyphs: what the cursor is on, or the one reason
/// pressing `Enter` would not send anything.
fn caption(theme: &Theme, picker: &ReactionPicker) -> Line<'static> {
    if !picker.available {
        return Line::from(vec![
            Span::styled(format!("no {IMSG} — "), Style::new().fg(theme.error)),
            Span::styled(IMSG_INSTALL, Style::new().fg(theme.text_secondary)),
        ]);
    }
    if picker.fallback.is_none() && !picker.bridge {
        return Line::from(Span::styled(SIP_REACH, Style::new().fg(theme.error)));
    }
    let reaction = picker.reaction();
    let mut spans = vec![Span::styled(
        reaction.label().to_string(),
        Style::new().fg(theme.text_primary),
    )];
    if picker.holds(reaction) {
        // Only the bridge takes a reaction back, so on a stock Mac the key
        // says what it would do rather than promising it.
        spans.push(Span::styled(
            if picker.bridge {
                " · yours, Enter takes it back"
            } else {
                " · yours, and taking it back needs SIP off"
            },
            Style::new().fg(theme.gray),
        ));
    }
    Line::from(spans)
}

/// The keys along the bottom border.
fn footer(picker: &ReactionPicker) -> &'static str {
    if picker.reaches() {
        " ←→ choose · 1–6 · Enter react · Esc close "
    } else {
        " Esc close "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::send::Reaction;

    fn picker(available: bool) -> ReactionPicker {
        ReactionPicker {
            target_guid: "ABCD-1234".to_string(),
            part: 0,
            chat_rowid: 1,
            selected: 0,
            standing: vec![Reaction::Like],
            available,
            bridge: true,
            fallback: None,
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn the_row_holds_every_sendable_reaction() {
        let theme = Theme::default();
        let drawn = text(&glyph_line(&theme, &picker(true)));
        for reaction in REACTIONS {
            assert!(
                drawn.contains(reaction.glyph()),
                "{} is missing from the picker",
                reaction.label()
            );
        }
    }

    #[test]
    fn a_reaction_you_already_gave_says_the_key_takes_it_back() {
        let theme = Theme::default();
        let mut picker = picker(true);
        picker.selected = 1;
        assert!(picker.holds(Reaction::Like));
        let said = text(&caption(&theme, &picker));
        assert!(said.starts_with("like"));
        assert!(said.contains("takes it back"));

        picker.selected = 0;
        assert!(!text(&caption(&theme, &picker)).contains("takes it back"));
    }

    #[test]
    fn without_the_helper_the_picker_only_says_how_to_get_it() {
        let theme = Theme::default();
        let said = text(&caption(&theme, &picker(false)));
        assert!(said.contains(IMSG_INSTALL));
        assert!(!footer(&picker(false)).contains("react"));
    }

    #[test]
    fn a_message_no_route_reaches_says_so_instead_of_naming_a_reaction() {
        // A stock Mac: SIP on, so the bridge is out, and this message is not
        // the newest incoming one the other route can get at.
        let theme = Theme::default();
        let mut picker = picker(true);
        picker.bridge = false;
        assert!(!picker.reaches());
        assert_eq!(text(&caption(&theme, &picker)), SIP_REACH);
        assert!(!footer(&picker).contains("react"));
    }

    #[test]
    fn every_caption_fits_between_the_borders() {
        // The caption is one centered line inside a box of a fixed width, so
        // a sentence that outgrows it is silently cut in half.
        let theme = Theme::default();
        let inner = usize::from(WIDTH) - 2 - 2 * usize::from(PAD);
        let reachable = crate::send::ReactFallback {
            chat_rowid: 1,
            db: std::path::PathBuf::from("/tmp/msgs-test.db"),
        };
        let mut said = vec![text(&caption(&theme, &picker(false)))];
        for bridge in [true, false] {
            for fallback in [None, Some(reachable.clone())] {
                for selected in 0..REACTIONS.len() {
                    let mut picker = picker(true);
                    picker.bridge = bridge;
                    picker.fallback = fallback.clone();
                    picker.selected = selected;
                    said.push(text(&caption(&theme, &picker)));
                }
            }
        }
        for line in said {
            assert!(line.chars().count() <= inner, "{} wide: {line}", line.len());
        }
        assert!(footer(&picker(true)).chars().count() <= usize::from(WIDTH) - 2);
    }

    #[test]
    fn without_the_bridge_the_key_does_not_promise_to_take_a_reaction_back() {
        let theme = Theme::default();
        let mut picker = picker(true);
        picker.bridge = false;
        picker.fallback = Some(crate::send::ReactFallback {
            chat_rowid: 1,
            db: std::path::PathBuf::from("/tmp/msgs-test.db"),
        });
        picker.selected = 1;
        assert!(picker.holds(Reaction::Like));
        let said = text(&caption(&theme, &picker));
        assert!(said.starts_with("like"));
        assert!(!said.contains("Enter takes it back"), "{said}");
        assert!(said.contains("SIP off"), "{said}");
        // The other five still go out that way, so the row stays live.
        assert!(picker.reaches());
        assert!(footer(&picker).contains("react"));
    }
}
