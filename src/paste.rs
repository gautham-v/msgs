//! What a bracketed paste is: files dropped from Finder, or plain text.
//!
//! Dropping a file on a macOS terminal types its path into whatever is at the
//! prompt, and every terminal spells that path a little differently: bare when
//! it has no spaces, with a backslash before each space, wrapped in single or
//! double quotes, or as a `file://` URL with the awkward characters
//! percent-encoded. Several files arrive as several such paths, separated by
//! spaces or newlines, usually with a trailing space on the end.
//!
//! Everything here is pure. [`dropped_files`] is handed the home directory and
//! a predicate that answers "is this a file", so the whole decision can be
//! tested without a home directory and without touching the filesystem the
//! test is running on.

use std::path::{Path, PathBuf};

/// Split a paste into the paths it might be naming.
///
/// Whitespace separates tokens; a backslash escapes the character after it;
/// and a run inside `'` or `"` is one token however much whitespace it holds.
/// A backslash inside single quotes is a literal one, the way a shell reads it.
#[must_use]
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quote != Some('\'') => {
                started = true;
                match chars.next() {
                    // A backslash before a newline is a continuation, and
                    // joins the two halves of one path.
                    Some('\n') | None => {}
                    Some(escaped) => current.push(escaped),
                }
            }
            '\'' | '"' if quote.is_none() => {
                started = true;
                quote = Some(c);
            }
            c if Some(c) == quote => quote = None,
            c if c.is_whitespace() && quote.is_none() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                started = true;
                current.push(c);
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// The path inside a `file://` URL, percent-decoding included.
///
/// `file:///Users/…` and `file://localhost/Users/…` name the same file, and
/// both are things a Mac hands over on a drop.
#[must_use]
fn file_url(token: &str) -> Option<String> {
    let rest = token.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    Some(percent_decode(rest))
}

/// Turn `%20` and friends back into the bytes they stand for. Anything that is
/// not a complete escape is left exactly as it came.
#[must_use]
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let pair = (index + 2 < bytes.len())
            .then(|| (hex(bytes[index + 1]), hex(bytes[index + 2])))
            .filter(|_| bytes[index] == b'%');
        if let Some((Some(high), Some(low))) = pair {
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One hexadecimal digit's value.
#[must_use]
const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// What one token names, once the URL and the `~` are gone: an absolute path,
/// or nothing at all.
///
/// A relative path is nothing: a drop always gives the whole path, so anything
/// relative is a word somebody pasted rather than a file they dropped.
#[must_use]
pub fn path_of(token: &str, home: Option<&Path>) -> Option<PathBuf> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let text = file_url(token).unwrap_or_else(|| token.to_string());
    let path = if text == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home?.join(rest)
    } else {
        PathBuf::from(text)
    };
    path.is_absolute().then_some(path)
}

