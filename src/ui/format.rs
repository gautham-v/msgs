//! Turning database rows into the short strings the panes draw.
//!
//! Everything here is a pure function of its arguments — the clock is passed
//! in — so the chat list's timestamps and preview lines can be tested without a
//! terminal and without a database.

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
    fn a_wide_character_is_dropped_rather_than_half_drawn() {
        // The wave is two cells wide, so the string is eight cells, not seven.
        assert_eq!(width("héllo 🌊"), 8);
        assert_eq!(truncate("héllo 🌊", 8), "héllo 🌊");
        assert_eq!(truncate("héllo 🌊", 7), "héllo …");
        assert_eq!(truncate("héllo 🌊", 3), "hé…");
        assert!(width(&truncate("🌊🌊🌊", 5)) <= 5);
    }
}
