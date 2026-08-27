//! The `chat` table: conversations, who is in them, and how stale they are.
//!
//! One grouped query carries the whole list — last message date, message count,
//! and unread count per chat — and a second pulls every chat's participants, so
//! a list of 500 chats costs two round trips rather than 1,001.

use std::collections::HashMap;

use chrono::{DateTime, Local};

use super::{Db, DbError, Handle, local_time};

/// `chat.style` for a group conversation. Anything else is one-to-one.
const STYLE_GROUP: i64 = 43;

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

    /// A name to show before contacts have been resolved: the group's own name
    /// if it has one, then its participants, then its raw identifier.
    ///
    /// The participant fallback is raw handles, so this is for the screen only.
    #[must_use]
    pub fn fallback_title(&self) -> String {
        if let Some(name) = self.display_name.as_deref().filter(|s| !s.is_empty()) {
            return name.to_string();
        }
        if !self.participants.is_empty() {
            return self
                .participants
                .iter()
                .map(|handle| handle.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
        }
        self.identifier.clone().unwrap_or_else(|| self.guid.clone())
    }
}

impl Db {
    /// Every chat, newest first, with participants and counts filled in.
    ///
    /// Chats that hold no messages sort to the bottom rather than being hidden,
    /// so a conversation you started but never sent in is still reachable.
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
                    COALESCE(MAX(m.date), 0), COUNT(m.ROWID), \
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
                message_count: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                unread_count: row.get::<_, Option<i64>>(9)?.unwrap_or_default(),
            })
        })?;

        let mut chats = rows.collect::<Result<Vec<_>, _>>()?;
        for chat in &mut chats {
            chat.participants = participants.remove(&chat.rowid).unwrap_or_default();
            // A two-way chat that somehow carries a group style, or a group
            // whose style was never set, still sorts out correctly here.
            chat.is_group = chat.style == STYLE_GROUP || chat.participants.len() > 1;
        }
        // Newest first, with empty chats after everything that has a message.
        chats.sort_by(|a, b| {
            b.last_message_date
                .cmp(&a.last_message_date)
                .then_with(|| a.rowid.cmp(&b.rowid))
        });
        Ok(chats)
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
                Handle {
                    rowid: row.get(1)?,
                    id: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    service: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                },
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
            message_count: 0,
            unread_count: 0,
            is_pinned: None,
        }
    }

    #[test]
    fn a_named_group_shows_its_name() {
        let mut group = chat(1);
        group.display_name = Some("Weekend".to_string());
        group.participants = vec![Handle {
            rowid: 1,
            id: "+15550000000".to_string(),
            service: "iMessage".to_string(),
        }];
        assert_eq!(group.fallback_title(), "Weekend");
    }

    #[test]
    fn an_unnamed_chat_falls_back_to_participants_then_the_identifier() {
        let mut unnamed = chat(2);
        unnamed.participants = vec![
            Handle {
                rowid: 1,
                id: "a@example.com".to_string(),
                service: "iMessage".to_string(),
            },
            Handle {
                rowid: 2,
                id: "b@example.com".to_string(),
                service: "iMessage".to_string(),
            },
        ];
        assert_eq!(unnamed.fallback_title(), "a@example.com, b@example.com");

        let empty = chat(3);
        assert_eq!(empty.fallback_title(), "chat3");
    }

    #[test]
    fn unread_is_a_predicate_not_just_a_number() {
        let mut chat = chat(4);
        assert!(!chat.is_unread());
        chat.unread_count = 2;
        assert!(chat.is_unread());
    }
}
