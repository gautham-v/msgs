//! Key bindings: the one place a `KeyEvent` turns into an [`Action`].
//!
//! Bindings are focus-sensitive — `j` moves the selection in a list but types a
//! letter in the composer — so [`resolve`] takes the current [`Focus`] as well
//! as the key. The same table drives the `?` help modal and the shortcuts bar,
//! so the documented keys and the working keys cannot drift apart.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, Focus};

/// One row in the help modal.
pub struct Binding {
    /// Keys as the user should read them, e.g. `"Ctrl+K"`.
    pub keys: &'static str,
    /// What the key does.
    pub description: &'static str,
    /// Where the binding applies.
    pub scope: &'static str,
}

/// Every binding, in the order the help modal lists them.
///
/// Rows are grouped by scope and each scope appears exactly once, because the
/// help modal opens a heading every time the scope changes: a table that
/// interleaved them would print `GLOBAL` four times.
pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "Tab",
        description: "switch focus: chat list → conversation → composer",
        scope: "global",
    },
    Binding {
        keys: "Enter",
        description: "open chat / send message",
        scope: "global",
    },
    Binding {
        keys: "Ctrl+K",
        description: "jump palette: chats, people, message search",
        scope: "global",
    },
    Binding {
        keys: "Ctrl+N",
        description: "new message to a number or address, via the palette",
        scope: "global",
    },
    Binding {
        keys: "Ctrl+B",
        description: "toggle the chat list",
        scope: "global",
    },
    Binding {
        keys: "Ctrl+T",
        description: "cycle the theme: dark / light / system / terminal",
        scope: "global",
    },
    Binding {
        keys: "Esc",
        description: "close the overlay / leave the composer",
        scope: "global",
    },
    Binding {
        keys: "?",
        description: "this help",
        scope: "global",
    },
    Binding {
        keys: "q / Ctrl+C",
        description: "quit",
        scope: "global",
    },
    Binding {
        keys: "↑ ↓ / k j",
        description: "select chat or message",
        scope: "list",
    },
    Binding {
        keys: "PgUp PgDn",
        description: "page through the conversation",
        scope: "list",
    },
    Binding {
        keys: "g / G",
        description: "jump to top / bottom",
        scope: "list",
    },
    Binding {
        keys: "Ctrl+U",
        description: "mark everything seen here, or give the unread back",
        scope: "list",
    },
    Binding {
        keys: "any letter",
        description: "start typing straight into the composer",
        scope: "list",
    },
    Binding {
        keys: "/",
        description: "filter chats by name",
        scope: "chat list",
    },
    Binding {
        keys: "→",
        description: "open the selected chat",
        scope: "chat list",
    },
    Binding {
        keys: "/",
        description: "open the jump palette",
        scope: "conversation",
    },
    Binding {
        keys: "←",
        description: "back to the chat list",
        scope: "conversation",
    },
    Binding {
        keys: "→ / i",
        description: "start typing in the composer",
        scope: "conversation",
    },
    Binding {
        keys: "o",
        description: "open the selected attachment",
        scope: "conversation",
    },
    Binding {
        keys: "s",
        description: "save the selected attachment",
        scope: "conversation",
    },
    Binding {
        keys: "h / l",
        description: "previous / next attachment on the selected message",
        scope: "conversation",
    },
    Binding {
        keys: "O",
        description: "open every attachment on the selected message",
        scope: "conversation",
    },
    Binding {
        keys: "r",
        description: "quote the selected message in a reply",
        scope: "conversation",
    },
    Binding {
        keys: "y",
        description: "copy the selected message",
        scope: "conversation",
    },
    Binding {
        keys: "Ctrl+L",
        description: "open the first link in the selected message",
        scope: "conversation",
    },
    Binding {
        keys: "Ctrl+A",
        description: "attach a file",
        scope: "composer",
    },
    Binding {
        keys: "@",
        description: "pick a file to attach, at the start of a word",
        scope: "composer",
    },
    Binding {
        keys: "Alt+Enter",
        description: "newline without sending (Shift+Enter where supported)",
        scope: "composer",
    },
    Binding {
        keys: "↑ ↓ / Tab",
        description: "move through the files",
        scope: "file picker",
    },
    Binding {
        keys: "/",
        description: "go into the highlighted directory",
        scope: "file picker",
    },
    Binding {
        keys: "Enter",
        description: "attach the highlighted file",
        scope: "file picker",
    },
    Binding {
        keys: "Esc",
        description: "close the picker, keeping the typed @",
        scope: "file picker",
    },
    Binding {
        keys: "Ctrl+W",
        description: "delete the word before the cursor",
        scope: "text field",
    },
    Binding {
        keys: "Ctrl+U",
        description: "clear the whole field",
        scope: "text field",
    },
    Binding {
        keys: "Tab",
        description: "cycle the filter: all / chats / messages / photos",
        scope: "palette",
    },
    Binding {
        keys: "r / Enter",
        description: "try to open chat.db again",
        scope: "first run",
    },
];

