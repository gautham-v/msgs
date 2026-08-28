//! Named color slots for the UI.
//!
//! Every color the app draws with comes from a slot on [`Theme`] so the palette
//! can be swapped wholesale (and overridden per-slot from `config.toml`) without
//! touching render code. Defaults are the GrokNight-ish values from
//! `docs/mockups.html`; [`Theme::light`] is the same layout on a light ground,
//! chosen with `base = "light"` in `[theme]` or `--theme light`.

use std::collections::BTreeMap;

use ratatui::style::Color;

/// How many stable accent colors a group conversation can hand out before it
/// wraps around.
pub const PARTICIPANT_SLOTS: usize = 4;

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

    /// Accent rail for messages you sent.
    pub accent_me: Color,
    /// Accent rail for the other person in a 1:1 chat.
    pub accent_them: Color,
    /// Stable per-participant accents for group chats. `participants[0]` is the
    /// same green as [`Theme::accent_them`] so 1:1 and group chats agree.
    pub participants: [Color; PARTICIPANT_SLOTS],

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
    /// Highlighted characters in fuzzy-match results.
    pub fuzzy: Color,

    /// Pane and composer borders at rest.
    pub border: Color,
    /// Border of the focused pane / selected row outline.
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

            accent_me: rgb(0x5e, 0xa8, 0xff),
            accent_them: rgb(0x7e, 0xc6, 0x99),
            participants: [
                rgb(0x7e, 0xc6, 0x99),
                rgb(0xe5, 0xb5, 0x67),
                rgb(0xd1, 0x7b, 0xe0),
                rgb(0x7f, 0xd6, 0xc9),
            ],

            text_primary: rgb(0xe4, 0xe4, 0xe7),
            text_secondary: rgb(0xa1, 0xa1, 0xaa),
            gray: rgb(0x52, 0x52, 0x5b),
            gray_dim: rgb(0x3a, 0x3a, 0x40),
            system: rgb(0x8a, 0x8a, 0x94),
            fuzzy: rgb(0x9d, 0xd0, 0xff),

            border: rgb(0x2c, 0x2c, 0x33),
            border_active: rgb(0x5e, 0xa8, 0xff),
            error: rgb(0xe0, 0x6c, 0x75),
        }
    }
}

/// The bases a config can name, in the order `--help` lists them and
/// `Ctrl+T` cycles through them.
pub const BASES: [&str; 3] = ["dark", "light", "system"];

/// Which palette to start from before per-slot overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Base {
    /// The mockup palette.
    #[default]
    Dark,
    /// [`Theme::light`].
    Light,
    /// Whichever of the two macOS is showing, asked on a timer.
    System,
}

