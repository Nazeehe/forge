//! Keyboard input routing: a small state machine.
//!
//! Normal keys flow to the active PTY. The configurable prefix key (default
//! `Ctrl-b`) opens a one-shot command mode; `Esc` cancels it. Paste
//! accumulation and modal/selection modes arrive in later slices.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Commands reachable from the prefix layer (v1 map; extended later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCommand {
    Quit,
    NextSession,
    PrevSession,
}

/// Where a key goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutedKey {
    /// Forward raw to the active PTY.
    Forward(KeyEvent),
    /// Prefix consumed; waiting for the command key.
    PrefixPending,
    /// A prefix command fired.
    Command(UserCommand),
    /// Prefix cancelled, back to normal.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Prefix,
}

pub struct InputRouter {
    mode: Mode,
}

impl InputRouter {
    pub fn new() -> Self {
        InputRouter { mode: Mode::Normal }
    }

    pub fn feed(&mut self, key: KeyEvent) -> RoutedKey {
        match self.mode {
            Mode::Normal => {
                if Self::is_prefix(&key) {
                    self.mode = Mode::Prefix;
                    RoutedKey::PrefixPending
                } else {
                    RoutedKey::Forward(key)
                }
            }
            Mode::Prefix => {
                self.mode = Mode::Normal;
                if key.code == KeyCode::Esc {
                    return RoutedKey::Cancelled;
                }
                if !key.modifiers.is_empty() {
                    return RoutedKey::Forward(key);
                }
                match key.code {
                    KeyCode::Char('q') => RoutedKey::Command(UserCommand::Quit),
                    KeyCode::Char('n') => RoutedKey::Command(UserCommand::NextSession),
                    KeyCode::Char('p') => RoutedKey::Command(UserCommand::PrevSession),
                    _ => RoutedKey::Forward(key),
                }
            }
        }
    }

    fn is_prefix(key: &KeyEvent) -> bool {
        // Compare code + modifiers only: real terminals vary kind/state
        // (press vs repeat) for the same physical chord.
        key.code == KeyCode::Char('b') && key.modifiers == KeyModifiers::CONTROL
    }
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn prefix_key() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn normal_keys_forward() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(key(KeyCode::Char('a'))), RoutedKey::Forward(key(KeyCode::Char('a'))));
        assert_eq!(r.feed(key(KeyCode::Enter)), RoutedKey::Forward(key(KeyCode::Enter)));
    }

    #[test]
    fn prefix_opens_command_mode() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
    }

    #[test]
    fn prefix_commands_fire_once() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(r.feed(key(KeyCode::Char('n'))), RoutedKey::Command(UserCommand::NextSession));
        // Back to normal: plain keys forward again.
        assert_eq!(r.feed(key(KeyCode::Char('x'))), RoutedKey::Forward(key(KeyCode::Char('x'))));

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(r.feed(key(KeyCode::Char('p'))), RoutedKey::Command(UserCommand::PrevSession));

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(r.feed(key(KeyCode::Char('q'))), RoutedKey::Command(UserCommand::Quit));
    }

    #[test]
    fn prefix_escape_cancels_and_unknown_keys_fall_through() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(r.feed(key(KeyCode::Esc)), RoutedKey::Cancelled);
        assert_eq!(r.feed(key(KeyCode::Char('z'))), RoutedKey::Forward(key(KeyCode::Char('z'))));
        // Unknown command key after prefix: no fire, back to normal.
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('z'))),
            RoutedKey::Forward(key(KeyCode::Char('z')))
        );
    }

    #[test]
    fn default_prefix_is_ctrl_b() {
        assert_eq!(prefix_key(), ctrl(KeyCode::Char('b')));
    }
}
