# msgs manual

The long version of the [README](../README.md).

A terminal client for iMessage on macOS. Read everything from `~/Library/Messages/chat.db`, send through Messages.app, all inside your terminal.

Built with Rust + [ratatui](https://ratatui.rs). Visual grammar borrowed from Grok Build: an accent rail per sender, a lighter band for your own messages, a bordered composer, a shortcuts bar.

Design mockups: [`mockups.html`](mockups.html).

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

That frame is drawn by the real widgets, at 96×26, against the synthetic
fixture under `tests/fixtures/` — every name, number, and body in it is
invented, because a picture of a terminal is the one thing that would carry
somebody's messages into a repository. `tests/screenshot.rs` regenerates it
(`TZ=UTC UPDATE_SCREENSHOT=1 cargo test --test screenshot` — the clocks in a
transcript are drawn in the local zone) and fails when the app stops drawing
what is printed above.

## Status

Working. Everything below is shipped; [Limitations](#limitations) is the
honest list of what msgs cannot do.

## Install

```
brew install gautham-v/tap/msgs
```

That installs the universal (arm64 + x86_64) binary from the latest
[release](https://github.com/gautham-v/msgs/releases). To build from source
instead, with a Rust toolchain:

```
cargo install --path .        # from a clone, into ~/.cargo/bin
brew install --HEAD --build-from-source packaging/msgs.rb
```

Every path builds the release profile: whole-program optimization, one codegen
unit, stripped — a 4 MB binary that starts in under a tenth of a second on a
200,000-message database. `.github/workflows/release.yml` builds the binary on
each `v*` tag and attaches it with its `.sha256` to the release;
`packaging/msgs.rb` mirrors the formula in
[gautham-v/homebrew-tap](https://github.com/gautham-v/homebrew-tap).

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
| `--redact` | mask phone numbers and addresses on screen (`+1 (•••) •••-••39`), for a demo or a screenshot; names and message bodies still show |
| `--check` | print a readiness report (Full Disk Access, database, row counts, unread, read state, live updates, Messages.app and whether it is running, `osascript`, `imsg`, search index, contacts, terminal graphics) and exit |
| `--no-index` | do not build or use the full-text message index |
| `--no-images` | do not draw pictures inline; show every attachment as a chip |
| `--no-contacts` | do not read Contacts; show numbers and addresses instead of names |
| `--version` | print the version |

## Keys

The same table drives the `?` help modal and this one, grouped by where each
binding applies.

**Everywhere**

| Key | Action |
|---|---|
| `Tab` | switch focus: chat list → conversation → composer |
| `Enter` | open chat / send message |
| `Ctrl+K` | jump palette: chats, people, message search |
| `Ctrl+B` | toggle the chat list |
| `Esc` | close the overlay / leave the composer |
| `?` | this help |
| `q` / `Ctrl+C` | quit |

**In either list**

| Key | Action |
|---|---|
| `↑` `↓` / `k` `j` | select chat or message |
| `PgUp` `PgDn` | page through the conversation |
| `g` / `G` (`Home` / `End`) | jump to top / bottom |
| `Ctrl+U` | mark everything seen here, or give the unread back |

**Chat list**

| Key | Action |
|---|---|
| `/` | filter chats by name |

**Conversation**

| Key | Action |
|---|---|
| `/` | open the jump palette |
| `i` | start typing in the composer |
| `o` | open the selected attachment |
| `s` | save the selected attachment |
| `r` | quote the selected message in a reply |
| `Ctrl+R` | react to the selected message |
| `y` | copy the selected message |
| `Ctrl+L` | open the first link in the selected message |

**Composer**

| Key | Action |
|---|---|
| `Ctrl+A` | attach a file |
| `Alt+Enter` | newline without sending (Shift+Enter where supported) |

**Any text field** — the composer, the palette input, the chat-list filter

| Key | Action |
|---|---|
| `Ctrl+W` | delete the word before the cursor |
| `Ctrl+U` | clear the whole field |

**Jump palette**

| Key | Action |
|---|---|
| `Tab` | cycle the filter: all / chats / messages / photos |
| `Ctrl+N` | new message to a typed number or address |

**React picker**

| Key | Action |
|---|---|
| `←` `→` | choose a reaction |
| `1`–`6` | send that reaction straight away |
| `Enter` | send it, or take back one of yours |

**First run, while `chat.db` cannot be read**

| Key | Action |
|---|---|
| `r` / `Enter` | try to open chat.db again |

The mouse works too, unless `--no-mouse` is passed: click a chat row or a
message block to select it, click a link to open it, click the `↓ N new` pill
to go to what it is counting, and roll the wheel over a pane to scroll it
without moving focus. The jump palette and the help modal are keyboard-only —
they swallow clicks so a stray one cannot act on the screen behind them.

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
search index, Contacts, the terminal's graphics support, `sips`, and
`qlmanage`. It prints
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

A video gets the same treatment through `qlmanage`, the Quick Look tool macOS
ships: its poster frame is cached as a PNG and drawn inline, marked with 🎬 on
the meta line so a still is not read as a photo. Until the frame lands the video
shows as a chip, and `o` still opens the clip in the default player — nothing
here plays video.

Anything that is not a picture or a video is a dashed chip: `┄ 📄 draft-order.pdf · 84 KB ┄`.
An attachment whose bytes never reached this Mac says
`(not downloaded on this Mac)` rather than pretending to be there.

`o` opens the selected message's attachment with `open` — clicking a picture
opens that one the same way — and `s` copies it into
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
older ones do not, which is where Messages.app puts the receipt too. A message
edited on another device says `· Edited` on the end of that line; msgs cannot
edit one itself, but it can say the body changed. Replies quote what they
answer, and group events (renames, joins, leaves) are dim italic lines without
a rail.

Days are separated by a centered header, and the day of whatever is at the top
of the pane is held on a band of its own between the chat header and the
messages — a row the messages never share, so nothing a message says is hidden
under it. When the separator itself is on screen the band stays blank and lets
it do the talking. Down the right edge, in the column the blocks keep clear, a
one-cell scrollbar says how much of the thread is on screen and where; a thread
that fits leaves the column empty.

Because block heights are measured once per page rather than once per frame,
only the blocks actually on screen are laid out, and a 25,000-message thread
scrolls at the same speed as a short one.

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
page_step = 10           # rows PageUp / PageDown move a list by
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

Every key, what it does, and what overrides it:

| Key | Default | Range | Meaning |
|---|---|---|---|
| `show_chat_list` | `true` | bool | show the chat list on startup; `Ctrl+B` toggles it at runtime |
| `chat_list_width` | `30` | 18–60 columns | width of the chat list, never more than half the screen |
| `page_step` | `10` | ≥ 1 rows | rows `PageUp` / `PageDown` move a list by; the conversation pages by its own height and the wheel is always three rows |
| `mouse` | `true` | bool | capture the mouse; `--no-mouse` overrides it to `false` |
| `images` | `true` | bool | draw pictures inline; `--no-images` overrides it to `false` |
| `contacts` | `true` | bool | read Contacts for names; `--no-contacts` overrides it to `false` |
| `[theme]` | — | color per slot | any slot below, as `"#rrggbb"`, `"#rgb"`, or an ANSI index `0`–`255` |

Slots: `bg_base`, `bg_light`, `bg_dark`, `bg_highlight`, `bg_hover`, `accent_me`, `accent_them`,
`participant0`–`participant3`, `text_primary`, `text_secondary`, `gray`, `gray_dim`, `system`,
`fuzzy`, `border`, `border_active`, `error`.

A value out of range is clamped and said on the status line; an unknown key,
a bad color, or TOML that will not parse is a warning under `NOTES` in the help
modal and defaults are used. Nothing in the file can stop msgs from starting.

The chat list hides itself below 90 columns regardless of `show_chat_list`.

## Speed

Everything is measured against `tests/fixtures`' own 200,000-message database —
invented, never a copy of anybody's `chat.db` — by `tests/perf.rs`, which
asserts the budgets rather than just printing them. On an M-series laptop, in
the release profile:

| | Budget | Measured |
|---|---|---|
| Cold start: open the database, load the chat list, open the newest thread, draw | 300 ms | ~96 ms |
| A keystroke, laid out and drawn, in a 100,000-message thread | 16.6 ms (60 fps) | ~0.17 ms |
| A page of 100 messages at the newest end / 50,000 back | 25 ms | ~1.2 ms / ~1.7 ms |
| Resident memory after opening and paging through that thread | 150 MB | ~17 MB |

The start-up figure is the fastest of three runs after a warm-up, because every
test in that binary is querying the same fixture on its own thread while it is
timed; a query that started walking the whole thread would be slow every time,
which is the shape the budget is there to catch.

The shape behind those numbers: the chat list is four queries whatever its
length, a conversation is read a page at a time from the newest end so a
25,000-message thread opens as fast as a short one, block heights are measured
once per page rather than once per frame, and only the blocks on screen are laid
out. The search index, the HEIC conversions, the sends, and the Messages.app
probe all run on their own threads, so none of them is on the path between a
keystroke and a frame. Contacts are the exception: they are read once during
start-up, on the UI thread, and are inside the number above.

## Limitations

Some of these are macOS's and some are msgs's, and it is worth knowing which.

**What macOS will not let anybody do**

- **No typing indicators.** They are never written to `chat.db` — they only
  exist inside Messages.app — so there is nothing to read. The mockup draws a
  greyed `Priya is typing…` to say so; msgs draws nothing at all rather than a
  line that would always be a lie.
- **Messages.app's badge and read flags cannot be cleared.** `chat.db` is
  read-only and there is no supported way in. Opening a chat clears msgs's own
  badge, kept in `seen.json`; the Dock badge stays Messages.app's own.
- **No editing and no unsending.** AppleScript can do neither. An edit made on
  another device is read like any other change and the message says `· Edited`;
  msgs cannot make one.
- **No pinned conversations.** macOS keeps pinning in Messages.app's
  preferences rather than in `chat.db`, so the list is one flat run of chats.
  The `Pinned` / `Recent` headings exist for a database that has the column,
  which no macOS to date does.
- **No group management** — no renaming, no adding or removing people — and no
  deleting a message or a thread. Group events are read and drawn.
- **Sending needs Messages.app** signed in and `osascript` available. msgs
  launches it hidden; it never takes the screen.
- **Reactions need [`imsg`](https://github.com/steipete/imsg).** Its IMCore
  bridge is what reaches an arbitrary message, and macOS will not load that
  while System Integrity Protection is on. With SIP on, the `imsg react`
  fallback can only reach a conversation's newest incoming message, taking a
  reaction back is not possible at all, and a custom-emoji reaction cannot be
  sent by either route. Every reaction is *read* and drawn regardless.
- **Attachments that never reached this Mac stay chips.** msgs reads files; it
  cannot ask iCloud for one. HEIC needs `sips`, which macOS ships.
- **A conversation with yourself shows every message twice.** Messages stores
  a sent copy and a received copy of each one as two rows with two GUIDs, and
  msgs draws what is in the database.
- **macOS 14+ only**, with Full Disk Access for the terminal msgs runs in.
  Without it msgs starts and explains what to do rather than failing.

**Where msgs itself stops**

- **A sent message stands on its own after twenty seconds.** The echo is
  matched against the real row as `chat.db` catches up; if it never does —
  a database being read through a scratch copy, most likely — the `Sending`
  note goes away and the echo stays as it is. No `Delivered` or `Read` stamp
  appears until the real row is read back.
- **Live updates re-read the newest page**, about a hundred rows. A tapback or
  an edit that lands further up the thread than that is picked up when the page
  around it is loaded again, not the moment it happens. A database held by
  Messages.app is read through a scratch copy, which can only keep up by being
  re-taken, and that is rate-limited to once every two seconds.
- **Search pages back 10,000 messages.** A hit further back than that opens the
  conversation and says so instead of reading the whole thread. The photo
  filter matches file names, because a file name is all Messages stores for an
  attachment.
- **The jump palette and the help modal are keyboard-only.** Both swallow
  clicks so a stray one cannot act on the screen behind them, so a result row
  is chosen with the arrow keys rather than the mouse.
- **Animated GIFs show their first frame.** A picture is also decoded on the
  frame it first becomes visible rather than ahead of time, so a very large
  photo can cost a fraction of a second the first time it is drawn. HEIC
  conversion is the exception and runs off the UI thread.
- **Contacts are read once, at startup, on the UI thread.** It is cheap in
  practice — a few hundred addresses, cached after the first launch — but a
  very large address book is felt on the first run. Nicknames (`ZNICKNAME`) are
  not read; a record with no personal name falls back to its organization.
- **A group's colors are given out in participant order.** Two addresses that
  Contacts calls one person share a slot, and the slots shift if somebody
  leaves the group.
- **Read state is always on** once the app data directory can be written; there
  is no config key or flag for turning it off. It records which database its
  row numbers belong to, so `--db` pointed at a copy starts a state of its own.
- **`--check` guesses at terminal graphics** from `$TERM_PROGRAM` and `$TERM`,
  because the real question is a control sequence whose answer would print onto
  your shell. The real detection runs inside the alternate screen at startup.
  Whether Messages.app is running comes from `pgrep -x Messages`, so a machine
  without `pgrep` reports `unknown`.
- **The help modal needs about 130 columns** for two columns of bindings;
  narrower terminals get one column and scroll.
- **`Shift+Enter` for a newline needs the kitty keyboard protocol.**
  `Alt+Enter` is the portable fallback and works everywhere.

## Development

```
cargo test                                    # unit, render, database, live, search, docs, perf
cargo test --release --test perf -- --nocapture   # the budgets above, with their numbers
cargo clippy --all-targets -- -D warnings
cargo fmt --check
TZ=UTC UPDATE_SCREENSHOT=1 cargo test --test screenshot   # redraw the frame above
```

The suite builds its own synthetic fixtures under `tests/fixtures/` on first
run — a small one, a 100,000-message one, and the 200,000-message one the perf
budgets are stated against — and every one of them is invented here. No test
opens `~/Library/Messages/chat.db` or the real Contacts stores, and nothing in
the repository holds a real name, number, or message.

`.github/workflows/ci.yml` runs all four on macOS for every push and pull
request; `.github/workflows/release.yml` builds the universal binary on a `v*`
tag, then writes `packaging/msgs.rb` with that version and checksum into
[gautham-v/homebrew-tap](https://github.com/gautham-v/homebrew-tap) (needs the
`TAP_TOKEN` secret, a fine-grained PAT with contents: write on the tap).

MIT licensed; see [LICENSE](../LICENSE).
