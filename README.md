# msgs

A terminal client for iMessage on macOS. Read everything from `~/Library/Messages/chat.db`, send through Messages.app, all inside your terminal.

Built with Rust + [ratatui](https://ratatui.rs). Visual grammar borrowed from Grok Build: an accent rail per sender, a lighter band for your own messages, a bordered composer, a shortcuts bar.

Design mockups: [`docs/mockups.html`](docs/mockups.html).

## Status

Early. See the GitHub issues for the build plan.

## Requirements

- macOS 14+
- Full Disk Access for your terminal (System Settings → Privacy & Security → Full Disk Access) so `chat.db` can be read
- Messages.app signed in (it is launched hidden in the background when you send; reading never needs it open)
- Optional: [`imsg`](https://github.com/openclaw/imsg) on `$PATH` for sending tapback reactions

## Run

```
cargo run --release
```

Flags:

| Flag | Meaning |
|---|---|
| `--db <PATH>` | read this database instead of `~/Library/Messages/chat.db` |
| `--config <PATH>` | read this config file instead of the default |
| `--no-mouse` | do not capture the mouse |
| `--check` | print a readiness report (database, Full Disk Access, Messages.app, `imsg`) and exit |
| `--version` | print the version |

## Keys

| Key | Action |
|---|---|
| `Tab` | cycle focus: chat list → conversation → composer |
| `↑` `↓` / `j` `k` | select chat or message |
| `Enter` | open chat / send message |
| `PgUp` `PgDn`, `g` `G` | page, jump to top / bottom |
| `Ctrl+K` | jump palette: chats, people, full-text message search |
| `Ctrl+B` | toggle chat list |
| `/` | filter the chat list by name |
| `o` / `s` | open / save selected attachment |
| `r` | quote the selected message in a reply |
| `Ctrl+R` | react to selected message |
| `y` | copy selected message |
| `Ctrl+A` | attach a file |
| `Alt+Enter` | newline in the composer (`Shift+Enter` where the terminal supports it) |
| `Esc` | close an overlay / leave the composer |
| `?` | help |
| `q` / `Ctrl+C` | quit |

## Config

Optional, at `~/.config/msgs/config.toml` (or `$XDG_CONFIG_HOME/msgs/config.toml`). Every key
is optional, and a malformed file is reported on the status line rather than being fatal.

```toml
show_chat_list = true    # show the chat list on startup
chat_list_width = 30     # columns, 18–60, never more than half the screen
page_step = 10           # rows moved by PageUp / PageDown
mouse = true             # --no-mouse overrides this

[theme]
# Any color slot, as "#rrggbb", "#rgb", or an ANSI index 0–255.
accent_me = "#5ea8ff"
accent_them = "#7ec699"
participant0 = "#7ec699" # participant0–participant3: stable group-chat accents
border_active = "#5ea8ff"
```

Slots: `bg_base`, `bg_light`, `bg_dark`, `bg_highlight`, `bg_hover`, `accent_me`, `accent_them`,
`participant0`–`participant3`, `text_primary`, `text_secondary`, `gray`, `gray_dim`, `system`,
`fuzzy`, `border`, `border_active`, `error`.

The chat list hides itself below 90 columns regardless of `show_chat_list`.
