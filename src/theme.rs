//! Named color slots for the UI.
//!
//! Every color the app draws with comes from a slot on [`Theme`] so the palette
//! can be swapped wholesale (and overridden per-slot from `config.toml`) without
//! touching render code. Defaults are the GrokNight-ish values from
//! `docs/mockups.html`; [`Theme::light`] is the same layout on a light ground,
//! chosen with `base = "light"` in `[theme]` or `--theme light`, and
//! [`Theme::terminal`] is the layout on whatever ground the terminal itself
//! draws, asked for with OSC 10/11 at startup ([`query_terminal`]).

use std::collections::BTreeMap;
use std::time::Duration;

use ratatui::style::Color;

/// The full set of color slots used by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Conversation background.
    pub bg_base: Color,
    /// Band behind your own messages, and behind overlays.
    pub bg_light: Color,
    /// Chat list background — one step darker than the conversation.
    pub bg_dark: Color,
    /// Selected row / selected message background.
    pub bg_highlight: Color,
    /// Mouse hover tint.
    pub bg_hover: Color,
    /// Behind text dragged over with the mouse: a blue like a terminal's own
    /// selection, so what is about to be copied is unmistakable, and the one
    /// background that is not a gray.
    pub bg_selection: Color,

    /// The one accent: your own name, an unread chat's time, and links.
    /// Never chrome — focus and the selected chat are grays; the mouse
    /// selection is [`Theme::bg_selection`].
    pub accent_me: Color,

    /// Message bodies, chat names.
    pub text_primary: Color,
    /// Secondary text: previews of unread chats, meta labels.
    pub text_secondary: Color,
    /// Timestamps, hints, dim chrome.
    pub gray: Color,
    /// Dimmer still: the rail of a message with no accent, quote borders.
    pub gray_dim: Color,
    /// System lines (renames, joins, leaves).
    pub system: Color,
    /// Highlighted characters in fuzzy-match results: bold, a step brighter
    /// than the text, and no hue.
    pub fuzzy: Color,

    /// Pane and composer borders at rest.
    pub border: Color,
    /// Border of the focused pane: a step brighter than [`Theme::border`],
    /// and gray like it.
    pub border_active: Color,
    /// Failures: send errors, unreadable database.
    pub error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg_base: rgb(0x12, 0x12, 0x14),
            bg_light: rgb(0x1a, 0x1a, 0x1e),
            bg_dark: rgb(0x0c, 0x0c, 0x0e),
            bg_highlight: rgb(0x23, 0x23, 0x29),
            bg_hover: rgb(0x1f, 0x1f, 0x25),
            bg_selection: rgb(0x26, 0x4f, 0x78),

            accent_me: rgb(0x5e, 0xa8, 0xff),

            text_primary: rgb(0xc8, 0xc8, 0xc8),
            text_secondary: rgb(0xa1, 0xa1, 0xaa),
            gray: rgb(0x52, 0x52, 0x5b),
            gray_dim: rgb(0x3a, 0x3a, 0x40),
            system: rgb(0x8a, 0x8a, 0x94),
            fuzzy: rgb(0xe4, 0xe4, 0xe7),

            border: rgb(0x2c, 0x2c, 0x33),
            border_active: rgb(0x6b, 0x6b, 0x74),
            error: rgb(0xe0, 0x6c, 0x75),
        }
    }
}

/// The bases a config can name, in the order `--help` lists them and
/// `Ctrl+T` cycles through them.
pub const BASES: [&str; 4] = ["dark", "light", "system", "terminal"];

/// Which palette to start from before per-slot overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Base {
    /// The mockup palette.
    Dark,
    /// [`Theme::light`].
    Light,
    /// Whichever of the two macOS is showing, asked on a timer.
    System,
    /// The terminal's own background and foreground, asked once at startup,
    /// with the bands and chrome derived from them. The default.
    #[default]
    Terminal,
}

