//! Turning database rows into the short strings the panes draw.
//!
//! Everything here is a pure function of its arguments — the clock is passed
//! in — so the chat list's timestamps and preview lines can be tested without a
//! terminal and without a database.

use std::borrow::Cow;

use chrono::{DateTime, Datelike, Local};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::db::handle;
use crate::db::{Chat, GroupAction, Handle, Preview};

/// A timestamp as the chat list writes it: `2m`, `1h`, `Tue`, `Aug 12`.
///
/// The scale coarsens with age — minutes for the last hour, hours for the last
/// day, weekday names for the last week, then a date — so a column six
/// characters wide can carry any age of conversation.
#[must_use]
pub fn relative_time(now: DateTime<Local>, when: DateTime<Local>) -> String {
    let delta = now.signed_duration_since(when);
    let minutes = delta.num_minutes();
    // A clock skew, or a message that arrived while we were drawing, reads as
    // "now" rather than as a negative age.
    if minutes < 1 {
        return "now".to_string();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = delta.num_hours();
    if hours < 24 {
        return format!("{hours}h");
    }
    if delta.num_days() < 7 {
        return when.format("%a").to_string();
    }
    if when.year() == now.year() {
        return when.format("%b %-d").to_string();
    }
    when.format("%-m/%-d/%y").to_string()
}

/// How long ago something happened, for the status line: `just now`, `12s
/// ago`, `4m ago`, `2h ago`.
///
/// Coarser than [`relative_time`] on purpose: the status line is saying how
/// fresh what is on screen is, not when a message was sent.
#[must_use]
pub fn age(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 5 {
        return "just now".to_string();
    }
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// The label on a day separator: `Today`, `Yesterday`, `Tuesday`, `August 12`.
///
/// The scale coarsens the same way [`relative_time`] does, but written out in
/// full, because a separator has a whole row to itself.
#[must_use]
pub fn day_label(now: DateTime<Local>, when: DateTime<Local>) -> String {
    let today = now.date_naive();
    let day = when.date_naive();
    if day == today {
        return "Today".to_string();
    }
    if today.pred_opt() == Some(day) {
        return "Yesterday".to_string();
    }
    let age = (today - day).num_days();
    if (0..7).contains(&age) {
        return when.format("%A").to_string();
    }
    if day.year() == today.year() {
        return when.format("%B %-d").to_string();
    }
    when.format("%B %-d, %Y").to_string()
}

/// The wall-clock time on a meta line, `18:02`.
#[must_use]
pub fn clock(when: DateTime<Local>) -> String {
    when.format("%H:%M").to_string()
}

/// A count with thousands separators, as the header writes it: `1,204`.
#[must_use]
pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A file size for a chip: `840 B`, `84 KB`, `2.1 MB`.
#[must_use]
pub fn bytes(size: i64) -> String {
    const KB: f64 = 1024.0;
    let size = size.max(0) as f64;
    if size < KB {
        return format!("{size:.0} B");
    }
    for (limit, suffix) in [(KB * KB, "KB"), (KB * KB * KB, "MB")] {
        if size < limit {
            let scaled = size / (limit / KB);
            let places = usize::from(scaled < 10.0);
            return format!("{scaled:.places$} {suffix}");
        }
    }
    format!("{:.1} GB", size / (KB * KB * KB))
}

/// The unread badge, capped so a runaway group cannot widen the column.
#[must_use]
pub fn unread_badge(count: i64) -> String {
    match count {
        n if n <= 0 => String::new(),
        n if n > 99 => "99+".to_string(),
        n => n.to_string(),
    }
}

/// The preview line of a chat, split into the `You: ` / `Name: ` prefix and the
/// body, so the two can be drawn in different colors.
///
/// The prefix is empty in a one-to-one chat when the other person spoke: the
/// name is already on the row above it.
#[must_use]
pub fn preview_line(chat: &Chat) -> (String, String) {
    let Some(preview) = chat.preview.as_ref() else {
        return (String::new(), String::new());
    };
    (preview_prefix(chat, preview), preview_body(preview))
}

/// `You: ` for your own messages, `Name: ` for anybody else in a group.
fn preview_prefix(chat: &Chat, preview: &Preview) -> String {
    if preview.is_from_me {
        return "You: ".to_string();
    }
    if !chat.is_group {
        return String::new();
    }
    // The participant list is the first place to look, because it is what a
    // later contact pass will resolve; a sender who somehow is not in it still
    // gets a name out of their own handle.
    let name = preview
        .sender_rowid
        .and_then(|rowid| {
            chat.participants
                .iter()
                .find(|handle| handle.rowid == rowid)
                .map(Handle::short_name)
        })
        .or_else(|| preview.sender.as_deref().map(handle::short_name));
    match name {
        Some(name) if !name.is_empty() => format!("{name}: "),
        _ => String::new(),
    }
}

/// What was said, or what was sent when nothing was said.
fn preview_body(preview: &Preview) -> String {
    if let Some(action) = preview.group_action.as_ref() {
        return announcement(action);
    }
    if let Some(text) = preview.text.as_deref() {
        return single_line(text);
    }
    if let Some(kind) = preview.attachment_kind {
        let name = preview
            .attachment_name
            .as_deref()
            .filter(|name| !name.is_empty());
        return match (name, preview.attachments) {
            (_, count) if count > 1 => format!("{} {count} attachments", kind.glyph()),
            (Some(name), _) => format!("{} {name}", kind.glyph()),
            (None, _) => format!("{} {}", kind.glyph(), kind.label()),
        };
    }
    String::new()
}

/// A group event as one line, with no names in it — the row has no room to
/// resolve a second person, and the conversation pane says it properly.
fn announcement(action: &GroupAction) -> String {
    match action {
        GroupAction::NameChange(name) if !name.is_empty() => {
            format!("named the conversation “{}”", single_line(name))
        }
        GroupAction::NameChange(_) => "renamed the conversation".to_string(),
        GroupAction::ParticipantAdded(_) => "added someone to the conversation".to_string(),
        GroupAction::ParticipantRemoved(_) => "removed someone from the conversation".to_string(),
        GroupAction::ParticipantLeft => "left the conversation".to_string(),
        GroupAction::IconChanged => "changed the group photo".to_string(),
        GroupAction::IconRemoved => "removed the group photo".to_string(),
        GroupAction::PhoneNumberChanged(_) => "changed their number".to_string(),
    }
}

/// Flatten a body onto one line: newlines and tabs become spaces, runs of
/// whitespace collapse.
#[must_use]
pub fn single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// How many terminal cells `text` occupies.
///
/// An emoji is two cells wide and a combining mark is none, so counting
/// characters would misplace everything drawn to the right of one.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Cut `text` to `columns` terminal cells, ending in `…` when anything was cut.
#[must_use]
pub fn truncate(text: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if width(text) <= columns {
        return text.to_string();
    }
    // One cell goes to the ellipsis, and a wide character that would straddle
    // the edge is dropped rather than half-drawn.
    let budget = columns - 1;
    let mut cut = String::new();
    let mut used = 0;
    for c in text.chars() {
        let cell = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cell > budget {
            break;
        }
        cut.push(c);
        used += cell;
    }
    cut.push('…');
    cut
}

/// Wrap `text` to `rest` cells a line, giving the first line `first` cells.
///
/// Newlines in the body are kept as hard breaks, words are broken only when a
/// single word is wider than the column, and the result always holds at least
/// one line so an empty body still occupies a row.
///
/// The shorter first line is what lets a group message carry the sender's name
/// in front of its opening words without the rest of the paragraph indenting.
#[must_use]
pub fn wrap(text: &str, first: usize, rest: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let start = if out.is_empty() { first } else { rest };
        wrap_paragraph(paragraph, start.max(1), rest.max(1), &mut out);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Wrap one newline-free run, appending its lines to `out`.
fn wrap_paragraph(text: &str, first: usize, rest: usize, out: &mut Vec<String>) {
    let before = out.len();
    let mut budget = first;
    let mut line = String::new();
    let mut used = 0usize;

    for word in text.split_whitespace() {
        let cells = width(word);
        if used > 0 && used + 1 + cells > budget {
            out.push(std::mem::take(&mut line));
            used = 0;
            budget = rest;
        }
        if cells > budget {
            // A single word wider than the column is cut across rows rather
            // than pushed off the edge.
            if used > 0 {
                out.push(std::mem::take(&mut line));
                budget = rest;
            }
            for chunk in split_cells(word, budget) {
                out.push(chunk);
                budget = rest;
            }
            // The last chunk becomes the line still being filled.
            line = out.pop().unwrap_or_default();
            used = width(&line);
            continue;
        }
        if used > 0 {
            line.push(' ');
            used += 1;
        }
        line.push_str(word);
        used += cells;
    }

    if !line.is_empty() || out.len() == before {
        out.push(line);
    }
}

/// Cut `word` into pieces at most `cells` terminal cells wide.
fn split_cells(word: &str, cells: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut used = 0;
    for c in word.chars() {
        let cell = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cell > cells && !chunk.is_empty() {
            chunks.push(std::mem::take(&mut chunk));
            used = 0;
        }
        chunk.push(c);
        used += cell;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// Byte ranges of the links in `text`.
///
/// Deliberately narrow: `http://`, `https://`, and a bare `www.` host. Trailing
/// punctuation is left out so a link at the end of a sentence still opens.
#[must_use]
pub fn find_links(text: &str) -> Vec<(usize, usize)> {
    const SCHEMES: [&str; 3] = ["https://", "http://", "www."];
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (index, _) in text.char_indices() {
        if found.last().is_some_and(|(_, end)| index < *end) {
            continue;
        }
        // A scheme only starts a link at a word boundary.
        if index > 0 && !text[..index].ends_with(char::is_whitespace) {
            continue;
        }
        let rest = &text[index..];
        if !SCHEMES
            .iter()
            .any(|scheme| starts_with_ignore_case(rest, scheme))
        {
            continue;
        }
        let mut end = index + rest.find(char::is_whitespace).unwrap_or(rest.len());
        while end > index {
            let tail = text[index..end].chars().next_back().unwrap_or(' ');
            if matches!(
                tail,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
            ) {
                end -= tail.len_utf8();
            } else {
                break;
            }
        }
        // `www.` alone is not a link; there has to be something after the dot.
        if text[index..end].len() > "www.".len() {
            found.push((index, end));
        }
    }
    found
}

/// Whether `haystack` opens with `needle`, ignoring ASCII case.
///
/// Compared as bytes: `needle` is always ASCII, and slicing the string itself
/// would panic the moment a message opens with a multi-byte character.
fn starts_with_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

/// The link a message opens with `Ctrl+L`: the first one in its body.
#[must_use]
pub fn first_link(text: &str) -> Option<String> {
    let (start, end) = find_links(text).first().copied()?;
    let raw = &text[start..end];
    Some(if starts_with_ignore_case(raw, "www.") {
        format!("https://{raw}")
    } else {
        raw.to_string()
    })
}

/// Fit `(keys, label)` hint pairs into `columns` terminal cells.
///
/// The bar is built at full length and then degraded in one fixed order, so
/// the right edge never eats half a word: pairs are dropped from the
/// middle-right inward with the first and last pinned, then labels go away
/// entirely, and only a last pair whose keys alone overflow is truncated.
/// A label is always present in full or not at all.
#[must_use]
pub fn fit_hints<'a>(
    hints: &[(&'a str, &'a str)],
    columns: usize,
) -> Vec<(Cow<'a, str>, Option<&'a str>)> {
    if hints.is_empty() || columns == 0 {
        return Vec::new();
    }
    // One cell of the line is the leading pad every renderer draws.
    let budget = columns.saturating_sub(1);

    // The candidate sets, widest first: everything, then the first `k` pairs
    // plus the pinned last one.
    let n = hints.len();
    let floor = n.min(2);
    for labelled in [true, false] {
        for k in (floor..=n).rev() {
            let picked: Vec<usize> = if k == n {
                (0..n).collect()
            } else {
                let mut v: Vec<usize> = (0..k - 1).collect();
                v.push(n - 1);
                v
            };
            let cost: usize = picked
                .iter()
                .map(|&i| {
                    if labelled {
                        width(hints[i].0) + 1 + width(hints[i].1)
                    } else {
                        width(hints[i].0)
                    }
                })
                .sum::<usize>()
                + 3 * picked.len().saturating_sub(1);
            if cost <= budget {
                return picked
                    .into_iter()
                    .map(|i| {
                        (
                            Cow::Borrowed(hints[i].0),
                            if labelled { Some(hints[i].1) } else { None },
                        )
                    })
                    .collect();
            }
        }
    }

    // Not even both pinned pairs' keys fit, so only the last one is left, and
    // this is the one place a hint is ever cut.
    let keys = hints[n - 1].0;
    if width(keys) <= budget {
        return vec![(Cow::Borrowed(keys), None)];
    }
    vec![(Cow::Owned(truncate(keys, budget)), None)]
}

/// How wide the fitted hints render, pad included.
#[must_use]
pub fn hints_width(fitted: &[(Cow<'_, str>, Option<&str>)]) -> usize {
    if fitted.is_empty() {
        return 0;
    }
    1 + fitted
        .iter()
        .map(|(keys, label)| width(keys) + label.map_or(0, |l| 1 + width(l)))
        .sum::<usize>()
        + 3 * (fitted.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("an unambiguous local time")
    }

    #[test]
    fn timestamps_coarsen_as_they_age() {
        let now = at(2025, 8, 27, 18, 30);
        assert_eq!(relative_time(now, now), "now");
        assert_eq!(relative_time(now, at(2025, 8, 27, 18, 28)), "2m");
        assert_eq!(relative_time(now, at(2025, 8, 27, 17, 0)), "1h");
        // 2025-08-26 was a Tuesday, a day and a half before `now`.
        assert_eq!(relative_time(now, at(2025, 8, 26, 9, 0)), "Tue");
        assert_eq!(relative_time(now, at(2025, 8, 12, 9, 0)), "Aug 12");
        assert_eq!(relative_time(now, at(2024, 8, 12, 9, 0)), "8/12/24");
    }

    #[test]
    fn a_message_from_the_future_still_reads_as_now() {
        let now = at(2025, 8, 27, 18, 30);
        assert_eq!(relative_time(now, at(2025, 8, 27, 18, 35)), "now");
    }

    #[test]
    fn the_unread_badge_is_capped() {
        assert_eq!(unread_badge(0), "");
        assert_eq!(unread_badge(-1), "");
        assert_eq!(unread_badge(7), "7");
        assert_eq!(unread_badge(120), "99+");
    }

    #[test]
    fn bodies_collapse_onto_one_line() {
        assert_eq!(single_line("two\nlines"), "two lines");
        assert_eq!(single_line("  padded \t out  "), "padded out");
        assert_eq!(single_line(""), "");
    }

    #[test]
    fn truncation_counts_cells_and_marks_the_cut() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("hello", 0), "");
    }

    #[test]
    fn day_labels_name_the_near_days_and_date_the_far_ones() {
        let now = at(2025, 8, 27, 18, 30);
        assert_eq!(day_label(now, at(2025, 8, 27, 0, 1)), "Today");
        assert_eq!(day_label(now, at(2025, 8, 26, 23, 59)), "Yesterday");
        // 2025-08-22 was a Friday, five days back.
        assert_eq!(day_label(now, at(2025, 8, 22, 9, 0)), "Friday");
        assert_eq!(day_label(now, at(2025, 8, 12, 9, 0)), "August 12");
        assert_eq!(day_label(now, at(2024, 8, 12, 9, 0)), "August 12, 2024");
    }

    #[test]
    fn the_clock_is_twenty_four_hour() {
        assert_eq!(clock(at(2025, 8, 27, 18, 2)), "18:02");
        assert_eq!(clock(at(2025, 8, 27, 6, 5)), "06:05");
    }

    #[test]
    fn counts_get_separators_and_sizes_get_units() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(-1204), "-1,204");

        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(840), "840 B");
        assert_eq!(bytes(86_016), "84 KB");
        assert_eq!(bytes(2_202_010), "2.1 MB");
    }

    #[test]
    fn wrapping_breaks_on_spaces_and_keeps_hard_newlines() {
        assert_eq!(wrap("one two three", 20, 20), vec!["one two three"]);
        assert_eq!(
            wrap("one two three four", 8, 8),
            vec!["one two", "three", "four"]
        );
        assert_eq!(
            wrap(
                "a
b", 10, 10
            ),
            vec!["a", "b"]
        );
        assert_eq!(wrap("", 10, 10), vec![""]);
        assert_eq!(wrap("   ", 10, 10), vec![""]);
    }

    #[test]
    fn the_first_line_can_be_shorter_than_the_rest() {
        // A sender name eats into the first line only.
        assert_eq!(wrap("aaa bbb ccc", 4, 8), vec!["aaa", "bbb ccc"]);
    }

    #[test]
    fn a_word_wider_than_the_column_is_cut_rather_than_lost() {
        let lines = wrap("abcdefghij", 4, 4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
        for line in &lines {
            assert!(width(line) <= 4);
        }
    }

    #[test]
    fn wrapping_never_overflows_the_column_it_was_given() {
        let text = "a short line, a rather longer one with 🌊 in it, and https://example.invalid/x";
        for columns in 4..40 {
            for line in wrap(text, columns, columns) {
                assert!(width(&line) <= columns, "{columns}: {line:?}");
            }
        }
    }

    #[test]
    fn links_are_found_without_their_trailing_punctuation() {
        let text = "see https://example.invalid/menu, and www.example.invalid too.";
        let found = find_links(text);
        assert_eq!(found.len(), 2);
        assert_eq!(
            &text[found[0].0..found[0].1],
            "https://example.invalid/menu"
        );
        assert_eq!(&text[found[1].0..found[1].1], "www.example.invalid");
    }

    #[test]
    fn scanning_for_links_never_splits_a_multi_byte_character() {
        // Every one of these opens or ends on a character wider than a byte.
        for text in ["’", "🌊 https://example.invalid/x", "don’t", "é", "😀😀"] {
            let _ = find_links(text);
        }
        assert_eq!(find_links("’https://example.invalid").len(), 0);
        assert_eq!(find_links("🌊 https://example.invalid/x").len(), 1);
    }

    #[test]
    fn a_bare_word_is_not_a_link_and_a_scheme_mid_word_is_not_either() {
        assert!(find_links("nothing to see").is_empty());
        assert!(find_links("xhttps://example.invalid").is_empty());
        assert!(find_links("www.").is_empty());
    }

    #[test]
    fn the_first_link_gets_a_scheme_when_the_text_left_it_out() {
        assert_eq!(
            first_link("go to www.example.invalid/x now"),
            Some("https://www.example.invalid/x".to_string())
        );
        assert_eq!(
            first_link("http://example.invalid one https://other.invalid two"),
            Some("http://example.invalid".to_string())
        );
        assert_eq!(first_link("no links"), None);
    }

    #[test]
    fn a_wide_character_is_dropped_rather_than_half_drawn() {
        // The wave is two cells wide, so the string is eight cells, not seven.
        assert_eq!(width("héllo 🌊"), 8);
        assert_eq!(truncate("héllo 🌊", 8), "héllo 🌊");
        assert_eq!(truncate("héllo 🌊", 7), "héllo …");
        assert_eq!(truncate("héllo 🌊", 3), "hé…");
        assert!(width(&truncate("🌊🌊🌊", 5)) <= 5);
    }

    #[test]
    fn fit_hints_keeps_everything_when_there_is_room() {
        let hints = crate::keymap::SHORTCUT_BAR;
        let fitted = fit_hints(hints, 120);
        assert_eq!(fitted.len(), hints.len());
        assert!(fitted.iter().all(|(_, label)| label.is_some()));
        assert!(hints_width(&fitted) <= 120);
    }

    #[test]
    fn fit_hints_pins_the_help_entry() {
        let hints = crate::keymap::SHORTCUT_BAR;
        for columns in [40usize, 20, 12, 8, 3] {
            let fitted = fit_hints(hints, columns);
            assert!(!fitted.is_empty(), "empty at {columns}");
            assert!(hints_width(&fitted) <= columns, "too wide at {columns}");
            assert_eq!(fitted.last().unwrap().0, "?", "lost help at {columns}");
        }
    }

    #[test]
    fn fit_hints_drops_labels_before_the_pinned_pair() {
        let fitted = fit_hints(crate::keymap::SHORTCUT_BAR, 12);
        assert!(fitted.iter().all(|(_, label)| label.is_none()));
        assert_eq!(fitted.first().unwrap().0, "Tab");
        assert_eq!(fitted.last().unwrap().0, "?");
    }

    #[test]
    fn fit_hints_never_renders_half_a_label() {
        let hints = crate::keymap::SHORTCUT_BAR;
        for columns in 0..120usize {
            let fitted = fit_hints(hints, columns);
            assert!(hints_width(&fitted) <= columns, "wide at {columns}");
            for (keys, label) in &fitted {
                if let Some(label) = label {
                    assert!(hints.iter().any(|(k, l)| k == keys && l == label));
                }
            }
        }
    }
}
