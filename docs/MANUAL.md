# msgs manual

The long version of the [README](../README.md).

A terminal client for iMessage on macOS. Read everything from `~/Library/Messages/chat.db`, send through Messages.app, all inside your terminal.

Built with Rust + [ratatui](https://ratatui.rs). Visual grammar borrowed from Grok Build: a transcript with one accent, the sender's name in bold, the clock right-aligned in a column of its own, a rule for a composer, and one footer.

Design mockups: [`mockups.html`](mockups.html).

```
  / filter                   │ Alex Nakamura · iMessage · +1 (55…  2 unread in 2 chats · ? help
                             │──────────────────────────────────────────────────────────────────
  Fixture Group     5/18/22  │ May 18, 2022
  Bailey: named the conver…  │
                             │
  Alex Nakamura     5/18/22  │ Alex  first fixture message                             12:29 AM
  still unread               │         😂  Alex   ❤️  You
                             │       recovered from the typedstream                    12:30 AM
  iMessage;-;+15550000009    │
                             │ You                                                     12:31 AM
                             │      ┄ 📷  photo.png · 2.0 KB · (not downloaded on t… ┄
                             │      Read 12:33 AM
                             │
                             │ Alex  still unread                                      12:34 AM
                             │
                             │
                             │
                             │
                             │
                             │
                             │
                             │
                             │
                             │ ╭──────────────────────────────────────────────────────────────╮
                             │ │ ❯ Message Alex Nakamura                                      │
                             │ ╰──────────────────────────────────────────────────────────────╯
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

## Run

```
cargo run --release
```

Flags:

| Flag | Meaning |
|---|---|
| `--db <PATH>` | read this database instead of `~/Library/Messages/chat.db` |
| `--config <PATH>` | read this config file instead of the default |
| `--no-mouse` | do not capture the mouse; the terminal's own selection works instead |
| `--theme <NAME>` | `dark`, `light`, `system`, or `terminal`; overrides `base` in the config's `[theme]` |
| `--redact` | mask phone numbers and addresses on screen (`+1 (•••) •••-••39`), for a demo or a screenshot; names and message bodies still show |
| `--check` | print a readiness report (Full Disk Access, database, row counts, unread, read state, live updates, Messages.app and whether it is running, `osascript`, search index, contacts, pins, terminal graphics, GIF playback) and exit |
| `--no-index` | do not build or use the full-text message index |
| `--no-images` | do not draw pictures inline; show every attachment as a chip |
| `--no-animate` | do not play animated GIFs; show the first frame and leave it there |
| `--no-contacts` | do not read Contacts; show numbers and addresses instead of names |
| `--no-pins` | do not read Messages.app's pinned conversations; list every chat by recency |
| `--no-link-previews` | do not show the link previews Messages stored; leave the URL alone |
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
| `Ctrl+N` | new message to a number or address, via the palette |
| `Ctrl+B` | toggle the chat list |
| `Ctrl+T` | cycle the theme: dark / light / system / terminal |
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
| `any letter` | start typing straight into the composer |

**Chat list**

| Key | Action |
|---|---|
| `/` | filter chats by name |
| `→` | open the selected chat |

**Conversation**

| Key | Action |
|---|---|
| `/` | open the jump palette |
| `←` | back to the chat list |
| `→` / `i` | start typing in the composer |
| `o` | open the selected attachment |
| `s` | save the selected attachment |
| `r` | quote the selected message in a reply |
| `y` | copy the selected message |
| `Ctrl+L` | open the first link in the selected message |

**Composer**

| Key | Action |
|---|---|
| `Ctrl+A` | attach a file |
| `@` | pick a file to attach, at the start of a word |
| `Alt+Enter` | newline without sending (Shift+Enter where supported) |

**File picker** — the list an `@` opens above the composer

| Key | Action |
|---|---|
| `↑` `↓` / `Tab` | move through the files |
| `/` | go into the highlighted directory |
| `Enter` | attach the highlighted file |
| `Esc` | close the picker, keeping the typed @ |

**Any text field** — the composer, the palette input, the chat-list filter

| Key | Action |
|---|---|
| `Ctrl+W` | delete the word before the cursor |
| `Ctrl+U` | clear the whole field |

**Jump palette**

| Key | Action |
|---|---|
| `Tab` | cycle the filter: all / chats / messages / photos |

**First run, while `chat.db` cannot be read**

| Key | Action |
|---|---|
| `r` / `Enter` | try to open chat.db again |

There are three ways to the composer without reaching for `Tab`: `→` from the
conversation, `i`, or simply typing. Any printable key a pane does not already
use — a letter, a digit, a space — goes to the composer along with the focus,
so a reply can be started by writing it. Every key that already meant
something still does: `j` and `k` still move, `y` still copies, `/` still
filters. The one place typing does nothing is the chat-list screen on a
terminal too narrow to dock the list, because no composer is drawn there.

`←` and `→` walk the same path `Tab` cycles: `→` from the chat list opens the
selected chat, `←` from the conversation goes back to the list — docking it
again if `Ctrl+B` had hidden it — and `→` goes on to the composer. In the
composer and the palette they stay cursor movement.

The mouse works too, unless `--no-mouse` is passed: click a chat row or a
message block to select it, click a link to open it, click in the composer to
put the text cursor where you clicked, click the `↓ N new` pill to go to what
it is counting, and roll the wheel over a pane to scroll it without moving
focus. The jump palette and the help modal are keyboard-only —
they swallow clicks so a stray one cannot act on the screen behind them.

Dragging across the conversation selects text, the way a terminal's own
selection does: everything between the two ends in reading order, tinted as the
pointer moves, and put on the clipboard the moment the button comes up. What is
copied is what was on screen — the visible rows, joined with newlines, each
trimmed of its trailing blanks, with neither the scrollbar's column nor the day
band in it. A picture comes across as the blank cells it covers rather than as
the sequence that drew it, and the tint stays off it. The next click, `Esc`, or
any scroll takes the selection away. Nothing else reads it: like `y`, the text
goes to the pasteboard and nowhere else, and `y` still copies the whole
selected message whichever way it was picked.

Because a press is also how a drag begins, a link, a picture, and the pill open
on the release rather than on the press — so a selection that starts on a link
selects the words instead of opening it. The block under the pointer is picked
as soon as the button goes down either way.

msgs captures the mouse to do all of this, which is what stops the terminal
from drawing its own selection over the app. `--no-mouse` (or `mouse = false`
in the config) hands the mouse back: no clicking, no wheel, no drag-select in
msgs, and the terminal's own selection and copy work everywhere again.

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
live updates, Messages.app and whether it is running, `osascript`, the
search index, Contacts, the terminal's graphics support, `sips`, and
`qlmanage`. It prints
paths and counts — never a name, a number, or a message.

## The corner

The app's chrome is the right end of the header row (the filter line, on a
narrow terminal's list screen): `? help`, and before it the unread total while
there is one — `3 unread in 2 chats · ? help` — and nothing when there is not.
No bar along the bottom, no rule, no row of its own. Anything transient — a save, a send that was refused, the theme
changing — takes the unread's place for two seconds and then gives it back;
failures are drawn in the error color. Whether Messages.app is running, what
the index is doing, and whether live updates are watching or polling are not
on screen: `--check` reports them, and a problem with any of them lands under
`NOTES` in the help modal.

## Copying

`y` puts the selected message on the pasteboard — `pbcopy`, or the OSC 52
escape over SSH. The receipt is the one piece of chrome that is not in the
corner: a single right-aligned line in the secondary gray, directly above the
composer's box, reading `copied 27 chars to clipboard` (`1 char`, in the
singular). It is a row of its own, taken off the bottom of the conversation
only while it is showing, so it never covers a message; it gives the row back
three seconds later, on the next keystroke, or on the next copy, whichever
comes first.

The count is all that is ever said. The text itself goes to the pasteboard and
nowhere else — not to the status line, not to a log. A copy that could not
happen at all is a status-line error like any other failure.

## Attachments

A picture in a thread is drawn where it was sent. msgs asks the terminal what it
can do the moment the alternate screen is up: Ghostty, Kitty, and WezTerm answer
with the kitty graphics protocol and get real pixels, iTerm2 gets its own inline
images, and everything else falls back to unicode half-blocks, which any
terminal can draw. Pictures are capped at ten rows and at forty-eight columns
and keep their aspect ratio, and the file name and size move onto the meta line
under them. `--no-images`, or `images = false` in the config, turns the whole
thing off and leaves every attachment as a chip.

A GIF plays where it was sent. Its frames are decoded and encoded once, on the
same thread the conversions run on, and then the event loop simply wakes when
the next one is due — the frames are already made, so a playing GIF costs a
lookup and a draw. Every frame is encoded at the size the still was measured
at, so a picture that starts moving never changes the height of the block it is
in, and a GIF that will not encode that way is left standing as its first frame.
The work is capped: at most 48 frames, and at most 24 MB of decoded frame data
for one file — a GIF over either cap shows its first frame and stays there, and
only the GIFs actually on screen are ever stepped. `--no-animate`, or
`animate = false` in the config, leaves every GIF on its first frame. All of it
needs a terminal that can draw pictures at all: half-blocks animate too, but a
terminal without kitty, iTerm2, or sixel graphics is drawing a coarse
approximation of one.

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
opens that one the same way — except a GIF, which goes to Quick Look instead:
`open` hands one to Preview.app, which lays its frames out as a list of pages
and never plays it, and Quick Look is the same preview the Finder gives a file
on the spacebar. A Mac without `qlmanage` falls back to `open`. `s` copies it
into
`~/Downloads` without ever overwriting anything — a name already taken gets
` (2)` before the extension. Both read the file and nothing else; `chat.db` is
untouched. `Ctrl+A` goes the other way and sends a file to the open
conversation.

Dragging a file from Finder onto the terminal is the same thing without the
typing. Dropping one types its path, which msgs reads as a path rather than as
a line of text: escaped spaces, single or double quotes, and `file://` URLs all
come out the same, `~` is expanded, and several files dropped at once stay in
the order they were dropped. Each one waits above the draft as a `📎 name` chip
until `Enter` sends it — the files first, one message each, and whatever was
typed as a message after them. `Esc` drops the chips and leaves the draft
alone. A path that is not a file on this Mac is not a drop at all and is typed
into the composer like any other paste; a paste that is not a path is typed
too, newlines included in the composer and flattened to spaces in the
one-line fields.

Whichever way a file is picked, Messages.app is the one that reads it, and it
is sandboxed: a file under `~/Desktop`, `~/Documents`, or `~/Downloads` is one
macOS would ask it permission for, and nothing can answer that prompt on a
script's behalf — the message would appear in the thread and never deliver. So
msgs first copies the file into `~/Library/Caches/msgs/outbox`, a place
Messages can read unasked, and sends that copy; copies older than a day are
swept out the next time something is sent.

An `@` at the start of a word in the composer goes the same way with a list
instead of a typed path. It opens a picker just above the send box, listing
what is in `~/Downloads`, `~/Desktop`, `~/Pictures`, `~/Documents`, and the
directory msgs was started in — most recently modified first, two hundred
entries at most, no hidden files, and nothing under `~/Library`. What is typed
after the `@` stays in the draft and narrows the list, matched fuzzily against
the path each entry is shown under; `↑` `↓` and `Tab` move, `/` goes into the
highlighted directory, and a `Backspace` on a `/` comes back out of it. `Enter`
takes the file, which lifts the `@` and everything typed after it back out of
the draft and hangs the file above the box as a chip: `📎 report.pdf`. Those
chips are sent, one message each, ahead of whatever was typed, so the words
about a photo arrive under it; a `Backspace` on an empty draft takes the last
one back off. An `@` after a letter — the one in an email address — is just a
character, and `Esc` closes the picker leaving the `@` where it was typed,
which is how a literal one is had at the start of a word.

## Link previews

When somebody sends a link, Messages.app fetches the page once — on the sending
or the receiving device — and archives what it found next to the message:
the title, the site's name, the page's own one-line summary, and the pictures it
pulled down. msgs reads that archive and nothing else. **It never opens a
socket.** A link Messages never previewed has no card, a preview stays exactly
as stale as Messages left it, and no page is ever told you read the message.

The card is drawn under the message, in the same column the rest of the block is
set in: the picture where the terminal can draw one, then the title, the site,
and one line of summary. Each row is truncated rather than wrapped, so a page
with a paragraph for a title cannot push the thread around, and rows the preview
has nothing for are simply not there. There is no box and no colour — the one
accent on the block stays on the URL itself, which keeps its own line above the
card, underlined the way every link is.

`o` on a message opens its attachment; a message whose only content is a link
has no file, so `o` opens the link in the browser instead. `Ctrl+L` does that
from anywhere, and a click on the URL does too. The card's picture is not a file
you can open or save — it belongs to the preview, not to the conversation — so
clicking it does nothing.

The pictures are ordinary attachment rows that Messages marks hidden and files
under a `.pluginPayloadAttachment` name with no MIME type, so msgs types them by
what their first bytes actually are and draws them through the same cache, at
the same cap, as any photo. `--no-images` leaves the card as its three lines of
text; `--no-link-previews`, or `link_previews = false` in the config, drops the
card entirely and never reads the payload at all.

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

A conversation is drawn as a transcript. Every message is the sender's name
as a gray label — `You` in the one accent, everybody else in the secondary
gray, neither in bold, so the words are the brightest thing on the line — the
body
wrapped to the pane, and the clock (`2:15 PM`) right-aligned in a column of its
own on the first line. Consecutive messages from one person within five minutes form a
run: the name is said once and the rest sit under the words — the name is a
column, and every row after the first of a message or a run is set in past it —
with just their clock, and a blank row opens each run and each day. The newest
message you sent carries `Delivered` or `Read 6:05 PM` on a gray line under
it, and one Messages could not send says `Not delivered` there in the error
color — the same red `Not Delivered` Messages.app shows under the bubble —
whichever message it is. A send `osascript` refused says `Failed — reason` on
the echo the moment it comes back. The receipt is on the newest of yours
and the older ones do not carry one, which is where Messages.app puts the receipt too.
A message edited on another device says `Edited` on that line; msgs cannot
edit one itself, but it can say the body changed. Replies quote what they
answer, and group events (renames, joins, leaves) are dim italic lines without
a name or a clock. Nothing is drawn on a background but the selected message.

Days are separated by a gray label at the left with a blank row under it, and
the day of whatever is at
the top of the pane is held on a row of its own between the chat header and
the messages — a row the messages never share, so nothing a message says is
hidden under it. When the separator itself is on screen that row stays blank
and lets it do the talking. Down the right edge, in the column the blocks keep clear, a
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

## Pinned conversations

The chats you pinned in Messages.app sit at the top of the list, under a
`PINNED` heading, with a middle dot in the gutter before the name so a pinned
row is still recognisable once you have scrolled past the heading. Inside each
section the order is the one the rest of the list uses: newest first.

The pins are not in `chat.db`. Every macOS to date leaves that database without
an `is_pinned` column at all, and Messages keeps the state in its own preference
file, `~/Library/Preferences/com.apple.messages.pinning.plist`. msgs reads that
file — read-only, never written, never copied — on startup and again on every
reload of the chat list, and tells the live-update watcher about it too, so a pin
made in Messages while msgs is open lands on the screen within a second.

Inside the file, a pinned person is a plain address and a pinned group is a
hex-encoded identifier with a small table behind it. An address is matched the
way Contacts entries are matched, so a number pinned as `(555) 000-0132` still
finds a chat stored as `+15550000132`; a group is matched against the ids the
chat row carries, of which `original_group_id` is the one that answers. A group
whose identifier has been rotated since it was pinned — a thread re-created, a
Mac restored — is simply not found, and stays in the recent section rather than
being guessed at.

Nothing about this can change what Messages itself shows: msgs never writes the
file, so pinning and unpinning still happen in Messages.app. A Mac that has
never pinned anything has no such file, which is not a problem and not a
warning; a file that will not parse leaves one line in the notes under `?` and
the list is ordered by recency alone. `--no-pins`, or `pins = false` in the
config, turns the whole thing off.

## Live updates

msgs keeps up with `chat.db` without Messages.app open. A `notify` watcher sits
on the directory the database lives in — the directory rather than the file,
because `chat.db-wal` is deleted and recreated around every checkpoint — and a
burst of writes is debounced into one re-read three tenths of a second after it
goes quiet. If no platform watcher will start, a two-second timer takes over;
`--check` says `watching chat.db` or `polling chat.db` accordingly.

A re-read asks the open conversation for its newest page. Rows already on
screen are replaced where they stand, so an edit or a tapback lands in the block
it belongs to; anything past them goes on the end. If you were at the newest
message the view follows it; if you were reading further back it stays where it
is and a `↓ 3 new` pill appears on the bottom edge, which you can click or clear
by pressing `G`. A message in some other thread moves that chat to the top of
the list and updates its preview and unread mark, and the chat-list selection
follows the conversation you were in rather than the row number it was at.

The watcher also has Messages.app's pin preference file on its list, so pinning
or unpinning a conversation over there re-sorts the list here without a
keystroke. See [Pinned conversations](#pinned-conversations).

## Read state

The chat list draws a bold name and the time in the accent on any chat with
incoming messages Messages has not marked read, and the corner of the header
adds them up:
`3 unread in 2 chats`, or nothing at all when there is nothing unread.

Opening a chat in msgs clears that mark — but only msgs's. `chat.db` is
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
`Ctrl+N` from anywhere opens the palette; `Enter` (or `Ctrl+N` again) with
something that looks like a phone number or an email addresses a
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
conversation. A file dragged from Finder skips the prompt and waits on the
composer as a chip; see [Attachments](#attachments).

## Config

Optional, at `~/.config/msgs/config.toml` (or `$XDG_CONFIG_HOME/msgs/config.toml`). Every key
is optional, and a malformed file is reported on the status line rather than being fatal.

```toml
show_chat_list = true    # show the chat list on startup
chat_list_width = 30     # columns, 18–60, never more than half the screen
page_step = 10           # rows PageUp / PageDown move a list by
mouse = true             # --no-mouse overrides this
images = true            # draw pictures inline; --no-images overrides this
animate = true           # play animated gifs; --no-animate overrides this
contacts = true          # read Contacts for names; --no-contacts overrides this
pins = true              # read Messages.app's pinned chats; --no-pins overrides this
link_previews = true     # show the link previews Messages stored; --no-link-previews overrides this

[theme]
base = "dark"            # "light", "system" to follow macOS, or "terminal" to match the terminal
# Any color slot, as "#rrggbb", "#rgb", or an ANSI index 0–255, on top of the base.
accent_me = "#5ea8ff"    # the one accent: your name, the selected chat, unread times, links
text_primary = "#c8c8c8"
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
| `animate` | `true` | bool | play animated GIFs inline, up to 48 frames and 24 MB of frames each; `--no-animate` overrides it to `false`, and so does anything that turns pictures off |
| `contacts` | `true` | bool | read Contacts for names; `--no-contacts` overrides it to `false` |
| `pins` | `true` | bool | read Messages.app's pinned conversations so pinned chats come first; `--no-pins` overrides it to `false` |
| `link_previews` | `true` | bool | show the link previews Messages already stored; `--no-link-previews` overrides it to `false` |
| `[theme] base` | `terminal` | `dark`, `light`, `system`, `terminal` | the palette to start from; `--theme` overrides it and `Ctrl+T` cycles it at runtime |
| `[theme]` | — | color per slot | any slot below, as `"#rrggbb"`, `"#rgb"`, or an ANSI index `0`–`255`, applied on top of `base` |

Slots: `bg_base`, `bg_light`, `bg_dark`, `bg_highlight`, `bg_hover`, `accent_me`,
`text_primary`, `text_secondary`, `gray`, `gray_dim`, `system`, `fuzzy`, `border`,
`border_active`, `error`.

`system` asks macOS (`defaults read -g AppleInterfaceStyle`, on its own thread,
every five seconds while it is the base) and follows the answer, so switching
the Mac's appearance switches msgs within a few seconds. Until the first answer
it draws dark.

`terminal` asks the terminal itself for its default background and foreground
(OSC 11 and OSC 10, once, as the alternate screen comes up) and builds the
palette on that: the conversation is drawn on exactly the terminal's ground,
message text in its foreground, and the chat list, the selection, the borders,
and the grays are steps between the two, so a warm or tinted terminal theme
stays warm in msgs. Accents come from the dark or light palette, whichever the
ground's brightness calls for, and slot overrides still apply on top. Ghostty,
kitty, iTerm2, WezTerm, and Terminal.app answer; a terminal that does not
gets its own background and foreground and the dark palette's everything else,
and says so under `NOTES`.

`Ctrl+T` changes the base for this run only; put it in the config to keep it.

A value out of range is clamped and said on the status line; an unknown key,
a bad color, or TOML that will not parse is a warning under `NOTES` in the help
modal and defaults are used. Nothing in the file can stop msgs from starting.

Below 90 columns the chat list cannot sit beside the conversation, so it
becomes a screen of its own: `Ctrl+B` (or `/`) swaps it in for the thread,
full width, the same rows; `Enter` opens the selected chat and `Esc` goes back.
A narrow terminal starts on that screen. `show_chat_list` only says whether
the list is docked when there is room.

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
- **Pinning is read, never set.** The pins come out of Messages.app's own
  preference file, which msgs only reads; pinning and unpinning still happen in
  Messages.app. A pinned group whose identifier has been rotated since — a
  thread re-created, a Mac restored — is not recognised and stays in the recent
  section, and Messages's own left-to-right pin order is not kept: pinned chats
  are sorted newest first like everything else.
- **No group management** — no renaming, no adding or removing people — and no
  deleting a message or a thread. Group events are read and drawn.
- **Sending needs Messages.app** signed in and `osascript` available. msgs
  launches it hidden; it never takes the screen.
- **Reactions cannot be sent from msgs.** Messages will not take a tapback
  from AppleScript. Every reaction is *read* and drawn regardless.
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
- **People are told apart by name, not color.** A group with two people
  Contacts has not named shows two addresses in bold; there is no per-person
  hue to fall back on.
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
