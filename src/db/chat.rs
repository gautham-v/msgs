//! The `chat` table: conversations, who is in them, and how stale they are.
//!
//! One grouped query carries the whole list — last message date, message count,
//! and unread count per chat — a second pulls every chat's participants, and
//! two more fetch the last message of each chat and its attachments for the
//! preview line. A list of 500 chats costs four round trips rather than 2,001.

use std::collections::HashMap;

use chrono::{DateTime, Local};

use super::handle::display_name;
use super::{AttachmentKind, Db, DbError, GroupAction, Handle, MAX_PAGE, body_text, local_time};

/// `chat.style` for a group conversation. Anything else is one-to-one.
const STYLE_GROUP: i64 = 43;

/// The last message in a chat, reduced to the one line the chat list shows.
///
/// It deliberately holds the sender's handle rather than a name: contact lookup
/// is a later pass, and the row is drawn from this in `ui::format`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    /// `message.ROWID` of the message being previewed.
    pub message_rowid: i64,
    /// Whether you sent it.
    pub is_from_me: bool,
    /// `handle.ROWID` of the sender, absent when you sent it.
    pub sender_rowid: Option<i64>,
    /// The sender's handle, for the `Name:` prefix in a group.
    pub sender: Option<String>,
    /// The body, with attachment placeholders already stripped.
    pub text: Option<String>,
    /// How many files came with it.
    pub attachments: usize,
    /// What sort of file the first of them is.
    pub attachment_kind: Option<AttachmentKind>,
    /// The name of the first file, when Messages recorded one.
    pub attachment_name: Option<String>,
    /// The group event this row announces, when it announces one.
    pub group_action: Option<GroupAction>,
}

impl Preview {
    /// Whether the previewed row is a group event rather than something
    /// somebody typed.
    #[must_use]
    pub const fn is_announcement(&self) -> bool {
        self.group_action.is_some()
    }
}

/// One conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    /// `chat.ROWID`, the id everything else joins against.
    pub rowid: i64,
    /// `chat.guid`, e.g. `iMessage;-;+15550000000`.
    pub guid: String,
    /// `chat.chat_identifier`: the handle or group id the chat is addressed by.
    pub identifier: Option<String>,
    /// The name somebody gave the group, when there is one.
    pub display_name: Option<String>,
    /// `iMessage`, `SMS`, or `RCS`.
    pub service: Option<String>,
    /// `chat.style`; see [`Chat::is_group`].
    pub style: i64,
    /// Whether this is a group conversation.
    pub is_group: bool,
    /// Everyone in the chat, in `handle.ROWID` order so per-participant colors
    /// stay put between sessions.
    pub participants: Vec<Handle>,
    /// Raw Messages timestamp of the newest message, `0` when the chat is empty.
    pub last_message_date: i64,
    /// `message.ROWID` of that newest message, `0` when the chat is empty.
    pub last_message_rowid: i64,
    /// That message reduced to one line, once [`Db::chats`] has filled it in.
    pub preview: Option<Preview>,
    /// How many messages the chat holds, tapbacks excluded.
    pub message_count: i64,
    /// Incoming messages Messages has not marked read. Group events such as
    /// renames and joins are not messages and do not count.
    pub unread_count: i64,
    /// Whether the chat is pinned, when the schema records that at all.
    ///
    /// macOS keeps pinned conversations in Messages.app's preferences rather
    /// than in `chat.db`, so this is `None` on every current system.
    pub is_pinned: Option<bool>,
}

impl Chat {
    /// When the newest message arrived, in local time.
    #[must_use]
    pub fn last_message_at(&self) -> Option<DateTime<Local>> {
        local_time(self.last_message_date)
    }

    /// Whether anything in the chat is unread.
    #[must_use]
    pub const fn is_unread(&self) -> bool {
        self.unread_count > 0
    }

