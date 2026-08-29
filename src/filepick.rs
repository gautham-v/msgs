//! The composer's `@` file picker: when an `@` means "pick a file", what the
//! picker lists, and how the text typed after it narrows the list.
//!
//! Everything here is pure but for the two functions that read a directory,
//! and those take the roots and the home directory as arguments so a test can
//! point them at a temporary tree rather than at anybody's real home. Nothing
//! recurses: one directory is one `read_dir`, capped, most recently modified
//! first, which is what keeps a keystroke from walking a disk.
//!
//! A query is a path as typed — `Downloads/scr` — so descending and ascending
//! are string edits and the directory being listed is derived from the text
//! rather than remembered beside it. Nothing here logs; entries only ever
//! reach the screen.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use crate::jump::group_indices;

/// Most entries one listing keeps, however many the directories hold.
pub const CAP: usize = 200;

/// The directories the picker opens on, under the home directory.
pub const ROOT_NAMES: [&str; 4] = ["Downloads", "Desktop", "Pictures", "Documents"];

/// Whether an `@` typed with `before` in front of it opens the picker.
///
/// Only at the start of the draft or straight after whitespace: the `@` in
/// `name@example.invalid` is a character in an address, not a command.
#[must_use]
pub fn triggers(before: &str) -> bool {
    before.chars().next_back().is_none_or(char::is_whitespace)
}

/// One file or directory the picker can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where the file is, which is what gets attached.
    pub path: PathBuf,
    /// What the query matches and the row shows: the path relative to its
    /// root with the root's own name in front, and a `/` on a directory.
    pub label: String,
    /// Whether the row can be entered rather than attached.
    pub is_dir: bool,
    /// Last modified, which is the order the list is in.
    pub modified: Option<SystemTime>,
}

/// One entry that matched, and the characters of its label that did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index into the listing the match came from.
    pub index: usize,
    /// Character ranges of [`Entry::label`] that matched.
    pub hits: Vec<(usize, usize)>,
}

/// The roots the picker opens on: a few well-known folders and the directory
/// msgs was started in.
///
/// Anything missing, anything under a `Library`, and anything whose name
/// another root already uses is dropped, because a root's name is how a query
/// addresses it and two roots answering to one name could not be told apart.
#[must_use]
pub fn roots() -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let mut roots: Vec<PathBuf> = Vec::with_capacity(ROOT_NAMES.len() + 1);
    if let Some(home) = home.as_deref() {
        roots.extend(ROOT_NAMES.iter().map(|name| home.join(name)));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    usable_roots(&roots, home.as_deref())
}

/// The roots of `candidates` worth listing, in order, one per name.
#[must_use]
pub fn usable_roots(candidates: &[PathBuf], home: Option<&Path>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::with_capacity(candidates.len());
    for root in candidates {
        if !root.is_dir() || under_library(root, home) {
            continue;
        }
        let name = root_label(root);
        if name.is_empty() || kept.iter().any(|kept| root_label(kept) == name) {
            continue;
        }
        kept.push(root.clone());
    }
    kept
}

