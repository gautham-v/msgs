# msgs

A terminal client for iMessage on macOS. Read everything from `~/Library/Messages/chat.db`, send through Messages.app, all inside your terminal.

Built with Rust + [ratatui](https://ratatui.rs). Visual grammar borrowed from Grok Build: an accent rail per sender, a lighter band for your own messages, a bordered composer, a shortcuts bar.

Design mockups: [`docs/mockups.html`](docs/mockups.html).

## Status

Early. See the GitHub issues for the build plan.

## Requirements

- macOS 14+
- Full Disk Access for your terminal (System Settings → Privacy & Security → Full Disk Access) so `chat.db` and your Contacts can be read — without it msgs starts and explains what to do instead of failing
- Messages.app signed in (it is launched hidden in the background when you send; reading never needs it open)
- Optional: [`imsg`](https://github.com/steipete/imsg) on `$PATH` (`brew install steipete/tap/imsg`) for sending tapback reactions

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
| `--check` | print a readiness report (Full Disk Access, database, row counts, unread, read state, live updates, Messages.app and whether it is running, `osascript`, `imsg`, search index, contacts, terminal graphics) and exit |
| `--no-index` | do not build or use the full-text message index |
| `--no-images` | do not draw pictures inline; show every attachment as a chip |
| `--no-contacts` | do not read Contacts; show numbers and addresses instead of names |
| `--version` | print the version |

## Keys

| Key | Action |
|---|---|
| `Tab` | cycle focus: chat list → conversation → composer |
| `↑` `↓` / `j` `k` | select chat or message |
| `Enter` | open chat / send message |
| `PgUp` `PgDn`, `g` `G` | page, jump to top / bottom |
| `Ctrl+K` | jump palette: chats, people, full-text message search |
| `/` (conversation) | open the jump palette |
| `Tab` (palette) | cycle the filter: all / chats / messages / photos |
| `Ctrl+N` (palette) | start a new message to a typed number or address |
| `Ctrl+B` | toggle chat list |
| `Ctrl+U` | mark everything seen here, or give the unread back |
| `/` (chat list) | filter the chat list by name |
| `o` / `s` | open / save selected attachment |
| `r` | quote the selected message in a reply |
| `Ctrl+R` | react to selected message (`←→` choose, `1`–`6` straight to one, `Enter` send) |
| `y` | copy selected message |
| `Ctrl+L` | open the first link in the selected message |
| `Ctrl+A` | attach a file to the open conversation |
| `Alt+Enter` | newline in the composer (`Shift+Enter` where the terminal supports it) |
| `Esc` | close an overlay / leave the composer |
| `?` | help |
| `r` (first run) | try to open `chat.db` again |
| `q` / `Ctrl+C` | quit |

## When it cannot start

`chat.db` lives behind Full Disk Access, so the first thing a new terminal sees
is a refusal. msgs does not fail on it: the panes are replaced by one panel that
says what happened, which file it happened to, and the three steps that fix it —
System Settings → Privacy & Security → Full Disk Access, switch on the app msgs
runs inside, quit that app and open it again, because macOS only applies the
change on a fresh launch. The same switch is what lets msgs read Contacts, so
the panel says so rather than letting names fail separately later.

msgs stays running while you do it. `r` opens the database again, and a retry
that works picks up everything a launch would have — the chats, the names, the
read state, and the index — rather than leaving an open database with nothing
hanging off it. A retry that fails leaves the panel up and says why on it. `?`
still opens the help modal over the panel and `q` still quits, and no other key
does anything, because there are no panes behind it to steer.

Nothing that goes wrong at startup is fatal. A config key that will not parse, a
file watcher that will not start, Contacts that will not open: each is said once
as a toast and then kept under `NOTES` at the bottom of the help modal, which is
where to look after the toast has gone.

`msgs --check` answers the same questions from the shell without starting the
UI: Full Disk Access, whether `chat.db` opens and what is in it, the read state,
live updates, Messages.app and whether it is running, `osascript`, `imsg`, the
search index, Contacts, the terminal's graphics support, and `sips`. It prints
paths and counts — never a name, a number, or a message.

## The status line

Along the bottom: whether Messages.app is running, how much is unread and in how
many chats, what the message index is doing while it builds, and whether live
updates are watching or polling with how long ago the screen was last refreshed.
Messages.app is asked about on its own thread every five seconds — by `pgrep`,
not by AppleScript, because asking Messages whether it is running would start
it — and until the first answer lands the line says `unknown` rather than
guessing.

Anything transient — a copy, a save, a send that was refused — takes the whole
line for two seconds and then gives it back. Failures are drawn in the error
color; everything else in the accent.

## Attachments

A picture in a thread is drawn where it was sent. msgs asks the terminal what it
can do the moment the alternate screen is up: Ghostty, Kitty, and WezTerm answer
with the kitty graphics protocol and get real pixels, iTerm2 gets its own inline
images, and everything else falls back to unicode half-blocks, which any
terminal can draw. Pictures are capped at ten rows and at forty-eight columns
and keep their aspect ratio, and the file name and size move onto the meta line
under them. `--no-images`, or `images = false` in the config, turns the whole
thing off and leaves every attachment as a chip.

HEIC is what an iPhone camera sends and nothing in Rust reads it, so `sips` —
the converter macOS ships — turns one into a JPEG the first time it is on
screen. That runs on its own thread, so a thread full of photos never blocks a
keystroke, and the result is cached under
`~/Library/Caches/msgs/attachments` and reused by every later session. Until it
lands the attachment shows as a chip.

Anything that is not a picture is a dashed chip: `┄ 📄 draft-order.pdf · 84 KB ┄`.
An attachment whose bytes never reached this Mac says
`(not downloaded on this Mac)` rather than pretending to be there.

`o` opens the selected message's attachment with `open`, and `s` copies it into
`~/Downloads` without ever overwriting anything — a name already taken gets
` (2)` before the extension. Both read the file and nothing else; `chat.db` is
untouched. `Ctrl+A` goes the other way and sends a file to the open
conversation.

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
wrapped to the pane, and a meta line with the time and any tapbacks. The newest
message you sent carries `Delivered` or `Read 18:05` after its clock and the
older ones do not, which is where Messages.app puts the receipt too. Replies
quote what they answer, group events (renames, joins, leaves) are dim italic
lines without a rail, and days are separated by a header
that sticks to the top edge as it scrolls. Because block heights are measured
once per page rather than once per frame, only the blocks actually on screen are
laid out, and a 25,000-message thread scrolls at the same speed as a short one.

## Names

`chat.db` stores phone numbers and email addresses and nothing else, so every
name on screen comes out of Contacts. macOS keeps that as a set of SQLite files
under `~/Library/Application Support/AddressBook` — one at the top level and one
per account under `Sources/` — and msgs reads all of them the same way it reads
`chat.db`: read-only, with a scratch copy when a lock refuses a reader, and
never a write.

Matching is by normalized address. An email is lowercased; a phone number is
reduced to `+` and its digits and gains the country code that a number saved as
`(415) 555-0132` is missing, so it lands on the `+14155550132` the message
database stores. Behind that sits an index on the last ten digits, which catches
a number saved without its country code in a country where the code is not `1`,
and which declines to answer when two different people share those digits.

A name reaches every pane at once, because it is attached to the participants of
a chat and the chat list, the conversation header, the group sender labels, the
preview prefixes, and the jump palette all read it from there. A conversation
with one person is titled with their whole name, `Sam Rivera`; an unnamed group
is titled with everybody's first name; a message in a group is labelled with the
sender's first name. The header keeps the address beside the name, because that
is the thing the name is hiding. Somebody who is not in Contacts is written
`+1 (415) 555-0132` rather than `+14155550132`, and a contact with no personal
name — a business — is called by its organization.

Somebody who is in a group twice, at a phone number and at an Apple ID, is one
person to Contacts and gets one accent color for both.

The result is cached at `~/Library/Application Support/msgs/contacts.json`
alongside the sizes and modification times of the files it was built from, `0600`
inside a `0700` directory. A launch that changes nothing costs one `stat` per
store; a contact added since the last launch moves a stamp and rebuilds the map.
If the stores cannot be read at all — no Full Disk Access, or no Contacts on this
Mac — the status line says so once, and every handle falls back to its
pretty-printed address. `--no-contacts`, or `contacts = false` in the config,
turns the whole thing off.

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

## Read state

The chat list draws an unread dot, a bold name, and a count on any chat with
incoming messages Messages has not marked read, and the status line adds them
up: `3 unread in 2 chats`.

Opening a chat in msgs clears that badge — but only msgs's. `chat.db` is
read-only and Messages.app's read flags and Dock badge are its own; there is no
supported way to clear either from outside that app, and msgs does not try. What
it keeps instead is a small state of its own at
`~/Library/Application Support/msgs/seen.json`, `0600` inside a `0700`
directory: for every chat, how many unread messages were already on screen here.
The badge is the database's count less that number, never below zero, so opening
a thread takes it to nothing, the next message to arrive brings it back showing
exactly the new ones, and reading the thread on your phone — which drops the
database's own count — lowers the stored number with it rather than swallowing
what comes next.

`Ctrl+U` marks every chat seen; pressing it again hands the unread straight back.
The state survives a restart, holds nothing but chat row numbers and counts — no
names, numbers, or message text — and records which database it was built from,
so `--db` pointed at a copy starts fresh instead of reading someone else's row
numbers as if they were this one's.

## Search

`Ctrl+K` opens a floating palette over a dimmed screen — `/` does the same from
the conversation. Chats and the people in them are matched fuzzily as you type,
with the matched characters picked out; at three characters the query also goes
to a full-text index of every message, and hits come back as the chat, who said
it, the matched line, and when. `Enter` opens the chat, and for a message hit it
opens the conversation with that message selected, loading pages upward until it
is on screen. `Tab` cycles the filter — all, chats, messages, photos — and
`Ctrl+N` with something that looks like a phone number or an email addresses a
new message to it, whether or not you have written to it before. `Esc` closes
the palette and hands focus back where it was.

The index is msgs's own SQLite FTS5 file at
`~/Library/Application Support/msgs/index.db`, created `0600` in a `0700`
directory. Nothing is ever written to `chat.db`. It is built on first launch by
a background thread — the status line says `indexing messages… 42%` while that
happens, and the rest of the app keeps working — and topped up afterward from
the same live-update pass that refreshes the screen, reading only the
`message.ROWID`s that arrived since the last time. A hundred thousand messages
index in a few seconds and queries come back in single-digit milliseconds.
`--no-index` turns the whole thing off.

A jump to a message pages upward from the newest end of the conversation, so
nothing already on screen moves and no gap can open in the transcript. That
paging stops after ten thousand messages; a hit further back than that opens the
conversation and says the message is further back than msgs will load.

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

## Reactions

`Ctrl+R` on the selected message opens a small picker: ❤️ 👍 👎 😂 ‼️ ❓. `←` and
`→` move along it, `1` through `6` go straight to one and send it, and `Enter`
sends whatever the cursor is on. A reaction you have already given
is drawn in the accent color and the picker starts on it, because pressing
`Enter` there takes it back, which is what tapping it again does in Messages.

`chat.db` is read-only and Messages will not take a tapback from AppleScript, so
reactions go out through [`imsg`](https://github.com/steipete/imsg), which has
two routes and msgs tries both. `imsg tapback` reaches any message by its own
GUID, which is the one msgs wants — but it needs a bridge injected into
Messages.app, and macOS will not load that while System Integrity Protection is
on. `imsg react` drives Messages' own UI instead, which works on a stock Mac but
can only reach the newest incoming message of a conversation, so msgs only falls
back to it for exactly that message. Taking a reaction back is something only
the bridge can do.

The chip goes under the message the moment you choose it, marked as yours, and
the loaded page is left exactly as `chat.db` handed it over — the optimistic
chip is drawn over the page rather than written into it. When the real row
arrives on the next live update the chip retires into it without moving, and if
`imsg` refuses, the chip comes back down and the status line says so. A reaction
that the database has still not confirmed twenty seconds later is dropped, so
what is on screen is the database's own answer rather than a guess.

Without `imsg` on `$PATH` the picker still opens and says
`brew install steipete/tap/imsg`; nothing is sent and no chip goes up.
Reactions with an arbitrary emoji are read and drawn but cannot be sent —
neither `imsg` route will put one on the wire.

## Config

Optional, at `~/.config/msgs/config.toml` (or `$XDG_CONFIG_HOME/msgs/config.toml`). Every key
is optional, and a malformed file is reported on the status line rather than being fatal.

```toml
show_chat_list = true    # show the chat list on startup
chat_list_width = 30     # columns, 18–60, never more than half the screen
page_step = 10           # rows moved by PageUp / PageDown
mouse = true             # --no-mouse overrides this
images = true            # draw pictures inline; --no-images overrides this
contacts = true          # read Contacts for names; --no-contacts overrides this

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
