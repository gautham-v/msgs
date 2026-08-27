//! The `?` help modal: every binding in two columns, scrollable.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use crate::app::App;
use crate::keymap::{BINDINGS, Binding};

/// Width of the key column, in characters.
const KEY_COLUMN: usize = 12;

/// Draw the modal centered over `area`.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let lines = help_lines(app);
    let wanted_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4);
    let modal = super::centered(area, 62.min(area.width), wanted_height.min(area.height));
    if modal.width < 8 || modal.height < 4 {
        return;
    }

    frame.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_active))
        .style(Style::new().bg(theme.bg_light).fg(theme.text_primary))
        .padding(Padding::horizontal(1))
        .title_top(Span::styled(
            " keys ",
            Style::new()
                .fg(theme.accent_me)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " ↑↓ scroll · Esc close ",
            Style::new().fg(theme.gray),
        ));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let max_scroll = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(inner.height);
    let scroll = app.help_scroll.min(max_scroll);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

/// One line per binding, grouped by scope with a heading between groups.
fn help_lines(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines = Vec::with_capacity(BINDINGS.len() + 4);
    let mut current_scope = "";

    for Binding {
        keys,
        description,
        scope,
    } in BINDINGS
    {
        if *scope != current_scope {
            if !current_scope.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                scope.to_uppercase(),
                Style::new().fg(theme.gray).add_modifier(Modifier::BOLD),
            )));
            current_scope = scope;
        }
        let padded = format!("{keys:<KEY_COLUMN$}");
        lines.push(Line::from(vec![
            Span::styled(
                padded,
                Style::new()
                    .fg(theme.accent_me)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(*description, Style::new().fg(theme.text_secondary)),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn every_binding_gets_a_line_plus_group_headings() {
        let app = App::new(Config::default(), Vec::new());
        let lines = help_lines(&app);
        assert!(lines.len() > BINDINGS.len());
        for binding in BINDINGS {
            assert!(
                lines.iter().any(|line| line
                    .spans
                    .iter()
                    .any(|span| span.content == binding.description)),
                "missing a line for {}",
                binding.keys
            );
        }
    }
}
