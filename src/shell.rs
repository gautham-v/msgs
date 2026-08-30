//! The few things msgs asks the rest of the machine to do: put text on the
//! clipboard, open a link, and open an attachment — with [`opener`] deciding
//! which of two ways an attachment is opened, because Preview.app shows a GIF
//! as a stack of pages and Quick Look plays it.
//!
//! All of them are deliberately tiny and all of them are one-way. Nothing here
//! reads anything back, nothing here logs what it was given — the text handed
//! to [`copy`] is a message body, and it goes to the pasteboard and nowhere
//! else.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Why a hand-off to the system failed. The message never carries the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The helper program could not be started.
    NotAvailable,
    /// It started but did not take the text.
    Failed,
    /// The link was not something worth opening.
    NotALink,
    /// There is no file at that path.
    NotAFile,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAvailable => "no clipboard helper",
            Self::Failed => "the clipboard refused it",
            Self::NotALink => "not a link",
            Self::NotAFile => "no file there",
        })
    }
}

impl std::error::Error for Error {}

/// Put `text` on the system clipboard.
///
/// `pbcopy` is the reliable path on macOS. When it is missing — over SSH, say —
/// the OSC 52 escape is written to the terminal instead, which the terminal
/// emulator may or may not honor.
///
/// # Errors
///
/// Returns [`Error::NotAvailable`] when neither path can be taken, and
/// [`Error::Failed`] when `pbcopy` was started but did not accept the text.
pub fn copy(text: &str) -> Result<(), Error> {
    match pbcopy(text) {
        Ok(()) => Ok(()),
        Err(Error::NotAvailable) => osc52(text),
        Err(err) => Err(err),
    }
}

fn pbcopy(text: &str) -> Result<(), Error> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::NotAvailable)?;
    {
        let stdin = child.stdin.as_mut().ok_or(Error::Failed)?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|_| Error::Failed)?;
    }
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        _ => Err(Error::Failed),
    }
}

fn osc52(text: &str) -> Result<(), Error> {
    let mut out = std::io::stdout();
    out.write_all(osc52_sequence(text).as_bytes())
        .map_err(|_| Error::NotAvailable)?;
    out.flush().map_err(|_| Error::NotAvailable)
}