/// The condensed bar along the bottom of the screen: `(keys, label)` pairs.
pub const SHORTCUT_BAR: &[(&str, &str)] = &[
    ("Tab", "focus list/convo"),
    ("↑↓", "select"),
    ("Enter", "open/send"),
    ("Ctrl+K", "jump / search"),
    ("o", "open attachment"),
    ("y", "copy"),
    ("?", "help"),
];

/// Map a key press to an action, given what currently has focus.
///
/// Returns `None` when the key is not bound in this context. In a pane —
/// the chat list or the conversation — a printable character that nothing
/// there claims is the one exception: it falls through to the composer, so
/// a reply can be started by typing it.
#[must_use]
pub fn resolve(key: KeyEvent, focus: Focus) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Bindings that win everywhere, including while typing. The first-run
    // surface is the exception: there is no conversation behind it to attach
    // to or open a link from, so only the keys that still mean
    // something over it (quit, the palette, the theme) get through and the
    // rest stay dead.
    if ctrl {
        match key.code {
            KeyCode::Char('c') => return Some(Action::Quit),
            KeyCode::Char('k') => return Some(Action::OpenPalette),
            KeyCode::Char('t') => return Some(Action::CycleTheme),
            KeyCode::Char('b') if focus != Focus::DbError => {
                return Some(Action::ToggleChatList);
            }
            KeyCode::Char('a') if focus != Focus::DbError => return Some(Action::Attach),
            KeyCode::Char('n') if focus != Focus::DbError => return Some(Action::NewChat),
            KeyCode::Char('l') if focus != Focus::DbError => return Some(Action::OpenLink),
            _ => {}
        }
    }

    match focus {
        Focus::DbError => db_error_keys(key),
        Focus::Help => help_keys(key),
        Focus::Palette => palette_keys(key, ctrl, alt, shift),
        Focus::FilePicker => file_picker_keys(key, ctrl, alt, shift),
        Focus::Composer => text_entry_keys(key, ctrl, alt, shift),
        Focus::ChatList | Focus::Conversation => navigation_keys(key, focus, ctrl, alt, shift),
    }
}

/// The palette is a text field with a key of its own: `Tab` cycles the
/// filter instead of moving focus. (`Ctrl+N` is global, and inside the
/// palette it addresses a new message to what has been typed.)
fn palette_keys(key: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> Option<Action> {
    match key.code {
        KeyCode::Tab | KeyCode::BackTab => Some(Action::PaletteFilter),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        _ => text_entry_keys(key, ctrl, alt, shift),
    }
}

/// The `@` picker is a list steered from a text field: `Tab` and the arrows
/// move through the files instead of moving focus, and everything else is
/// still typed into the draft, where it narrows the list.
fn file_picker_keys(key: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> Option<Action> {
    match key.code {
        KeyCode::Tab => Some(Action::SelectNext),
        KeyCode::BackTab => Some(Action::SelectPrev),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        _ => text_entry_keys(key, ctrl, alt, shift),
    }
}

/// The first-run surface has three keys and no panes behind it, so every other
/// key is deliberately dead rather than steering a list nobody can see.
fn db_error_keys(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('r') | KeyCode::Enter => Some(Action::RetryDb),
        KeyCode::Char('?') => Some(Action::OpenHelp),
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        _ => None,
    }
}

fn help_keys(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
            Some(Action::Cancel)
        }
        KeyCode::Up | KeyCode::Char('k') => Some(Action::SelectPrev),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::SelectNext),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::ToTop),
        KeyCode::End | KeyCode::Char('G') => Some(Action::ToBottom),
        _ => None,
    }
}