    /// Whether the chat sits in the pinned section of the list.
    ///
    /// `false` on every database that does not record pinning at all, which is
    /// every current macOS.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.is_pinned == Some(true)
    }

    /// What the chat is called: the group's own name if it has one, then its
    /// participants, then its raw identifier.
    ///
    /// A conversation with one person is that person, written in full: `Sam
    /// Rivera`, or a spaced-out number for somebody Contacts does not know. An
    /// unnamed group is the short form of everybody in it — a first name once
    /// [`crate::contacts`] has been over the row, and otherwise the local part
    /// of an email or the number — so it reads as a list of people rather than
    /// a list of addresses.
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(name) = self.display_name.as_deref().filter(|s| !s.is_empty()) {
            return name.to_string();
        }
        match self.participants.as_slice() {
            [] => self
                .identifier
                .as_deref()
                .map_or_else(|| self.guid.clone(), display_name),
            [only] => only.display_name(),
            many => many
                .iter()
                .map(Handle::short_name)
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// Whether `needle`, already lowercased, appears in anything the chat can
    /// be found by: its name, its identifier, or a participant's name or
    /// address.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        if self.title().to_lowercase().contains(needle) {
            return true;
        }
        if self
            .identifier
            .as_deref()
            .is_some_and(|id| id.to_lowercase().contains(needle))
        {
            return true;
        }
        self.participants
            .iter()
            .any(|handle| handle.matches(needle))
    }
}

