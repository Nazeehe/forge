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
    NewSession,
}

/// Encode a forwarded key as PTY input bytes. Keys with no useful encoding
/// (arrows, function keys, modified chords beyond the prefix) return `None`
/// and are dropped rather than mis-sent.
pub fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    match key.code {
        // SHIFT is inherent in producing this char (uppercase, `!`, ...);
        // any other modifier means a real chord with no plain encoding.
        KeyCode::Char(c)
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            Some(c.to_string().into_bytes())
        }
        KeyCode::Char(_) => None,
        _ if !key.modifiers.is_empty() => None,
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::Backspace => Some(b"\x7f".to_vec()),
        KeyCode::Esc => Some(b"\x1b".to_vec()),
        _ => None,
    }
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
                    KeyCode::Char('c') => RoutedKey::Command(UserCommand::NewSession),
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

    #[test]
    fn prefix_c_creates_sessions() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('c'))),
            RoutedKey::Command(UserCommand::NewSession)
        );
    }

    #[test]
    fn shift_char_keys_encode_to_pty_bytes() {
        // Real terminals report uppercase/symbol chars with SHIFT set;
        // the shift is inherent in the char and must not drop the key.
        let shift_s = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(encode_key(&shift_s), Some(b"S".to_vec()));
        let shift_bang = KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT);
        assert_eq!(encode_key(&shift_bang), Some(b"!".to_vec()));
        // Real chords still drop.
        let ctrl_s = KeyEvent::new(
            KeyCode::Char('S'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(encode_key(&ctrl_s), None);
    }

    #[test]
    fn keys_encode_to_pty_bytes() {
        assert_eq!(encode_key(&key(KeyCode::Char('a'))), Some(b"a".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Enter)), Some(b"\r".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Tab)), Some(b"\t".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Backspace)), Some(b"\x7f".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Esc)), Some(b"\x1b".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Char('é'))), Some("é".as_bytes().to_vec()));
        // No useful encoding: dropped, never mis-sent.
        assert_eq!(encode_key(&key(KeyCode::Up)), None);
        assert_eq!(encode_key(&key(KeyCode::F(1))), None);
        assert_eq!(encode_key(&ctrl(KeyCode::Char('a'))), None);
    }
}
