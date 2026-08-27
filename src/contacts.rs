//! Names for the addresses in `chat.db`, read out of the macOS Contacts stores.
//!
//! `chat.db` holds phone numbers and email addresses and nothing else, so every
//! name on screen comes from here. macOS keeps Contacts as a set of Core Data
//! SQLite files under `~/Library/Application Support/AddressBook`: one at the
//! top level and one per account in `Sources/<UUID>/`. All of them are opened
//! the same way `chat.db` is — read-only, `?mode=ro`, `PRAGMA query_only`, with
//! a scratch copy when a lock refuses a reader — and nothing here ever writes to
//! them.
//!
//! Matching is by normalized address: an email lowercased, a phone number
//! reduced to `+` and digits, with a last-ten-digits index behind it so a
//! contact saved without a country code still matches a handle that has one.
//!
//! The result is cached at `~/Library/Application Support/msgs/contacts.json`
//! alongside the modification stamps of the files it was built from, so a
//! launch that changes nothing costs one `stat` per store instead of a scan.
//! That file holds names and numbers, so it is written `0600` inside a `0700`
//! directory, and nothing in this module prints a name, a number, or an address
//! anywhere: errors carry a reason and at most a path.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::db::handle::{self, Name};
use crate::db::{Chat, Handle, Scratch, read_only_uri};

/// Bumped whenever the cache layout changes, which throws the old file away.
pub const CACHE_VERSION: u32 = 1;

/// The Core Data store macOS writes each Contacts account into.
const STORE_FILE: &str = "AddressBook-v22.abcddb";

/// Digits an address must have before it earns a suffix entry. Ten is a North
/// American number without its country code, and the shortest suffix that is
/// still specific enough to belong to one person.
const SUFFIX_DIGITS: usize = 10;

/// Where the names came from, for the status line and `--check`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    /// Nobody asked for contacts — the tests, and anything holding
    /// [`Contacts::empty`].
    #[default]
    Off,
    /// Contacts were read.
    Ready {
        /// How many addresses have a name. A count, never a name.
        addresses: usize,
        /// Whether this came from the cache rather than a fresh scan.
        cached: bool,
    },
    /// Contacts could not be read; every handle falls back to its address.
    Unavailable(String),
}

impl Status {
    /// One line for the status line and for `--check`.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Off => "not loaded".to_string(),
            Self::Ready { addresses, cached } => {
                let how = if *cached { "cached" } else { "read" };
                format!("{addresses} addresses named ({how})")
            }
            Self::Unavailable(reason) => format!("unavailable — {reason}"),
        }
    }

    /// The warning the status line shows when there are no names to be had.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Unavailable(reason) => Some(format!(
                "contacts: {reason} — showing numbers instead of names"
            )),
            _ => None,
        }
    }

    /// Whether names are available at all.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// Every address Contacts knows, and what it calls the person behind it.
#[derive(Debug, Clone, Default)]
pub struct Contacts {
    /// Normalized address → name.
    names: HashMap<String, Name>,
    /// The last [`SUFFIX_DIGITS`] digits of a number → the one name that owns
    /// them, or `None` when two different people share the suffix and the
    /// answer would be a guess.
    suffixes: HashMap<String, Option<Name>>,
    status: Status,
}

