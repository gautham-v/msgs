//! Pictures in the transcript, and the files behind the chips.
//!
//! Three separate jobs live here, kept apart on purpose:
//!
//! - [`fit`] is pure arithmetic: how many cells a picture of a given pixel size
//!   is allowed to cover. [`super::ui::message::block`] calls it through
//!   [`Images::cells`] to reserve rows, so a block's height is decided by the
//!   same number the drawing uses and the two can never disagree.
//! - [`Images`] is the cache: what a picture measures, and — once it has been
//!   on screen — the encoded protocol data for it. Terminals that speak the
//!   kitty graphics protocol get real pixels; everything else falls back to
//!   unicode half-blocks, which every terminal can draw.
//! - [`reel`] and [`Animation`] are the moving part: a GIF's frames, decoded
//!   and encoded on the worker thread and then played by the event loop. Every
//!   frame is encoded at the size the still was measured at, so a picture that
//!   starts moving never changes the height of the block it is in.
//! - [`convert`], [`poster`] and [`save_to_downloads`] are the file-system
//!   errands: HEIC through `sips` into a cached JPEG, a video's poster frame
//!   through `qlmanage` into a cached PNG, and `s` copying an attachment out to
//!   `~/Downloads`. [`prep`] is the one place that says which route a file
//!   takes, so measuring and drawing can never pick different ones.
//!
//! Nothing here logs a filename or reads a message body. The bytes go from
//! `~/Library/Messages/Attachments` to the screen and nowhere else, and the one
//! copy msgs ever makes is the one the reader asked for by pressing `s`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use ratatui_image::{FontSize, Resize};

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, ImageDecoder, metadata::Orientation};

use crate::db::AttachmentRef;

/// The tallest an inline picture is ever drawn, in rows.
pub const MAX_ROWS: u16 = 10;
/// The widest an inline picture is ever drawn, in columns. Wide terminals would
/// otherwise blow a photo up to the whole pane.
pub const MAX_COLS: u16 = 48;
/// What a block says when the bytes never made it to this Mac.
pub const NOT_DOWNLOADED: &str = "(not downloaded on this Mac)";

/// Pictures bigger than this are left as a chip rather than decoded. A photo
/// out of a phone is a few megabytes; a hundred-megabyte file is something
/// else, and decoding it would stall the frame.
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;

/// How many encoded pictures are kept at once. Measured sizes are kept forever
/// — they are two numbers each, and dropping one would change a block's height
/// under the reader — but the encoded pixels behind them are evicted oldest
/// first, so a long scroll through a photo thread does not grow without bound.
const MAX_ENCODED: usize = 32;

/// How many frames of an animation are ever decoded. A longer GIF plays its
/// first [`MAX_FRAMES`] and loops there rather than costing the whole file.
pub const MAX_FRAMES: usize = 48;

/// What the decoded frames of one animation may come to. A GIF over the cap is
/// left as its first frame: playing it is not worth the memory.
pub const MAX_FRAME_BYTES: u64 = 24 * 1024 * 1024;

/// The shortest a frame is ever shown. GIFs in the wild ask for nothing at
/// all, which would have the event loop spinning instead of waiting.
const MIN_DELAY: Duration = Duration::from_millis(30);

/// What a frame that asks for no delay is shown for, which is what a browser
/// does with one.
const DEFAULT_DELAY: Duration = Duration::from_millis(100);

/// How many animations are kept encoded past the ones on screen. The pictures
/// actually being looked at are never dropped — the screen is what bounds
/// those — so this only decides how far back a scroll can go and still find a
/// GIF still moving.
const MAX_PLAYING: usize = 4;

/// How the terminal is drawing pictures, for `--check` and the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Inline images are switched off.
    Off,
    /// The kitty graphics protocol: real pixels.
    Kitty,
    /// iTerm2's inline-image escape.
    Iterm2,
    /// Sixels.
    Sixel,
    /// Unicode half-blocks, which any terminal can draw.
    Halfblocks,
}

impl Backend {
    /// The word `--check` prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Kitty => "kitty graphics protocol",
            Self::Iterm2 => "iTerm2 inline images",
            Self::Sixel => "sixels",
            Self::Halfblocks => "unicode half-blocks",
        }
    }

    /// Whether pictures are drawn at all.
    #[must_use]
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// How an attachment is turned into something the `image` crate can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prep {
    /// Decode the file as it is.
    Direct,
    /// HEIC through `sips`, which is what an iPhone camera sends.
    Sips,
    /// A video's poster frame through `qlmanage`, the still Quick Look shows.
    QuickLook,
    /// Nothing here can draw it. It stays a chip.
    Skip,
}

/// Which of the three routes an attachment takes to the screen.
#[must_use]
pub fn prep(attachment: &AttachmentRef) -> Prep {
    let heic = |text: &str| {
        let lower = text.to_ascii_lowercase();
        lower.contains("heic") || lower.contains("heif")
    };
    let is_heic = attachment.mime_type.as_deref().is_some_and(heic)
        || attachment.uti.as_deref().is_some_and(heic)
        || attachment.filename.as_deref().is_some_and(heic);
    if is_heic {
        return Prep::Sips;
    }
    if attachment.kind() == crate::db::AttachmentKind::Video {
        return Prep::QuickLook;
    }
    if attachment.is_image() {
        return Prep::Direct;
    }
    Prep::Skip
}

/// Whether a file has to be turned into a readable still first.
#[must_use]
pub fn needs_conversion(attachment: &AttachmentRef) -> bool {
    matches!(prep(attachment), Prep::Sips | Prep::QuickLook)
}

/// Where converted pictures are kept: `~/Library/Caches/msgs/attachments`.
///
/// `None` when there is no home directory to put it under.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("msgs").join("attachments"))
}

/// The cached still an attachment converts to: a JPEG for HEIC, a PNG poster
/// frame for a video.
///
/// Named after the attachment's `guid`, which is what `chat.db` guarantees to
/// be unique and stable, so a second session reuses the first one's work. The
/// two conversions get different names so a video never collides with a photo.
#[must_use]
pub fn converted_path(attachment: &AttachmentRef) -> Option<PathBuf> {
    let stem: String = attachment
        .guid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if stem.is_empty() {
        return None;
    }
    let name = match prep(attachment) {
        Prep::QuickLook => format!("{stem}-poster.png"),
        _ => format!("{stem}.jpg"),
    };
    Some(cache_dir()?.join(name))
}

