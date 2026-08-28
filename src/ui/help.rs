//! The `?` help modal: every binding in two columns, scrollable.
//!
//! The rows come from [`BINDINGS`], the same table [`crate::keymap::resolve`]
//! is written against, so a documented key and a working key cannot drift
//! apart. Groups are kept whole: the modal goes to two columns when the
//! terminal is wide enough for both of them, and the split lands on a group
//! boundary rather than in the middle of one. The column width is measured
//! from the rows themselves, so a longer description widens the modal instead
//! of being cut off.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use crate::app::App;
use crate::keymap::{BINDINGS, Binding};
use crate::theme::Theme;

/// Width of the key column, in characters.
const KEY_COLUMN: usize = 12;
/// Blank columns between the two columns of bindings.
const GUTTER: usize = 3;
/// Borders plus one column of padding on each side.
const CHROME: u16 = 4;

/// Draw the modal centered over `area`.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let groups = groups(app);
    let column = column_width(&groups);
    let two_up = fits_two_columns(area.width, column);

    let mut lines = if two_up {
        two_columns(&groups, column)
    } else {
        groups.into_iter().flatten().collect()
    };

    // Startup warnings run the width of the modal rather than joining a
    // column, because a path in one is longer than any binding row. The modal
    // widens for one up to what the terminal has, and only then cuts it.
    let bindings_width = if two_up { column * 2 + GUTTER } else { column };
    let room = usize::from(area.width.saturating_sub(CHROME));
    let body = bindings_width.max(note_width(app).min(room));
    lines.extend(notes(app, body));

    let wanted_width = u16::try_from(body)
        .unwrap_or(u16::MAX)
        .saturating_add(CHROME);
    let wanted_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4);
    let modal = super::centered(
        area,
        wanted_width.min(area.width),
        wanted_height.min(area.height),
    );
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
        .title_top(Span::styled(" keys ", Style::new().fg(theme.gray)))
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

/// Whether two columns of `column` cells each fit in a modal `width` wide.
fn fits_two_columns(width: u16, column: usize) -> bool {
    let wanted = column * 2 + GUTTER + usize::from(CHROME);
    usize::from(width) >= wanted
}

/// The widest row in any group, which is what one column has to be.
fn column_width(groups: &[Vec<Line<'static>>]) -> usize {
    groups
        .iter()
        .flatten()
        .map(line_width)
        .max()
        .unwrap_or(KEY_COLUMN)
}

/// The bindings as groups of lines: a heading, its rows, and a blank line
/// under it. Groups are the unit the two-column split works in, so a scope
/// never has its heading in one column and its keys in the other.
fn groups(app: &App) -> Vec<Vec<Line<'static>>> {
    let theme = &app.theme;
    let mut groups: Vec<Vec<Line<'static>>> = Vec::new();
    let mut current_scope = "";

    for Binding {
        keys,
        description,
        scope,
    } in BINDINGS
    {
        if *scope != current_scope {
            if let Some(last) = groups.last_mut() {
                last.push(Line::default());
            }
            groups.push(vec![heading(scope, theme)]);
            current_scope = scope;
        }
        let padded = format!("{keys:<KEY_COLUMN$}");
        let row = Line::from(vec![
            Span::styled(
                padded,
                Style::new()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(*description, Style::new().fg(theme.gray)),
        ]);
        if let Some(last) = groups.last_mut() {
            last.push(row);
        }
    }

    groups
}

/// Anything that went wrong at startup — a config key that did not parse,
/// Contacts that would not open, a file watcher that would not start. Each is
/// said once as a toast when it happens and then lives here, which is the one
/// place it can be read again.
fn notes(app: &App, width: usize) -> Vec<Line<'static>> {
    if app.status.warnings.is_empty() {
        return Vec::new();
    }
    let theme = &app.theme;
    let mut lines = vec![Line::default(), heading("notes", theme)];
    lines.extend(app.status.warnings.iter().map(|warning| {
        Line::from(Span::styled(
            super::format::truncate(warning, width),
            Style::new().fg(theme.text_secondary),
        ))
    }));
    lines
}