impl Base {
    /// The base a config value names, case-insensitively.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "system" => Some(Self::System),
            "terminal" => Some(Self::Terminal),
            _ => None,
        }
    }

    /// The name the config and the toast use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
            Self::Terminal => "terminal",
        }
    }

    /// The base after this one in [`BASES`], wrapping around.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::System,
            Self::System => Self::Terminal,
            Self::Terminal => Self::Dark,
        }
    }

    /// Whether this base draws dark, given the system's answer (`None` while
    /// it has not answered yet, which reads as dark) and the terminal's.
    #[must_use]
    pub fn is_dark(self, system_dark: Option<bool>, terminal: Option<&TerminalColors>) -> bool {
        match self {
            Self::Dark => true,
            Self::Light => false,
            Self::System => system_dark.unwrap_or(true),
            Self::Terminal => terminal.is_none_or(TerminalColors::is_dark),
        }
    }
}

/// Ask macOS whether it is in dark mode: `defaults read -g
/// AppleInterfaceStyle` says `Dark` when it is and fails when it is not.
/// `None` when the question could not be asked.
#[must_use]
pub fn system_is_dark() -> Option<bool> {
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim() == "Dark")
    } else {
        Some(false)
    }
}

/// What the terminal said it draws with, from an OSC 10 / OSC 11 query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalColors {
    /// The default background (OSC 11).
    pub bg: (u8, u8, u8),
    /// The default foreground (OSC 10), when the terminal answered that too.
    pub fg: Option<(u8, u8, u8)>,
}

impl TerminalColors {
    /// Whether the ground is dark enough that light text belongs on it.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        luma(self.bg) < 128
    }
}

/// Perceived brightness, 0–255.
fn luma((r, g, b): (u8, u8, u8)) -> u32 {
    (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000
}

/// `a` moved `t` of the way to `b`, `t` in 0–100.
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: u8) -> (u8, u8, u8) {
    let step = |x: u8, y: u8| {
        let x = i32::from(x);
        let y = i32::from(y);
        (x + (y - x) * i32::from(t) / 100).clamp(0, 255) as u8
    };
    (step(a.0, b.0), step(a.1, b.1), step(a.2, b.2))
}

/// Parse one OSC color reply body — what follows `]10;` or `]11;` up to the
/// terminator: `rgb:rrrr/gggg/bbbb` (xterm, Ghostty, kitty, iTerm2) or
/// `rgb:rr/gg/bb`. The first two hex digits of each channel are its byte.
#[must_use]
pub fn parse_osc_color(body: &str) -> Option<(u8, u8, u8)> {
    let mut parts = body.trim().strip_prefix("rgb:")?.split('/');
    let mut channel = || {
        let part = parts.next()?;
        let byte = match part.len() {
            1 => u8::from_str_radix(part, 16).ok()? * 17,
            2..=4 => u8::from_str_radix(&part[..2], 16).ok()?,
            _ => return None,
        };
        Some(byte)
    };
    let r = channel()?;
    let g = channel()?;
    let b = channel()?;
    Some((r, g, b))
}

/// Pick the OSC 10 and OSC 11 replies out of whatever the terminal sent
/// back, in either order and with either terminator (`ESC \` or BEL).
/// `None` until the background has been answered.
#[must_use]
pub fn parse_terminal_replies(bytes: &[u8]) -> Option<TerminalColors> {
    let text = String::from_utf8_lossy(bytes);
    let find = |code: &str| {
        let start = text.find(&format!("\x1b]{code};"))? + code.len() + 3;
        let rest = &text[start..];
        let end = rest.find(['\x1b', '\x07'])?;
        parse_osc_color(&rest[..end])
    };
    Some(TerminalColors {
        bg: find("11")?,
        fg: find("10"),
    })
}