/// How a root is written at the head of a label, and how a query names it.
#[must_use]
pub fn root_label(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Whether `path` is somewhere msgs will not look.
///
/// `~/Library` holds the message store, the caches, and every app's private
/// state; none of it is a file anybody means to attach, so the picker does not
/// go there — nor into the system's own `Library` directories.
#[must_use]
pub fn under_library(path: &Path, home: Option<&Path>) -> bool {
    if let Some(home) = home
        && path.starts_with(home.join("Library"))
    {
        return true;
    }
    path.starts_with("/Library") || path.starts_with("/System")
}

/// What is directly inside `dir`, most recently modified first.
///
/// `prefix` is what the labels are written under, so an entry of a root reads
/// `Downloads/report.pdf` and the query that finds it is the label itself.
/// Hidden files are left out, and nothing recurses.
#[must_use]
pub fn list_dir(dir: &Path, prefix: &str, home: Option<&Path>, cap: usize) -> Vec<Entry> {
    if under_library(dir, home) {
        return Vec::new();
    }
    let Ok(reading) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = Vec::new();
    for found in reading.flatten() {
        let name = found.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = found.metadata() else {
            continue;
        };
        let is_dir = meta.is_dir();
        let path = found.path();
        if is_dir && under_library(&path, home) {
            continue;
        }
        entries.push(Entry {
            path,
            label: format!("{prefix}{name}{}", if is_dir { "/" } else { "" }),
            is_dir,
            modified: meta.modified().ok(),
        });
    }
    newest_first(&mut entries, cap);
    entries
}

/// What is directly inside every root, most recently modified first.
#[must_use]
pub fn list_roots(roots: &[PathBuf], home: Option<&Path>, cap: usize) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for root in roots {
        let prefix = format!("{}/", root_label(root));
        entries.extend(list_dir(root, &prefix, home, cap));
    }
    newest_first(&mut entries, cap);
    entries
}

/// Newest first, with the entries the filesystem could not date at the end,
/// then cut to `cap`.
fn newest_first(entries: &mut Vec<Entry>, cap: usize) {
    entries.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.label.cmp(&b.label))
    });
    entries.truncate(cap);
}

/// Split a query into the directory part and the leaf being typed:
/// `Downloads/rep` is `("Downloads/", "rep")`.
#[must_use]
pub fn split(query: &str) -> (&str, &str) {
    match query.rfind('/') {
        Some(slash) => query.split_at(slash + 1),
        None => ("", query),
    }
}

/// Which directory a query is listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every root at once, which is where an empty query starts.
    Roots,
    /// One directory, named by the query's directory part.
    Dir(PathBuf),
}

/// The directory a query's directory part names, resolved against the roots.
///
/// The first component is a root's name and the rest is a path under it. A
/// part that names no root — a typo, or a `..` — falls back to the roots
/// rather than to somewhere the reader did not ask for.
#[must_use]
pub fn scope_for(dir: &str, roots: &[PathBuf]) -> Scope {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        return Scope::Roots;
    }
    let mut parts = dir.split('/');
    let Some(head) = parts.next() else {
        return Scope::Roots;
    };
    let Some(root) = roots.iter().find(|root| root_label(root) == head) else {
        return Scope::Roots;
    };
    let mut path = root.clone();
    for part in parts {
        // A query is typed text, so it never gets to climb out of a root.
        if part.is_empty() || part == "." || part == ".." {
            return Scope::Roots;
        }
        path.push(part);
    }
    Scope::Dir(path)
}

/// The entries a query lists, before any matching.
#[must_use]
pub fn entries_for(query: &str, roots: &[PathBuf], home: Option<&Path>, cap: usize) -> Vec<Entry> {
    let (dir, _) = split(query);
    match scope_for(dir, roots) {
        Scope::Roots => list_roots(roots, home, cap),
        Scope::Dir(path) => list_dir(&path, dir, home, cap),
    }
}

/// The query that opens `entry`, which is its label: a directory's label
/// already ends in the `/` that puts the picker inside it.
#[must_use]
pub fn descend(entry: &Entry) -> String {
    entry.label.clone()
}

/// The query one directory up, or `None` when the query is already listing
/// the roots and there is nowhere above it.
///
/// It is the directory part that moves: whatever leaf was being typed goes
/// with it, because it named something in the directory being left.
#[must_use]
pub fn ascend(query: &str) -> Option<String> {
    let (dir, _) = split(query);
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        return None;
    }
    Some(match dir.rfind('/') {
        Some(slash) => dir[..=slash].to_string(),
        None => String::new(),
    })
}