/// The `OSC 52` escape that asks the terminal to set the clipboard.
#[must_use]
pub fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Standard base64, which is all OSC 52 needs and less than a dependency.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut block = [0u8; 3];
        block[..chunk.len()].copy_from_slice(chunk);
        let packed = (u32::from(block[0]) << 16) | (u32::from(block[1]) << 8) | u32::from(block[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let shift = 18 - index * 6;
                out.push(ALPHABET[((packed >> shift) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether `url` is something [`open_url`] is willing to hand to the browser.
///
/// Only `http` and `https`, so a message body cannot talk msgs into launching
/// an arbitrary scheme handler.
#[must_use]
pub fn is_web_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.contains(char::is_whitespace)
        && url.len() > "https://".len()
}

/// Open `url` in the default browser.
///
/// # Errors
///
/// Returns [`Error::NotALink`] for anything that is not a plain web link, and
/// [`Error::NotAvailable`] when `open` cannot be run.
pub fn open_url(url: &str) -> Result<(), Error> {
    if !is_web_link(url) {
        return Err(Error::NotALink);
    }
    Command::new("open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| Error::NotAvailable)
}

/// Which of the two ways a file is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opener {
    /// `open`, which is whatever the Finder would use.
    Finder,
    /// Quick Look, which plays an animation rather than paging through it.
    QuickLook,
}

/// Which opener a file gets, decided by its extension alone.
///
/// A GIF handed to `open` lands in Preview.app, which lays its frames out as a
/// list of pages and never plays one. Quick Look — the same preview the Finder
/// gives a file on the spacebar — animates it.
#[must_use]
pub fn opener(path: &Path) -> Opener {
    let animated = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"));
    if animated {
        Opener::QuickLook
    } else {
        Opener::Finder
    }
}

/// Open a file with whatever the Finder would open it with.
///
/// The path comes from the `attachment` table, so it is checked to be a real
/// file first: `open` is given a file and never a URL, and never anything a
/// message body could have chosen.
///
/// # Errors
///
/// Returns [`Error::NotAFile`] when nothing is at the path, and
/// [`Error::NotAvailable`] when `open` cannot be run.
pub fn open_path(path: &Path) -> Result<(), Error> {
    if !path.is_file() {
        return Err(Error::NotAFile);
    }
    // Quick Look is a nicety, so a Mac without `qlmanage` falls back to the
    // ordinary opener rather than failing to open the file at all.
    if opener(path) == Opener::QuickLook && quick_look(path).is_ok() {
        return Ok(());
    }
    Command::new("open")
        .arg("--")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| Error::NotAvailable)
}

/// Open several files at once: one Quick Look window that the arrow keys
/// flip through, or — on a Mac without `qlmanage` — each with `open`.
///
/// # Errors
///
/// Returns [`Error::NotAFile`] when any path is not a file, and
/// [`Error::NotAvailable`] when neither program can be run.
pub fn open_paths(paths: &[PathBuf]) -> Result<(), Error> {
    if paths.iter().any(|path| !path.is_file()) {
        return Err(Error::NotAFile);
    }
    if paths.iter().all(|path| path.is_absolute()) {
        let spawned = Command::new("qlmanage")
            .arg("-p")
            .args(paths)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return Ok(());
        }
    }
    Command::new("open")
        .arg("--")
        .args(paths)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| Error::NotAvailable)
}

/// Show a file in a Quick Look window, the way the spacebar does in the Finder.
///
/// `qlmanage` has no `--` to end its flags, so only an absolute path is handed
/// to it; anything else falls back to `open`, which does.
fn quick_look(path: &Path) -> Result<(), Error> {
    if !path.is_absolute() {
        return Err(Error::NotAvailable);
    }
    Command::new("qlmanage")
        .arg("-p")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| Error::NotAvailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_the_way_the_standard_says() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_osc52_sequence_is_shaped_the_way_terminals_expect() {
        let sequence = osc52_sequence("hi");
        assert!(sequence.starts_with("\x1b]52;c;"));
        assert!(sequence.ends_with('\x07'));
        assert!(sequence.contains(&base64(b"hi")));
    }

    #[test]
    fn only_plain_web_links_are_opened() {
        assert!(is_web_link("https://example.invalid/menu"));
        assert!(is_web_link("HTTP://example.invalid"));
        assert!(!is_web_link("file:///etc/passwd"));
        assert!(!is_web_link("javascript:alert(1)"));
        assert!(!is_web_link("https://"));
        assert!(!is_web_link("https://a b"));
        assert_eq!(
            open_url("mailto:someone@example.invalid"),
            Err(Error::NotALink)
        );
    }

    #[test]
    fn a_gif_is_played_and_everything_else_is_opened() {
        assert_eq!(opener(Path::new("/tmp/clip.gif")), Opener::QuickLook);
        assert_eq!(opener(Path::new("/tmp/CLIP.GIF")), Opener::QuickLook);
        assert_eq!(opener(Path::new("/tmp/a.b/clip.Gif")), Opener::QuickLook);
        assert_eq!(opener(Path::new("/tmp/shot.png")), Opener::Finder);
        assert_eq!(opener(Path::new("/tmp/clip.mov")), Opener::Finder);
        assert_eq!(opener(Path::new("/tmp/gif")), Opener::Finder);
        assert_eq!(opener(Path::new("/tmp/notagif.pdf")), Opener::Finder);
    }

    #[test]
    fn only_a_real_file_is_handed_to_open() {
        let missing = std::env::temp_dir().join("msgs-no-such-attachment.png");
        assert_eq!(open_path(&missing), Err(Error::NotAFile));
        assert_eq!(open_path(std::path::Path::new("/")), Err(Error::NotAFile));
    }
}
