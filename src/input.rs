//! Keyboard input routing: a small state machine.
//!
//! Normal keys flow to the active PTY. The configurable prefix key (default
//! `Ctrl-b`) opens a one-shot command mode; `Esc` cancels it. Paste
//! accumulation and modal/selection modes arrive in later slices.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Commands reachable from the prefix layer (v1 map; extended later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCommand {
    Quit,
    NextSession,
    PrevSession,
    CreateSession,
    /// Focus session by zero-based order index (`Ctrl-b 1` is index 0).
    SelectSession(usize),
    /// Toggle the active session in/out of the shared `peers` group (4c).
    /// Full group management lives in the group dialog; this covers the
    /// manual ask/tell gate with one key.
    TogglePeerGroup,
    /// Open the keyboard-first group management dialog (new, members,
    /// rename, delete).
    ManageGroups,
    /// Cycle the active session's visible tab (agent <-> terminal).
    /// No-op for single-tab shell sessions.
    SwitchTab,
    /// Toggle the permission mode Off <-> Yolo (sidebar buttons set each
    /// directly with the mouse).
    TogglePermissionMode,
    /// Terminate the active session: it leaves its groups and drops out
    /// of the UI at once.
    TerminateSession,
    /// Toggle grid mode: every session tiles the main area at once.
    ToggleGrid,
    /// Open the Telegram mobile-transport settings dialog.
    TelegramSettings,
}

/// Encode a forwarded key as PTY input bytes (xterm-style). `app_cursor`
/// selects SS3 (`\x1bO…`) arrows for applications that request them
/// (e.g. lazygit); anything else uses CSI. Keys with no useful encoding
/// return `None` and are dropped rather than mis-sent. Numpad
/// application-keypad mode is not honored: crossterm reports no
/// numpad-distinct events, so there is nothing to switch on.
pub fn encode_key(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    let mods = key.modifiers;
    if mods.contains(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META) {
        return None;
    }
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);
    // xterm modifyOtherKeys-style parameter: 1 + shift + 2*alt + 4*ctrl.
    let param = || (1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl)).to_string();
    match key.code {
        KeyCode::Char(c) => {
            if ctrl && !alt {
                ctrl_byte(c).map(|b| vec![b])
            } else if ctrl && alt {
                ctrl_byte(c).map(|b| vec![0x1b, b])
            } else if alt {
                let mut out = vec![0x1b];
                out.extend_from_slice(c.to_string().as_bytes());
                Some(out)
            } else {
                // Bare or SHIFT-only: the shift is inherent in the char
                // (uppercase, `!`, ...).
                Some(c.to_string().into_bytes())
            }
        }
        KeyCode::Enter if alt && !ctrl => Some(b"\x1b\r".to_vec()),
        // Shift-only Enter: distinct CSI-u, never merged into submit.
        // (Shift+Alt / Ctrl chords stay dropped: no agreed encoding.)
        KeyCode::Enter if shift && !alt && !ctrl => Some(b"\x1b[13;2u".to_vec()),
        KeyCode::Enter if mods.is_empty() => Some(b"\r".to_vec()),
        KeyCode::Tab if shift => Some(b"\x1b[Z".to_vec()),
        KeyCode::Tab if mods.is_empty() => Some(b"\t".to_vec()),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace if mods.is_empty() => Some(b"\x7f".to_vec()),
        KeyCode::Esc if mods.is_empty() => Some(b"\x1b".to_vec()),
        KeyCode::Up | KeyCode::Down | KeyCode::Right | KeyCode::Left => {
            let letter = match key.code {
                KeyCode::Up => "A",
                KeyCode::Down => "B",
                KeyCode::Right => "C",
                _ => "D",
            };
            if mods.is_empty() && app_cursor {
                Some(format!("\x1bO{letter}").into_bytes())
            } else if mods.is_empty() {
                Some(format!("\x1b[{letter}").into_bytes())
            } else {
                Some(format!("\x1b[1;{}{letter}", param()).into_bytes())
            }
        }
        KeyCode::Home | KeyCode::End => {
            let letter = if key.code == KeyCode::Home { "H" } else { "F" };
            if mods.is_empty() {
                Some(format!("\x1b[{letter}").into_bytes())
            } else {
                Some(format!("\x1b[1;{}{letter}", param()).into_bytes())
            }
        }
        KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown => {
            let n = match key.code {
                KeyCode::Insert => "2",
                KeyCode::Delete => "3",
                KeyCode::PageUp => "5",
                _ => "6",
            };
            if mods.is_empty() {
                Some(format!("\x1b[{n}~").into_bytes())
            } else {
                Some(format!("\x1b[{n};{}~", param()).into_bytes())
            }
        }
        KeyCode::F(n) => f_key(n, mods, &param()),
        _ => None,
    }
}