/// Keys for the composer, the palette input, and the chat-list filter box.
fn text_entry_keys(key: KeyEvent, ctrl: bool, alt: bool, shift: bool) -> Option<Action> {
    if ctrl {
        return match key.code {
            KeyCode::Char('u') => Some(Action::ClearLine),
            KeyCode::Char('w') => Some(Action::DeleteWordBack),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Esc => Some(Action::Cancel),
        // Shift+Enter only reaches us on terminals that speak the kitty
        // keyboard protocol; Alt+Enter is the portable fallback.
        KeyCode::Enter if alt || shift => Some(Action::Newline),
        KeyCode::Enter => Some(Action::Activate),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Delete => Some(Action::DeleteForward),
        KeyCode::Left => Some(Action::CursorLeft),
        KeyCode::Right => Some(Action::CursorRight),
        KeyCode::Home => Some(Action::CursorHome),
        KeyCode::End => Some(Action::CursorEnd),
        KeyCode::Up => Some(Action::SelectPrev),
        KeyCode::Down => Some(Action::SelectNext),
        KeyCode::Tab => Some(Action::FocusNext),
        KeyCode::BackTab => Some(Action::FocusPrev),
        KeyCode::Char(c) => Some(Action::Insert(c)),
        _ => None,
    }
}

fn navigation_keys(
    key: KeyEvent,
    focus: Focus,
    ctrl: bool,
    alt: bool,
    shift: bool,
) -> Option<Action> {
    // `Ctrl+U` is only the read-state toggle out here; in a text field it is
    // still the line the shell would clear.
    if ctrl && key.code == KeyCode::Char('u') {
        return Some(Action::ToggleAllSeen);
    }
    let bound = match key.code {
        KeyCode::Tab => Some(Action::FocusNext),
        KeyCode::BackTab => Some(Action::FocusPrev),
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Enter => Some(Action::Activate),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::SelectPrev),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::SelectNext),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home => Some(Action::ToTop),
        KeyCode::End => Some(Action::ToBottom),
        KeyCode::Char('g') if !shift => Some(Action::ToTop),
        KeyCode::Char('G') => Some(Action::ToBottom),
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('?') => Some(Action::OpenHelp),
        KeyCode::Char('/') if focus == Focus::ChatList => Some(Action::StartFilter),
        KeyCode::Char('/') => Some(Action::OpenPalette),
        KeyCode::Char('o') if focus == Focus::Conversation => Some(Action::OpenAttachment),
        KeyCode::Char('s') if focus == Focus::Conversation => Some(Action::SaveAttachment),
        KeyCode::Char('O') if focus == Focus::Conversation => Some(Action::OpenAllAttachments),
        KeyCode::Char('h') if focus == Focus::Conversation => Some(Action::AttachmentPrev),
        KeyCode::Char('l') if focus == Focus::Conversation => Some(Action::AttachmentNext),
        KeyCode::Char('r') if focus == Focus::Conversation => Some(Action::QuoteReply),
        KeyCode::Char('y') if focus == Focus::Conversation => Some(Action::CopySelection),
        KeyCode::Char('i') if focus == Focus::Conversation => Some(Action::FocusComposer),
        // `←` `→` walk the panes the way `Tab` cycles them: the list opens
        // the chat on its right, the conversation goes back to the list or
        // on to the composer.
        KeyCode::Right if focus == Focus::ChatList => Some(Action::Activate),
        KeyCode::Left if focus == Focus::Conversation => Some(Action::FocusChatList),
        KeyCode::Right if focus == Focus::Conversation => Some(Action::FocusComposer),
        _ => None,
    };
    // The last resort, after every bound key has had its chance: a printable
    // character nothing out here claims is the start of a message, so it
    // goes to the composer along with the focus. `App::compose_char` is
    // where the list screen — which has no composer on it — opts out.
    bound.or_else(|| match key.code {
        KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => Some(Action::ComposeChar(c)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_quits_from_every_focus() {
        for focus in [
            Focus::ChatList,
            Focus::Conversation,
            Focus::Composer,
            Focus::Palette,
            Focus::Help,
        ] {
            let action = resolve(with(KeyCode::Char('c'), KeyModifiers::CONTROL), focus);
            assert_eq!(action, Some(Action::Quit), "focus {focus:?}");
        }
    }

    #[test]
    fn q_quits_from_lists_but_types_in_the_composer() {
        assert_eq!(
            resolve(key(KeyCode::Char('q')), Focus::ChatList),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('q')), Focus::Composer),
            Some(Action::Insert('q'))
        );
    }

    #[test]
    fn ctrl_u_toggles_read_state_in_the_lists_and_clears_the_line_in_a_field() {
        for focus in [Focus::ChatList, Focus::Conversation] {
            assert_eq!(
                resolve(with(KeyCode::Char('u'), KeyModifiers::CONTROL), focus),
                Some(Action::ToggleAllSeen),
                "focus {focus:?}"
            );
        }
        for focus in [Focus::Composer, Focus::Palette] {
            assert_eq!(
                resolve(with(KeyCode::Char('u'), KeyModifiers::CONTROL), focus),
                Some(Action::ClearLine),
                "focus {focus:?}"
            );
        }
    }

    #[test]
    fn vim_keys_navigate_lists_and_type_in_the_composer() {
        assert_eq!(
            resolve(key(KeyCode::Char('j')), Focus::Conversation),
            Some(Action::SelectNext)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('j')), Focus::Palette),
            Some(Action::Insert('j'))
        );
    }

    #[test]
    fn enter_sends_but_modified_enter_makes_a_newline() {
        assert_eq!(
            resolve(key(KeyCode::Enter), Focus::Composer),
            Some(Action::Activate)
        );
        assert_eq!(
            resolve(with(KeyCode::Enter, KeyModifiers::ALT), Focus::Composer),
            Some(Action::Newline)
        );
        assert_eq!(
            resolve(with(KeyCode::Enter, KeyModifiers::SHIFT), Focus::Composer),
            Some(Action::Newline)
        );
    }

    #[test]
    fn overlays_open_from_anywhere() {
        assert_eq!(
            resolve(
                with(KeyCode::Char('k'), KeyModifiers::CONTROL),
                Focus::Composer
            ),
            Some(Action::OpenPalette)
        );
        assert_eq!(
            resolve(
                with(KeyCode::Char('b'), KeyModifiers::CONTROL),
                Focus::Palette
            ),
            Some(Action::ToggleChatList)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('?')), Focus::ChatList),
            Some(Action::OpenHelp)
        );
    }

    #[test]
    fn conversation_only_keys_do_not_act_in_the_chat_list() {
        assert_eq!(
            resolve(key(KeyCode::Char('o')), Focus::Conversation),
            Some(Action::OpenAttachment)
        );
        // Nothing in the chat list opens an attachment, so `o` is just the
        // first letter of a message there.
        assert_eq!(
            resolve(key(KeyCode::Char('o')), Focus::ChatList),
            Some(Action::ComposeChar('o'))
        );
    }

    #[test]
    fn the_first_run_surface_retries_and_quits_and_nothing_else() {
        assert_eq!(
            resolve(key(KeyCode::Char('r')), Focus::DbError),
            Some(Action::RetryDb)
        );
        assert_eq!(
            resolve(key(KeyCode::Enter), Focus::DbError),
            Some(Action::RetryDb)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('?')), Focus::DbError),
            Some(Action::OpenHelp)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('q')), Focus::DbError),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve(
                with(KeyCode::Char('c'), KeyModifiers::CONTROL),
                Focus::DbError
            ),
            Some(Action::Quit)
        );
        // Nothing to select and nothing to type into.
        assert_eq!(resolve(key(KeyCode::Down), Focus::DbError), None);
        assert_eq!(resolve(key(KeyCode::Char('x')), Focus::DbError), None);
        // And nothing behind it to attach to, react to, or open a link from.
        for code in ['a', 'l'] {
            assert_eq!(
                resolve(
                    with(KeyCode::Char(code), KeyModifiers::CONTROL),
                    Focus::DbError
                ),
                None,
                "Ctrl+{code} should be dead on the first-run surface"
            );
        }
        // The palette still opens over it, which is what draws it a surface.
        assert_eq!(
            resolve(
                with(KeyCode::Char('k'), KeyModifiers::CONTROL),
                Focus::DbError
            ),
            Some(Action::OpenPalette)
        );
    }

    #[test]
    fn the_file_picker_steers_a_list_and_still_types_into_the_draft() {
        // `Tab` moves through the files rather than moving focus, and the
        // arrows do what they do in any list.
        assert_eq!(
            resolve(key(KeyCode::Tab), Focus::FilePicker),
            Some(Action::SelectNext)
        );
        assert_eq!(
            resolve(key(KeyCode::BackTab), Focus::FilePicker),
            Some(Action::SelectPrev)
        );
        assert_eq!(
            resolve(key(KeyCode::Down), Focus::FilePicker),
            Some(Action::SelectNext)
        );
        // Everything else is still typing.
        assert_eq!(
            resolve(key(KeyCode::Char('r')), Focus::FilePicker),
            Some(Action::Insert('r'))
        );
        assert_eq!(
            resolve(key(KeyCode::Char('/')), Focus::FilePicker),
            Some(Action::Insert('/'))
        );
        assert_eq!(
            resolve(key(KeyCode::Enter), Focus::FilePicker),
            Some(Action::Activate)
        );
        assert_eq!(
            resolve(key(KeyCode::Esc), Focus::FilePicker),
            Some(Action::Cancel)
        );
        assert_eq!(
            resolve(key(KeyCode::Backspace), Focus::FilePicker),
            Some(Action::Backspace)
        );
    }

    #[test]
    fn an_unbound_letter_in_a_pane_starts_a_message() {
        for focus in [Focus::ChatList, Focus::Conversation] {
            assert_eq!(
                resolve(key(KeyCode::Char('m')), focus),
                Some(Action::ComposeChar('m')),
                "focus {focus:?}"
            );
            // Shift is still typing; a space starts a draft with a space.
            assert_eq!(
                resolve(with(KeyCode::Char('H'), KeyModifiers::SHIFT), focus),
                Some(Action::ComposeChar('H'))
            );
            assert_eq!(
                resolve(key(KeyCode::Char(' ')), focus),
                Some(Action::ComposeChar(' '))
            );
            // A modifier means it was meant as a command, not as text.
            assert_eq!(
                resolve(with(KeyCode::Char('m'), KeyModifiers::ALT), focus),
                None
            );
            assert_eq!(
                resolve(with(KeyCode::Char('x'), KeyModifiers::CONTROL), focus),
                None
            );
        }
        // Every key that was already bound still is: the fallback is last.
        assert_eq!(
            resolve(key(KeyCode::Char('j')), Focus::ChatList),
            Some(Action::SelectNext)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('q')), Focus::Conversation),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('/')), Focus::ChatList),
            Some(Action::StartFilter)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('y')), Focus::Conversation),
            Some(Action::CopySelection)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('?')), Focus::Conversation),
            Some(Action::OpenHelp)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('G')), Focus::Conversation),
            Some(Action::ToBottom)
        );
        // In the chat list `y` was never bound, so now it types.
        assert_eq!(
            resolve(key(KeyCode::Char('y')), Focus::ChatList),
            Some(Action::ComposeChar('y'))
        );
        // Nothing types into the first-run surface.
        assert_eq!(resolve(key(KeyCode::Char('z')), Focus::DbError), None);
    }

    #[test]
    fn left_and_right_walk_the_panes_but_not_the_text_fields() {
        assert_eq!(
            resolve(key(KeyCode::Right), Focus::ChatList),
            Some(Action::Activate)
        );
        assert_eq!(resolve(key(KeyCode::Left), Focus::ChatList), None);
        assert_eq!(
            resolve(key(KeyCode::Left), Focus::Conversation),
            Some(Action::FocusChatList)
        );
        assert_eq!(
            resolve(key(KeyCode::Right), Focus::Conversation),
            Some(Action::FocusComposer)
        );
        // The composer moves its cursor.
        assert_eq!(
            resolve(key(KeyCode::Left), Focus::Composer),
            Some(Action::CursorLeft)
        );
        assert_eq!(
            resolve(key(KeyCode::Right), Focus::Composer),
            Some(Action::CursorRight)
        );
        assert_eq!(
            resolve(key(KeyCode::Left), Focus::Palette),
            Some(Action::CursorLeft)
        );
    }

    #[test]
    fn every_binding_row_is_filled_in() {
        for binding in BINDINGS {
            assert!(!binding.keys.is_empty());
            assert!(!binding.description.is_empty());
            assert!(!binding.scope.is_empty());
        }
        assert!(!SHORTCUT_BAR.is_empty());
    }

    /// The help modal opens a heading every time the scope changes, so a scope
    /// that appears in two runs prints its heading twice.
    #[test]
    fn each_scope_is_one_contiguous_run() {
        let mut seen: Vec<&str> = Vec::new();
        let mut current = "";
        for binding in BINDINGS {
            if binding.scope == current {
                continue;
            }
            assert!(
                !seen.contains(&binding.scope),
                "scope {:?} appears in more than one run",
                binding.scope
            );
            seen.push(binding.scope);
            current = binding.scope;
        }
    }
}