/// Ask Quick Look for a video's poster frame, the still Finder shows.
///
/// `qlmanage -t` writes `<name>.png` into an output directory, so this renders
/// into a scratch directory beside the cache and moves the one file it made
/// onto `target`. Returns whether a readable still now exists.
pub fn poster(source: &Path, target: &Path) -> bool {
    if target.is_file() {
        return true;
    }
    let Some(dir) = target.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    private(dir, 0o700);
    let scratch = dir.join(format!(
        "poster-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    if std::fs::create_dir_all(&scratch).is_err() {
        return false;
    }
    private(&scratch, 0o700);
    let ran = Command::new("qlmanage")
        .arg("-t")
        .arg("-s")
        .arg("640")
        .arg("-o")
        .arg(&scratch)
        .arg(source)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let made = matches!(ran, Ok(status) if status.success())
        .then(|| {
            std::fs::read_dir(&scratch).ok().and_then(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .find(|path| path.is_file())
            })
        })
        .flatten();
    let landed = made.is_some_and(|path| std::fs::rename(&path, target).is_ok());
    let _ = std::fs::remove_dir_all(&scratch);
    if landed {
        private(target, 0o600);
    }
    landed && target.is_file()
}

/// Turn `source` into a JPEG at `target` with `sips`, the converter macOS ships.
///
/// Does nothing when the target is already there, so this is cheap to call
/// again. Returns whether a readable JPEG now exists.
pub fn convert(source: &Path, target: &Path) -> bool {
    if target.is_file() {
        return true;
    }
    let Some(dir) = target.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    private(dir, 0o700);
    let ran = Command::new("sips")
        .arg("-s")
        .arg("format")
        .arg("jpeg")
        .arg(source)
        .arg("--out")
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match ran {
        Ok(status) if status.success() => {
            private(target, 0o600);
            target.is_file()
        }
        _ => false,
    }
}

/// Copy an attachment into `~/Downloads`, without overwriting anything.
///
/// A name already taken gets ` (2)`, ` (3)`, and so on before the extension,
/// the way a browser download does.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory cannot be made
/// or the copy fails, and a `NotFound` error when there is no home directory.
pub fn save_to_downloads(source: &Path) -> std::io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))?;
    let dir = dirs::download_dir().unwrap_or_else(|| home.join("Downloads"));
    std::fs::create_dir_all(&dir)?;
    let name = source
        .file_name()
        .map_or_else(|| "attachment".to_string(), |n| n.to_string_lossy().into());
    let target = free_name(&dir, &name);
    std::fs::copy(source, &target)?;
    Ok(target)
}

/// The first name in `dir` that nothing is using yet.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem} ({n}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}

#[cfg(unix)]
fn private(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn private(_path: &Path, _mode: u32) {}

/// How many cells a picture of `pixels` covers at `font`, never more than
/// `max_cols` × `max_rows`, and never stretched.
///
/// This mirrors what `ratatui-image` does when it fits an image into an area,
/// so the rows a block reserves are the rows the picture actually fills.
/// `None` for a degenerate size, which is how a corrupt header comes back.
#[must_use]
pub fn fit(pixels: (u32, u32), font: FontSize, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
    let (width, height) = pixels;
    if width == 0 || height == 0 || max_cols == 0 || max_rows == 0 {
        return None;
    }
    let cell_width = u32::from(font.width.max(1));
    let cell_height = u32::from(font.height.max(1));
    let cells = |w: u32, h: u32| {
        (
            u16::try_from(w.div_ceil(cell_width))
                .unwrap_or(u16::MAX)
                .max(1),
            u16::try_from(h.div_ceil(cell_height))
                .unwrap_or(u16::MAX)
                .max(1),
        )
    };

    let natural = cells(width, height);
    if natural.0 <= max_cols && natural.1 <= max_rows {
        return Some(natural);
    }
    let room_width = (u32::from(max_cols) * cell_width).min(width);
    let room_height = (u32::from(max_rows) * cell_height).min(height);
    let ratio = f64::min(
        f64::from(room_width) / f64::from(width),
        f64::from(room_height) / f64::from(height),
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let scaled = (
        ((f64::from(width) * ratio).round() as u32).max(1),
        ((f64::from(height) * ratio).round() as u32).max(1),
    );
    let fitted = cells(scaled.0, scaled.1);
    Some((fitted.0.min(max_cols), fitted.1.min(max_rows)))
}

/// A decoder for a file on disk, format guessed from the bytes.
///
/// Both decode sites go through this so they read the same file the same way,
/// and so [`image::ImageDecoder::orientation`] — the EXIF tag an iPhone photo
/// carries instead of rotated pixels — is available to both.
fn decoder(path: &Path) -> Option<impl image::ImageDecoder> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()
}

/// The size a picture is once its EXIF orientation has been applied.
///
/// The quarter-turns swap width and height, which is why this and
/// [`decode_oriented`] have to agree: [`fit`] reserves rows from this number
/// and the drawing uses the rotated image.
fn oriented_dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut decoder = decoder(path)?;
    let (width, height) = decoder.dimensions();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    Some(match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    })
}

/// Decode a picture and turn it the way it was shot.
fn decode_oriented(path: &Path) -> Option<image::DynamicImage> {
    let mut decoder = decoder(path)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some(image)
}

/// Whether an attachment is a GIF, the one thing here that moves.
///
/// Only a file that is decoded as it is: a HEIC and a video both reach the
/// screen as a converted still, and neither of those has frames to play.
#[must_use]
pub fn is_animated(attachment: &AttachmentRef) -> bool {
    if prep(attachment) != Prep::Direct {
        return false;
    }
    let gif = |text: &str| text.to_ascii_lowercase().contains("gif");
    attachment.mime_type.as_deref().is_some_and(gif)
        || attachment.uti.as_deref().is_some_and(gif)
        || attachment
            .filename
            .as_deref()
            .and_then(|name| name.rsplit_once('.'))
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("gif"))
}

/// The frames of an animation, decoded and waiting to be encoded.
pub struct Reel {
    /// Every frame, all the size of the first: a GIF's own canvas.
    pub frames: Vec<image::RgbaImage>,
    /// How long each frame is shown, in step with [`Reel::frames`].
    pub delays: Vec<Duration>,
}

/// Decode `path` as an animation, inside the caps.
///
/// `None` for everything that should stay a still: a file that is not a
/// readable GIF, one that holds a single frame, and one whose decoded frames
/// would come to more than `max_bytes`. Frames past `max_frames` are simply
/// left undecoded, so a very long GIF costs a bounded amount of work rather
/// than being refused.
#[must_use]
pub fn reel(path: &Path, max_frames: usize, max_bytes: u64) -> Option<Reel> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = GifDecoder::new(std::io::BufReader::new(file)).ok()?;
    let mut frames: Vec<image::RgbaImage> = Vec::new();
    let mut delays = Vec::new();
    let mut bytes: u64 = 0;
    for frame in decoder.into_frames() {
        // A truncated GIF keeps whatever decoded cleanly rather than losing
        // the animation over its last frame.
        let Ok(frame) = frame else { break };
        let delay = Duration::from(frame.delay());
        let buffer = frame.into_buffer();
        // Every frame is the canvas the GIF was authored at, which is what
        // lets all of them encode to one size — and so to one block height.
        if frames
            .first()
            .is_some_and(|first| first.dimensions() != buffer.dimensions())
        {
            break;
        }
        bytes = bytes.saturating_add(u64::from(buffer.width()) * u64::from(buffer.height()) * 4);
        if bytes > max_bytes {
            return None;
        }
        frames.push(buffer);
        delays.push(if delay.is_zero() {
            DEFAULT_DELAY
        } else {
            delay.max(MIN_DELAY)
        });
        if frames.len() >= max_frames {
            break;
        }
    }
    (frames.len() > 1).then_some(Reel { frames, delays })
}