impl Contacts {
    /// No names at all: every handle keeps its address.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            names: HashMap::new(),
            suffixes: HashMap::new(),
            status: Status::Off,
        }
    }

    /// Read the Contacts stores from their usual places, through the cache.
    ///
    /// Never fails: a Mac whose stores cannot be read comes back
    /// [`Status::Unavailable`] with an empty map, and every pane falls back to
    /// pretty-printed addresses.
    #[must_use]
    pub fn load() -> Self {
        let Some(dir) = default_store_dir() else {
            return Self::unavailable("no home directory");
        };
        Self::load_from(&store_paths(&dir), default_cache_path().as_deref())
    }

    /// Read `stores`, using `cache` when its stamps still match.
    ///
    /// The tests drive this against a synthetic store and their own cache path,
    /// so nothing under test reads the real Contacts database.
    #[must_use]
    pub fn load_from(stores: &[PathBuf], cache: Option<&Path>) -> Self {
        let stamps = stamps(stores);
        if !stamps.is_empty()
            && let Some(path) = cache
            && let Some(cached) = read_cache(path)
            && cached.sources == stamps
        {
            return Self::ready(cached.names, true);
        }

        match scan(stores) {
            Ok(names) => {
                if let Some(path) = cache
                    && let Err(err) = write_cache(path, &stamps, &names)
                {
                    // A cache that cannot be written costs a scan next launch
                    // and nothing else, so it is not worth a warning.
                    let _ = err;
                }
                Self::ready(names, false)
            }
            Err(reason) => Self::unavailable(reason),
        }
    }

    /// A map built in memory, for tests and for callers with their own source.
    #[must_use]
    pub fn from_names(names: BTreeMap<String, Name>) -> Self {
        Self::ready(names, false)
    }

    fn ready(names: BTreeMap<String, Name>, cached: bool) -> Self {
        let mut contacts = Self {
            names: HashMap::with_capacity(names.len()),
            suffixes: HashMap::new(),
            status: Status::Ready {
                addresses: names.len(),
                cached,
            },
        };
        for (key, name) in names {
            if let Some(suffix) = digit_suffix(&key) {
                match contacts.suffixes.get(&suffix) {
                    Some(Some(seen)) if *seen != name => {
                        contacts.suffixes.insert(suffix, None);
                    }
                    Some(_) => {}
                    None => {
                        contacts.suffixes.insert(suffix, Some(name.clone()));
                    }
                }
            }
            contacts.names.insert(key, name);
        }
        contacts
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            names: HashMap::new(),
            suffixes: HashMap::new(),
            status: Status::Unavailable(reason.into()),
        }
    }

    /// Where the names came from.
    #[must_use]
    pub const fn status(&self) -> &Status {
        &self.status
    }

    /// How many addresses carry a name.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether nothing is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// What Contacts calls `address`, if anybody.
    ///
    /// The exact normalized address is tried first; a number that misses falls
    /// back to its last ten digits, which is what makes a contact saved as
    /// `415-555-0132` match a handle stored as `+14155550132`.
    #[must_use]
    pub fn name(&self, address: &str) -> Option<&Name> {
        let key = normalize(address)?;
        if let Some(name) = self.names.get(&key) {
            return Some(name);
        }
        self.suffixes.get(&digit_suffix(&key)?)?.as_ref()
    }

    /// The whole name for `address`, or the address written for a reader.
    #[must_use]
    pub fn label(&self, address: &str) -> String {
        self.name(address)
            .filter(|name| !name.is_empty())
            .map_or_else(|| handle::display_name(address), Name::full)
    }

    /// The one word that identifies whoever is behind `address`.
    #[must_use]
    pub fn short(&self, address: &str) -> String {
        self.name(address)
            .filter(|name| !name.is_empty())
            .map_or_else(|| handle::short_name(address), Name::short)
    }

    /// Hang a name on every handle in `handles`.
    pub fn apply_handles(&self, handles: &mut [Handle]) {
        for handle in handles {
            handle.name = self.name(&handle.id).cloned();
        }
    }

    /// Hang a name on everybody in every chat.
    ///
    /// This is the one place names enter the app: the chat list, the
    /// conversation header, the group sender labels, and the palette all read
    /// [`Handle::display_name`] and [`Handle::short_name`], so resolving the
    /// participants resolves the whole screen.
    pub fn apply(&self, chats: &mut [Chat]) {
        for chat in chats {
            self.apply_handles(&mut chat.participants);
        }
    }
}

/// `~/Library/Application Support/AddressBook`.
#[must_use]
pub fn default_store_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("AddressBook"))
}

/// `~/Library/Application Support/msgs/contacts.json`.
#[must_use]
pub fn default_cache_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("msgs").join("contacts.json"))
}

