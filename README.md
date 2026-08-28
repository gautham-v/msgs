# msgs

iMessage in your terminal. Reads `~/Library/Messages/chat.db`, sends through Messages.app.
Rust + [ratatui](https://ratatui.rs), macOS 14+.

<!-- demo video: drag the .mov onto this file in GitHub's editor and paste the link it makes here -->

```
 / search chats…             │ Alex Nakamura · iMessage · +1 (555) 000-0001    4 msgs · 1 photo
 ● Fixture Group 5/18/22  1  │──────────────────────────────────────────────────────────────────
   Bailey: named the conver… │ May 18, 2022
▌● Alex Nakamura 5/18/22  1  │
▌  still unread              │ ▌  first fixture message
   iMessage;-;+15550000009   │ ▌  00:29  😂  Alex   ❤️  You
                             │ ▌  recovered from the typedstream
                             │ ▌  00:30
                             │ ▌  ┄ 📷  photo.png · 2.0 KB · (not downloaded on this Mac) ┄
                             │ ▌  00:31 · Read 00:33
                             │ ▌  still unread
                             │ ▌  00:34
                             │
                             │
                             │
                             │
                             │
                             │
                             │
                             │╭────────────────────────────────────────────────────────────────╮
                             ││› message Alex Nakamura…                                        │
                             │╰────────────────────────────────────────────────────────────────╯
                             │ o open · s save · y copy · Ctrl+R react
────────────────────────────────────────────────────────────────────────────────────────────────
 Messages.app unknown  │  2 unread in 2 chats  │  watching chat.db
 Tab focus list/convo · ↑↓ select · Enter open/send · Ctrl+K jump / search · ? help
```

## Install

```
brew install gautham-v/tap/msgs
```

Or from source: `cargo install --path .`

Then give your terminal **Full Disk Access** (System Settings → Privacy & Security),
relaunch it, and run `msgs`. `msgs --check` tells you what is missing.
Reactions need [`imsg`](https://github.com/steipete/imsg) (`brew install steipete/tap/imsg`).

## What it does

- Chats, messages, group chats, names from Contacts, pictures inline, video posters
- Send text, files, replies, and reactions; live updates as messages land
- `Ctrl+K` fuzzy jump to any chat or person, full-text search over every message
- Local unread state, so opening a chat here does not touch Messages.app's badge
- Never writes to `chat.db`; nothing leaves your Mac

## Keys

| Key | Action | Where |
|---|---|---|
| `Tab` | switch focus: chat list → conversation → composer | everywhere |
| `Enter` | open chat / send message | everywhere |
| `Ctrl+K` | jump palette: chats, people, message search | everywhere |
| `Ctrl+B` | toggle the chat list | everywhere |
| `Esc` | close the overlay / leave the composer | everywhere |
| `?` | this help | everywhere |
| `q` / `Ctrl+C` | quit | everywhere |
| `↑` `↓` / `k` `j` | select chat or message | lists |
| `PgUp` `PgDn` | page through the conversation | lists |
| `g` / `G` (`Home` / `End`) | jump to top / bottom | lists |
| `Ctrl+U` | mark everything seen here, or give the unread back | lists |
| `/` | filter chats by name | chat list |
| `/` | open the jump palette | conversation |
| `i` | start typing in the composer | conversation |
| `o` | open the selected attachment | conversation |
| `s` | save the selected attachment | conversation |
| `r` | quote the selected message in a reply | conversation |
| `Ctrl+R` | react to the selected message | conversation |
| `y` | copy the selected message | conversation |
| `Ctrl+L` | open the first link in the selected message | conversation |
| `Ctrl+A` | attach a file | composer |
| `Alt+Enter` | newline without sending (Shift+Enter where supported) | composer |
| `Ctrl+W` | delete the word before the cursor | any text field |
| `Ctrl+U` | clear the whole field | any text field |
| `Tab` | cycle the filter: all / chats / messages / photos | jump palette |
| `Ctrl+N` | new message to a typed number or address | jump palette |
| `←` `→` | choose a reaction | react picker |
| `1`–`6` | send that reaction straight away | react picker |
| `Enter` | send it, or take back one of yours | react picker |
| `r` / `Enter` | try to open chat.db again | first run |

The mouse works too: click to select, click a link or picture to open it, wheel to scroll.

## Limits

No typing indicators, no editing or unsending, no pinned chats, no clearing Messages.app's own badge — macOS keeps all of those out of reach. Reactions to arbitrary messages need `imsg` with SIP off. The full list, config keys, and how everything works are in [docs/MANUAL.md](docs/MANUAL.md).

## Development

`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. Tests only ever open synthetic fixtures. MIT licensed.
