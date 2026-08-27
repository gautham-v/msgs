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
}
