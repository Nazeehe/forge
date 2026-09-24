//! Keyboard input routing: a small state machine.
//!
//! Normal keys flow to the active PTY. The configurable prefix key (default
//! `Ctrl-b`) opens a one-shot command mode; `Esc` cancels it. Paste
//! accumulation and modal/selection modes arrive in later slices.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// How one `Ctrl-b <key>` row resolves. Exact characters fire a
/// command; the three structural rows cover the digit range, the
/// Enter key, and explicit help.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefixInvoke {
    /// One exact character with no modifiers held.
    Command(char, UserCommand),
    /// `1`–`9` with no modifiers: the digit picks the session index.
    Digit,
    /// Bare Enter: activate the fleet cursor.
    FleetEnter,
    /// `?`, with or without Shift: pin the which-key HUD.
    Help,
}

impl PrefixInvoke {
    fn matches(&self, key: &KeyEvent) -> bool {
        match self {
            PrefixInvoke::Command(c, _) => {
                key.code == KeyCode::Char(*c) && key.modifiers.is_empty()
            }
            PrefixInvoke::Digit => {
                matches!(key.code, KeyCode::Char('1'..='9')) && key.modifiers.is_empty()
            }
            PrefixInvoke::FleetEnter => {
                key.code == KeyCode::Enter && key.modifiers.is_empty()
            }
            PrefixInvoke::Help => {
                key.code == KeyCode::Char('?')
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            }
        }
    }
}

/// One `Ctrl-b <key>` binding: the whole contract in one row.
///
/// Adding a shortcut means adding a row here, in display order: the
/// router fires it and the which-key HUD lists it, so neither can
/// drift. The `hud_covers_everything_the_router_accepts` test probes
/// the live router against the HUD and fails on any mismatch.
pub struct PrefixBinding {
    /// HUD display label: the key itself, or `"1–9"` / `"Enter"`.
    pub label: &'static str,
    /// Short human label, e.g. `"Next session"`.
    pub desc: &'static str,
    /// HUD section, e.g. `"Session"`.
    pub group: &'static str,
    pub(crate) invokes: PrefixInvoke,
}

/// Every prefix binding, in HUD display order.
pub static PREFIX_BINDINGS: &[PrefixBinding] = &[
    PrefixBinding { label: "c", desc: "New session", group: "Session", invokes: PrefixInvoke::Command('c', UserCommand::CreateSession) },
    PrefixBinding { label: "n", desc: "Next session", group: "Session", invokes: PrefixInvoke::Command('n', UserCommand::NextSession) },
    PrefixBinding { label: "p", desc: "Previous session", group: "Session", invokes: PrefixInvoke::Command('p', UserCommand::PrevSession) },
    PrefixBinding { label: "1–9", desc: "Go to session 1–9", group: "Session", invokes: PrefixInvoke::Digit },
    PrefixBinding { label: "j", desc: "Fleet cursor down", group: "Session", invokes: PrefixInvoke::Command('j', UserCommand::FleetStep(1)) },
    PrefixBinding { label: "k", desc: "Fleet cursor up", group: "Session", invokes: PrefixInvoke::Command('k', UserCommand::FleetStep(-1)) },
    PrefixBinding { label: "Enter", desc: "Activate fleet cursor", group: "Session", invokes: PrefixInvoke::FleetEnter },
    PrefixBinding { label: "x", desc: "Terminate session", group: "Session", invokes: PrefixInvoke::Command('x', UserCommand::TerminateSession) },
    PrefixBinding { label: "g", desc: "Manage groups", group: "Groups", invokes: PrefixInvoke::Command('g', UserCommand::ManageGroups) },
    PrefixBinding { label: "t", desc: "Switch tab", group: "View", invokes: PrefixInvoke::Command('t', UserCommand::SwitchTab) },
    PrefixBinding { label: "w", desc: "Toggle grid", group: "View", invokes: PrefixInvoke::Command('w', UserCommand::ToggleGrid) },
    PrefixBinding { label: "y", desc: "Toggle permission mode", group: "Safety", invokes: PrefixInvoke::Command('y', UserCommand::TogglePermissionMode) },
    PrefixBinding { label: "m", desc: "Telegram settings", group: "Settings", invokes: PrefixInvoke::Command('m', UserCommand::TelegramSettings) },
    PrefixBinding { label: "e", desc: "Theme picker", group: "Settings", invokes: PrefixInvoke::Command('e', UserCommand::ThemePicker) },
    PrefixBinding { label: "q", desc: "Quit", group: "App", invokes: PrefixInvoke::Command('q', UserCommand::Quit) },
    PrefixBinding { label: "h", desc: "Help manual", group: "App", invokes: PrefixInvoke::Command('h', UserCommand::HelpManual) },
    PrefixBinding { label: "?", desc: "All keys", group: "App", invokes: PrefixInvoke::Help },
];

