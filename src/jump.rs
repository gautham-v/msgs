//! What the `Ctrl+K` palette knows: the filter, the result rows, and the
//! matching that produces them.
//!
//! Two very different searches feed one list. Chats and the people in them are
//! matched fuzzily in memory — a few hundred short strings, so every keystroke
//! can re-rank the whole list — while message bodies come from the FTS5 index
//! in [`crate::search`], which only gets asked once the query is long enough to
//! be worth a query. Both arrive as [`Row`]s carrying the character ranges that
//! matched, so the drawing code highlights without redoing the matching.
//!
//! Nothing here logs: bodies and handles go into [`Row`]s, which only ever
//! reach the screen.

use std::collections::HashMap;

use chrono::{DateTime, Local};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::app::ListPane;
use crate::db::{Chat, local_time};
use crate::search::{self, Hit};
use crate::ui::format::{relative_time, single_line};

/// Most rows the palette keeps, however many the index matched.
pub const MAX_ROWS: usize = 50;

/// Most chats the fuzzy pass keeps, so one letter cannot fill the list with
/// every conversation you have ever had.
const MAX_CHAT_ROWS: usize = 12;

/// Which of the four result sets the palette is showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    /// Chats and messages together.
    #[default]
    All,
    /// Chats and people only.
    Chats,
    /// Message bodies only.
    Messages,
    /// Pictures only, matched on their file names.
    Photos,
}

impl Filter {
    /// The next filter `Tab` moves to.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Chats,
            Self::Chats => Self::Messages,
            Self::Messages => Self::Photos,
            Self::Photos => Self::All,
        }
    }

    /// How the filter reads in the palette footer.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Chats => "chats",
            Self::Messages => "messages",
            Self::Photos => "photos",
        }
    }

    /// Whether chats are part of this filter's results.
    #[must_use]
    pub const fn wants_chats(self) -> bool {
        matches!(self, Self::All | Self::Chats)
    }

    /// Whether the message index is part of this filter's results.
    #[must_use]
    pub const fn wants_messages(self) -> bool {
        !matches!(self, Self::Chats)
    }

    /// The index kind this filter asks for, or `None` for everything.
    #[must_use]
    pub const fn kind(self) -> Option<search::Kind> {
        match self {
            Self::Messages => Some(search::Kind::Message),
            Self::Photos => Some(search::Kind::Photo),
            Self::All | Self::Chats => None,
        }
    }
}

/// What one result row points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A conversation; `Enter` opens it at its newest message.
    Chat,
    /// A message; `Enter` opens its conversation with it selected.
    Message,
    /// A picture, which is a message with a file hanging off it.
    Photo,
}

/// One drawable result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What the row points at.
    pub kind: Kind,
    /// `chat.ROWID` of the conversation this row lives in.
    pub chat_rowid: i64,
    /// `message.ROWID` to select, for a message or a picture.
    pub message_rowid: Option<i64>,
    /// The chat's name, on the left.
    pub label: String,
    /// Character ranges of `label` that matched.
    pub label_hits: Vec<(usize, usize)>,
    /// The matched line, for a message row.
    pub body: String,
    /// Character ranges of `body` that matched.
    pub body_hits: Vec<(usize, usize)>,
    /// The dim text on the right: a date, or what sort of chat this is.
    pub meta: String,
}

/// Everything the palette is showing right now.
pub struct Jump {
    /// Which results are being shown.
    pub filter: Filter,
    /// The rows, chats first and then messages newest first.
    pub rows: Vec<Row>,
    /// Selection and scroll of the result list.
    pub list: ListPane,
    /// How many of [`Jump::rows`] are chats.
    pub chats: usize,
    /// How many of [`Jump::rows`] are messages or pictures.
    pub messages: usize,
    /// The state the rows were built for, so a keystroke rebuilds and a
    /// redraw does not.
    built: Option<Built>,
    /// Reused scratch memory; building one costs about 135KB.
    matcher: Matcher,
}

/// What [`Jump::rows`] was built from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Built {
    query: String,
    filter: Filter,
    /// Whether the message index could answer at the time.
    indexed: bool,
    /// How many chats were in the list, so a new message rebuilds the rows.
    chat_count: usize,
}

impl Default for Jump {
    fn default() -> Self {
        Self {
            filter: Filter::default(),
            rows: Vec::new(),
            list: ListPane::default(),
            chats: 0,
            messages: 0,
            built: None,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }
}

impl std::fmt::Debug for Jump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jump")
            .field("filter", &self.filter)
            .field("rows", &self.rows.len())
            .field("selected", &self.list.selected)
            .finish_non_exhaustive()
    }
}

