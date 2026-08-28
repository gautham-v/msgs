//! The `handle` table: one row per address a person reaches you from.
//!
//! A single person often has several handles (a phone number and an Apple ID,
//! or the same number on SMS and iMessage). The rows themselves carry only
//! addresses; [`crate::contacts`] fills [`Handle::name`] in from the macOS
//! Contacts stores after a read, and everything that writes a person onto the
//! screen goes through [`Handle::display_name`] or [`Handle::short_name`], so a
//! resolved name reaches every pane at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::{Db, DbError};

/// What Contacts calls somebody.
///
/// Kept split rather than joined because the two halves are used in different
/// places: a chat-list row and a conversation header have space for the whole
/// name, while the sender label on a message in a group has space for one word.
/// A contact with no personal name at all — a business — carries its
/// organization in [`Name::first`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Name {
    /// Given name, or the organization when there is no personal name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub first: String,
    /// Family name, when Contacts has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last: String,
}

impl Name {
    /// Both halves, joined: `Sam Rivera`.
    #[must_use]
    pub fn full(&self) -> String {
        match (self.first.as_str(), self.last.as_str()) {
            ("", last) => last.to_string(),
            (first, "") => first.to_string(),
            (first, last) => format!("{first} {last}"),
        }
    }

    /// The one word that identifies the person: `Sam`.
    #[must_use]
    pub fn short(&self) -> String {
        if self.first.is_empty() {
            return self.last.clone();
        }
        self.first.clone()
    }

    /// Whether Contacts gave us nothing usable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.first.is_empty() && self.last.is_empty()
    }
}

/// One row of `handle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    /// `handle.ROWID`, the id messages and chats join against.
    pub rowid: i64,
    /// `handle.id`: a phone number in E.164, or an email address.
    pub id: String,
    /// `handle.service`: `iMessage`, `SMS`, `RCS`.
    pub service: String,
    /// What Contacts calls this address, once [`crate::contacts::Contacts`] has
    /// been over the row. `None` before that, and for anybody who is not in
    /// Contacts at all.
    pub name: Option<Name>,
}

impl Handle {
    /// A row with no contact name attached yet.
    #[must_use]
    pub const fn new(rowid: i64, id: String, service: String) -> Self {
        Self {
            rowid,
            id,
            service,
            name: None,
        }
    }

    /// Whether the address looks like an email rather than a phone number.
    #[must_use]
    pub fn is_email(&self) -> bool {
        self.id.contains('@')
    }

    /// The name Contacts has for this address, or the address written for a
    /// reader; see [`display_name`].
    #[must_use]
    pub fn display_name(&self) -> String {
        match self.name.as_ref().filter(|name| !name.is_empty()) {
            Some(name) => name.full(),
            None => display_name(&self.id),
        }
    }

    /// The shortest thing that still identifies the person: a first name once
    /// Contacts has been read, and otherwise see [`short_name`].
    #[must_use]
    pub fn short_name(&self) -> String {
        match self.name.as_ref().filter(|name| !name.is_empty()) {
            Some(name) => name.short(),
            None => short_name(&self.id),
        }
    }

    /// Whether `needle`, already lowercased, appears in this person's name or
    /// address. Both halves of the name are searched, and so is the address, so
    /// a number still finds somebody whose name you cannot spell.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        if self.id.to_lowercase().contains(needle) {
            return true;
        }
        self.name.as_ref().is_some_and(|name| {
            name.first.to_lowercase().contains(needle) || name.last.to_lowercase().contains(needle)
        })
    }
}

/// Whether addresses are masked on screen (`--redact`), for a demo or a
/// screenshot that would otherwise carry somebody's number.
static REDACT: AtomicBool = AtomicBool::new(false);

/// Mask every address from now on. Names from Contacts still show; message
/// bodies are left alone.
pub fn set_redact(on: bool) {
    REDACT.store(on, Ordering::Relaxed);
}

/// Whether [`set_redact`] is on.
#[must_use]
pub fn redacted() -> bool {
    REDACT.load(Ordering::Relaxed)
}