/// Ask the terminal for its default foreground and background. Must run in
/// raw mode, after the alternate screen is up and before the first key is
/// read, because the reply arrives on stdin. A terminal that does not answer
/// within `timeout` — or a stdin that is not a terminal — gives `None`.
#[must_use]
pub fn query_terminal(timeout: Duration) -> Option<TerminalColors> {
    use std::io::{Read, Write};

    // SAFETY: `isatty` only inspects the descriptor.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
        return None;
    }
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b]10;?\x1b\\").ok()?;
    out.flush().ok()?;

    let deadline = std::time::Instant::now() + timeout;
    let mut buf = Vec::new();
    let mut stdin = std::io::stdin();
    loop {
        // Stop once both replies are in; keep reading a moment for the second
        // after the first, and give up at the deadline.
        if let Some(colors) = parse_terminal_replies(&buf)
            && colors.fg.is_some()
        {
            return Some(colors);
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return parse_terminal_replies(&buf);
        }
        let mut fds = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, and its length is 1.
        let ready = unsafe { libc::poll(&mut fds, 1, left.as_millis().min(1000) as i32) };
        if ready <= 0 {
            return parse_terminal_replies(&buf);
        }
        let mut chunk = [0u8; 256];
        match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => return parse_terminal_replies(&buf),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

impl Theme {
    /// The palette a name picks out: `dark` or `light`.
    /// `system` and `terminal` need an answer, so they are not names here.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match Base::parse(name)? {
            Base::Dark => Some(Self::default()),
            Base::Light => Some(Self::light()),
            Base::System | Base::Terminal => None,
        }
    }

    /// The palette for a base, given what the system and the terminal said.
    #[must_use]
    pub fn for_base(
        base: Base,
        system_dark: Option<bool>,
        terminal: Option<&TerminalColors>,
    ) -> Self {
        match base {
            Base::Terminal => Self::terminal(terminal),
            _ if base.is_dark(system_dark, terminal) => Self::default(),
            _ => Self::light(),
        }
    }

    /// The layout on the terminal's own ground: its background exactly, its
    /// foreground for text, and every band and line in between mixed from
    /// the two so the chat list, the selection, and the borders are steps of
    /// the same color. Accents come from the dark or light palette, whichever
    /// the ground calls for. Without an answer the ground and the text are
    /// left to the terminal (`Color::Reset`) and the rest is the dark palette.
    #[must_use]
    pub fn terminal(colors: Option<&TerminalColors>) -> Self {
        let Some(colors) = colors else {
            return Self {
                bg_base: Color::Reset,
                bg_dark: Color::Reset,
                text_primary: Color::Reset,
                ..Self::default()
            };
        };
        let dark = colors.is_dark();
        let accents = if dark { Self::default() } else { Self::light() };
        let bg = colors.bg;
        // Without a foreground answer, the palette's text color stands in as
        // the far end of the mixes, but the text itself stays the terminal's.
        let fg = colors.fg.unwrap_or(if dark {
            (0xc8, 0xc8, 0xc8)
        } else {
            (0x3a, 0x3a, 0x3a)
        });
        let toward_fg = |t| Color::from(mix(bg, fg, t));
        // A dark ground has a long way to go toward its text and a short way
        // to black; a light ground is the reverse. The steps are sized so
        // both come out as the shipped palettes do: the chat list a shade
        // darker, the bands and borders a little way toward the text, the
        // grays far enough along to read.
        let (list_step, band, hover, highlight, border, dim, gray, sys, secondary) = if dark {
            (25, 4, 6, 9, 12, 20, 32, 50, 65)
        } else {
            (2, 5, 6, 9, 13, 27, 50, 60, 75)
        };
        // A light ground's focused divider is a hairline a shade darker, not
        // a dark line down the screen.
        let active = if dark { 40 } else { 25 };
        // The chat list is one step darker than the conversation on both
        // grounds; a ground that cannot get darker steps the other way.
        let mut darker = mix(bg, (0, 0, 0), list_step);
        if luma(bg).abs_diff(luma(darker)) < 2 {
            darker = mix(bg, fg, band);
        }
        Self {
            bg_base: Color::from(bg),
            bg_light: toward_fg(band),
            bg_dark: Color::from(darker),
            bg_highlight: toward_fg(highlight),
            bg_hover: toward_fg(hover),
            text_primary: colors.fg.map_or(Color::Reset, Color::from),
            text_secondary: toward_fg(secondary),
            gray: toward_fg(gray),
            gray_dim: toward_fg(dim),
            system: toward_fg(sys),
            border: toward_fg(border),
            border_active: toward_fg(active),
            ..accents
        }
    }

    /// The base the `[theme]` table asks for, and a warning if it names one
    /// that does not exist.
    #[must_use]
    pub fn base_from(overrides: &BTreeMap<String, String>) -> (Base, Option<String>) {
        let Some(name) = overrides.get("base") else {
            return (Base::default(), None);
        };
        match Base::parse(name) {
            Some(base) => (base, None),
            None => (
                Base::default(),
                Some(format!(
                    "config: unknown theme base `{name}` (one of {})",
                    BASES.join(", ")
                )),
            ),
        }
    }

    /// The layout on a light ground: a soft gray page rather than white, dark
    /// gray text rather than black, and the accent darkened enough to read.
    #[must_use]
    pub fn light() -> Self {
        Self {
            bg_base: rgb(0xf0, 0xf0, 0xf0),
            bg_light: rgb(0xe6, 0xe6, 0xe6),
            bg_dark: rgb(0xec, 0xec, 0xec),
            bg_highlight: rgb(0xde, 0xde, 0xde),
            bg_hover: rgb(0xe4, 0xe4, 0xe4),
            bg_selection: rgb(0xb4, 0xd5, 0xfe),

            accent_me: rgb(0x1f, 0x6f, 0xe5),

            text_primary: rgb(0x3a, 0x3a, 0x3a),
            text_secondary: rgb(0x6a, 0x6a, 0x6a),
            gray: rgb(0x96, 0x96, 0x96),
            gray_dim: rgb(0xc0, 0xc0, 0xc0),
            system: rgb(0x80, 0x80, 0x80),
            fuzzy: rgb(0x1c, 0x1c, 0x1f),

            border: rgb(0xd8, 0xd8, 0xd8),
            border_active: rgb(0xbd, 0xbd, 0xbd),
            error: rgb(0xc0, 0x39, 0x2b),
        }
    }

    /// Border color for a pane, given whether it currently has focus.
    #[must_use]
    pub const fn border_for(&self, focused: bool) -> Color {
        if focused {
            self.border_active
        } else {
            self.border
        }
    }

    /// Overwrite one slot by name. Returns `false` if the name is not a slot.
    pub fn set_slot(&mut self, name: &str, color: Color) -> bool {
        match name {
            "bg_base" => self.bg_base = color,
            "bg_light" => self.bg_light = color,
            "bg_dark" => self.bg_dark = color,
            "bg_highlight" => self.bg_highlight = color,
            "bg_hover" => self.bg_hover = color,
            "bg_selection" => self.bg_selection = color,
            "accent_me" => self.accent_me = color,
            "text_primary" => self.text_primary = color,
            "text_secondary" => self.text_secondary = color,
            "gray" => self.gray = color,
            "gray_dim" => self.gray_dim = color,
            "system" => self.system = color,
            "fuzzy" => self.fuzzy = color,
            "border" => self.border = color,
            "border_active" => self.border_active = color,
            "error" => self.error = color,
            _ => return false,
        }
        true
    }

    /// Apply the `slot = "#rrggbb"` entries of the `[theme]` table on top of
    /// this palette. `base` is not a slot — [`Theme::base_from`] reads it —
    /// so it is stepped over here.
    ///
    /// Bad entries are skipped and described in the returned warnings so the
    /// status line can surface them instead of the app refusing to start.
    pub fn apply_overrides(&mut self, overrides: &BTreeMap<String, String>) -> Vec<String> {
        let mut warnings = Vec::new();
        for (name, value) in overrides {
            if name == "base" {
                continue;
            }
            match parse_color(value) {
                Some(color) => {
                    if !self.set_slot(name, color) {
                        warnings.push(format!("config: unknown theme slot `{name}`"));
                    }
                }
                None => warnings.push(format!("config: `{name}` is not a color: `{value}`")),
            }
        }
        warnings
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Parse a color written as `#rgb`, `#rrggbb`, or a bare ANSI index (`0`–`255`).
#[must_use]
pub fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Ok(index) = value.parse::<u8>() {
        return Some(Color::Indexed(index));
    }
    None
}