impl Jump {
    /// Forget the rows, so the next open starts from nothing.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.list = ListPane::default();
        self.chats = 0;
        self.messages = 0;
        self.built = None;
    }

    /// Whether the rows describe something other than this state.
    #[must_use]
    pub fn is_stale(&self, query: &str, filter: Filter, indexed: bool, chat_count: usize) -> bool {
        self.built.as_ref()
            != Some(&Built {
                query: query.to_string(),
                filter,
                indexed,
                chat_count,
            })
    }

    /// The selected row, if the list has one.
    #[must_use]
    pub fn selected(&self) -> Option<&Row> {
        self.rows.get(self.list.selected)
    }

    /// Rebuild the rows for `query`.
    ///
    /// `hits` is what the index answered — empty when the query is too short,
    /// when the filter excludes messages, or when the index is not built yet.
    #[allow(clippy::too_many_arguments)]
    pub fn rebuild(
        &mut self,
        query: &str,
        filter: Filter,
        indexed: bool,
        chats: &[Chat],
        hits: &[Hit],
        now: DateTime<Local>,
        body_columns: usize,
    ) {
        self.filter = filter;
        self.built = Some(Built {
            query: query.to_string(),
            filter,
            indexed,
            chat_count: chats.len(),
        });

        let words = search::tokens(query);
        let mut rows = Vec::new();
        if filter.wants_chats() {
            rows.extend(self.chat_rows(query, chats));
        }
        self.chats = rows.len();
        rows.extend(message_rows(hits, chats, &words, now, body_columns));
        self.messages = rows.len() - self.chats;
        rows.truncate(MAX_ROWS);

        // The selection stays on the row it was on where the row survives,
        // which is what keeps arrowing down while typing from jumping around.
        let anchor = self.selected().cloned();
        self.rows = rows;
        self.list.set_len(self.rows.len());
        if let Some(anchor) = anchor
            && let Some(index) = self
                .rows
                .iter()
                .position(|row| row.chat_rowid == anchor.chat_rowid && row.kind == anchor.kind)
        {
            self.list.selected = index;
        } else {
            self.list.selected = 0;
            self.list.offset = 0;
        }
    }

    /// Fuzzy-match the chat list, best first.
    fn chat_rows(&mut self, query: &str, chats: &[Chat]) -> Vec<Row> {
        if query.is_empty() {
            // An empty query is not a search: it is the list you already have,
            // newest first, which is what makes `Ctrl+K` `Enter` a no-op.
            return chats
                .iter()
                .take(MAX_CHAT_ROWS)
                .map(|chat| chat_row(chat, Vec::new()))
                .collect();
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize, Row)> = Vec::new();
        let mut buffer = Vec::new();
        let mut indices = Vec::new();
        for (position, chat) in chats.iter().enumerate() {
            let title = chat.title();
            indices.clear();
            buffer.clear();
            let haystack = Utf32Str::new(&title, &mut buffer);
            if let Some(score) = pattern.indices(haystack, &mut self.matcher, &mut indices) {
                indices.sort_unstable();
                indices.dedup();
                let ranges = group_indices(&indices);
                scored.push((score, position, chat_row(chat, ranges)));
                continue;
            }
            // A chat whose name does not match can still be found by the
            // address of somebody in it, which is what makes a raw number work.
            if participant_matches(&pattern, &mut self.matcher, chat) {
                scored.push((1, position, chat_row(chat, Vec::new())));
            }
        }
        // Best score first, and the more recent conversation when two tie.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.truncate(MAX_CHAT_ROWS);
        scored.into_iter().map(|(_, _, row)| row).collect()
    }
}

/// Whether anybody in `chat` has a name or an address the pattern matches.
///
/// A named group does not put its people in its title, so this is what finds
/// the conversation you are in with somebody by typing their name.
fn participant_matches(pattern: &Pattern, matcher: &mut Matcher, chat: &Chat) -> bool {
    let mut buffer = Vec::new();
    chat.participants.iter().any(|handle| {
        let mut hit = false;
        for candidate in [handle.id.clone(), handle.display_name()] {
            buffer.clear();
            let haystack = Utf32Str::new(&candidate, &mut buffer);
            hit = hit || pattern.score(haystack, matcher).is_some();
        }
        hit
    })
}

/// One chat as a result row.
fn chat_row(chat: &Chat, label_hits: Vec<(usize, usize)>) -> Row {
    let meta = if chat.is_group {
        format!("chat · {} people", chat.participants.len())
    } else {
        "chat".to_string()
    };
    Row {
        kind: Kind::Chat,
        chat_rowid: chat.rowid,
        message_rowid: None,
        label: chat.title(),
        label_hits,
        body: String::new(),
        body_hits: Vec::new(),
        meta,
    }
}