/// `id` with its identifying part hidden: a number keeps its last two digits,
/// an email its first letter and domain, anything else becomes dots.
#[must_use]
pub fn mask(id: &str) -> String {
    if let Some(digits) = id.strip_prefix("+1").filter(|d| d.len() == 10) {
        return format!("+1 (•••) •••-••{}", &digits[8..]);
    }
    if let Some((local, domain)) = id.split_once('@') {
        let first = local.chars().next().map(String::from).unwrap_or_default();
        return format!("{first}•••@{domain}");
    }
    let digits = id.chars().filter(char::is_ascii_digit).count();
    if digits >= 7 {
        let tail: String = id
            .chars()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return format!("•••{tail}");
    }
    "•••".to_string()
}

/// An address written for a reader: a North American number spaced out,
/// anything else as it is stored — or masked, under `--redact`.
///
/// This is the last resort, for somebody Contacts does not know and for a Mac
/// whose Contacts stores could not be read at all: a row showing a number
/// should at least show a readable one.
#[must_use]
pub fn display_name(id: &str) -> String {
    if redacted() {
        return mask(id);
    }
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
    if redacted() {
        return mask(id);
    }
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
            Ok(Handle::new(
                row.get(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
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
    fn a_mask_keeps_the_shape_and_hides_the_person() {
        assert_eq!(mask("+15555550132"), "+1 (•••) •••-••32");
        assert_eq!(mask("pat@example.com"), "p•••@example.com");
        assert_eq!(mask("+447700900123"), "•••23");
        assert_eq!(mask("iMessage;-;+15555550132"), "•••32");
        assert_eq!(mask("chat12"), "•••");
        assert!(!mask("+15555550132").contains("5550"));
        assert!(!mask("+447700900123").contains("7700"));
        assert!(!mask("pat@example.com").contains("pat"));
    }

    fn named(id: &str, first: &str, last: &str) -> Handle {
        let mut handle = Handle::new(1, id.to_string(), "iMessage".to_string());
        handle.name = Some(Name {
            first: first.to_string(),
            last: last.to_string(),
        });
        handle
    }

    #[test]
    fn emails_and_numbers_are_told_apart() {
        let email = Handle::new(1, "someone@example.com".to_string(), "iMessage".to_string());
        let phone = Handle::new(2, "+15550000000".to_string(), "SMS".to_string());
        assert!(email.is_email());
        assert!(!phone.is_email());
    }

    #[test]
    fn a_north_american_number_is_spaced_out_and_anything_else_is_left_alone() {
        let phone = Handle::new(1, "+15550000000".to_string(), "SMS".to_string());
        assert_eq!(phone.display_name(), "+1 (555) 000-0000");
        assert_eq!(phone.short_name(), phone.display_name());

        let short_code = Handle::new(2, "26236".to_string(), "SMS".to_string());
        assert_eq!(short_code.display_name(), "26236");

        let international = Handle::new(3, "+442071234567".to_string(), "SMS".to_string());
        assert_eq!(international.display_name(), international.id);
    }

    #[test]
    fn an_email_keeps_only_its_local_part_as_a_short_name() {
        let email = Handle::new(1, "sam@example.invalid".to_string(), "iMessage".to_string());
        assert_eq!(email.short_name(), "sam");
        assert_eq!(email.display_name(), email.id);
    }

    #[test]
    fn a_contact_name_replaces_the_address_everywhere_it_is_written() {
        let handle = named("+15550000000", "Sam", "Rivera");
        assert_eq!(handle.display_name(), "Sam Rivera");
        assert_eq!(handle.short_name(), "Sam");
    }

    #[test]
    fn half_a_name_is_still_a_name_and_an_empty_one_is_not() {
        assert_eq!(named("+15550000000", "", "Rivera").display_name(), "Rivera");
        assert_eq!(named("+15550000000", "", "Rivera").short_name(), "Rivera");
        assert_eq!(named("+15550000000", "Sam", "").display_name(), "Sam");

        let blank = named("+15550000000", "", "");
        assert_eq!(blank.display_name(), "+1 (555) 000-0000");
        assert_eq!(blank.short_name(), "+1 (555) 000-0000");
    }

    #[test]
    fn a_person_is_found_by_either_half_of_their_name_or_by_their_number() {
        let handle = named("+15550000000", "Sam", "Rivera");
        assert!(handle.matches("sam"));
        assert!(handle.matches("rivera"));
        assert!(handle.matches("5550000000"));
        assert!(!handle.matches("morgan"));

        let anonymous = Handle::new(1, "+15550000000".to_string(), "SMS".to_string());
        assert!(anonymous.matches("555"));
        assert!(!anonymous.matches("sam"));
    }
}
