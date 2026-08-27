# msgs — agent notes

Rust + ratatui terminal client for iMessage on macOS.

## Layout
- `src/main.rs` — clap entry, terminal setup, event loop
- `src/db/` — read-only access to `chat.db` (rusqlite + `imessage-database` for typedstream parsing)
- `src/ui/` — ratatui widgets: chat list, conversation, composer, palette, help
- `src/send.rs` — outbound messages via `osascript` → Messages.app; tapbacks via `imsg`
- `src/search.rs` — FTS5 index kept next to the app (not inside chat.db)
- `docs/mockups.html` — the design target; match it

## Rules
- Never write to `chat.db`. Open it read-only (`?mode=ro` / `OpenFlags::SQLITE_OPEN_READ_ONLY`), and copy to a temp file if a lock blocks reads.
- Message content is private. Do not print message bodies, phone numbers, or names to logs, test output, or commit messages. Tests use a fixture DB under `tests/fixtures/`, never the real one.
- `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` must pass before a commit.
- Keep the UI calm: no feature creep beyond the mockups.