/// The index hits as result rows, newest first.
fn message_rows(
    hits: &[Hit],
    chats: &[Chat],
    words: &[String],
    now: DateTime<Local>,
    columns: usize,
) -> Vec<Row> {
    let by_rowid: HashMap<i64, &Chat> = chats.iter().map(|chat| (chat.rowid, chat)).collect();
    hits.iter()
        .filter_map(|hit| {
            // A hit in a conversation that is no longer in the list has
            // nowhere to jump to.
            let chat = by_rowid.get(&hit.chat_rowid)?;
            Some(message_row(hit, chat, words, now, columns))
        })
        .collect()
}

/// One index hit as a result row.
fn message_row(
    hit: &Hit,
    chat: &Chat,
    words: &[String],
    now: DateTime<Local>,
    columns: usize,
) -> Row {
    // In a group it matters who said it; in a two-way thread the chat name
    // already says it.
    let prefix = if chat.is_group {
        if hit.is_from_me {
            Some("You".to_string())
        } else {
            hit.handle.as_deref().map(|id| sender_name(chat, id))
        }
    } else if hit.is_from_me {
        Some("You".to_string())
    } else {
        None
    };
    let icon = if hit.kind == search::Kind::Photo {
        "📷 "
    } else if hit.kind == search::Kind::File {
        "📄 "
    } else {
        ""
    };

    let head = match prefix {
        Some(name) => format!("{name}: {icon}"),
        None => icon.to_string(),
    };
    let room = columns.saturating_sub(head.chars().count()).max(8);
    let (line, hits) = matched_line(&hit.body, words, room);
    let shift = head.chars().count();

    Row {
        kind: if hit.kind == search::Kind::Photo {
            Kind::Photo
        } else {
            Kind::Message
        },
        chat_rowid: hit.chat_rowid,
        message_rowid: Some(hit.message_rowid),
        label: chat.title(),
        label_hits: Vec::new(),
        body: format!("{head}{line}"),
        body_hits: hits
            .into_iter()
            .map(|(start, end)| (start + shift, end + shift))
            .collect(),
        meta: local_time(hit.date).map_or_else(String::new, |when| relative_time(now, when)),
    }
}

/// What to call whoever sent a message, given only their address.
///
/// The chat's participants are the first place to look, because they are the
/// rows [`crate::contacts`] has already resolved; anybody who is somehow not
/// among them falls back to their address.
fn sender_name(chat: &Chat, address: &str) -> String {
    chat.participants
        .iter()
        .find(|handle| handle.id.eq_ignore_ascii_case(address))
        .map_or_else(
            || crate::db::handle::short_name(address),
            crate::db::Handle::short_name,
        )
}

/// Consecutive character indices, as half-open ranges.
///
/// The matcher reports every matched character on its own; a highlight wants
/// the runs.
#[must_use]
pub fn group_indices(indices: &[u32]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for index in indices {
        let index = *index as usize;
        match ranges.last_mut() {
            Some(last) if last.1 == index => last.1 = index + 1,
            _ => ranges.push((index, index + 1)),
        }
    }
    ranges
}