/// Encode every frame at `size`, so playing one is a lookup and a draw.
///
/// `None` when a frame will not encode, and when one comes back a size other
/// than the still's: a picture that changed size mid-play would move every
/// message under it, so the animation is dropped and the still stands.
fn encode_reel(
    picker: &Picker,
    reel: Reel,
    size: Size,
) -> Option<(Vec<SlicedProtocol>, Vec<Duration>)> {
    let mut frames = Vec::with_capacity(reel.frames.len());
    for buffer in reel.frames {
        let image = image::DynamicImage::ImageRgba8(buffer);
        let protocol =
            SlicedProtocol::new_with_resize(picker, image, size, Resize::Fit(None)).ok()?;
        if protocol.size() != size {
            return None;
        }
        frames.push(protocol);
    }
    Some((frames, reel.delays))
}

/// A GIF on screen: every frame already encoded, and which one is up.
///
/// The frames all report the size the still was measured at — a reel that
/// encodes to anything else is thrown away rather than drawn — so
/// [`Images::cells`] answers the same number whether a picture is playing or
/// standing still.
pub struct Animation {
    frames: Vec<SlicedProtocol>,
    delays: Vec<Duration>,
    current: usize,
    /// When the frame on screen stops being the right one.
    due: Instant,
}

impl Animation {
    /// Build one, `None` unless there are at least two frames and a delay for
    /// each of them.
    fn new(frames: Vec<SlicedProtocol>, delays: Vec<Duration>, now: Instant) -> Option<Self> {
        let first = *delays.first()?;
        (frames.len() > 1 && frames.len() == delays.len()).then(|| Self {
            frames,
            delays,
            current: 0,
            due: now + first,
        })
    }

    /// The frame to draw.
    fn frame(&self) -> &SlicedProtocol {
        &self.frames[self.current]
    }

    /// The size every frame reports, which is the size the block reserved.
    fn size(&self) -> Size {
        self.frame().size()
    }

    /// Step to the frame `now` calls for. `true` when the picture changed.
    ///
    /// One lap at most: a window left in the background for an hour comes back
    /// moving again rather than replaying every frame it missed.
    fn advance(&mut self, now: Instant) -> bool {
        if now < self.due {
            return false;
        }
        for _ in 0..self.frames.len() {
            self.current = (self.current + 1) % self.frames.len();
            self.due += self.delays[self.current];
            if now < self.due {
                return true;
            }
        }
        self.due = now + self.delays[self.current];
        true
    }

    /// How long until this picture is due for its next frame.
    fn due_in(&self, now: Instant) -> Duration {
        self.due.saturating_duration_since(now)
    }
}

/// What the worker thread is asked for. Both jobs are the work that must not
/// happen on the thread that draws.
enum Job {
    /// A HEIC, or a video's poster frame, through the tool macOS ships.
    Convert(PathBuf, PathBuf, Prep),
    /// A GIF's frames, decoded and then encoded for the terminal.
    Animate(Key, PathBuf, Size, Box<Picker>),
}

/// A finished animation on its way back to the cache.
type Encoded = (Key, Vec<SlicedProtocol>, Vec<Duration>);

/// What the cache knows about one attachment.
enum Entry {
    /// The size is known; the protocol data has not been built yet.
    Sized(Size),
    /// Ready to draw.
    Ready(Size, Box<SlicedProtocol>),
    /// Ready to draw, and moving: a GIF with every frame encoded.
    Playing(Size, Box<Animation>),
    /// Not a picture msgs can draw. It stays a chip.
    Unusable,
}

impl Entry {
    const fn size(&self) -> Option<Size> {
        match self {
            Self::Sized(size) | Self::Ready(size, _) | Self::Playing(size, _) => Some(*size),
            Self::Unusable => None,
        }
    }
}

/// The key an entry is filed under: the attachment, and the pane width it was
/// measured for. A resized terminal re-measures rather than drawing a stale
/// picture at the wrong size.
type Key = (i64, u16);

/// Pictures, measured once and encoded once.
///
/// Held by the app and consulted from two places that must agree: the layout,
/// which asks [`Images::cells`] how many rows to leave, and the drawing, which
/// asks [`Images::render`] to put the picture in them. Both take `&self` —
/// the cache is behind a [`RefCell`] — because drawing a frame only ever has a
/// shared borrow of the app.
pub struct Images {
    backend: Backend,
    picker: Option<Picker>,
    entries: RefCell<HashMap<Key, Entry>>,
    /// Conversions handed to the worker, so none is queued twice.
    queued: RefCell<Vec<i64>>,
    /// Encoded pictures in the order they were encoded, for eviction.
    encoded: RefCell<Vec<Key>>,
    /// Whether GIFs play. `false` leaves every one of them on its first frame.
    animate: bool,
    /// Animations already asked for, so none is decoded twice.
    asked: RefCell<Vec<Key>>,
    /// Encoded animations, oldest first, for [`Images::retire`].
    playing: RefCell<Vec<Key>>,
    /// Pictures drawn on the last frame: the only ones a tick advances, so a
    /// GIF far up the scrollback costs nothing.
    visible: RefCell<Vec<Key>>,
    jobs: Option<Sender<Job>>,
    /// Finished animations coming back from the worker.
    reels: Option<Receiver<Encoded>>,
    /// Set by the worker when a conversion lands, so the app knows to measure
    /// the page again and let the picture in.
    arrived: Arc<AtomicBool>,
}

impl Default for Images {
    fn default() -> Self {
        Self::off()
    }
}

impl std::fmt::Debug for Images {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Images")
            .field("backend", &self.backend)
            .field("cached", &self.entries.borrow().len())
            .field("animate", &self.animate)
            .field("playing", &self.playing.borrow().len())
            .finish()
    }
}

