//! The `@` file picker: a short list of files standing on top of the
//! composer.
//!
//! It is drawn the way the `Ctrl+K` palette is — a rounded box, gray on gray,
//! the matched characters picked out in the one highlight the palette uses and
//! no color of its own — but it does not float over the screen and does not
//! dim it: the draft below keeps the cursor, because what is typed while this
//! is open is still going into the message.
//!
//! The matching happens in [`crate::filepick`]; this file only draws what it
//! produced, highlighting exactly the characters the matcher saw.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use crate::app::{App, FilePicker};
use crate::filepick::Entry;
use crate::theme::Theme;
use crate::ui::format::{truncate, width};
use crate::ui::palette::highlight;

/// Widest the picker gets, before it is clamped to the composer.
const WIDTH: u16 = 56;
/// Most rows shown at once.
const MAX_VISIBLE: u16 = 8;

/// Draw the picker directly above the composer.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = app.file_picker.as_ref() else {
        return;
    };
    let composer = app.panes.composer;
    if composer.width < 12 || composer.y <= area.y + 1 {
        return;
    }

    // As many rows as there are files, inside what there is room for between
    // the top of the composer and the top of the screen.
    let room = composer.y - area.y;
    let rows = u16::try_from(picker.rows.len()).unwrap_or(MAX_VISIBLE);
    let visible = rows.clamp(1, MAX_VISIBLE).min(room.saturating_sub(2));
    if visible == 0 {
        return;
    }
    let height = visible + 2;
    let width = WIDTH.min(composer.width);
    let modal = Rect::new(composer.x, composer.y - height, width, height);

    let theme = &app.theme;
    frame.render_widget(Clear, modal);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_active))
        .style(Style::new().bg(theme.bg_light).fg(theme.text_primary))
        .padding(Padding::horizontal(1))
        .title_bottom(Span::styled(footer(picker), Style::new().fg(theme.gray)));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if picker.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                empty_note(picker),
                Style::new().fg(theme.gray).add_modifier(Modifier::ITALIC),
            ))),
            inner,
        );
        return;
    }

    // The window follows the selection without a stored offset: the list is
    // short and rebuilt on every keystroke, so there is nothing to remember.
    let height = usize::from(inner.height);
    let selected = picker.list.selected;
    let offset = selected.saturating_sub(height.saturating_sub(1));
    for (index, row) in picker.rows.iter().enumerate().skip(offset).take(height) {
        let Some(entry) = picker.entries.get(row.index) else {
            continue;
        };
        let y = inner.y + u16::try_from(index - offset).unwrap_or(0);
        let line = Rect {
            y,
            height: 1,
            ..inner
        };
        if index == selected {
            frame.render_widget(
                Block::new().style(Style::new().bg(theme.bg_highlight)),
                line,
            );
        }
        frame.render_widget(
            Paragraph::new(row_line(
                theme,
                entry,
                &row.hits,
                index == selected,
                inner.width,
            )),
            line,
        );
    }
}

/// One file as a row: the marker, then the path relative to its root with the
/// matched characters picked out.
#[must_use]
pub fn row_line(
    theme: &Theme,
    entry: &Entry,
    hits: &[(usize, usize)],
    selected: bool,
    columns: u16,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if selected { "▸ " } else { "  " },
        Style::new().fg(theme.text_secondary),
    )];
    // A directory is a step on the way somewhere, so it is the dimmer of the
    // two; a file is what the picker is for.
    let base = Style::new().fg(if entry.is_dir {
        theme.text_secondary
    } else {
        theme.text_primary
    });
    let hit = Style::new().fg(theme.fuzzy).add_modifier(Modifier::BOLD);
    spans.extend(highlight(&entry.label, hits, base, hit));
    Line::from(fit(spans, usize::from(columns).saturating_sub(2)))
}

/// Cut `spans` to `columns`, so a long path never runs past the border.
fn fit(spans: Vec<Span<'static>>, columns: usize) -> Vec<Span<'static>> {
    let mut used = 0usize;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if used >= columns {
            break;
        }
        let span_width = width(&span.content);
        if used + span_width <= columns {
            used += span_width;
            out.push(span);
        } else {
            let cut = truncate(&span.content, columns - used);
            out.push(Span::styled(cut, span.style));
            break;
        }
    }
    out
}

/// `12 files · Enter attach · / open · Esc keeps the @`.
#[must_use]
pub fn footer(picker: &FilePicker) -> String {
    let n = picker.rows.len();
    let count = if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    };
    let opens = if picker.selected().is_some_and(|entry| entry.is_dir) {
        " · / open"
    } else {
        ""
    };
    format!(" {count} · Enter attach{opens} · Esc keeps the @ ")
}

/// Why the list is empty, which is different before and after a query.
fn empty_note(picker: &FilePicker) -> String {
    if picker.entries.is_empty() {
        "nothing to list here".to_string()
    } else {
        "no files match".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(label: &str, is_dir: bool) -> Entry {
        Entry {
            path: PathBuf::from("/nowhere").join(label),
            label: label.to_string(),
            is_dir,
            modified: None,
        }
    }

    #[test]
    fn a_row_never_runs_past_the_border() {
        let theme = Theme::default();
        let long = entry(&format!("Downloads/{}.png", "x".repeat(80)), false);
        let line = row_line(&theme, &long, &[(10, 14)], true, 40);
        let drawn: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(width(&drawn) <= 40, "{}", width(&drawn));
    }

    #[test]
    fn the_marker_says_which_row_is_highlighted() {
        let theme = Theme::default();
        let file = entry("Desktop/notes.txt", false);
        let on = row_line(&theme, &file, &[], true, 40);
        let off = row_line(&theme, &file, &[], false, 40);
        assert_eq!(on.spans[0].content, "▸ ");
        assert_eq!(off.spans[0].content, "  ");
    }
}