/// Character ranges of `text` where any of `words` appears, case-insensitively.
///
/// Lowercasing is done one character at a time so the ranges keep describing
/// the original text: `str::to_lowercase` can change a string's length.
#[must_use]
pub fn token_ranges(text: &str, words: &[String]) -> Vec<(usize, usize)> {
    if words.is_empty() {
        return Vec::new();
    }
    let folded: Vec<char> = text
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for word in words {
        let needle: Vec<char> = word.chars().collect();
        if needle.is_empty() || needle.len() > folded.len() {
            continue;
        }
        for start in 0..=folded.len() - needle.len() {
            if folded[start..start + needle.len()] == needle[..] {
                ranges.push((start, start + needle.len()));
            }
        }
    }
    ranges.sort_unstable();
    // Overlapping runs would be drawn twice; one run is one highlight.
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// The one line of `text` worth showing for a match, and where the match is.
///
/// The body is collapsed to a single line, then a window of `columns`
/// characters is taken around the first match, so a hit deep in a long message
/// is still the part you see.
#[must_use]
pub fn matched_line(text: &str, words: &[String], columns: usize) -> (String, Vec<(usize, usize)>) {
    let line = single_line(text);
    let ranges = token_ranges(&line, words);
    let total = line.chars().count();
    if total <= columns {
        return (line, ranges);
    }

    // Keep a little context before the match rather than starting on it, and
    // never open a window that would run off the end of the line.
    let first = ranges.first().map_or(0, |range| range.0);
    let start = first.saturating_sub(columns / 4).min(total - columns);
    let head = start > 0;
    let mut take = columns - usize::from(head);
    let tail = start + take < total;
    if tail {
        take -= 1;
    }

    let mut window = String::new();
    if head {
        window.push('…');
    }
    window.extend(line.chars().skip(start).take(take));
    if tail {
        window.push('…');
    }

    // A range that the window cuts in half is not a highlight worth drawing.
    let shift = usize::from(head);
    let shifted = ranges
        .into_iter()
        .filter(|(a, b)| *a >= start && *b <= start + take)
        .map(|(a, b)| (a - start + shift, b - start + shift))
        .collect();
    (window, shifted)
}

/// The address behind a query that looks like one, ready to address a message.
///
/// Emails are taken as typed; a run of digits is written the way `handle.id`
/// stores one, because that is what Messages will be asked to send to.
#[must_use]
pub fn looks_like_address(query: &str) -> Option<String> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    if let Some((local, domain)) = query.split_once('@') {
        let ok = !local.is_empty()
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !query.contains(char::is_whitespace);
        return ok.then(|| query.to_string());
    }

    let plus = query.starts_with('+');
    if !query
        .chars()
        .all(|c| c.is_ascii_digit() || "+-() .".contains(c))
    {
        return None;
    }
    let digits: String = query.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 7 || digits.len() > 15 {
        return None;
    }
    Some(match () {
        () if plus => format!("+{digits}"),
        () if digits.len() == 11 && digits.starts_with('1') => format!("+{digits}"),
        () if digits.len() == 10 => format!("+1{digits}"),
        () => digits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_cycles_the_four_filters() {
        let mut filter = Filter::All;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(filter.label());
            filter = filter.next();
        }
        assert_eq!(seen, vec!["all", "chats", "messages", "photos"]);
        assert_eq!(filter, Filter::All);

        assert!(Filter::All.wants_chats() && Filter::All.wants_messages());
        assert!(Filter::Chats.wants_chats() && !Filter::Chats.wants_messages());
        assert_eq!(Filter::Photos.kind(), Some(search::Kind::Photo));
        assert_eq!(Filter::All.kind(), None);
    }

    #[test]
    fn matched_characters_group_into_runs() {
        assert_eq!(group_indices(&[0, 1, 2]), vec![(0, 3)]);
        assert_eq!(group_indices(&[0, 3, 4]), vec![(0, 1), (3, 5)]);
        assert_eq!(group_indices(&[]), Vec::new());
    }

    #[test]
    fn token_ranges_are_case_insensitive_and_merged() {
        let words = vec!["thai".to_string()];
        assert_eq!(token_ranges("Thai and thai", &words), vec![(0, 4), (9, 13)]);
        assert_eq!(token_ranges("nothing here", &words), Vec::new());
        assert_eq!(token_ranges("thai", &[]), Vec::new());

        // Overlapping words become one run rather than two highlights.
        let words = vec!["ab".to_string(), "bc".to_string()];
        assert_eq!(token_ranges("abc", &words), vec![(0, 3)]);
    }

    #[test]
    fn a_long_body_is_windowed_around_the_match() {
        let words = vec!["needle".to_string()];
        let body = format!("{} needle {}", "x".repeat(80), "y".repeat(80));
        let (line, ranges) = matched_line(&body, &words, 40);
        assert!(line.chars().count() <= 40, "{}", line.chars().count());
        assert!(line.contains("needle"));
        assert_eq!(ranges.len(), 1);
        let (start, end) = ranges[0];
        let matched: String = line.chars().skip(start).take(end - start).collect();
        assert_eq!(matched, "needle");
    }

    #[test]
    fn a_short_body_is_left_whole() {
        let words = vec!["thai".to_string()];
        let (line, ranges) = matched_line("that thai place", &words, 40);
        assert_eq!(line, "that thai place");
        assert_eq!(ranges, vec![(5, 9)]);
    }

    #[test]
    fn addresses_are_told_apart_from_words() {
        assert_eq!(
            looks_like_address("sam@example.invalid").as_deref(),
            Some("sam@example.invalid")
        );
        assert_eq!(
            looks_like_address("(555) 000-0000").as_deref(),
            Some("+15550000000")
        );
        assert_eq!(
            looks_like_address("+44 20 7123 4567").as_deref(),
            Some("+442071234567")
        );
        assert!(looks_like_address("thai").is_none());
        assert!(looks_like_address("sam@localhost").is_none());
        assert!(looks_like_address("12345").is_none());
        assert!(looks_like_address("").is_none());
    }
}
