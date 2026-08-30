//! The send box: a rounded hairline box with the `❯` prompt and the draft, a
//! column in from either edge of the pane. A file dropped from Finder or
//! queued by the `@` picker waits above the draft as a `📎 name` chip
//! until `Enter` sends it.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{App, Focus};

/// Draw the composer, and place the terminal cursor in it when it has focus.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    // One rectangle for the draft, so a click lands on the character the
    // cursor was drawn under.
    let Some(inner) = text_area(area) else {
        return;
    };
    let theme = &app.theme;
    // The `@` picker takes the keys but not the draft: the cursor stays here
    // while it is open, because that is where what is typed is going.
    let focused = matches!(app.focus, Focus::Composer | Focus::FilePicker);
    let attaching = app.attach_prompt.as_ref();
    let field = attaching.unwrap_or(&app.composer);

    // A column of air either side, the way the mockup insets the box. The
    // border is gray, and a step brighter while the box has focus: that is
    // the whole focus signal.
    let boxed = Rect {
        x: area.x + 1,
        width: area.width - 2,
        ..area
    };
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border_for(focused)))
        .style(Style::new().bg(theme.bg_base));
    // The path prompt names itself on the border, so the box is never
    // mistaken for the draft it is standing in front of.
    if attaching.is_some() {
        block = block.title_top(Line::from(Span::styled(
            " attach ",
            Style::new().fg(theme.text_secondary),
        )));
    }
    debug_assert_eq!(inner, block.inner(boxed));
    frame.render_widget(block, boxed);

    let text = field.text();
    let rows = rows(text, columns(area.width));
    let (text_row, cursor_column) = cursor_cell(text, &rows, field.cursor());
    // The dropped files sit above the draft, so the cursor is that many rows
    // further down than the text alone would put it.
    let cursor_row = text_row + app.attached.len();
    // Keep the cursor line visible once the draft is taller than the box.
    let scroll = cursor_row.saturating_sub(usize::from(inner.height).saturating_sub(1));

    let prompt = Style::new().fg(if focused {
        theme.text_secondary
    } else {
        theme.gray
    });
    // One chip per dropped file, in the order they were dropped: a name, not
    // a path, in the same gray a label is drawn in.
    let mut lines: Vec<Line> = app
        .attached
        .iter()
        .map(|path| {
            let name = path
                .file_name()
                .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy());
            let room = usize::from(inner.width).saturating_sub(usize::from(PROMPT_WIDTH) + 2);
            Line::from(vec![
                Span::styled(" 📎 ", Style::new().fg(theme.gray)),
                Span::styled(
                    super::format::truncate(&name, room.max(4)),
                    Style::new().fg(theme.text_secondary),
                ),
            ])
        })
        .collect();
    if text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(PROMPT, prompt),
            Span::styled(
                placeholder(app, attaching.is_some(), inner.width),
                Style::new().fg(theme.gray),
            ),
        ]));
    } else {
        for (index, row) in rows.iter().enumerate() {
            let marker = if index == 0 { PROMPT } else { "   " };
            lines.push(Line::from(vec![
                Span::styled(marker, prompt),
                Span::styled(
                    text[row.start..row.end].to_string(),
                    Style::new().fg(theme.text_primary),
                ),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        inner,
    );

    if focused {
        let x = inner
            .x
            .saturating_add(PROMPT_WIDTH)
            .saturating_add(u16::try_from(cursor_column).unwrap_or(u16::MAX))
            .min(inner.x + inner.width.saturating_sub(1));
        let y = inner
            .y
            .saturating_add(u16::try_from(cursor_row - scroll).unwrap_or(0))
            .min(inner.y + inner.height.saturating_sub(1));
        frame.set_cursor_position((x, y));
    }
}

/// The `❯ ` marker in front of the first line of the draft, a cell in from
/// the border. Continuation lines are set in past it, so one width serves
/// every row.
const PROMPT: &str = " ❯ ";
const PROMPT_WIDTH: u16 = 3;

/// The rows the draft itself is drawn on: a column of air either side of the
/// pane, then the border. `None` when the pane is too small to draw at all,
/// which is the same test [`render`] leaves on.
///
/// One function, so a click lands where the cursor was drawn.
#[must_use]
pub fn text_area(area: Rect) -> Option<Rect> {
    if area.width < 5 || area.height < 3 {
        return None;
    }
    Some(Rect::new(
        area.x + 2,
        area.y + 1,
        area.width - 4,
        area.height - 2,
    ))
}

/// Where a click at `position` puts the text cursor: a byte offset into
/// `text`, or `None` when the click missed the drawn draft.
///
/// `cursor` is where the cursor is now, because that is what decides how far
/// the box has scrolled. Past the end of what is written — a click in the
/// blank right of the last character, or on a row below the draft — the
/// answer is the end of the draft, where `End` goes. `chips` is how many
/// attachment rows stand above the draft; a click on one of them is the
/// start of the draft.
#[must_use]
pub fn offset_at(
    text: &str,
    cursor: usize,
    chips: usize,
    area: Rect,
    position: Position,
) -> Option<usize> {
    let inner = text_area(area)?;
    if !inner.contains(position) {
        return None;
    }
    let rows = rows(text, columns(area.width));
    let (cursor_row, _) = cursor_cell(text, &rows, cursor);
    let scroll = (cursor_row + chips).saturating_sub(usize::from(inner.height).saturating_sub(1));
    let Some(row) = (usize::from(position.y - inner.y) + scroll).checked_sub(chips) else {
        return Some(0);
    };

    let Some(row) = rows.get(row) else {
        return Some(text.len());
    };
    // Every row is set in past the prompt column, so a click left of it is
    // the start of that row.
    let text_x = inner.x + PROMPT_WIDTH;
    if position.x < text_x {
        return Some(row.start);
    }
    let column = usize::from(position.x - text_x);
    // Walk the row's cells, so a click on the right half of an emoji is still
    // that emoji; past the last character is the end of the row.
    let mut used = 0;
    for (index, c) in text[row.start..row.end].char_indices() {
        let cells = super::format::width(c.encode_utf8(&mut [0; 4]));
        if used + cells > column {
            return Some(row.start + index);
        }
        used += cells;
    }
    Some(row.end)
}

/// One drawn row of the draft: the bytes of `text` on it. The newline or
/// the space a break landed on belongs to no row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

/// Cells across the box a row of the draft has, in a pane `pane_width` wide:
/// the air and the border either side, then the prompt column.
#[must_use]
pub fn columns(pane_width: u16) -> usize {
    usize::from(pane_width.saturating_sub(4).saturating_sub(PROMPT_WIDTH)).max(1)
}

/// Lay the draft out in `columns` cells: a hard break at every newline, and
/// a soft one wherever a line runs past the box — at the last space, or
/// through a word that is itself wider than the box. The space a soft
/// break lands on is drawn on neither row.
///
/// Always at least one row, so an empty draft still stands a line tall. The
/// height of the box, the drawing, the cursor, and a click all come from
/// this one answer.
#[must_use]
pub fn rows(text: &str, columns: usize) -> Vec<Row> {
    let columns = columns.max(1);
    let mut out = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        wrap_line(line, line_start, columns, &mut out);
        line_start += line.len() + 1;
    }
    out
}

fn wrap_line(line: &str, base: usize, columns: usize, out: &mut Vec<Row>) {
    let mut row_start = 0;
    let mut used = 0;
    let mut last_space = None;
    for (index, c) in line.char_indices() {
        let cells = super::format::width(c.encode_utf8(&mut [0; 4]));
        if used + cells > columns {
            if c == ' ' {
                // A space landing on the edge is the break itself.
                out.push(Row {
                    start: base + row_start,
                    end: base + index,
                });
                row_start = index + c.len_utf8();
                used = 0;
                last_space = None;
                continue;
            }
            let (end, next) = match last_space {
                Some(space) => (space, space + 1),
                None => (index, index),
            };
            out.push(Row {
                start: base + row_start,
                end: base + end,
            });
            row_start = next;
            used = super::format::width(&line[row_start..index]);
            last_space = None;
        }
        if c == ' ' {
            last_space = Some(index);
        }
        used += cells;
    }
    out.push(Row {
        start: base + row_start,
        end: base + line.len(),
    });
}

/// The dim line shown in an empty box: `Message Priya`, or what the path
/// prompt is waiting for.
fn placeholder(app: &App, attaching: bool, width: u16) -> String {
    if attaching {
        return "Path to a file — ~ works".to_string();
    }
    let Some(chat) = app.current_chat() else {
        return "Message".to_string();
    };
    // The name is content, so it is truncated to the box rather than allowed
    // to push the layout around.
    let room = usize::from(width.saturating_sub(PROMPT_WIDTH + 10)).max(8);
    format!("Message {}", super::format::truncate(&chat.title(), room))
}

/// Translate a byte offset into `(row, column)` in cells over `rows`. A
/// cursor on the space a soft break consumed, or at the end of a full row,
/// sits one cell past the row's last character.
fn cursor_cell(text: &str, rows: &[Row], cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    let index = rows
        .iter()
        .rposition(|row| row.start <= cursor)
        .unwrap_or(0);
    let row = rows[index];
    let column = super::format::width(&text[row.start..cursor.min(row.end).max(row.start)]);
    (index, column)
}

/// `key label   key label` with the keys picked out, fitted to `columns`.
/// `sep` goes between pairs and is always three cells wide, which is what
/// the fitting counts on.
pub(crate) fn hint_line<'a>(
    app: &App,
    hints: &[(&'a str, &'a str)],
    sep: &'static str,
    columns: u16,
) -> Line<'a> {
    let theme = &app.theme;
    let fitted = super::format::fit_hints(hints, usize::from(columns));
    let mut spans = vec![Span::raw(" ")];
    for (index, (keys, label)) in fitted.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(sep, Style::new().fg(theme.gray_dim)));
        }
        spans.push(Span::styled(
            keys.into_owned(),
            Style::new()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(label) = label.filter(|label| !label.is_empty()) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(label, Style::new().fg(theme.gray)));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::{PROMPT_WIDTH, Row, columns, offset_at, rows, text_area};
    use ratatui::layout::{Position, Rect};

    fn cursor_cell(text: &str, cursor: usize) -> (usize, usize) {
        super::cursor_cell(text, &rows(text, 40), cursor)
    }

    fn spans(text: &str, columns: usize) -> Vec<&str> {
        rows(text, columns)
            .iter()
            .map(|row| &text[row.start..row.end])
            .collect()
    }

    #[test]
    fn cursor_cell_counts_lines_and_characters() {
        assert_eq!(cursor_cell("", 0), (0, 0));
        assert_eq!(cursor_cell("hello", 5), (0, 5));
        assert_eq!(cursor_cell("one\ntwo", 7), (1, 3));
        assert_eq!(cursor_cell("a\nb\nc", 2), (1, 0));
    }

    #[test]
    fn cursor_cell_counts_cells_not_bytes() {
        let text = "héllo";
        assert_eq!(cursor_cell(text, text.len()), (0, 5));
        let wide = "🌊x";
        assert_eq!(cursor_cell(wide, wide.len()), (0, 3));
    }

    #[test]
    fn a_long_line_wraps_at_the_last_space() {
        assert_eq!(spans("one two three", 8), vec!["one two", "three"]);
        assert_eq!(spans("one two three", 7), vec!["one two", "three"]);
        assert_eq!(spans("one two three", 6), vec!["one", "two", "three"]);
        // Newlines are still hard breaks, and an empty draft is one row.
        assert_eq!(spans("a\nb", 40), vec!["a", "b"]);
        assert_eq!(spans("", 40), vec![""]);
        assert_eq!(spans("a\n", 40), vec!["a", ""]);
    }

    #[test]
    fn a_word_wider_than_the_box_is_cut_through() {
        assert_eq!(spans("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(spans("ab cdefghij", 4), vec!["ab", "cdef", "ghij"]);
        // An emoji is two cells and is never split.
        assert_eq!(spans("🌊🌊🌊", 5), vec!["🌊🌊", "🌊"]);
    }

    #[test]
    fn the_cursor_follows_the_wrap() {
        let text = "one two three";
        let at = |cursor| super::cursor_cell(text, &rows(text, 8), cursor);
        assert_eq!(at(3), (0, 3));
        // On the consumed space: one past the end of the first row.
        assert_eq!(at(7), (0, 7));
        assert_eq!(at(8), (1, 0));
        assert_eq!(at(text.len()), (1, 5));
    }

    #[test]
    fn columns_come_off_the_pane_width() {
        let pane = Rect::new(0, 0, 50, 3);
        let inner = text_area(pane).expect("room for the box");
        assert_eq!(columns(pane.width), usize::from(inner.width - PROMPT_WIDTH));
        assert_eq!(columns(0), 1);
        assert_eq!(rows("", 0), vec![Row { start: 0, end: 0 }]);
    }

    #[test]
    fn a_click_on_a_soft_wrapped_row_lands_on_its_own_characters() {
        // 50 wide is 43 columns; a run past that wraps.
        let pane = Rect::new(0, 0, 50, 4);
        let inner = text_area(pane).expect("room for the box");
        let text_x = inner.x + PROMPT_WIDTH;
        let text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbb cccc";
        assert_eq!(spans(text, 43).len(), 2);
        let at = |x, y| offset_at(text, 0, 0, pane, Position::new(x, y));
        assert_eq!(at(text_x + 1, inner.y + 1), Some(42));
        assert_eq!(at(text_x + 30, inner.y + 1), Some(text.len()));
        // Past the end of the first row is that row's end, not the draft's.
        assert_eq!(at(text_x + 42, inner.y), Some(40));
    }

    #[test]
    fn a_click_lands_on_the_character_under_it() {
        let pane = Rect::new(30, 20, 50, 3);
        let inner = text_area(pane).expect("room for the box");
        assert_eq!(inner, Rect::new(32, 21, 46, 1));
        let text_x = inner.x + PROMPT_WIDTH;

        let text = "one two";
        let at = |x| offset_at(text, 0, 0, pane, Position::new(x, inner.y));
        assert_eq!(at(text_x), Some(0));
        assert_eq!(at(text_x + 4), Some(4));
        // Left of the prompt is the start of the line; past the last
        // character is the end of the draft.
        assert_eq!(at(inner.x), Some(0));
        assert_eq!(at(text_x + 7), Some(text.len()));
        assert_eq!(at(text_x + 30), Some(text.len()));
        // Outside the box entirely.
        assert_eq!(
            offset_at(text, 0, 0, pane, Position::new(31, inner.y)),
            None
        );
        assert_eq!(offset_at(text, 0, 0, pane, Position::new(text_x, 20)), None);
    }

    #[test]
    fn a_click_on_a_wrapped_draft_counts_the_lines_above_it() {
        let pane = Rect::new(0, 0, 50, 4);
        let inner = text_area(pane).expect("room for the box");
        assert_eq!(inner.height, 2);
        let text = "one\ntwo";
        let cursor = text.len();
        let at = |x, y| offset_at(text, cursor, 0, pane, Position::new(x, y));
        let text_x = inner.x + PROMPT_WIDTH;
        assert_eq!(at(text_x + 1, inner.y), Some(1));
        assert_eq!(at(text_x + 1, inner.y + 1), Some(5));
        // A row with nothing written on it is the end of the draft.
        let short = "one";
        assert_eq!(
            offset_at(short, 0, 0, pane, Position::new(text_x, inner.y + 1)),
            Some(short.len())
        );
    }

    #[test]
    fn a_pane_too_small_for_the_box_has_no_text_area() {
        assert_eq!(text_area(Rect::new(0, 0, 4, 3)), None);
        assert_eq!(text_area(Rect::new(0, 0, 50, 2)), None);
    }

    #[test]
    fn a_click_on_a_scrolled_draft_follows_the_cursor_line() {
        // Four lines in a two-row box: the box is showing the last two.
        let pane = Rect::new(0, 0, 50, 4);
        let text = "one\ntwo\nthree\nfour";
        let inner = text_area(pane).expect("room for the box");
        let text_x = inner.x + PROMPT_WIDTH;
        assert_eq!(
            offset_at(text, text.len(), 0, pane, Position::new(text_x, inner.y)),
            Some(8),
            "the top row of the box is the third line"
        );
    }
}