impl Db {
    /// Every chat, pinned first and newest first inside each group, with
    /// participants, counts, and the one-line preview filled in.
    ///
    /// Chats that hold no messages sort to the bottom rather than being hidden,
    /// so a conversation you started but never sent in is still reachable.
    ///
    /// Four queries carry the whole list however long it is: the grouped chat
    /// query, the participants, the previewed messages, and their attachments.
    ///
    /// # Errors
    ///
    /// Fails if `chat`, `chat_message_join`, or `chat_handle_join` cannot be
    /// read.
    pub fn chats(&self) -> Result<Vec<Chat>, DbError> {
        let pinned = if self.schema().chat_is_pinned {
            "c.is_pinned"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT c.ROWID, c.guid, c.chat_identifier, c.display_name, c.service_name, \
                    c.style, {pinned}, \
                    COALESCE(MAX(m.date), 0), COALESCE(MAX(m.ROWID), 0), COUNT(m.ROWID), \
                    COALESCE(SUM(CASE WHEN m.is_from_me = 0 AND COALESCE(m.is_read, 0) = 0 \
                                       AND COALESCE(m.item_type, 0) = 0 \
                                      THEN 1 ELSE 0 END), 0) \
             FROM chat c \
             LEFT JOIN chat_message_join j ON j.chat_id = c.ROWID \
             LEFT JOIN message m ON m.ROWID = j.message_id \
                  AND COALESCE(m.associated_message_type, 0) = 0 \
             GROUP BY c.ROWID"
        );

        let mut participants = self.participants()?;
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            let style: i64 = row.get::<_, Option<i64>>(5)?.unwrap_or_default();
            Ok(Chat {
                rowid: row.get(0)?,
                guid: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                identifier: row.get(2)?,
                display_name: row.get::<_, Option<String>>(3)?.filter(|s| !s.is_empty()),
                service: row.get(4)?,
                style,
                is_group: style == STYLE_GROUP,
                participants: Vec::new(),
                is_pinned: row.get::<_, Option<i64>>(6)?.map(|flag| flag != 0),
                last_message_date: row.get::<_, Option<i64>>(7)?.unwrap_or_default(),
                last_message_rowid: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                preview: None,
                message_count: row.get::<_, Option<i64>>(9)?.unwrap_or_default(),
                unread_count: row.get::<_, Option<i64>>(10)?.unwrap_or_default(),
            })
        })?;

        let mut chats = rows.collect::<Result<Vec<_>, _>>()?;
        for chat in &mut chats {
            chat.participants = participants.remove(&chat.rowid).unwrap_or_default();
            // A two-way chat that somehow carries a group style, or a group
            // whose style was never set, still sorts out correctly here.
            chat.is_group = chat.style == STYLE_GROUP || chat.participants.len() > 1;
        }

        let mut previews = self.previews(
            &chats
                .iter()
                .map(|chat| chat.last_message_rowid)
                .filter(|rowid| *rowid != 0)
                .collect::<Vec<_>>(),
        )?;
        for chat in &mut chats {
            chat.preview = previews.remove(&chat.last_message_rowid);
        }

        // Pinned first, then newest first, with empty chats after everything
        // that has a message.
        chats.sort_by(|a, b| {
            b.is_pinned()
                .cmp(&a.is_pinned())
                .then_with(|| b.last_message_date.cmp(&a.last_message_date))
                .then_with(|| a.rowid.cmp(&b.rowid))
        });
        Ok(chats)
    }

    /// One-line previews of the given messages, keyed by `message.ROWID`.
    ///
    /// Ids are read a bounded chunk at a time so a long chat list cannot build
    /// an unbounded `IN (…)` list.
    ///
    /// # Errors
    ///
    /// Fails if `message` or the attachment tables cannot be read.
    pub fn previews(&self, message_rowids: &[i64]) -> Result<HashMap<i64, Preview>, DbError> {
        let mut previews = HashMap::with_capacity(message_rowids.len());
        for chunk in message_rowids.chunks(MAX_PAGE) {
            self.previews_chunk(chunk, &mut previews)?;
        }
        Ok(previews)
    }

    fn previews_chunk(
        &self,
        message_rowids: &[i64],
        into: &mut HashMap<i64, Preview>,
    ) -> Result<(), DbError> {
        if message_rowids.is_empty() {
            return Ok(());
        }
        let placeholders = (1..=message_rowids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT m.ROWID, m.is_from_me, m.handle_id, h.id, m.text, m.attributedBody, \
                    m.item_type, m.group_action_type, m.group_title, m.other_handle \
             FROM message m \
             LEFT JOIN handle h ON h.ROWID = m.handle_id \
             WHERE m.ROWID IN ({placeholders})"
        );

        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(
            rusqlite::params_from_iter(message_rowids.iter().copied()),
            |row| {
                let sender_rowid = row.get::<_, Option<i64>>(2)?.filter(|id| *id != 0);
                let text: Option<String> = row.get(4)?;
                let attributed: Option<Vec<u8>> = row.get(5)?;
                let item_type = row.get::<_, Option<i64>>(6)?.unwrap_or_default();
                let group_action_type = row.get::<_, Option<i64>>(7)?.unwrap_or_default();
                let group_title: Option<String> = row.get(8)?;
                let other_handle = row.get::<_, Option<i64>>(9)?.filter(|id| *id != 0);
                Ok(Preview {
                    message_rowid: row.get(0)?,
                    is_from_me: row.get::<_, Option<i64>>(1)?.unwrap_or_default() != 0,
                    sender_rowid,
                    sender: row.get(3)?,
                    text: body_text(text.as_deref(), attributed.as_deref()),
                    attachments: 0,
                    attachment_kind: None,
                    attachment_name: None,
                    group_action: GroupAction::from_row(
                        item_type,
                        group_action_type,
                        other_handle,
                        sender_rowid,
                        group_title.as_deref(),
                    ),
                })
            },
        )?;

        for preview in rows {
            let preview = preview?;
            into.insert(preview.message_rowid, preview);
        }

        // One more query hangs the files off the previews that have any.
        for (message_rowid, files) in self.attachments_by_message(message_rowids)? {
            let Some(preview) = into.get_mut(&message_rowid) else {
                continue;
            };
            preview.attachments = files.len();
            if let Some(first) = files.first() {
                preview.attachment_kind = Some(first.kind());
                preview.attachment_name = first.display_name().map(ToString::to_string);
            }
        }
        Ok(())
    }

    /// Total unread messages, and how many chats hold them.
    ///
    /// # Errors
    ///
    /// Fails if the chat list cannot be read.
    pub fn unread_totals(&self) -> Result<(i64, usize), DbError> {
        let chats = self.chats()?;
        let total = chats.iter().map(|chat| chat.unread_count).sum();
        let with_unread = chats.iter().filter(|chat| chat.is_unread()).count();
        Ok((total, with_unread))
    }

    /// Participants of every chat, keyed by `chat.ROWID`.
    fn participants(&self) -> Result<HashMap<i64, Vec<Handle>>, DbError> {
        let mut statement = self.conn().prepare(
            "SELECT k.chat_id, h.ROWID, h.id, h.service \
             FROM chat_handle_join k \
             JOIN handle h ON h.ROWID = k.handle_id \
             ORDER BY k.chat_id, h.ROWID",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Handle::new(
                    row.get(1)?,
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                ),
            ))
        })?;

        let mut by_chat: HashMap<i64, Vec<Handle>> = HashMap::new();
        for row in rows {
            let (chat_rowid, handle) = row?;
            by_chat.entry(chat_rowid).or_default().push(handle);
        }
        Ok(by_chat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(rowid: i64) -> Chat {
        Chat {
            rowid,
            guid: format!("iMessage;-;chat{rowid}"),
            identifier: Some(format!("chat{rowid}")),
            display_name: None,
            service: Some("iMessage".to_string()),
            style: 45,
            is_group: false,
            participants: Vec::new(),
            last_message_date: 0,
            last_message_rowid: 0,
            preview: None,
            message_count: 0,
            unread_count: 0,
            is_pinned: None,
        }
    }

    #[test]
    fn a_named_group_shows_its_name() {
        let mut group = chat(1);
        group.display_name = Some("Weekend".to_string());
        group.participants = vec![Handle::new(
            1,
            "+15550000000".to_string(),
            "iMessage".to_string(),
        )];
        assert_eq!(group.title(), "Weekend");
    }

    #[test]
    fn an_unnamed_chat_falls_back_to_participants_then_the_identifier() {
        let mut unnamed = chat(2);
        unnamed.participants = vec![
            Handle::new(1, "a@example.com".to_string(), "iMessage".to_string()),
            Handle::new(2, "b@example.com".to_string(), "iMessage".to_string()),
        ];
        assert_eq!(unnamed.title(), "a, b");

        let empty = chat(3);
        assert_eq!(empty.title(), "chat3");
    }

    #[test]
    fn an_unnamed_group_reads_as_its_people_not_its_addresses() {
        let mut group = chat(5);
        group.participants = vec![
            Handle::new(1, "sam@example.invalid".to_string(), "iMessage".to_string()),
            Handle::new(2, "+15550000000".to_string(), "SMS".to_string()),
        ];
        assert_eq!(group.title(), "sam, +1 (555) 000-0000");
    }

    #[test]
    fn the_filter_matches_names_identifiers_and_addresses() {
        let mut named = chat(6);
        named.display_name = Some("Weekend Plans".to_string());
        named.participants = vec![Handle::new(
            1,
            "casey@example.invalid".to_string(),
            "iMessage".to_string(),
        )];

        assert!(named.matches(""));
        assert!(named.matches("weekend"));
        assert!(named.matches("plans"));
        assert!(named.matches("casey"));
        assert!(named.matches("chat6"));
        assert!(!named.matches("nobody"));
    }

    #[test]
    fn pinning_is_a_predicate_that_is_false_when_the_schema_is_silent() {
        let mut chat = chat(7);
        assert!(!chat.is_pinned());
        chat.is_pinned = Some(false);
        assert!(!chat.is_pinned());
        chat.is_pinned = Some(true);
        assert!(chat.is_pinned());
    }

    #[test]
    fn unread_is_a_predicate_not_just_a_number() {
        let mut chat = chat(4);
        assert!(!chat.is_unread());
        chat.unread_count = 2;
        assert!(chat.is_unread());
    }
}
