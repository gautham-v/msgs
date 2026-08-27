# msgs — agent notes

Rust + ratatui terminal client for iMessage on macOS.

## Layout
- `src/main.rs` — thin binary: clap entry, terminal setup/restore, event loop
- `src/lib.rs` — the crate everything else lives in, so `tests/` can drive it headlessly
- `src/app.rs` — `App` state, `Focus`, `Action`, the single `update()`; `keymap` → `update` → `ui` is the only input path
- `src/config.rs` — optional `~/.config/msgs/config.toml`; never fatal, problems become warnings
- `src/theme.rs` — named color slots (mockup palette), overridable per slot from config
- `src/keymap.rs` — focus-sensitive key → `Action`; `BINDINGS` also feeds the help modal and shortcuts bar
- `src/db/` — read-only access to `chat.db` (rusqlite + `imessage-database` for typedstream parsing)
- `src/ui/` — ratatui widgets: chat list, conversation, composer, palette, help, reactions, `db_error` (the first-run surface), `status` (the line and its toasts); `ui::compute`, `chat_list::Shape`, `conversation::Scroll`, and `ui::message::block` are pure layout functions with tests, and `ui::format` holds the string helpers (relative times, previews, wrapping, truncation)
- `src/media.rs` — attachments as pictures: `fit` (pure cell arithmetic), `Images` (the measure/encode cache, `ratatui-image` under it), HEIC through `sips` into `~/Library/Caches/msgs/attachments`, and `s` copying a file to `~/Downloads`
- `src/shell.rs` — the clipboard (`pbcopy`, then OSC 52), the browser, and `open` for an attachment; the only things msgs asks the rest of the machine to do
- `src/send.rs` — outbound messages via `osascript` → Messages.app; `Presence`, the `pgrep` probe behind the status line's Messages.app segment; tapbacks via `imsg` (`tapback` by message GUID first, `react` by chat rowid as the SIP-on fallback), plus `Pending` / `PendingTapback`, the optimistic echoes
- `src/watch.rs` — live updates: `notify` on the directory `chat.db` lives in, debounced, with a 2s timer as the fallback; `App::on_db_change` does the re-reading
- `src/search.rs` — the FTS5 message index at `~/Library/Application Support/msgs/index.db` (never inside chat.db): a worker thread builds it, tops it up from the live-update stream by `ROWID`, and answers `MATCH` queries
- `src/seen.rs` — the local read state: `~/Library/Application Support/msgs/seen.json`, a per-chat floor of unread already shown here; `Seen::apply` sets `Chat::unread` from `Chat::unread_count`, `Ctrl+U` marks all seen or gives it back, and Messages.app's own flags and badge are never touched
- `src/contacts.rs` — names for handles: the macOS AddressBook stores read read-only, normalized phone/email keys, and the stamped cache at `~/Library/Application Support/msgs/contacts.json`; `Contacts::apply` hangs a `Name` on every `Handle`, which is how names reach every pane at once
- `src/jump.rs` — what the `Ctrl+K` palette matches and shows: the filter, fuzzy chat/people matching (`nucleo-matcher`), and the result rows with their highlight ranges
- `docs/mockups.html` — the design target; match it

## Rules
- Never write to `chat.db`. Open it read-only (`?mode=ro` / `OpenFlags::SQLITE_OPEN_READ_ONLY`), and copy to a temp file if a lock blocks reads.
- Message content is private. Do not print message bodies, phone numbers, or names to logs, test output, or commit messages. Tests use a fixture DB under `tests/fixtures/`, never the real one — including `fixtures::address_book`, the synthetic Contacts store; never the real `~/Library/Application Support/AddressBook`.
- A person is named in one place: `Contacts::apply` fills `Handle::name`, and every pane reads `Handle::display_name` / `Handle::short_name`. Do not resolve a name inside a widget.
- Unread is counted in one place too: `Chat::unread_count` is the database's own answer and never moves, `Seen::apply` is the only thing that writes `Chat::unread`, and every badge, dot, and status-line total reads that. Messages.app's read flags and Dock badge cannot be cleared from here — say so rather than pretending otherwise.
- `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` must pass before a commit.
- Keep the UI calm: no feature creep beyond the mockups.
- Nothing at startup is fatal. A failure becomes a warning on `Status::warnings`, which is toasted once and then listed under `NOTES` in the help modal; an unreadable `chat.db` becomes `App::db_error` and the full-screen `ui::db_error` surface, never an exit.
- `Focus::DbError` is never assigned to `App::focus`. `App::key_focus` reports it while `db_error` is set and no overlay is up, which is how the first-run surface gets its own keys without disturbing the pane focus the app goes back to.
- `--check` prints paths and counts only. No name, number, or message body may reach it.
- An optimistic chip is drawn over the loaded page, never written into it: `App::pending_tapbacks` reaches the blocks through `ui::message::Ctx::tapbacks`, so `App::message_rows` stays exactly what `chat.db` handed over and reconciling is a comparison rather than an undo.
- A block's height and its drawing must come from one number. `ui::message::block` reserves rows for a picture from `Images::cells`, and `Images::render` draws into exactly those rows; never let the two compute a size separately.