/// Every Contacts store under `dir`: the top-level one, then one per account.
///
/// Sorted, so the cache stamps of two launches that changed nothing compare
/// equal.
#[must_use]
pub fn store_paths(dir: &Path) -> Vec<PathBuf> {
    let mut stores = Vec::new();
    let top = dir.join(STORE_FILE);
    if top.is_file() {
        stores.push(top);
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("Sources")) {
        let mut sources: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path().join(STORE_FILE))
            .filter(|path| path.is_file())
            .collect();
        sources.sort();
        stores.extend(sources);
    }
    stores
}

/// An address reduced to the form both sides of the match are written in.
///
/// Emails lowercase. Numbers keep their digits and gain the `+` and the country
/// code a North American number is usually saved without, so
/// `(415) 555-0132`, `415-555-0132`, and `+1 415 555 0132` all land on
/// `+14155550132`.
#[must_use]
pub fn normalize(address: &str) -> Option<String> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('@') {
        return Some(trimmed.to_lowercase());
    }
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    if trimmed.starts_with('+') || digits.len() > SUFFIX_DIGITS {
        return Some(format!("+{digits}"));
    }
    if digits.len() == SUFFIX_DIGITS {
        return Some(format!("+1{digits}"));
    }
    // A short code, or something else that is not a phone number at all.
    Some(format!("+{digits}"))
}

/// The last [`SUFFIX_DIGITS`] digits of a normalized number, when it has that
/// many.
fn digit_suffix(key: &str) -> Option<String> {
    if key.contains('@') {
        return None;
    }
    let digits: Vec<char> = key.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < SUFFIX_DIGITS {
        return None;
    }
    Some(digits[digits.len() - SUFFIX_DIGITS..].iter().collect())
}

/// What the cache remembers about one file it was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    path: String,
    modified: u64,
    len: u64,
}

/// The cache file itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cache {
    version: u32,
    sources: Vec<Stamp>,
    names: BTreeMap<String, Name>,
}

/// Stamp every store and its write-ahead log.
///
/// The `-wal` matters: a contact saved a minute ago may live only there, and
/// the store's own modification time will not have moved.
fn stamps(stores: &[PathBuf]) -> Vec<Stamp> {
    let mut stamps = Vec::new();
    for store in stores {
        for path in [store.clone(), sidecar(store, "-wal")] {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|when| when.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
            stamps.push(Stamp {
                path: path.to_string_lossy().into_owned(),
                modified,
                len: meta.len(),
            });
        }
    }
    stamps
}

/// `chat.db` → `chat.db-wal`, keeping whatever the path already was.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(suffix);
    PathBuf::from(raw)
}

fn read_cache(path: &Path) -> Option<Cache> {
    let text = std::fs::read_to_string(path).ok()?;
    let cache: Cache = serde_json::from_str(&text).ok()?;
    (cache.version == CACHE_VERSION).then_some(cache)
}

fn write_cache(
    path: &Path,
    sources: &[Stamp],
    names: &BTreeMap<String, Name>,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        crate::private(dir, 0o700);
    }
    let cache = Cache {
        version: CACHE_VERSION,
        sources: sources.to_vec(),
        names: names.clone(),
    };
    let text = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
    std::fs::write(path, text)?;
    crate::private(path, 0o600);
    Ok(())
}

/// Read every store into one map, newest information winning.
fn scan(stores: &[PathBuf]) -> Result<BTreeMap<String, Name>, String> {
    if stores.is_empty() {
        return Err("no Contacts database found".to_string());
    }
    let mut names = BTreeMap::new();
    let mut opened = 0usize;
    let mut failure: Option<String> = None;
    for store in stores {
        match read_store(store, &mut names) {
            Ok(()) => opened += 1,
            Err(reason) => {
                if failure.is_none() {
                    failure = Some(reason);
                }
            }
        }
    }
    if opened == 0 {
        return Err(failure.unwrap_or_else(|| "unreadable".to_string()));
    }
    Ok(names)
}

