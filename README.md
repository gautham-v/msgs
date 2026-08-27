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

## Keys

| Key | Action |
|---|---|
| `Tab` | switch focus between chat list and conversation |
| `↑` `↓` / `j` `k` | select chat or message |
| `Enter` | open chat / send message |
| `Ctrl+K` | jump palette: chats, people, full-text message search |
| `Ctrl+B` | toggle chat list |
| `o` | open selected attachment |
| `Ctrl+R` | react to selected message |
| `y` | copy selected message |
| `?` | help |
| `q` / `Ctrl+C` | quit |