/// Encode a mouse event for the pane. `column`/`row` are already 1-based
/// pane-grid cells (see `ui::translate_mouse`). Events the pane's mode does
/// not cover (releases in press-only mode, passive motion without
/// any-motion) return `None`.
pub fn encode_mouse(
    ev: &MouseEvent,
    mode: vt100::MouseProtocolMode,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as M;
    let (button, release) = match ev.kind {
        MouseEventKind::Down(b) => (mouse_button(b)?, false),
        MouseEventKind::Drag(b) => (mouse_button(b)? + 32, false),
        MouseEventKind::Up(b) => (mouse_button(b)?, true),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        MouseEventKind::Moved => (3, false),
    };
    let wanted = match ev.kind {
        MouseEventKind::Down(_) => !matches!(mode, M::None),
        MouseEventKind::Up(_) => !matches!(mode, M::None | M::Press),
        MouseEventKind::Drag(_) => matches!(mode, M::ButtonMotion | M::AnyMotion),
        MouseEventKind::Moved => matches!(mode, M::AnyMotion),
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
            !matches!(mode, M::None)
        }
    };
    if !wanted {
        return None;
    }
    let mods = ev.modifiers;
    if mods.contains(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META) {
        return None;
    }
    let mut code = button;
    if mods.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let (col, row) = (ev.column.max(1), ev.row.max(1));
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let end = if release { "m" } else { "M" };
            Some(format!("\x1b[<{code};{col};{row}{end}").into_bytes())
        }
        vt100::MouseProtocolEncoding::Utf8 => {
            let mut out = vec![0x1b, b'[', b'M'];
            push_utf8(&mut out, 32u32 + code as u32);
            push_utf8(&mut out, 32u32 + col as u32);
            push_utf8(&mut out, 32u32 + row as u32);
            Some(out)
        }
        vt100::MouseProtocolEncoding::Default => Some(vec![
            0x1b,
            b'[',
            b'M',
            32u8.saturating_add(code),
            32u8.saturating_add(col.min(223 - 32) as u8),
            32u8.saturating_add(row.min(223 - 32) as u8),
        ]),
    }
}

fn push_utf8(out: &mut Vec<u8>, v: u32) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(char::from_u32(v).unwrap_or('\u{FFFD}').encode_utf8(&mut buf).as_bytes());
}

/// Encode a paste for the pane: wrapped in bracketed-paste markers only
/// when the application requested them, raw otherwise.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

fn mouse_button(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
    }
}

/// Ctrl+letter as a C0 control byte, case-insensitive.
fn ctrl_byte(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    if ('a'..='z').contains(&lower) {
        Some(lower as u8 - b'a' + 1)
    } else {
        None
    }
}