/// The two queries that carry a whole store: numbers, then email addresses.
///
/// Both join the address to its owning record and take the personal name, with
/// the organization standing in for a business that has no personal name.
const PHONES: &str = "SELECT r.ZFIRSTNAME, r.ZLASTNAME, r.ZORGANIZATION, p.ZFULLNUMBER \
     FROM ZABCDPHONENUMBER p JOIN ZABCDRECORD r ON r.Z_PK = p.ZOWNER \
     WHERE p.ZFULLNUMBER IS NOT NULL";

const EMAILS: &str = "SELECT r.ZFIRSTNAME, r.ZLASTNAME, r.ZORGANIZATION, e.ZADDRESS \
     FROM ZABCDEMAILADDRESS e JOIN ZABCDRECORD r ON r.Z_PK = e.ZOWNER \
     WHERE e.ZADDRESS IS NOT NULL";

fn read_store(path: &Path, into: &mut BTreeMap<String, Name>) -> Result<(), String> {
    // The scratch copy has to outlive the connection, so it is bound here.
    let (conn, _scratch) = open_store(path)?;
    let mut read_any = false;
    for sql in [PHONES, EMAILS] {
        // A store from an older macOS may not have both tables; the one it does
        // have is still worth reading.
        if read_rows(&conn, sql, into).is_ok() {
            read_any = true;
        }
    }
    if read_any {
        Ok(())
    } else {
        Err("unrecognized Contacts schema".to_string())
    }
}

fn read_rows(
    conn: &Connection,
    sql: &str,
    into: &mut BTreeMap<String, Name>,
) -> rusqlite::Result<()> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        ))
    })?;
    for row in rows {
        let (first, last, organization, address) = row?;
        let Some(key) = normalize(&address) else {
            continue;
        };
        let Some(name) = person(&first, &last, &organization) else {
            continue;
        };
        // Two accounts often hold the same person; the entry that says more
        // about them wins, and a tie leaves the first one alone.
        match into.get(&key) {
            Some(seen) if rank(seen) >= rank(&name) => {}
            _ => {
                into.insert(key, name);
            }
        }
    }
    Ok(())
}

/// A record's name, or `None` when it has none worth showing.
fn person(first: &str, last: &str, organization: &str) -> Option<Name> {
    let name = Name {
        first: first.trim().to_string(),
        last: last.trim().to_string(),
    };
    if !name.is_empty() {
        return Some(name);
    }
    let organization = organization.trim();
    if organization.is_empty() {
        return None;
    }
    Some(Name {
        first: organization.to_string(),
        last: String::new(),
    })
}

/// How much a name says, so the better of two duplicates wins.
const fn rank(name: &Name) -> u8 {
    match (name.first.is_empty(), name.last.is_empty()) {
        (false, false) => 2,
        (true, true) => 0,
        _ => 1,
    }
}

/// Open a Contacts store read-only, through a scratch copy if a lock says no.
fn open_store(path: &Path) -> Result<(Connection, Option<Scratch>), String> {
    match connect(path) {
        Ok(conn) => Ok((conn, None)),
        Err(_) => {
            let scratch = Scratch::new(path).map_err(|err| err.summary())?;
            let conn = connect(scratch.db()).map_err(|err| reason(&err))?;
            Ok((conn, Some(scratch)))
        }
    }
}

/// The same read-only open `chat.db` gets: no writes are possible from here.
fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(read_only_uri(path), flags)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(conn)
}

