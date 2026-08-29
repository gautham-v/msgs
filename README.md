# msgs

iMessage in your terminal. Reads `~/Library/Messages/chat.db`, sends through Messages.app.
Rust + [ratatui](https://ratatui.rs), macOS 14+.

https://github.com/user-attachments/assets/3ca1d87a-6de4-4186-a88a-520aac5984d7

## Install

```
brew install gautham-v/tap/msgs
```

Or from source: `cargo install --path .`

Then give your terminal **Full Disk Access** (System Settings → Privacy & Security),
relaunch it, and run `msgs`. `msgs --check` tells you what is missing.
`msgs --redact` masks phone numbers and addresses, for screenshots and demos.
Reactions need [`imsg`](https://github.com/steipete/imsg) (`brew install steipete/tap/imsg`).

## What it does

- Chats, messages, group chats, names from Contacts, pictures inline, video posters
- Conversations you pinned in Messages.app sit at the top of the list
- Link previews from what Messages already stored — msgs never touches the network
- Send text, files, replies, and reactions; live updates as messages land
- Drag a file from Finder onto the window to attach it; `Enter` sends
- `Ctrl+K` fuzzy jump to any chat or person, full-text search over every message
- Local unread state, so opening a chat here does not touch Messages.app's badge
- Never writes to `chat.db`; nothing leaves your Mac

## Keys

| Key | Action | Where |
|---|---|---|
| `Tab` | switch focus: chat list → conversation → composer | everywhere |
| `Enter` | open chat / send message | everywhere |
| `Ctrl+K` | jump palette: chats, people, message search | everywhere |
| `Ctrl+N` | new message to a number or address, via the palette | everywhere |
| `Ctrl+B` | toggle the chat list | everywhere |
| `Ctrl+T` | cycle the theme: dark / light / system / terminal | everywhere |
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
| `@` | pick a file to attach, at the start of a word | composer |
| `Alt+Enter` | newline without sending (Shift+Enter where supported) | composer |
| `↑` `↓` / `Tab` | move through the files | file picker |
| `/` | go into the highlighted directory | file picker |
| `Enter` | attach the highlighted file | file picker |
| `Esc` | close the picker, keeping the typed @ | file picker |
| `Ctrl+W` | delete the word before the cursor | any text field |
| `Ctrl+U` | clear the whole field | any text field |
| `Tab` | cycle the filter: all / chats / messages / photos | jump palette |
| `←` `→` | choose a reaction | react picker |
| `1`–`6` | send that reaction straight away | react picker |
| `Enter` | send it, or take back one of yours | react picker |
| `r` / `Enter` | try to open chat.db again | first run |

The mouse works too: click to select, drag across the conversation to copy what you drag over, click a link or picture to open it, wheel to scroll. `--no-mouse` hands the mouse back to the terminal.

## Limits

No typing indicators, no editing or unsending, no pinning from here, no clearing Messages.app's own badge — macOS keeps all of those out of reach. Reactions to arbitrary messages need `imsg` with SIP off; with SIP on, `imsg` reaches only a conversation's newest incoming message, and needs Messages.app open and Accessibility granted to your terminal. The full list, config keys, and how everything works are in [docs/MANUAL.md](docs/MANUAL.md).

## Development

`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. Tests only ever open synthetic fixtures. MIT licensed.