/// Which entries match `query`, best first.
///
/// An empty query is not a search: it is the listing as it stands, newest
/// first. The whole label is matched, directory part and all, so `dow rep`
/// finds `Downloads/report.pdf` without either half having to be exact.
#[must_use]
pub fn filter(entries: &[Entry], query: &str, matcher: &mut Matcher) -> Vec<Match> {
    if query.trim().is_empty() {
        return (0..entries.len())
            .map(|index| Match {
                index,
                hits: Vec::new(),
            })
            .collect();
    }

    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<Scored> = Vec::new();
    let mut buffer = Vec::new();
    let mut indices = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        buffer.clear();
        indices.clear();
        let haystack = Utf32Str::new(&entry.label, &mut buffer);
        if let Some(score) = pattern.indices(haystack, matcher, &mut indices) {
            indices.sort_unstable();
            indices.dedup();
            scored.push(Scored {
                score,
                index,
                hits: group_indices(&indices),
            });
        }
    }
    // Best score first, and the more recently modified entry when two tie,
    // which is the order the listing already carries.
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
    scored
        .into_iter()
        .map(|scored| Match {
            index: scored.index,
            hits: scored.hits,
        })
        .collect()
}

/// One match while it is still being ranked.
struct Scored {
    score: u32,
    index: usize,
    hits: Vec<(usize, usize)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nucleo_matcher::Config;
    use std::fs;
    use std::time::Duration;

