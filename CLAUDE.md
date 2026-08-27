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
- `src/ui/` — ratatui widgets: chat list, conversation, composer, palette, help; `ui::compute`, `chat_list::Shape`, `conversation::Scroll`, and `ui::message::block` are pure layout functions with tests, and `ui::format` holds the string helpers (relative times, previews, wrapping, truncation)
- `src/shell.rs` — the clipboard (`pbcopy`, then OSC 52) and the browser (`open`); the only two things msgs asks the rest of the machine to do
- `src/send.rs` — outbound messages via `osascript` → Messages.app; tapbacks via `imsg`
- `src/watch.rs` — live updates: `notify` on the directory `chat.db` lives in, debounced, with a 2s timer as the fallback; `App::on_db_change` does the re-reading
- `src/search.rs` — FTS5 index kept next to the app (not inside chat.db)
- `docs/mockups.html` — the design target; match it

## Rules
- Never write to `chat.db`. Open it read-only (`?mode=ro` / `OpenFlags::SQLITE_OPEN_READ_ONLY`), and copy to a temp file if a lock blocks reads.
- Message content is private. Do not print message bodies, phone numbers, or names to logs, test output, or commit messages. Tests use a fixture DB under `tests/fixtures/`, never the real one.
- `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` must pass before a commit.
- Keep the UI calm: no feature creep beyond the mockups.