impl Images {
    /// A cache that draws nothing, which is what the tests and `--no-images`
    /// use.
    #[must_use]
    pub fn off() -> Self {
        Self {
            backend: Backend::Off,
            picker: None,
            entries: RefCell::new(HashMap::new()),
            queued: RefCell::new(Vec::new()),
            encoded: RefCell::new(Vec::new()),
            animate: false,
            asked: RefCell::new(Vec::new()),
            playing: RefCell::new(Vec::new()),
            visible: RefCell::new(Vec::new()),
            jobs: None,
            reels: None,
            arrived: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Ask the terminal what it can draw, and take the best of it.
    ///
    /// This writes a query to stdout and reads the reply, so it must run after
    /// the alternate screen is entered and before any key is read. Terminals
    /// that answer nothing fall back to half-blocks.
    #[must_use]
    pub fn detect() -> Self {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        Self::with_picker(picker)
    }

    /// A cache that draws with unicode half-blocks, which ask nothing of the
    /// terminal. This is what the tests use, and what any terminal without a
    /// graphics protocol falls back to.
    #[must_use]
    pub fn halfblocks() -> Self {
        Self::with_picker(Picker::halfblocks())
    }

    /// Build a cache around an already-made picker, which is what the tests do.
    #[must_use]
    pub fn with_picker(picker: Picker) -> Self {
        let backend = match picker.protocol_type() {
            ProtocolType::Kitty => Backend::Kitty,
            ProtocolType::Iterm2 => Backend::Iterm2,
            ProtocolType::Sixel => Backend::Sixel,
            ProtocolType::Halfblocks => Backend::Halfblocks,
        };
        let arrived = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = channel::<Job>();
        let (done, reels) = channel::<Encoded>();
        let flag = Arc::clone(&arrived);
        // `sips` and `qlmanage` are a process launch per picture, and a GIF is
        // a decode and an encode per frame. All of it happens off the UI thread
        // so a thread full of photos never stalls a keystroke; the flag and the
        // channel are how the finished work gets back onto the screen.
        let spawned = std::thread::Builder::new()
            .name("msgs-convert".to_string())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    match job {
                        Job::Convert(source, target, how) => {
                            let made = match how {
                                Prep::QuickLook => poster(&source, &target),
                                _ => convert(&source, &target),
                            };
                            if made {
                                flag.store(true, Ordering::SeqCst);
                            }
                        }
                        Job::Animate(key, path, size, picker) => {
                            let Some(reel) = reel(&path, MAX_FRAMES, MAX_FRAME_BYTES) else {
                                continue;
                            };
                            let Some((frames, delays)) = encode_reel(&picker, reel, size) else {
                                continue;
                            };
                            if done.send((key, frames, delays)).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .is_ok();

        Self {
            backend,
            picker: Some(picker),
            entries: RefCell::new(HashMap::new()),
            queued: RefCell::new(Vec::new()),
            encoded: RefCell::new(Vec::new()),
            animate: true,
            asked: RefCell::new(Vec::new()),
            playing: RefCell::new(Vec::new()),
            visible: RefCell::new(Vec::new()),
            jobs: spawned.then_some(sender),
            reels: spawned.then_some(reels),
            arrived,
        }
    }

    /// Whether GIFs play, which is what `animate` in the config and
    /// `--no-animate` decide. A cache that does not animate draws every GIF's
    /// first frame and nothing else.
    #[must_use]
    pub fn animated(mut self, on: bool) -> Self {
        self.animate = on;
        self
    }

    /// Whether GIFs play.
    #[must_use]
    pub const fn animates(&self) -> bool {
        self.animate
    }

    /// How pictures are being drawn.
    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// Whether a conversion finished since this was last asked, which is the
    /// signal to measure the page again.
    pub fn take_arrived(&self) -> bool {
        self.arrived.swap(false, Ordering::SeqCst)
    }

    /// How many cells `attachment` gets, at a body `room` columns wide.
    ///
    /// `None` means it is not drawn inline and stays a chip: not a picture or
    /// a video, not downloaded, a format nothing here can read, or one whose
    /// conversion has not finished yet.
    #[must_use]
    pub fn cells(&self, attachment: &AttachmentRef, room: u16) -> Option<(u16, u16)> {
        let route = prep(attachment);
        if !self.backend.is_on() || route == Prep::Skip || attachment.hide_attachment {
            return None;
        }
        let key = (attachment.rowid, room);
        if let Some(entry) = self.entries.borrow().get(&key) {
            return entry.size().map(|size| (size.width, size.height));
        }
        let entry = self.measure(attachment, room);
        let size = entry.size();
        self.entries.borrow_mut().insert(key, entry);
        size.map(|size| (size.width, size.height))
    }

    /// Work out an attachment's size, converting or queueing a conversion first
    /// where one is needed.
    fn measure(&self, attachment: &AttachmentRef, room: u16) -> Entry {
        let Some(picker) = self.picker.as_ref() else {
            return Entry::Unusable;
        };
        let Some(source) = attachment.path().filter(|path| path.is_file()) else {
            return Entry::Unusable;
        };
        let route = prep(attachment);
        let readable = if route == Prep::Direct {
            source
        } else {
            let Some(target) = converted_path(attachment) else {
                return Entry::Unusable;
            };
            if !target.is_file() {
                self.queue(attachment.rowid, source, target, route);
                // Not a failure: the chip stands in until the still lands.
                return Entry::Unusable;
            }
            target
        };
        // The guard belongs to the file that is actually decoded: a video is
        // routinely larger than the cap while its poster frame is tiny.
        if std::fs::metadata(&readable).is_ok_and(|meta| meta.len() > MAX_DECODE_BYTES) {
            return Entry::Unusable;
        }
        let Some(dimensions) = oriented_dimensions(&readable) else {
            return Entry::Unusable;
        };
        let columns = room.min(MAX_COLS);
        match fit(dimensions, picker.font_size(), columns, MAX_ROWS) {
            Some((width, height)) => Entry::Sized(Size::new(width, height)),
            None => Entry::Unusable,
        }
    }

    /// Hand one conversion to the converter thread, at most once per
    /// attachment.
    fn queue(&self, rowid: i64, source: PathBuf, target: PathBuf, how: Prep) {
        let Some(jobs) = self.jobs.as_ref() else {
            return;
        };
        let mut queued = self.queued.borrow_mut();
        if queued.contains(&rowid) {
            return;
        }
        if jobs.send(Job::Convert(source, target, how)).is_ok() {
            queued.push(rowid);
        }
    }

    /// Forget every attachment that came back undrawable, so the next measure
    /// looks again.
    ///
    /// Called when the converter reports something landed. The list of what has
    /// already been handed to the converter is deliberately kept, so a
    /// conversion that failed is not asked for over and over.
    pub fn reconsider(&self) {
        self.entries
            .borrow_mut()
            .retain(|_, entry| !matches!(entry, Entry::Unusable));
    }

    /// How many pictures are currently encoded, for the eviction test.
    #[cfg(test)]
    fn encoded_count(&self) -> usize {
        self.entries
            .borrow()
            .values()
            .filter(|entry| matches!(entry, Entry::Ready(..)))
            .count()
    }

    /// Draw `attachment` into `area`, with its top edge `offset` rows below the
    /// top of `area` — negative when it has scrolled off the top.
    ///
    /// `room` is the body width [`Images::cells`] was asked with, which is what
    /// the picture is filed under; passing a different one would draw nothing.
    ///
    /// Encoding happens here, the first time a picture is actually on screen,
    /// so opening a thread full of photos only costs the ones being looked at.
    pub fn render(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        offset: i16,
        attachment: &AttachmentRef,
        room: u16,
    ) {
        let key = (attachment.rowid, room);
        self.encode(attachment, key);
        let entries = self.entries.borrow();
        let protocol = match entries.get(&key) {
            Some(Entry::Ready(_, protocol)) => protocol.as_ref(),
            Some(Entry::Playing(_, animation)) => animation.frame(),
            _ => return,
        };
        SlicedImage::new(protocol, SignedPosition::from((0, offset))).render(area, buffer);
        drop(entries);
        // On screen this frame, which is what decides whether it is worth
        // waking the loop up for.
        let mut visible = self.visible.borrow_mut();
        if !visible.contains(&key) {
            visible.push(key);
        }
    }

    /// Forget what was on screen, at the top of every frame.
    ///
    /// [`Images::render`] fills the list back in as the pictures are drawn, so
    /// a GIF that scrolled away stops costing anything the moment it is no
    /// longer painted.
    pub fn begin_frame(&self) {
        self.visible.borrow_mut().clear();
    }

    /// Take in whatever animations the worker finished. `true` when one landed,
    /// which is a redraw.
    pub fn absorb(&self) -> bool {
        let Some(reels) = self.reels.as_ref() else {
            return false;
        };
        let now = Instant::now();
        let mut landed = false;
        // Empty and disconnected are the same answer here: nothing more is
        // coming this frame.
        while let Ok((key, frames, delays)) = reels.try_recv() {
            landed |= self.install(key, frames, delays, now);
        }
        if landed {
            self.retire();
        }
        landed
    }

    /// Put one finished animation over the still it was decoded for.
    ///
    /// Refused unless the frames report exactly the size the entry is already
    /// holding: the rows the block reserved are the rows the picture gets,
    /// whether or not it moves.
    fn install(
        &self,
        key: Key,
        frames: Vec<SlicedProtocol>,
        delays: Vec<Duration>,
        now: Instant,
    ) -> bool {
        let Some(animation) = Animation::new(frames, delays, now) else {
            return false;
        };
        let mut entries = self.entries.borrow_mut();
        // A resize since the job went out has already thrown the entry away,
        // and the frames were encoded for a size nothing is asking for now.
        let Some(size) = entries.get(&key).and_then(Entry::size) else {
            return false;
        };
        if size != animation.size() {
            return false;
        }
        entries.insert(key, Entry::Playing(size, Box::new(animation)));
        drop(entries);
        self.playing.borrow_mut().push(key);
        true
    }

    /// Drop the animations nothing is looking at, back to their measured size.
    ///
    /// What is on screen is kept however many that is — the screen itself is
    /// what bounds it, and dropping a picture being watched would only have it
    /// decoded again. A dropped one is forgotten rather than blacklisted, so
    /// scrolling back to it starts it moving again.
    fn retire(&self) {
        let visible = self.visible.borrow();
        let mut playing = self.playing.borrow_mut();
        let mut entries = self.entries.borrow_mut();
        let mut asked = self.asked.borrow_mut();
        let mut index = 0;
        while playing.len() > MAX_PLAYING && index < playing.len() {
            let key = playing[index];
            if visible.contains(&key) {
                index += 1;
                continue;
            }
            playing.remove(index);
            asked.retain(|held| *held != key);
            if let Some(Entry::Playing(size, _)) = entries.get(&key) {
                let size = *size;
                entries.insert(key, Entry::Sized(size));
            }
        }
    }

    /// How long until a picture on screen is due for its next frame.
    ///
    /// `None` when nothing on screen is moving, which is how the event loop
    /// goes back to its ordinary wait.
    #[must_use]
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        let entries = self.entries.borrow();
        self.visible
            .borrow()
            .iter()
            .filter_map(|key| match entries.get(key) {
                Some(Entry::Playing(_, animation)) => Some(animation.due_in(now)),
                _ => None,
            })
            .min()
    }

    /// Step every picture on screen to the frame `now` calls for. `true` when
    /// one of them changed, which is a redraw.
    ///
    /// The frames are already encoded, so this is a comparison and an index.
    pub fn advance(&self, now: Instant) -> bool {
        let visible = self.visible.borrow();
        let mut entries = self.entries.borrow_mut();
        let mut moved = false;
        for key in visible.iter() {
            if let Some(Entry::Playing(_, animation)) = entries.get_mut(key) {
                moved |= animation.advance(now);
            }
        }
        moved
    }

    /// Turn a measured picture into protocol data, once.
    fn encode(&self, attachment: &AttachmentRef, key: Key) {
        let size = match self.entries.borrow().get(&key) {
            Some(Entry::Sized(size)) => *size,
            _ => return,
        };
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let source = if prep(attachment) == Prep::Direct {
            attachment.path().filter(|path| path.is_file())
        } else {
            converted_path(attachment).filter(|path| path.is_file())
        };
        let decoded = source.and_then(|path| decode_oriented(&path));
        let entry = decoded
            .and_then(|image| {
                SlicedProtocol::new_with_resize(picker, image, size, Resize::Fit(None)).ok()
            })
            .map_or(Entry::Unusable, |protocol| {
                Entry::Ready(protocol.size(), Box::new(protocol))
            });
        // The still goes up first, so a GIF is on screen from the frame it is
        // measured on; the frames replace it whenever the worker is done.
        let encoded = match &entry {
            Entry::Ready(size, _) => Some(*size),
            _ => None,
        };
        self.entries.borrow_mut().insert(key, entry);
        if let Some(size) = encoded {
            self.encoded.borrow_mut().push(key);
            self.evict();
            self.queue_animation(attachment, key, size);
        }
    }

    /// Ask the worker for a GIF's frames, at most once per picture.
    ///
    /// `size` is what the still encoded to, and what every frame has to come
    /// back as, so the block's height is decided once.
    fn queue_animation(&self, attachment: &AttachmentRef, key: Key, size: Size) {
        if !self.animate || !is_animated(attachment) {
            return;
        }
        let (Some(jobs), Some(picker)) = (self.jobs.as_ref(), self.picker.as_ref()) else {
            return;
        };
        let Some(path) = attachment.path().filter(|path| path.is_file()) else {
            return;
        };
        let mut asked = self.asked.borrow_mut();
        if asked.contains(&key) {
            return;
        }
        if jobs
            .send(Job::Animate(key, path, size, Box::new(picker.clone())))
            .is_ok()
        {
            asked.push(key);
        }
    }

    /// Drop the oldest encoded pictures back to their measured size, so the
    /// rows they occupy stay the same and only the pixels go.
    fn evict(&self) {
        let mut encoded = self.encoded.borrow_mut();
        let mut entries = self.entries.borrow_mut();
        while encoded.len() > MAX_ENCODED {
            let oldest = encoded.remove(0);
            // A playing picture is not on this list's terms: `retire` is what
            // bounds those, and dropping one here would stop it mid-frame.
            if let Some(Entry::Ready(size, _)) = entries.get(&oldest) {
                let size = *size;
                entries.insert(oldest, Entry::Sized(size));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(mime: &str, name: &str) -> AttachmentRef {
        AttachmentRef {
            rowid: 1,
            guid: "A-1".to_string(),
            message_rowid: 1,
            filename: Some(format!("~/Library/Messages/Attachments/x/{name}")),
            mime_type: Some(mime.to_string()),
            uti: None,
            transfer_name: Some(name.to_string()),
            total_bytes: 1024,
            transfer_state: 5,
            is_sticker: false,
            hide_attachment: false,
        }
    }

    const FONT: FontSize = FontSize::new(10, 20);

    #[test]
    fn a_small_picture_keeps_its_natural_size() {
        assert_eq!(fit((100, 200), FONT, 40, MAX_ROWS), Some((10, 10)));
        assert_eq!(fit((45, 21), FONT, 40, MAX_ROWS), Some((5, 2)));
    }

    #[test]
    fn a_tall_picture_is_capped_at_ten_rows_and_keeps_its_aspect() {
        let (columns, rows) = fit((3024, 4032), FONT, 48, MAX_ROWS).expect("a size");
        assert_eq!(rows, MAX_ROWS);
        // Ten rows is 200 pixels tall, and 3:4 makes that 150 pixels wide,
        // which is fifteen ten-pixel columns.
        assert_eq!(columns, 15);
    }

    #[test]
    fn a_wide_picture_is_capped_by_the_pane() {
        let (columns, rows) = fit((4000, 1000), FONT, 20, MAX_ROWS).expect("a size");
        assert_eq!(columns, 20);
        assert!((1..=MAX_ROWS).contains(&rows), "{rows} rows");
    }

    #[test]
    fn a_degenerate_size_is_not_drawn() {
        assert_eq!(fit((0, 100), FONT, 40, 10), None);
        assert_eq!(fit((100, 0), FONT, 40, 10), None);
        assert_eq!(fit((100, 100), FONT, 0, 10), None);
    }

    #[test]
    fn only_heic_goes_through_sips() {
        assert_eq!(prep(&attachment("image/heic", "IMG.HEIC")), Prep::Sips);
        assert_eq!(prep(&attachment("image/heif", "IMG.heif")), Prep::Sips);
        assert!(needs_conversion(&attachment("image/heic", "IMG.HEIC")));
        assert!(!needs_conversion(&attachment("image/jpeg", "IMG.jpg")));
        assert!(!needs_conversion(&attachment("image/png", "shot.png")));
    }

    #[test]
    fn a_video_takes_the_quick_look_route_and_a_picture_does_not() {
        assert_eq!(
            prep(&attachment("video/quicktime", "clip.mov")),
            Prep::QuickLook
        );
        assert_eq!(prep(&attachment("video/mp4", "clip.mp4")), Prep::QuickLook);
        assert_eq!(prep(&attachment("image/jpeg", "IMG.jpg")), Prep::Direct);
        assert_eq!(prep(&attachment("application/pdf", "a.pdf")), Prep::Skip);
        assert_eq!(prep(&attachment("audio/x-caf", "note.caf")), Prep::Skip);
        assert!(needs_conversion(&attachment("video/quicktime", "clip.mov")));
    }

    #[test]
    fn a_poster_and_a_converted_photo_never_share_a_cache_name() {
        let mut photo = attachment("image/heic", "IMG.HEIC");
        photo.guid = "SAME".to_string();
        let mut video = attachment("video/quicktime", "clip.mov");
        video.guid = "SAME".to_string();
        let photo = converted_path(&photo).expect("a cache path");
        let video = converted_path(&video).expect("a cache path");
        assert!(photo.ends_with("SAME.jpg"));
        assert!(video.ends_with("SAME-poster.png"));
        assert_ne!(photo, video);
    }

    #[test]
    fn a_video_without_a_poster_yet_stays_a_chip() {
        let images = Images::halfblocks();
        // The fixture path does not exist, so no poster can have been made.
        assert_eq!(
            images.cells(&attachment("video/quicktime", "clip.mov"), 40),
            None
        );
    }

    #[test]
    fn a_converted_picture_is_named_after_its_guid() {
        let mut source = attachment("image/heic", "IMG.HEIC");
        source.guid = "AB/CD-12".to_string();
        let path = converted_path(&source).expect("a cache path");
        assert!(path.ends_with("AB-CD-12.jpg"));
        assert!(path.to_string_lossy().contains("msgs"));
    }

    #[test]
    fn a_cache_that_is_off_draws_nothing() {
        let images = Images::off();
        assert_eq!(images.backend(), Backend::Off);
        assert!(!images.backend().is_on());
        assert_eq!(images.cells(&attachment("image/png", "shot.png"), 40), None);
    }

    #[test]
    fn a_missing_file_stays_a_chip() {
        let images = Images::halfblocks();
        assert!(images.backend().is_on());
        // The fixture path does not exist, so there is nothing to draw.
        assert_eq!(images.cells(&attachment("image/png", "shot.png"), 40), None);
        // A file that is not a picture is never even looked at.
        assert_eq!(
            images.cells(&attachment("application/pdf", "a.pdf"), 40),
            None
        );
    }

    /// A real PNG on disk, so the decode path is exercised rather than mocked.
    /// Nothing here touches `chat.db` or the real attachment store.
    fn png(tag: &str, width: u32, height: u32) -> (PathBuf, AttachmentRef) {
        let dir = std::env::temp_dir().join(format!("msgs-media-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("shot.png");
        let buffer =
            image::ImageBuffer::from_pixel(width, height, image::Rgba::<u8>([90, 140, 190, 255]));
        image::DynamicImage::from(buffer)
            .save(&path)
            .expect("a written png");
        let mut attachment = attachment("image/png", "shot.png");
        attachment.filename = Some(path.display().to_string());
        (dir, attachment)
    }

    /// A JPEG carrying nothing but an EXIF orientation tag, written by hand:
    /// `image` can decode the tag but not write one. Synthetic pixels only.
    fn jpeg_with_orientation(tag: &str, width: u32, height: u32, orientation: u16) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("msgs-media-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("shot.jpg");

        let buffer =
            image::ImageBuffer::from_pixel(width, height, image::Rgb::<u8>([90, 140, 190]));
        let mut plain = Vec::new();
        image::DynamicImage::from(buffer)
            .write_to(
                &mut std::io::Cursor::new(&mut plain),
                image::ImageFormat::Jpeg,
            )
            .expect("an encoded jpeg");

        // Little-endian TIFF header, one IFD entry: orientation, SHORT, count 1.
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(b"II\x2a\x00\x08\x00\x00\x00");
        exif.extend_from_slice(&1u16.to_le_bytes());
        exif.extend_from_slice(&0x0112u16.to_le_bytes());
        exif.extend_from_slice(&3u16.to_le_bytes());
        exif.extend_from_slice(&1u32.to_le_bytes());
        exif.extend_from_slice(&orientation.to_le_bytes());
        exif.extend_from_slice(&[0, 0]);
        exif.extend_from_slice(&0u32.to_le_bytes());

        let mut bytes = plain[..2].to_vec(); // SOI
        bytes.extend_from_slice(&[0xff, 0xe1]);
        let length = u16::try_from(exif.len() + 2).expect("a small segment");
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(&exif);
        bytes.extend_from_slice(&plain[2..]);
        std::fs::write(&path, bytes).expect("a written jpeg");
        path
    }

    #[test]
    fn a_quarter_turn_is_applied_and_measured_the_same_way() {
        // Orientation 6 is a quarter turn: a wide picture is drawn tall.
        let path = jpeg_with_orientation("rotate90", 60, 20, 6);
        assert_eq!(oriented_dimensions(&path), Some((20, 60)));
        let decoded = decode_oriented(&path).expect("a decoded picture");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (20, 60),
            "the reserved size and the drawn size are the one number"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("a temp directory"));
    }

    #[test]
    fn a_picture_shot_upright_is_left_alone() {
        let path = jpeg_with_orientation("upright", 60, 20, 1);
        assert_eq!(oriented_dimensions(&path), Some((60, 20)));
        let decoded = decode_oriented(&path).expect("a decoded picture");
        assert_eq!((decoded.width(), decoded.height()), (60, 20));
        let _ = std::fs::remove_dir_all(path.parent().expect("a temp directory"));
    }

    #[test]
    fn a_picture_on_disk_is_measured_once_and_then_drawn() {
        let (dir, attachment) = png("draw", 100, 100);
        let images = Images::halfblocks();
        // The half-blocks picker uses ten-by-twenty cells, so a hundred-pixel
        // square is ten columns and five rows.
        assert_eq!(images.cells(&attachment, 40), Some((10, 5)));
        // A second ask is the cache, not another decode.
        assert_eq!(images.cells(&attachment, 40), Some((10, 5)));

        let blank = Buffer::empty(Rect::new(0, 0, 40, 8));
        let mut buffer = blank.clone();
        images.render(&mut buffer, Rect::new(0, 0, 40, 8), 0, &attachment, 40);
        assert_ne!(buffer, blank, "the picture reached the buffer");
        // Only the ten-by-five block it was measured at, and nothing below it.
        for y in 5..8 {
            for x in 0..40 {
                assert_eq!(buffer[(x, y)], blank[(x, y)], "row {y} is not the picture");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_wider_than_the_pane_is_brought_down_to_it() {
        let (dir, attachment) = png("wide", 400, 100);
        let images = Images::halfblocks();
        let (columns, rows) = images.cells(&attachment, 12).expect("a size");
        assert!(columns <= 12, "{columns} columns");
        assert!(rows <= MAX_ROWS, "{rows} rows");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_so_many_pictures_stay_encoded_and_the_rest_keep_their_size() {
        let (dir, attachment) = png("evict", 60, 40);
        let images = Images::halfblocks();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 4));
        // Every width is its own cache entry, which is the cheapest way to
        // make more encoded pictures than the cap allows.
        for room in 20..20 + u16::try_from(MAX_ENCODED).unwrap_or(0) + 8 {
            assert!(images.cells(&attachment, room).is_some());
            images.render(&mut buffer, Rect::new(0, 0, 20, 4), 0, &attachment, room);
        }
        assert_eq!(images.encoded_count(), MAX_ENCODED);
        // The sizes survive eviction, so no block changes height.
        for room in 20..20 + u16::try_from(MAX_ENCODED).unwrap_or(0) + 8 {
            assert!(images.cells(&attachment, room).is_some());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A synthetic animated GIF: flat frames of a made-up color, written by
    /// `image`'s own encoder. Never a real attachment, and never a file out of
    /// `~/Library/Messages`.
    fn gif(tag: &str, count: u32, delay_ms: u32) -> (PathBuf, PathBuf) {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame, Rgba, RgbaImage};

        let dir = std::env::temp_dir().join(format!("msgs-media-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("clip.gif");
        let file = std::fs::File::create(&path).expect("a file to write");
        let mut encoder = GifEncoder::new(std::io::BufWriter::new(file));
        encoder.set_repeat(Repeat::Infinite).expect("a looping gif");
        for index in 0..count {
            let shade = u8::try_from(40 + index * 40).unwrap_or(u8::MAX);
            // Sixty by forty: six cells by two at the half-blocks picker's
            // ten-by-twenty, so the frames need no resizing to land on cells.
            let buffer = RgbaImage::from_pixel(60, 40, Rgba([shade, 120, 200, 255]));
            encoder
                .encode_frame(Frame::from_parts(
                    buffer,
                    0,
                    0,
                    Delay::from_numer_denom_ms(delay_ms, 1),
                ))
                .expect("an encoded frame");
        }
        drop(encoder);
        (dir, path)
    }

    /// The attachment row a written GIF stands behind.
    fn gif_attachment(path: &Path) -> AttachmentRef {
        let mut attachment = attachment("image/gif", "clip.gif");
        attachment.filename = Some(path.display().to_string());
        attachment
    }

    #[test]
    fn only_a_gif_is_treated_as_something_that_moves() {
        assert!(is_animated(&attachment("image/gif", "clip.gif")));
        assert!(is_animated(&attachment("image/png", "clip.GIF")));
        // A picture whose name says nothing, told apart by its UTI.
        let mut by_uti = attachment("image/x-unknown", "attachment");
        by_uti.uti = Some("com.compuserve.gif".to_string());
        assert!(is_animated(&by_uti));
        assert!(!is_animated(&attachment("image/png", "shot.png")));
        assert!(!is_animated(&attachment("image/heic", "IMG.HEIC")));
        assert!(!is_animated(&attachment("video/quicktime", "clip.mov")));
        assert!(!is_animated(&attachment("application/pdf", "a.pdf")));
    }

    #[test]
    fn a_gifs_frames_come_back_with_their_delays() {
        let (dir, path) = gif("frames", 3, 60);
        let reel = reel(&path, MAX_FRAMES, MAX_FRAME_BYTES).expect("an animation");
        assert_eq!(reel.frames.len(), 3);
        assert_eq!(reel.delays.len(), 3);
        for frame in &reel.frames {
            assert_eq!(frame.dimensions(), (60, 40), "every frame is the canvas");
        }
        for delay in &reel.delays {
            assert_eq!(*delay, Duration::from_millis(60));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_that_asks_for_nothing_is_still_shown_for_a_moment() {
        let (dir, path) = gif("nodelay", 2, 0);
        let reel = reel(&path, MAX_FRAMES, MAX_FRAME_BYTES).expect("an animation");
        for delay in &reel.delays {
            assert!(*delay >= MIN_DELAY, "{delay:?} would spin the event loop");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_caps_leave_a_gif_as_a_still() {
        let (dir, path) = gif("caps", 4, 60);
        // One frame is not an animation, which is what the frame cap comes to
        // when it is asked for fewer than two.
        assert!(reel(&path, 1, MAX_FRAME_BYTES).is_none());
        // Past the frame cap, what is decoded is what was asked for.
        let short = reel(&path, 2, MAX_FRAME_BYTES).expect("an animation");
        assert_eq!(short.frames.len(), 2);
        // 60 x 40 x 4 bytes is 9600 a frame: room for two of the four frames
        // is not room for the animation, and it falls back to a still.
        assert!(reel(&path, MAX_FRAMES, 9_600).is_none());
        assert!(reel(&path, MAX_FRAMES, 19_200).is_none());
        assert!(reel(&path, MAX_FRAMES, 38_400).is_some());
        // Not a gif at all.
        let (still, _) = png("notagif", 60, 40);
        assert!(reel(&still.join("shot.png"), MAX_FRAMES, MAX_FRAME_BYTES).is_none());
        let _ = std::fs::remove_dir_all(&still);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_frame_encodes_to_the_size_the_block_reserved() {
        let (dir, path) = gif("sizes", 3, 60);
        let images = Images::halfblocks();
        let attachment = gif_attachment(&path);
        let (columns, rows) = images.cells(&attachment, 40).expect("a size");
        let size = Size::new(columns, rows);

        let reel = reel(&path, MAX_FRAMES, MAX_FRAME_BYTES).expect("an animation");
        let (frames, delays) =
            encode_reel(&Picker::halfblocks(), reel, size).expect("every frame encoded");
        assert_eq!(frames.len(), 3);
        assert_eq!(delays.len(), 3);
        for frame in &frames {
            assert_eq!(frame.size(), size, "the height cannot move mid-play");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_is_due_when_its_delay_is_up_and_a_long_stall_costs_one_lap() {
        let (dir, path) = gif("timing", 3, 60);
        let reel = reel(&path, MAX_FRAMES, MAX_FRAME_BYTES).expect("an animation");
        let (frames, delays) =
            encode_reel(&Picker::halfblocks(), reel, Size::new(6, 2)).expect("encoded frames");
        let now = Instant::now();
        let mut animation = Animation::new(frames, delays, now).expect("an animation");

        assert!(!animation.advance(now), "nothing is due yet");
        assert_eq!(animation.due_in(now), Duration::from_millis(60));
        assert!(!animation.advance(now + Duration::from_millis(59)));
        assert!(animation.advance(now + Duration::from_millis(60)));
        assert_eq!(animation.current, 1);
        assert!(animation.advance(now + Duration::from_millis(120)));
        assert_eq!(animation.current, 2);
        // Round the loop.
        assert!(animation.advance(now + Duration::from_millis(180)));
        assert_eq!(animation.current, 0);

        // A window left in the background for an hour catches up in one lap
        // rather than replaying every frame it missed.
        let later = now + Duration::from_secs(3600);
        assert!(animation.advance(later));
        assert!(animation.due_in(later) > Duration::ZERO, "it keeps playing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gif_on_screen_plays_and_one_that_is_not_costs_nothing() {
        let (dir, path) = gif("play", 3, 60);
        let images = Images::halfblocks();
        assert!(images.animates());
        let attachment = gif_attachment(&path);
        let (columns, rows) = images.cells(&attachment, 40).expect("a size");

        // Nothing has been drawn, so nothing is on screen to animate.
        assert_eq!(images.next_due(Instant::now()), None);

        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 8));
        let area = Rect::new(0, 0, 40, 8);
        // The still goes up first; the frames land whenever the worker is done
        // with them, and the picture keeps the size it was measured at.
        let deadline = Instant::now() + Duration::from_secs(10);
        let playing = loop {
            images.begin_frame();
            images.render(&mut buffer, area, 0, &attachment, 40);
            if images.absorb() {
                break true;
            }
            if Instant::now() > deadline {
                break false;
            }
            std::thread::yield_now();
        };
        assert!(playing, "the worker encoded the frames");
        assert_eq!(images.cells(&attachment, 40), Some((columns, rows)));

        // On screen and playing: the loop is told when to come back, and the
        // frame moves on when it does.
        images.begin_frame();
        images.render(&mut buffer, area, 0, &attachment, 40);
        let due = images.next_due(Instant::now()).expect("a frame is coming");
        assert!(due <= Duration::from_millis(60), "{due:?}");
        assert!(images.advance(Instant::now() + Duration::from_millis(60)));

        // Scrolled away: nothing is on screen, so the loop goes back to its
        // ordinary wait and no frame is stepped.
        images.begin_frame();
        assert_eq!(images.next_due(Instant::now()), None);
        assert!(!images.advance(Instant::now() + Duration::from_secs(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cache_that_does_not_animate_leaves_a_gif_on_its_first_frame() {
        let (dir, path) = gif("still", 3, 60);
        let images = Images::halfblocks().animated(false);
        assert!(!images.animates());
        let attachment = gif_attachment(&path);
        assert!(images.cells(&attachment, 40).is_some());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 8));
        images.begin_frame();
        images.render(&mut buffer, Rect::new(0, 0, 40, 8), 0, &attachment, 40);
        // Nothing was ever asked for, so nothing can arrive.
        assert!(!images.absorb());
        assert_eq!(images.next_due(Instant::now()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_free_name_steps_around_what_is_already_there() {
        let dir = std::env::temp_dir().join(format!("msgs-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp directory");
        assert_eq!(free_name(&dir, "a.png"), dir.join("a.png"));
        std::fs::write(dir.join("a.png"), b"x").expect("a file");
        assert_eq!(free_name(&dir, "a.png"), dir.join("a (2).png"));
        std::fs::write(dir.join("a (2).png"), b"x").expect("a file");
        assert_eq!(free_name(&dir, "a.png"), dir.join("a (3).png"));
        assert_eq!(free_name(&dir, "noext"), dir.join("noext"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