/// The widest startup warning, which is what the modal grows for.
fn note_width(app: &App) -> usize {
    app.status
        .warnings
        .iter()
        .map(|warning| super::format::width(warning))
        .max()
        .unwrap_or(0)
}

fn heading(scope: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        scope.to_uppercase(),
        Style::new().fg(theme.gray),
    ))
}

/// Lay the groups out side by side, balanced by line count.
///
/// The left column takes whole groups until it holds at least half the lines;
/// everything after that goes on the right. A row that only one side has is
/// still emitted, so nothing is dropped when the columns come out uneven.
fn two_columns(groups: &[Vec<Line<'static>>], column: usize) -> Vec<Line<'static>> {
    let total: usize = groups.iter().map(Vec::len).sum();
    let mut split = groups.len();
    let mut running = 0;
    for (index, group) in groups.iter().enumerate() {
        running += group.len();
        // Break after the group that carries us past halfway — unless that is
        // the only group there is, because one column of one is not two.
        if running * 2 >= total && index + 1 < groups.len() {
            split = index + 1;
            break;
        }
    }

    let left: Vec<Line<'static>> = groups[..split].iter().flatten().cloned().collect();
    let right: Vec<Line<'static>> = groups[split..].iter().flatten().cloned().collect();

    (0..left.len().max(right.len()))
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let used = match left.get(row) {
                Some(line) => {
                    spans.extend(line.spans.iter().cloned());
                    line_width(line)
                }
                None => 0,
            };
            if let Some(line) = right.get(row) {
                spans.push(Span::raw(" ".repeat(column + GUTTER - used.min(column))));
                spans.extend(line.spans.iter().cloned());
            }
            Line::from(spans)
        })
        .collect()
}

/// How many terminal cells a line takes.
fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| super::format::width(&span.content))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn flat(app: &App) -> Vec<Line<'static>> {
        groups(app).into_iter().flatten().collect()
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn every_binding_gets_a_line_plus_group_headings() {
        let app = App::new(Config::default(), Vec::new());
        let lines = flat(&app);
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

    #[test]
    fn startup_warnings_are_readable_again_after_the_toast_expires() {
        let mut app = App::new(Config::default(), Vec::new());
        assert!(notes(&app, 60).is_empty());
        app.status.warnings.push("config: no such key".to_string());
        assert_eq!(note_width(&app), "config: no such key".len());
        let lines = notes(&app, 60);
        assert!(lines.iter().any(|line| text(line) == "NOTES"));
        assert!(lines.iter().any(|line| text(line) == "config: no such key"));

        // A note wider than the modal is cut to it rather than clipped away.
        let narrow = notes(&app, 12);
        for line in &narrow {
            assert!(line_width(line) <= 12, "{:?}", text(line));
        }
    }

    #[test]
    fn two_columns_keep_every_line_and_stay_inside_the_width() {
        let app = App::new(Config::default(), Vec::new());
        let groups = groups(&app);
        let column = column_width(&groups);
        let single = groups.iter().flatten().count();
        let paired = two_columns(&groups, column);

        // Two columns are shorter than one, and no line is lost.
        assert!(paired.len() < single);
        assert!(paired.len() >= single.div_ceil(2));
        for binding in BINDINGS {
            assert!(
                paired
                    .iter()
                    .any(|line| text(line).contains(binding.description)),
                "missing {} after pairing",
                binding.keys
            );
        }
        for line in &paired {
            assert!(
                line_width(line) <= column * 2 + GUTTER,
                "line too wide: {:?}",
                text(line)
            );
        }
    }

    #[test]
    fn a_narrow_terminal_gets_one_column_and_a_wide_one_gets_two() {
        let app = App::new(Config::default(), Vec::new());
        let column = column_width(&groups(&app));
        let two_up = u16::try_from(column * 2 + GUTTER).unwrap() + CHROME;
        assert!(fits_two_columns(two_up, column));
        assert!(!fits_two_columns(two_up - 1, column));
        assert!(!fits_two_columns(80, column));
    }

    #[test]
    fn a_single_group_is_not_split_against_itself() {
        let one = vec![vec![Line::from("A"), Line::from("B"), Line::from("C")]];
        assert_eq!(two_columns(&one, 10).len(), 3);
    }
}