/// A SQLite failure as a short reason, with nothing out of the file in it.
fn reason(err: &rusqlite::Error) -> String {
    match err {
        rusqlite::Error::SqliteFailure(error, _) => error.to_string(),
        _ => "unreadable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(first: &str, last: &str) -> Name {
        Name {
            first: first.to_string(),
            last: last.to_string(),
        }
    }

    #[test]
    fn a_number_normalizes_the_same_however_it_was_typed() {
        let e164 = normalize("+14155550132");
        assert_eq!(normalize("(415) 555-0132"), e164);
        assert_eq!(normalize("415-555-0132"), e164);
        assert_eq!(normalize("+1 (415) 555-0132"), e164);
        assert_eq!(normalize(" 1 415 555 0132 "), e164);
    }

    #[test]
    fn an_email_normalizes_to_lower_case_and_a_blank_to_nothing() {
        assert_eq!(
            normalize("Sam@Example.Invalid"),
            Some("sam@example.invalid".to_string())
        );
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("no digits here"), None);
    }

    #[test]
    fn a_contact_saved_without_a_country_code_still_matches_a_handle_that_has_one() {
        let contacts = Contacts::from_names(BTreeMap::from([(
            "+447700900123".to_string(),
            name("Robin", "Ash"),
        )]));
        // A UK number normalizes to different keys on the two sides, so only
        // the suffix index can join them.
        assert_eq!(contacts.name("+447700900123"), Some(&name("Robin", "Ash")));
        assert_eq!(contacts.name("07700900123"), Some(&name("Robin", "Ash")));
    }

    #[test]
    fn a_suffix_two_people_share_names_neither_of_them() {
        let contacts = Contacts::from_names(BTreeMap::from([
            ("+15550000001".to_string(), name("Sam", "Rivera")),
            ("+445550000001".to_string(), name("Robin", "Ash")),
        ]));
        assert_eq!(contacts.name("+15550000001"), Some(&name("Sam", "Rivera")));
        assert_eq!(contacts.name("+335550000001"), None);
    }

    #[test]
    fn an_unknown_address_falls_back_to_a_readable_one() {
        let contacts = Contacts::empty();
        assert_eq!(contacts.label("+15550000001"), "+1 (555) 000-0001");
        assert_eq!(contacts.short("sam@example.invalid"), "sam");
        assert_eq!(contacts.status(), &Status::Off);
        assert!(contacts.is_empty());
    }

    #[test]
    fn a_known_address_becomes_a_name_on_every_handle_in_a_chat() {
        let contacts = Contacts::from_names(BTreeMap::from([(
            "+15550000001".to_string(),
            name("Sam", "Rivera"),
        )]));
        let mut handles = vec![
            Handle::new(1, "+15550000001".to_string(), "iMessage".to_string()),
            Handle::new(2, "+15550000002".to_string(), "iMessage".to_string()),
        ];
        contacts.apply_handles(&mut handles);
        assert_eq!(handles[0].display_name(), "Sam Rivera");
        assert_eq!(handles[0].short_name(), "Sam");
        assert_eq!(handles[1].display_name(), "+1 (555) 000-0002");
        assert!(contacts.status().is_ready());
    }

    #[test]
    fn a_business_is_named_by_its_organization() {
        assert_eq!(person("", "", "Blue Bottle"), Some(name("Blue Bottle", "")));
        assert_eq!(
            person(" Sam ", " Rivera ", "Acme"),
            Some(name("Sam", "Rivera"))
        );
        assert_eq!(person("", "", "  "), None);
    }

    #[test]
    fn the_fuller_of_two_records_for_one_address_wins() {
        assert!(rank(&name("Sam", "Rivera")) > rank(&name("Sam", "")));
        assert!(rank(&name("Sam", "")) > rank(&name("", "")));
    }

    #[test]
    fn a_status_with_no_names_explains_itself_once() {
        let unavailable = Status::Unavailable("permission denied".to_string());
        assert!(unavailable.warning().is_some_and(|line| {
            line.starts_with("contacts: ") && line.contains("showing numbers")
        }));
        assert!(!unavailable.is_ready());
        assert!(
            Status::Ready {
                addresses: 3,
                cached: true
            }
            .warning()
            .is_none()
        );
    }

    #[test]
    fn missing_stores_are_unavailable_rather_than_empty() {
        let contacts = Contacts::load_from(&[], None);
        assert!(matches!(contacts.status(), Status::Unavailable(_)));
        assert!(contacts.is_empty());
        assert_eq!(contacts.label("+15550000001"), "+1 (555) 000-0001");
    }
}