fn f_key(n: u8, mods: KeyModifiers, param: &str) -> Option<Vec<u8>> {
    let plain = match n {
        1 => "\x1bOP",
        2 => "\x1bOQ",
        3 => "\x1bOR",
        4 => "\x1bOS",
        5 => "\x1b[15~",
        6 => "\x1b[17~",
        7 => "\x1b[18~",
        8 => "\x1b[19~",
        9 => "\x1b[20~",
        10 => "\x1b[21~",
        11 => "\x1b[23~",
        12 => "\x1b[24~",
        _ => return None,
    };
    if mods.is_empty() {
        return Some(plain.as_bytes().to_vec());
    }
    match n {
        1..=4 => {
            let letter = &plain[2..];
            Some(format!("\x1b[1;{param}{letter}").into_bytes())
        }
        5..=12 => {
            let n = &plain[2..plain.len() - 1];
            Some(format!("\x1b[{n};{param}~").into_bytes())
        }
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
                    KeyCode::Char('c') => RoutedKey::Command(UserCommand::CreateSession),
                    KeyCode::Char('g') => RoutedKey::Command(UserCommand::ManageGroups),
                    KeyCode::Char('o') => RoutedKey::Command(UserCommand::TogglePeerGroup),
                    KeyCode::Char('t') => RoutedKey::Command(UserCommand::SwitchTab),
                    KeyCode::Char('y') => RoutedKey::Command(UserCommand::TogglePermissionMode),
                    KeyCode::Char('x') => RoutedKey::Command(UserCommand::TerminateSession),
                    KeyCode::Char('w') => RoutedKey::Command(UserCommand::ToggleGrid),
                    KeyCode::Char('m') => RoutedKey::Command(UserCommand::TelegramSettings),
                    KeyCode::Char(d @ '1'..='9') => {
                        RoutedKey::Command(UserCommand::SelectSession(d as usize - '1' as usize))
                    }
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
    fn prefix_digits_select_sessions() {
        let mut r = InputRouter::new();
        for (digit, index) in [('1', 0), ('5', 4), ('9', 8)] {
            assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
            assert_eq!(
                r.feed(key(KeyCode::Char(digit))),
                RoutedKey::Command(UserCommand::SelectSession(index))
            );
        }
        // Zero is not a session: forwarded to the pane.
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('0'))),
            RoutedKey::Forward(key(KeyCode::Char('0')))
        );
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

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('g'))),
            RoutedKey::Command(UserCommand::ManageGroups)
        );

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('o'))),
            RoutedKey::Command(UserCommand::TogglePeerGroup)
        );

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('t'))),
            RoutedKey::Command(UserCommand::SwitchTab)
        );

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('y'))),
            RoutedKey::Command(UserCommand::TogglePermissionMode)
        );

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('x'))),
            RoutedKey::Command(UserCommand::TerminateSession)
        );

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('w'))),
            RoutedKey::Command(UserCommand::ToggleGrid)
        );

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('m'))),
            RoutedKey::Command(UserCommand::TelegramSettings)
        );
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
            RoutedKey::Command(UserCommand::CreateSession)
        );
    }

    #[test]
    fn shift_char_keys_encode_to_pty_bytes() {
        // Real terminals report uppercase/symbol chars with SHIFT set;
        // the shift is inherent in the char and must not drop the key.
        let shift_s = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(encode_key(&shift_s, false), Some(b"S".to_vec()));
        let shift_bang = KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT);
        assert_eq!(encode_key(&shift_bang, false), Some(b"!".to_vec()));
        // Ctrl still wins over shift: the chord, not the char.
        let ctrl_s = KeyEvent::new(
            KeyCode::Char('S'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(encode_key(&ctrl_s, false), Some(vec![0x13]));
    }

    #[test]
    fn keys_encode_to_pty_bytes() {
        assert_eq!(encode_key(&key(KeyCode::Char('a')), false), Some(b"a".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Enter), false), Some(b"\r".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Tab), false), Some(b"\t".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Backspace), false), Some(b"\x7f".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::Esc), false), Some(b"\x1b".to_vec()));
        assert_eq!(encode_key(&key(KeyCode::BackTab), false), Some(b"\x1b[Z".to_vec()));
        assert_eq!(
            encode_key(&key(KeyCode::Char('é')), false),
            Some("é".as_bytes().to_vec())
        );
        // No useful encoding: dropped, never mis-sent.
        assert_eq!(encode_key(&key(KeyCode::Null), false), None);
        assert_eq!(encode_key(&key(KeyCode::F(13)), false), None);
    }

    #[test]
    fn arrows_honor_application_cursor_mode() {
        let dirs = [
            (KeyCode::Up, "A"),
            (KeyCode::Down, "B"),
            (KeyCode::Right, "C"),
            (KeyCode::Left, "D"),
        ];
        for (code, letter) in dirs {
            assert_eq!(
                encode_key(&key(code), false),
                Some(format!("\x1b[{letter}").into_bytes()),
                "normal {code:?}"
            );
            assert_eq!(
                encode_key(&key(code), true),
                Some(format!("\x1bO{letter}").into_bytes()),
                "app-cursor {code:?}"
            );
        }
    }

    #[test]
    fn modified_special_keys_use_xterm_params() {
        let shift = KeyModifiers::SHIFT;
        let alt = KeyModifiers::ALT;
        let ctrlm = KeyModifiers::CONTROL;
        let ev = |code, m| KeyEvent::new(code, m);
        assert_eq!(encode_key(&ev(KeyCode::Up, shift), false), Some(b"\x1b[1;2A".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::Up, ctrlm), true), Some(b"\x1b[1;5A".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::Down, alt), false), Some(b"\x1b[1;3B".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::Home, KeyModifiers::NONE), false), Some(b"\x1b[H".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::End, shift), false), Some(b"\x1b[1;2F".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::Delete, ctrlm), false), Some(b"\x1b[3;5~".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::Insert, KeyModifiers::NONE), false), Some(b"\x1b[2~".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::PageUp, KeyModifiers::NONE), false), Some(b"\x1b[5~".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::PageDown, alt), false), Some(b"\x1b[6;3~".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::F(1), KeyModifiers::NONE), false), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::F(4), KeyModifiers::NONE), false), Some(b"\x1bOS".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::F(5), KeyModifiers::NONE), false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::F(12), KeyModifiers::NONE), false), Some(b"\x1b[24~".to_vec()));
        assert_eq!(encode_key(&ev(KeyCode::F(5), ctrlm), false), Some(b"\x1b[15;5~".to_vec()));
        // Shift-Tab is backtab even when reported as Tab.
        assert_eq!(encode_key(&ev(KeyCode::Tab, shift), false), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn paste_wraps_only_when_requested() {
        assert_eq!(paste_bytes("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
        assert_eq!(paste_bytes("hi", false), b"hi".to_vec());
    }

    #[test]
    fn mouse_modes_gate_kinds() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use vt100::{MouseProtocolEncoding, MouseProtocolMode};
        let mev = |kind| MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let enc = MouseProtocolEncoding::Sgr;
        let down = MouseEventKind::Down(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        assert_eq!(encode_mouse(&mev(down), MouseProtocolMode::None, enc), None);
        assert!(encode_mouse(&mev(down), MouseProtocolMode::Press, enc).is_some());
        assert_eq!(encode_mouse(&mev(up), MouseProtocolMode::Press, enc), None);
        assert_eq!(encode_mouse(&mev(drag), MouseProtocolMode::Press, enc), None);
        assert!(encode_mouse(&mev(MouseEventKind::ScrollUp), MouseProtocolMode::Press, enc).is_some());
        assert!(encode_mouse(&mev(up), MouseProtocolMode::PressRelease, enc).is_some());
        assert!(encode_mouse(&mev(drag), MouseProtocolMode::ButtonMotion, enc).is_some());
        assert_eq!(encode_mouse(&mev(MouseEventKind::Moved), MouseProtocolMode::ButtonMotion, enc), None);
        assert!(encode_mouse(&mev(MouseEventKind::Moved), MouseProtocolMode::AnyMotion, enc).is_some());
    }

    #[test]
    fn mouse_sgr_and_default_encodings() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use vt100::{MouseProtocolEncoding, MouseProtocolMode};
        let mev = |kind| MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let mode = MouseProtocolMode::PressRelease;
        let down = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(
            encode_mouse(&mev(down), mode, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<0;10;5M".to_vec())
        );
        assert_eq!(
            encode_mouse(&mev(MouseEventKind::Up(MouseButton::Left)), mode, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<0;10;5m".to_vec())
        );
        assert_eq!(
            encode_mouse(&mev(MouseEventKind::ScrollUp), mode, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<64;10;5M".to_vec())
        );
        let shift = MouseEvent {
            modifiers: KeyModifiers::SHIFT,
            ..mev(down)
        };
        assert_eq!(
            encode_mouse(&shift, mode, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<4;10;5M".to_vec())
        );
        assert_eq!(
            encode_mouse(&mev(down), mode, MouseProtocolEncoding::Default),
            Some(vec![0x1b, b'[', b'M', 32, 42, 37])
        );
        // Small coordinates encode identically in UTF-8 mode.
        assert_eq!(
            encode_mouse(&mev(down), mode, MouseProtocolEncoding::Utf8),
            encode_mouse(&mev(down), mode, MouseProtocolEncoding::Default)
        );
    }

    #[test]
    fn ctrl_alt_chords_and_release_guard() {
        let ctrlm = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let ev = |code, m| KeyEvent::new(code, m);
        assert_eq!(encode_key(&ev(KeyCode::Char('a'), ctrlm), false), Some(vec![0x01]));
        assert_eq!(encode_key(&ev(KeyCode::Char('c'), ctrlm), false), Some(vec![0x03]));
        assert_eq!(encode_key(&ev(KeyCode::Char('Z'), ctrlm), false), Some(vec![0x1a]));
        assert_eq!(encode_key(&ev(KeyCode::Char('a'), alt), false), Some(b"\x1ba".to_vec()));
        assert_eq!(
            encode_key(&ev(KeyCode::Char('c'), ctrlm | alt), false),
            Some(b"\x1b\x03".to_vec())
        );
        assert_eq!(encode_key(&ev(KeyCode::Enter, alt), false), Some(b"\x1b\r".to_vec()));
        // Shift-only Enter stays distinct (CSI-u) instead of dropping:
        // agents like Claude read it as newline, not submit.
        assert_eq!(
            encode_key(&ev(KeyCode::Enter, KeyModifiers::SHIFT), false),
            Some(b"\x1b[13;2u".to_vec())
        );
        // Releases must never double-send a press.
        let release = KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(encode_key(&release, false), None);
    }
}