fn parse_hex(hex: &str) -> Option<Color> {
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => {
            let mut nibbles = hex.chars().map(|c| c.to_digit(16).unwrap_or(0) as u8);
            let r = nibbles.next()?;
            let g = nibbles.next()?;
            let b = nibbles.next()?;
            Some(Color::Rgb(r * 17, g * 17, b * 17))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_digit_hex() {
        assert_eq!(parse_color("#5ea8ff"), Some(Color::Rgb(0x5e, 0xa8, 0xff)));
        assert_eq!(
            parse_color("  #0C0C0E "),
            Some(Color::Rgb(0x0c, 0x0c, 0x0e))
        );
    }

    #[test]
    fn parses_three_digit_hex_and_ansi_index() {
        assert_eq!(parse_color("#fff"), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(parse_color("#048"), Some(Color::Rgb(0, 0x44, 0x88)));
        assert_eq!(parse_color("14"), Some(Color::Indexed(14)));
    }

    #[test]
    fn rejects_nonsense() {
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gggggg"), None);
        assert_eq!(parse_color("blue"), None);
        assert_eq!(parse_color("999"), None);
    }

    #[test]
    fn overrides_apply_and_report_problems() {
        let mut theme = Theme::default();
        let overrides = BTreeMap::from([
            ("accent_me".to_string(), "#ff0000".to_string()),
            ("gray".to_string(), "#00ff00".to_string()),
            ("participant2".to_string(), "#00ff00".to_string()),
            ("nope".to_string(), "#000000".to_string()),
            ("border".to_string(), "chartreuse".to_string()),
        ]);

        let warnings = theme.apply_overrides(&overrides);

        assert_eq!(theme.accent_me, Color::Rgb(255, 0, 0));
        assert_eq!(theme.gray, Color::Rgb(0, 255, 0));
        assert_eq!(theme.border, Theme::default().border);
        assert_eq!(warnings.len(), 3);
    }

    #[test]
    fn base_is_read_apart_from_the_slots() {
        let overrides = BTreeMap::from([
            ("base".to_string(), "Light".to_string()),
            ("accent_me".to_string(), "#ff0000".to_string()),
        ]);

        let (base, warning) = Theme::base_from(&overrides);
        let mut theme = Theme::for_base(base, None, None);
        let warnings = theme.apply_overrides(&overrides);

        assert_eq!(base, Base::Light);
        assert!(warning.is_none());
        assert!(warnings.is_empty(), "`base` is not an unknown slot");
        assert_eq!(theme.bg_base, Theme::light().bg_base);
        assert_eq!(theme.accent_me, Color::Rgb(255, 0, 0));
    }

    #[test]
    fn unknown_base_warns_and_keeps_the_default() {
        let overrides = BTreeMap::from([("base".to_string(), "sepia".to_string())]);
        let (base, warning) = Theme::base_from(&overrides);
        assert_eq!(base, Base::Terminal);
        assert!(warning.is_some_and(|w| w.contains("system")));
    }

    #[test]
    fn every_base_parses_and_cycles() {
        for name in BASES {
            let base = Base::parse(name).expect(name);
            assert_eq!(base.name(), name);
        }
        assert_eq!(Base::parse("nope"), None);
        assert_eq!(Base::Dark.next().next().next().next(), Base::Dark);
        assert_eq!(Theme::named("terminal"), None);
        assert_eq!(Theme::named("dark"), Some(Theme::default()));
        assert_eq!(Theme::named("system"), None);
    }

    #[test]
    fn system_follows_the_answer_and_reads_dark_until_there_is_one() {
        assert!(Base::System.is_dark(None, None));
        assert!(Base::System.is_dark(Some(true), None));
        assert!(!Base::System.is_dark(Some(false), None));
        assert!(Base::Dark.is_dark(Some(false), None));
        assert!(!Base::Light.is_dark(Some(true), None));
        assert_eq!(
            Theme::for_base(Base::System, Some(false), None),
            Theme::light()
        );
    }

    #[test]
    fn osc_replies_parse_in_any_order_and_either_terminator() {
        assert_eq!(
            parse_osc_color("rgb:2b2b/1f1f/1a1a"),
            Some((0x2b, 0x1f, 0x1a))
        );
        assert_eq!(parse_osc_color("rgb:2b/1f/1a"), Some((0x2b, 0x1f, 0x1a)));
        assert_eq!(parse_osc_color("rgb:2b2b/1f1f"), None);
        assert_eq!(parse_osc_color("#2b1f1a"), None);

        let both = b"\x1b]10;rgb:eeee/e8e8/d5d5\x1b\\\x1b]11;rgb:2b2b/1f1f/1a1a\x07";
        assert_eq!(
            parse_terminal_replies(both),
            Some(TerminalColors {
                bg: (0x2b, 0x1f, 0x1a),
                fg: Some((0xee, 0xe8, 0xd5)),
            })
        );
        let bg_only = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\";
        let colors = parse_terminal_replies(bg_only).expect("background is enough");
        assert_eq!(colors.fg, None);
        assert!(!colors.is_dark());
        assert_eq!(parse_terminal_replies(b"\x1b]10;rgb:0/0/0\x07"), None);
        assert_eq!(parse_terminal_replies(b"\x1b]11;rgb:2b2b/1f1f"), None);
    }

    #[test]
    fn terminal_palette_stands_on_the_terminal_ground() {
        let brown = TerminalColors {
            bg: (0x2b, 0x1f, 0x1a),
            fg: Some((0xee, 0xe8, 0xd5)),
        };
        let theme = Theme::terminal(Some(&brown));
        assert_eq!(theme.bg_base, Color::Rgb(0x2b, 0x1f, 0x1a));
        assert_eq!(theme.text_primary, Color::Rgb(0xee, 0xe8, 0xd5));
        assert_ne!(
            theme.bg_dark, theme.bg_base,
            "the chat list is its own step"
        );
        assert_ne!(theme.bg_highlight, theme.bg_base);
        assert_eq!(theme.accent_me, Theme::default().accent_me, "dark accents");
        assert_eq!(
            theme.bg_selection,
            Theme::default().bg_selection,
            "the drag tint is the dark palette's blue"
        );
        assert!(Base::Terminal.is_dark(None, Some(&brown)));

        let paper = TerminalColors {
            bg: (0xfd, 0xf6, 0xe3),
            fg: None,
        };
        let theme = Theme::terminal(Some(&paper));
        assert_eq!(theme.accent_me, Theme::light().accent_me, "light accents");
        // A light ground steps down gently for the list, not into gray.
        let Color::Rgb(r, _, _) = theme.bg_dark else {
            panic!("an rgb list ground");
        };
        assert!(
            r >= 0xf0,
            "list ground {r:#x} is barely darker than the page"
        );
        assert_eq!(
            theme.text_primary,
            Color::Reset,
            "no answer, so the terminal's"
        );
        assert_ne!(theme.text_secondary, theme.bg_base);

        let black = TerminalColors {
            bg: (0, 0, 0),
            fg: None,
        };
        let theme = Theme::terminal(Some(&black));
        assert_ne!(theme.bg_dark, theme.bg_base, "black steps the other way");

        let unknown = Theme::terminal(None);
        assert_eq!(unknown.bg_base, Color::Reset);
        assert_eq!(unknown.text_primary, Color::Reset);
        assert_eq!(Theme::for_base(Base::Terminal, Some(false), None), unknown);
        assert!(Base::Terminal.is_dark(None, None));
    }

    #[test]
    fn light_and_dark_disagree_on_the_ground() {
        assert_ne!(Theme::light().bg_base, Theme::default().bg_base);
        assert_ne!(Theme::light().accent_me, Theme::default().accent_me);
    }

    #[test]
    fn border_follows_focus() {
        let theme = Theme::default();
        assert_eq!(theme.border_for(true), theme.border_active);
        assert_eq!(theme.border_for(false), theme.border);
    }
}