/// The files a paste dropped, in the order they were handed over, or an empty
/// vector when the paste is not a drop.
///
/// Every token has to be a file for the paste to count as one: half a drop is
/// not a drop, and a sentence that happens to start with a path is text.
#[must_use]
pub fn dropped_files(
    text: &str,
    home: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for token in tokens(text) {
        match path_of(&token, home) {
            Some(path) if exists(&path) => files.push(path),
            _ => return Vec::new(),
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filesystem that is nothing but the paths listed, so no test here ever
    /// asks the real one a question.
    fn only(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn home() -> PathBuf {
        PathBuf::from("/Users/someone")
    }

    #[test]
    fn a_bare_path_is_one_token() {
        assert_eq!(tokens("/tmp/a.png"), vec!["/tmp/a.png"]);
        // Terminals put a space after a drop.
        assert_eq!(tokens("/tmp/a.png "), vec!["/tmp/a.png"]);
        assert_eq!(tokens("  \n "), Vec::<String>::new());
    }

    #[test]
    fn escaped_spaces_and_quotes_hold_a_path_together() {
        assert_eq!(tokens(r"/tmp/two\ words.png"), vec!["/tmp/two words.png"]);
        assert_eq!(tokens("'/tmp/two words.png'"), vec!["/tmp/two words.png"]);
        assert_eq!(tokens("\"/tmp/two words.png\""), vec!["/tmp/two words.png"]);
        // A backslash is literal inside single quotes, as a shell reads it.
        assert_eq!(tokens(r"'/tmp/back\slash'"), vec![r"/tmp/back\slash"]);
        // Two files, however they were spelled.
        assert_eq!(
            tokens("/tmp/a.png '/tmp/b c.png'"),
            vec!["/tmp/a.png", "/tmp/b c.png"]
        );
    }

    #[test]
    fn a_file_url_becomes_the_path_it_names() {
        assert_eq!(
            path_of("file:///tmp/two%20words.png", None).as_deref(),
            Some(Path::new("/tmp/two words.png"))
        );
        assert_eq!(
            path_of("file://localhost/tmp/a.png", None).as_deref(),
            Some(Path::new("/tmp/a.png"))
        );
        // An incomplete escape is left alone rather than eaten.
        assert_eq!(
            path_of("file:///tmp/100%.png", None).as_deref(),
            Some(Path::new("/tmp/100%.png"))
        );
        // Not a local file URL, so not a path.
        assert_eq!(path_of("https://example.com/a.png", None), None);
    }

    #[test]
    fn a_tilde_needs_a_home_and_a_relative_path_is_never_one() {
        assert_eq!(
            path_of("~/Desktop/a.png", Some(&home())).as_deref(),
            Some(Path::new("/Users/someone/Desktop/a.png"))
        );
        assert_eq!(path_of("~", Some(&home())).as_deref(), Some(&*home()));
        assert_eq!(path_of("~/Desktop/a.png", None), None);
        assert_eq!(path_of("Desktop/a.png", Some(&home())), None);
        assert_eq!(path_of("   ", Some(&home())), None);
    }

    #[test]
    fn a_drop_is_every_token_being_a_file_and_nothing_less() {
        let files = only(&["/tmp/a.png", "/tmp/b c.png"]);
        let there = |candidate: &Path| files.iter().any(|path| path == candidate);
        assert_eq!(
            dropped_files("/tmp/a.png ", None, &there),
            vec![PathBuf::from("/tmp/a.png")]
        );
        // Order is the order they were dropped in.
        assert_eq!(
            dropped_files(r"/tmp/a.png /tmp/b\ c.png", None, &there),
            vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b c.png")]
        );
        // Newlines separate them just as well.
        assert_eq!(
            dropped_files("'/tmp/b c.png'\n/tmp/a.png\n", None, &there),
            vec![PathBuf::from("/tmp/b c.png"), PathBuf::from("/tmp/a.png")]
        );
        // One token that is not a file makes the whole paste text.
        assert!(dropped_files("/tmp/a.png /tmp/gone.png", None, &there).is_empty());
        assert!(dropped_files("see /tmp/a.png", None, &there).is_empty());
        assert!(dropped_files("just some words", None, &there).is_empty());
        assert!(dropped_files("", None, &there).is_empty());
        assert!(dropped_files("   \n", None, &there).is_empty());
    }

    #[test]
    fn a_drop_of_a_real_file_reads_the_filesystem_only_where_it_is_told_to() {
        let dir = std::env::temp_dir().join(format!("msgs-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("two words.png");
        std::fs::write(&file, b"x").expect("temp file");

        let quoted = format!("'{}' ", file.display());
        assert_eq!(
            dropped_files(&quoted, None, &|path| path.is_file()),
            vec![file.clone()]
        );
        let url = format!("file://{}", file.display().to_string().replace(' ', "%20"));
        assert_eq!(
            dropped_files(&url, None, &|path| path.is_file()),
            vec![file.clone()]
        );
        // The directory itself is not a file, so dropping it is not a drop.
        assert!(dropped_files(&dir.display().to_string(), None, &|path| path.is_file()).is_empty());

        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
