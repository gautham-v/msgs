# msgs

A terminal client for iMessage on macOS. Read everything from `~/Library/Messages/chat.db`, send through Messages.app, all inside your terminal.

Built with Rust + [ratatui](https://ratatui.rs). Visual grammar borrowed from Grok Build: an accent rail per sender, a lighter band for your own messages, a bordered composer, a shortcuts bar.

Design mockups: [`docs/mockups.html`](docs/mockups.html).

## Status

Early. See the GitHub issues for the build plan.

## Requirements

- macOS 14+
- Full Disk Access for your terminal (System Settings → Privacy & Security → Full Disk Access) so `chat.db` can be read — without it msgs starts and explains what to do instead of failing
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
| `--check` | print a readiness report (database, row counts, live updates, Messages.app, `osascript`, `imsg`) and exit |
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
| `Ctrl+L` | open the first link in the selected message |
| `Ctrl+A` | attach a file to the open conversation |
| `Alt+Enter` | newline in the composer (`Shift+Enter` where the terminal supports it) |
| `Esc` | close an overlay / leave the composer |
| `?` | help |
| `q` / `Ctrl+C` | quit |

## Reading

`chat.db` is only ever opened read-only: `SQLITE_OPEN_READ_ONLY`, a `?mode=ro`
URI, and `PRAGMA query_only`. Nothing in msgs can write to it. macOS keeps the
database in WAL mode, so if a read is refused because Messages.app holds the
file, msgs copies `chat.db` and its `-wal` / `-shm` sidecars to a scratch
directory, reads the copy, and deletes the copy on exit.

Conversations load a page at a time from the newest end, so opening a thread
with 25,000 messages in it costs the same as opening a short one. Scrolling past
the top of a page fetches the one above it and keeps the view on the message it
was on. The chat list costs four queries however long it is, and only the rows
on screen are drawn.

A conversation is drawn as blocks: a colored rail per sender — blue for you,
green for the other person, and a color per participant in a group, assigned in
`handle.ROWID` order so it follows a person for the life of the thread — a body
wrapped to the pane, and a meta line with the time, `Delivered` / `Read`, and
any tapbacks. Replies quote what they answer, group events (renames, joins,
leaves) are dim italic lines without a rail, and days are separated by a header
that sticks to the top edge as it scrolls. Because block heights are measured
once per page rather than once per frame, only the blocks actually on screen are
laid out, and a 25,000-message thread scrolls at the same speed as a short one.

## Live updates

msgs keeps up with `chat.db` without Messages.app open. A `notify` watcher sits
on the directory the database lives in — the directory rather than the file,
because `chat.db-wal` is deleted and recreated around every checkpoint — and a
burst of writes is debounced into one re-read three tenths of a second after it
goes quiet. If no platform watcher will start, a two-second timer takes over;
the status line says `watching chat.db` or `polling chat.db`, followed by how
long ago the screen was last refreshed.

A re-read asks the open conversation for its newest page. Rows already on
screen are replaced where they stand, so an edit or a tapback lands in the block
it belongs to; anything past them goes on the end. If you were at the newest
message the view follows it; if you were reading further back it stays where it
is and a `↓ 3 new` pill appears on the bottom edge, which you can click or clear
by pressing `G`. A message in some other thread moves that chat to the top of
the list and updates its preview and unread badge, and the chat-list selection
follows the conversation you were in rather than the row number it was at.

Pinned conversations are shown as their own section when the database records
pinning. macOS keeps that in Messages.app's preferences rather than in
`chat.db`, so on every current system the list is one flat run of chats,
newest first.

## Sending

`chat.db` is read-only, so outbound messages go the only supported way: an
AppleScript handed to `osascript`, which asks Messages.app to send. The
conversation is addressed by its `chat.guid` first — the exact thread you are
looking at, group or not — and a one-to-one chat falls back to the handle on
the other end of it. iMessage and SMS threads each ask for their own service.
Messages.app is `launch`ed, never `activate`d, so it starts hidden and does not
take the screen from your terminal.

Every value that reaches the script is escaped for an AppleScript string
literal, so quotes, backslashes, and newlines in a message cannot end the
literal they live in, and errors coming back have their quoted parts stripped
before they reach the status line — the quoted part is usually a phone number.

Pressing `Enter` puts the message on screen immediately, marked `· Sending…`,
and runs `osascript` on its own thread so the UI never blocks on it. When the
real row shows up in `chat.db` the echo is replaced by it. A refused send is
marked `· Failed` with the reason, and the text goes back in the composer.

`Ctrl+A` asks for a path — `~` is expanded — and sends that file to the open
conversation.

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