/// Commands reachable from the prefix layer (v1 map; extended later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCommand {
    Quit,
    NextSession,
    PrevSession,
    CreateSession,
    /// Focus session by zero-based order index (`Ctrl-b 1` is index 0).
    SelectSession(usize),
    /// Move the sidebar fleet cursor (`Ctrl-b j` down, `Ctrl-b k` up).
    FleetStep(i32),
    /// Activate the sidebar fleet cursor (`Ctrl-b Enter`).
    FleetActivate,
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
    /// Open the theme picker (`theme.json` files under `~/.forge/themes`).
    ThemePicker,
    /// Extract the embedded HTML manual to /tmp and open it in the
    /// default browser.
    HelpManual,
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
    /// Explicit hotkey help (`Ctrl-b ?`): pin the which-key HUD open
    /// for browsing instead of firing a command.
    Help,
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

    /// True while the prefix key is waiting for its second key.
    pub fn is_pending(&self) -> bool {
        matches!(self.mode, Mode::Prefix)
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
                // Table-driven: the first matching row wins, so adding
                // a shortcut is adding a row (see [`PREFIX_BINDINGS`]).
                match PREFIX_BINDINGS.iter().find(|b| b.invokes.matches(&key)) {
                    Some(binding) => match binding.invokes {
                        PrefixInvoke::Command(_, cmd) => RoutedKey::Command(cmd),
                        PrefixInvoke::Digit => match key.code {
                            KeyCode::Char(d @ '1'..='9') => RoutedKey::Command(
                                UserCommand::SelectSession(d as usize - '1' as usize),
                            ),
                            _ => RoutedKey::Forward(key),
                        },
                        PrefixInvoke::FleetEnter => {
                            RoutedKey::Command(UserCommand::FleetActivate)
                        }
                        PrefixInvoke::Help => RoutedKey::Help,
                    },
                    None => RoutedKey::Forward(key),
                }
            }
        }
    }

    pub(crate) fn is_prefix(key: &KeyEvent) -> bool {
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
    fn prefix_fleet_keys_step_and_activate() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('j'))),
            RoutedKey::Command(UserCommand::FleetStep(1))
        );
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('k'))),
            RoutedKey::Command(UserCommand::FleetStep(-1))
        );
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Enter)),
            RoutedKey::Command(UserCommand::FleetActivate)
        );
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

        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('e'))),
            RoutedKey::Command(UserCommand::ThemePicker)
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
    fn retired_o_shortcut_falls_through_to_the_pane() {
        // The `Ctrl-b o` peers toggle is gone: `o` behaves like any
        // other unbound key after the prefix.
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('o'))),
            RoutedKey::Forward(key(KeyCode::Char('o')))
        );
        assert!(!r.is_pending());
    }

    #[test]
    fn prefix_table_has_no_ambiguous_rows() {
        let mut chars = std::collections::HashSet::new();
        let mut structural = 0;
        for binding in PREFIX_BINDINGS {
            match binding.invokes {
                PrefixInvoke::Command(c, _) => {
                    assert!(chars.insert(c), "duplicate prefix key {c:?}")
                }
                PrefixInvoke::Digit | PrefixInvoke::FleetEnter | PrefixInvoke::Help => {
                    structural += 1
                }
            }
        }
        assert_eq!(structural, 3, "the Digit, Enter, and Help rows");
    }

    #[test]
    fn prefix_h_routes_to_help_manual() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('h'))),
            RoutedKey::Command(UserCommand::HelpManual)
        );
        assert!(!r.is_pending(), "help resolves the prefix like a command");
    }

    #[test]
    fn prefix_question_opens_explicit_help() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert!(r.is_pending(), "prefix waits for its second key");
        assert_eq!(r.feed(key(KeyCode::Char('?'))), RoutedKey::Help);
        assert!(!r.is_pending(), "help resolves the prefix like a command");
    }

    #[test]
    fn prefix_shifted_question_opens_explicit_help() {
        // Real terminals report `?` with SHIFT held; it must still help.
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(r.feed(shifted), RoutedKey::Help);
    }

    #[test]
    fn router_reports_prefix_pending() {
        let mut r = InputRouter::new();
        assert!(!r.is_pending());
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert!(r.is_pending());
        assert_eq!(
            r.feed(key(KeyCode::Char('n'))),
            RoutedKey::Command(UserCommand::NextSession)
        );
        assert!(!r.is_pending(), "a fired command leaves prefix mode");
    }

    #[test]
    fn pending_clears_on_cancel_and_fallthrough() {
        let mut r = InputRouter::new();
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(r.feed(key(KeyCode::Esc)), RoutedKey::Cancelled);
        assert!(!r.is_pending());
        assert_eq!(r.feed(prefix_key()), RoutedKey::PrefixPending);
        assert_eq!(
            r.feed(key(KeyCode::Char('z'))),
            RoutedKey::Forward(key(KeyCode::Char('z')))
        );
        assert!(!r.is_pending(), "fallthrough leaves prefix mode");
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
