//! The `handle` table: one row per address a person reaches you from.
//!
//! A single person often has several handles (a phone number and an Apple ID,
//! or the same number on SMS and iMessage). Turning handles into names is a
//! later pass; this module only reads them.

use std::collections::HashMap;

use super::{Db, DbError};

/// One row of `handle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    /// `handle.ROWID`, the id messages and chats join against.
    pub rowid: i64,
    /// `handle.id`: a phone number in E.164, or an email address.
    pub id: String,
    /// `handle.service`: `iMessage`, `SMS`, `RCS`.
    pub service: String,
}

impl Handle {
    /// Whether the address looks like an email rather than a phone number.
    #[must_use]
    pub fn is_email(&self) -> bool {
        self.id.contains('@')
    }

    /// The address written for a reader; see [`display_name`].
    #[must_use]
    pub fn display_name(&self) -> String {
        display_name(&self.id)
    }

    /// The shortest thing that still identifies the person; see [`short_name`].
    #[must_use]
    pub fn short_name(&self) -> String {
        short_name(&self.id)
    }
}

/// An address written for a reader: a North American number spaced out,
/// anything else as it is stored.
///
/// This is the last resort. Once contact lookup lands, a real name replaces it;
/// until then a row showing a number should at least show a readable one.
#[must_use]
pub fn display_name(id: &str) -> String {
    format_phone(id).unwrap_or_else(|| id.to_string())
}

/// The shortest thing that still identifies a person, for the joined
/// participant list of an unnamed group and for the `Name:` prefix on a
/// chat-list preview.
///
/// Emails lose their domain; numbers keep every digit, because half a phone
/// number identifies nobody.
#[must_use]
pub fn short_name(id: &str) -> String {
    match id.split_once('@') {
        Some((local, _)) if !local.is_empty() => local.to_string(),
        _ => display_name(id),
    }
}

/// `+15555550132` → `+1 (555) 555-0132`, and `None` for anything that is not a
/// plain eleven-digit North American number.
fn format_phone(id: &str) -> Option<String> {
    let digits = id.strip_prefix("+1")?;
    if digits.len() != 10 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "+1 ({}) {}-{}",
        &digits[0..3],
        &digits[3..6],
        &digits[6..10]
    ))
}

const SELECT: &str = "SELECT ROWID, id, service FROM handle";

impl Db {
    /// Every handle in the database, in `ROWID` order.
    ///
    /// The order is stable across sessions, which is what the per-participant
    /// color assignment keys off.
    ///
    /// # Errors
    ///
    /// Fails if `handle` cannot be read.
    pub fn handles(&self) -> Result<Vec<Handle>, DbError> {
        let mut statement = self.conn().prepare(&format!("{SELECT} ORDER BY ROWID"))?;
        let rows = statement.query_map([], |row| {
            Ok(Handle {
                rowid: row.get(0)?,
                id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                service: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every handle keyed by `ROWID`, for joining message rows to people.
    ///
    /// # Errors
    ///
    /// Fails if `handle` cannot be read.
    pub fn handle_map(&self) -> Result<HashMap<i64, Handle>, DbError> {
        Ok(self
            .handles()?
            .into_iter()
            .map(|handle| (handle.rowid, handle))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_and_numbers_are_told_apart() {
        let email = Handle {
            rowid: 1,
            id: "someone@example.com".to_string(),
            service: "iMessage".to_string(),
        };
        let phone = Handle {
            rowid: 2,
            id: "+15550000000".to_string(),
            service: "SMS".to_string(),
        };
        assert!(email.is_email());
        assert!(!phone.is_email());
    }

    #[test]
    fn a_north_american_number_is_spaced_out_and_anything_else_is_left_alone() {
        let phone = Handle {
            rowid: 1,
            id: "+15550000000".to_string(),
            service: "SMS".to_string(),
        };
        assert_eq!(phone.display_name(), "+1 (555) 000-0000");
        assert_eq!(phone.short_name(), phone.display_name());

        let short_code = Handle {
            rowid: 2,
            id: "26236".to_string(),
            service: "SMS".to_string(),
        };
        assert_eq!(short_code.display_name(), "26236");

        let international = Handle {
            rowid: 3,
            id: "+442071234567".to_string(),
            service: "SMS".to_string(),
        };
        assert_eq!(international.display_name(), international.id);
    }

    #[test]
    fn an_email_keeps_only_its_local_part_as_a_short_name() {
        let email = Handle {
            rowid: 1,
            id: "sam@example.invalid".to_string(),
            service: "iMessage".to_string(),
        };
        assert_eq!(email.short_name(), "sam");
        assert_eq!(email.display_name(), email.id);
    }
}