    /// A throwaway directory tree, removed when the test ends.
    ///
    /// Never the real home: every test here builds its own roots and hands
    /// them to the functions under test.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "msgs-filepick-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(&base).expect("a temporary directory");
            Self(base)
        }

        fn dir(&self, path: &str) -> PathBuf {
            let full = self.0.join(path);
            fs::create_dir_all(&full).expect("a directory");
            full
        }

        fn file(&self, path: &str) -> PathBuf {
            let full = self.0.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("a parent directory");
            }
            fs::write(&full, b"x").expect("a file");
            full
        }

        fn age(&self, path: &str, seconds: u64) {
            // Set an explicit mtime so the ordering under test is the one the
            // test asked for rather than whatever the clock did.
            let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 - seconds);
            let file = fs::File::options()
                .write(true)
                .open(self.0.join(path))
                .expect("open for a timestamp");
            file.set_modified(when).expect("set the timestamp");
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn matcher() -> Matcher {
        Matcher::new(Config::DEFAULT)
    }

    #[test]
    fn an_at_only_triggers_at_a_word_start() {
        assert!(triggers(""));
        assert!(triggers("here you go "));
        assert!(triggers("one\n"));
        assert!(!triggers("name"));
        assert!(!triggers("send to sam"));
        assert!(!triggers("a@b"));
    }

    #[test]
    fn a_query_splits_into_a_directory_and_a_leaf() {
        assert_eq!(split(""), ("", ""));
        assert_eq!(split("rep"), ("", "rep"));
        assert_eq!(split("Downloads/"), ("Downloads/", ""));
        assert_eq!(split("Downloads/sub/rep"), ("Downloads/sub/", "rep"));
    }

    #[test]
    fn ascending_walks_the_query_back_up() {
        assert_eq!(ascend("Downloads/sub/"), Some("Downloads/".to_string()));
        assert_eq!(ascend("Downloads/sub/re"), Some("Downloads/".to_string()));
        assert_eq!(ascend("Downloads/"), Some(String::new()));
        assert_eq!(ascend("Downloads/rep"), Some(String::new()));
        // Already at the roots: there is nowhere above them.
        assert_eq!(ascend("rep"), None);
        assert_eq!(ascend(""), None);
    }

    #[test]
    fn a_directory_part_resolves_to_one_root_and_nothing_above_it() {
        let temp = Temp::new("scope");
        let downloads = temp.dir("Downloads");
        temp.dir("Downloads/nested");
        let roots = vec![downloads.clone()];

        assert_eq!(scope_for("", &roots), Scope::Roots);
        assert_eq!(
            scope_for("Downloads/", &roots),
            Scope::Dir(downloads.clone())
        );
        assert_eq!(
            scope_for("Downloads/nested/", &roots),
            Scope::Dir(downloads.join("nested"))
        );
        // Nothing a query says can climb out of a root or name one that is
        // not there.
        assert_eq!(scope_for("Downloads/../", &roots), Scope::Roots);
        assert_eq!(scope_for("Elsewhere/", &roots), Scope::Roots);
    }

    #[test]
    fn a_listing_is_newest_first_without_hidden_files() {
        let temp = Temp::new("listing");
        let downloads = temp.dir("Downloads");
        temp.file("Downloads/old.txt");
        temp.file("Downloads/new.txt");
        temp.file("Downloads/.secret");
        temp.dir("Downloads/folder");
        temp.age("Downloads/old.txt", 900);
        temp.age("Downloads/new.txt", 10);

        let entries = list_dir(&downloads, "Downloads/", None, CAP);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Downloads/folder/",
                "Downloads/new.txt",
                "Downloads/old.txt"
            ]
        );
        assert!(entries[0].is_dir, "a directory is enterable");
        assert!(!entries[1].is_dir);

        // The cap is a cap.
        assert_eq!(list_dir(&downloads, "Downloads/", None, 2).len(), 2);
    }

    #[test]
    fn nothing_under_library_is_listed() {
        let temp = Temp::new("library");
        let home = temp.dir("home");
        let library = temp.dir("home/Library/Messages");
        temp.file("home/Library/Messages/chat.db");

        assert!(under_library(&library, Some(&home)));
        assert!(list_dir(&library, "Messages/", Some(&home), CAP).is_empty());
        assert!(usable_roots(&[library], Some(&home)).is_empty());
    }

    #[test]
    fn roots_are_kept_once_each_and_only_where_they_exist() {
        let temp = Temp::new("roots");
        let downloads = temp.dir("Downloads");
        let other = temp.dir("elsewhere/Downloads");
        let desktop = temp.dir("Desktop");
        let missing = temp.0.join("Nowhere");

        let kept = usable_roots(&[downloads.clone(), other, desktop.clone(), missing], None);
        assert_eq!(kept, vec![downloads, desktop]);
    }

    #[test]
    fn the_query_matches_the_relative_path_fuzzily() {
        let temp = Temp::new("filter");
        let downloads = temp.dir("Downloads");
        temp.file("Downloads/report.pdf");
        temp.file("Downloads/notes.txt");
        let roots = vec![downloads];

        let entries = entries_for("", &roots, None, CAP);
        assert_eq!(entries.len(), 2);
        let mut matcher = matcher();

        // An empty query is the listing as it stands.
        assert_eq!(filter(&entries, "", &mut matcher).len(), 2);

        let hits = filter(&entries, "dowrep", &mut matcher);
        assert_eq!(hits.len(), 1);
        assert_eq!(entries[hits[0].index].label, "Downloads/report.pdf");
        assert!(!hits[0].hits.is_empty(), "the matched characters come back");

        assert!(filter(&entries, "zzzz", &mut matcher).is_empty());
    }

    #[test]
    fn descending_writes_the_query_that_lists_the_directory() {
        let temp = Temp::new("descend");
        let downloads = temp.dir("Downloads");
        temp.dir("Downloads/trip");
        temp.file("Downloads/trip/beach.jpg");
        let roots = vec![downloads];

        let entries = entries_for("", &roots, None, CAP);
        let folder = entries
            .iter()
            .find(|entry| entry.is_dir)
            .expect("the directory");
        let query = descend(folder);
        assert_eq!(query, "Downloads/trip/");

        let inside = entries_for(&query, &roots, None, CAP);
        let labels: Vec<&str> = inside.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, vec!["Downloads/trip/beach.jpg"]);

        // And back up again.
        assert_eq!(ascend(&query).as_deref(), Some("Downloads/"));
    }
}