impl Base {
    /// The base a config value names, case-insensitively.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "system" => Some(Self::System),
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
        }
    }

    /// The base after this one in [`BASES`], wrapping around.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::System,
            Self::System => Self::Dark,
        }
    }

    /// Whether this base draws dark, given the system's answer (`None` while
    /// it has not answered yet, which reads as dark).
    #[must_use]
    pub fn is_dark(self, system_dark: Option<bool>) -> bool {
        match self {
            Self::Dark => true,
            Self::Light => false,
            Self::System => system_dark.unwrap_or(true),
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

impl Theme {
    /// The palette a name picks out: `dark` (the default) or `light`.
    /// `system` needs an answer, so it is not a name here.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match Base::parse(name)? {
            Base::Dark => Some(Self::default()),
            Base::Light => Some(Self::light()),
            Base::System => None,
        }
    }

    /// The palette for a base, given what the system said.
    #[must_use]
    pub fn for_base(base: Base, system_dark: Option<bool>) -> Self {
        if base.is_dark(system_dark) {
            Self::default()
        } else {
            Self::light()
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

    /// The dark palette on a light ground: the same accents, darkened enough
    /// to read on white, with the bands and borders inverted.
    #[must_use]
    pub fn light() -> Self {
        Self {
            bg_base: rgb(0xff, 0xff, 0xff),
            bg_light: rgb(0xf4, 0xf4, 0xf6),
            bg_dark: rgb(0xf7, 0xf7, 0xf9),
            bg_highlight: rgb(0xe6, 0xe6, 0xeb),
            bg_hover: rgb(0xec, 0xec, 0xf0),

            accent_me: rgb(0x1f, 0x6f, 0xe5),
            accent_them: rgb(0x2e, 0x8b, 0x57),
            participants: [
                rgb(0x2e, 0x8b, 0x57),
                rgb(0xb0, 0x7a, 0x1a),
                rgb(0x9b, 0x3f, 0xb0),
                rgb(0x1f, 0x8a, 0x80),
            ],

            text_primary: rgb(0x1c, 0x1c, 0x1f),
            text_secondary: rgb(0x52, 0x52, 0x5b),
            gray: rgb(0x8a, 0x8a, 0x94),
            gray_dim: rgb(0xc4, 0xc4, 0xcb),
            system: rgb(0x6e, 0x6e, 0x78),
            fuzzy: rgb(0x1f, 0x6f, 0xe5),

            border: rgb(0xd6, 0xd6, 0xdc),
            border_active: rgb(0x1f, 0x6f, 0xe5),
            error: rgb(0xc0, 0x39, 0x2b),
        }
    }

    /// The accent for the `n`th participant of a group chat, wrapping around
    /// the palette. Callers pass a stable index (handle rowid order) so a
    /// person keeps the same color for the whole thread.
    #[must_use]
    pub fn participant(&self, n: usize) -> Color {
        self.participants[n % PARTICIPANT_SLOTS]
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
    ///
    /// Participant accents are addressed as `participant0` … `participant3`.
    pub fn set_slot(&mut self, name: &str, color: Color) -> bool {
        match name {
            "bg_base" => self.bg_base = color,
            "bg_light" => self.bg_light = color,
            "bg_dark" => self.bg_dark = color,
            "bg_highlight" => self.bg_highlight = color,
            "bg_hover" => self.bg_hover = color,
            "accent_me" => self.accent_me = color,
            "accent_them" => self.accent_them = color,
            "text_primary" => self.text_primary = color,
            "text_secondary" => self.text_secondary = color,
            "gray" => self.gray = color,
            "gray_dim" => self.gray_dim = color,
            "system" => self.system = color,
            "fuzzy" => self.fuzzy = color,
            "border" => self.border = color,
            "border_active" => self.border_active = color,
            "error" => self.error = color,
            _ => {
                let Some(idx) = name.strip_prefix("participant") else {
                    return false;
                };
                let Ok(idx) = idx.parse::<usize>() else {
                    return false;
                };
                if idx >= PARTICIPANT_SLOTS {
                    return false;
                }
                self.participants[idx] = color;
            }
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
    fn participants_wrap_and_start_at_accent_them() {
        let theme = Theme::default();
        assert_eq!(theme.participant(0), theme.accent_them);
        assert_eq!(theme.participant(4), theme.participant(0));
        assert_eq!(theme.participant(5), theme.participant(1));
    }

    #[test]
    fn overrides_apply_and_report_problems() {
        let mut theme = Theme::default();
        let overrides = BTreeMap::from([
            ("accent_me".to_string(), "#ff0000".to_string()),
            ("participant2".to_string(), "#00ff00".to_string()),
            ("participant9".to_string(), "#00ff00".to_string()),
            ("nope".to_string(), "#000000".to_string()),
            ("border".to_string(), "chartreuse".to_string()),
        ]);

        let warnings = theme.apply_overrides(&overrides);

        assert_eq!(theme.accent_me, Color::Rgb(255, 0, 0));
        assert_eq!(theme.participants[2], Color::Rgb(0, 255, 0));
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
        let mut theme = Theme::for_base(base, None);
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
        assert_eq!(base, Base::Dark);
        assert!(warning.is_some_and(|w| w.contains("system")));
    }

    #[test]
    fn every_base_parses_and_cycles() {
        for name in BASES {
            let base = Base::parse(name).expect(name);
            assert_eq!(base.name(), name);
        }
        assert_eq!(Base::parse("nope"), None);
        assert_eq!(Base::Dark.next().next().next(), Base::Dark);
        assert_eq!(Theme::named("dark"), Some(Theme::default()));
        assert_eq!(Theme::named("system"), None);
    }

    #[test]
    fn system_follows_the_answer_and_reads_dark_until_there_is_one() {
        assert!(Base::System.is_dark(None));
        assert!(Base::System.is_dark(Some(true)));
        assert!(!Base::System.is_dark(Some(false)));
        assert!(Base::Dark.is_dark(Some(false)));
        assert!(!Base::Light.is_dark(Some(true)));
        assert_eq!(Theme::for_base(Base::System, Some(false)), Theme::light());
    }

    #[test]
    fn light_and_dark_disagree_on_the_ground() {
        assert_ne!(Theme::light().bg_base, Theme::default().bg_base);
        assert_eq!(Theme::light().participant(0), Theme::light().accent_them);
    }

    #[test]
    fn border_follows_focus() {
        let theme = Theme::default();
        assert_eq!(theme.border_for(true), theme.border_active);
        assert_eq!(theme.border_for(false), theme.border);
    }
}
